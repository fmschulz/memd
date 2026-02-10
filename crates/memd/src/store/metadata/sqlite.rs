//! SQLite-backed metadata store
//!
//! Implements MetadataStore using SQLite with WAL mode for crash safety
//! and tenant isolation via indexes.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use super::{ChunkMetadata, IndexState, MetadataStore};
use crate::error::Result;
use crate::store::{normalize_query, FeedbackEntry, RelevanceLabel};
use crate::types::{ChunkId, ChunkStatus, ChunkType, TenantId};

/// SQLite-backed metadata store
///
/// Uses WAL mode for crash safety and concurrent readers.
/// Single writer protected by Mutex.
pub struct SqliteMetadataStore {
    conn: Mutex<Connection>,
}

impl SqliteMetadataStore {
    /// Open or create a SQLite metadata store
    ///
    /// Configures WAL mode, busy timeout, and initializes schema.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for crash safety + concurrent readers
        conn.pragma_update(None, "journal_mode", "WAL")?;

        // NORMAL synchronous is safe with WAL mode
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        // 5 second busy timeout to prevent SQLITE_BUSY errors
        conn.pragma_update(None, "busy_timeout", 5000)?;

        // 64MB cache for better read performance
        conn.pragma_update(None, "cache_size", -64000)?;

        // Enable foreign keys for referential integrity
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self {
            conn: Mutex::new(conn),
        };

        store.init_schema()?;

        Ok(store)
    }

    /// Initialize the database schema
    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        // Create chunks table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT,
                index_state TEXT NOT NULL DEFAULT 'indexed',
                index_attempts INTEGER NOT NULL DEFAULT 0,
                index_last_error TEXT,
                indexed_at_ms INTEGER,
                index_updated_at_ms INTEGER NOT NULL DEFAULT 0,
                UNIQUE(segment_id, ordinal)
            )",
            [],
        )?;

        Self::ensure_index_columns(&conn)?;

        // Critical: tenant_id index for isolation queries
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chunks_tenant
             ON chunks(tenant_id, status)",
            [],
        )?;

        // Secondary index for type + time queries
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chunks_tenant_type
             ON chunks(tenant_id, chunk_type, timestamp_created DESC)",
            [],
        )?;

        // Segment index for tombstone sync
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chunks_segment
             ON chunks(segment_id)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS feedback (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                tenant_id TEXT NOT NULL,
                query TEXT NOT NULL,
                chunk_id TEXT NOT NULL,
                relevance INTEGER NOT NULL,
                timestamp_ms INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_feedback_tenant_query
             ON feedback(tenant_id, query, timestamp_ms DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_feedback_tenant_chunk
             ON feedback(tenant_id, chunk_id)",
            [],
        )?;

        Ok(())
    }

    fn ensure_index_columns(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(chunks)")?;
        let rows = stmt.query_map([], |row| row.get::<usize, String>(1))?;
        let mut column_names = std::collections::HashSet::new();
        for name in rows {
            column_names.insert(name?);
        }

        Self::ensure_index_column(
            conn,
            &column_names,
            "index_state",
            "ALTER TABLE chunks ADD COLUMN index_state TEXT NOT NULL DEFAULT 'indexed'",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "index_attempts",
            "ALTER TABLE chunks ADD COLUMN index_attempts INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "index_last_error",
            "ALTER TABLE chunks ADD COLUMN index_last_error TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "indexed_at_ms",
            "ALTER TABLE chunks ADD COLUMN indexed_at_ms INTEGER",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "index_updated_at_ms",
            "ALTER TABLE chunks ADD COLUMN index_updated_at_ms INTEGER NOT NULL DEFAULT 0",
        )?;

        Ok(())
    }

    fn ensure_index_column(
        conn: &Connection,
        column_names: &std::collections::HashSet<String>,
        column_name: &str,
        alter_sql: &str,
    ) -> Result<()> {
        if !column_names.contains(column_name) {
            conn.execute(alter_sql, [])?;
        }
        Ok(())
    }

    /// Insert one feedback event.
    pub fn insert_feedback(&self, feedback: &FeedbackEntry) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO feedback (tenant_id, query, chunk_id, relevance, timestamp_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                feedback.tenant_id.as_str(),
                normalize_query(&feedback.query),
                feedback.chunk_id.to_string(),
                relevance_to_int(feedback.relevance),
                feedback.timestamp_ms,
            ],
        )?;
        Ok(())
    }

    /// Fetch feedback events for one tenant/query.
    pub fn list_feedback_for_query(
        &self,
        tenant_id: &TenantId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FeedbackEntry>> {
        let normalized = normalize_query(query);
        if normalized.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT query, chunk_id, relevance, timestamp_ms
             FROM feedback
             WHERE tenant_id = ?1 AND query = ?2
             ORDER BY timestamp_ms DESC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), normalized, limit as i64],
            |row| {
                let query: String = row.get(0)?;
                let chunk_id: String = row.get(1)?;
                let relevance: i64 = row.get(2)?;
                let timestamp_ms: i64 = row.get(3)?;
                Ok((query, chunk_id, relevance, timestamp_ms))
            },
        )?;

        let mut feedback = Vec::new();
        for row in rows {
            let (query, chunk_id_str, relevance_raw, timestamp_ms) = row?;
            let Ok(chunk_id) = ChunkId::parse(&chunk_id_str) else {
                continue;
            };
            let relevance = int_to_relevance(relevance_raw);
            feedback.push(FeedbackEntry {
                tenant_id: tenant_id.clone(),
                query,
                chunk_id,
                relevance,
                timestamp_ms,
            });
        }
        Ok(feedback)
    }

    /// Convert a database row to ChunkMetadata
    fn row_to_metadata(row: &rusqlite::Row) -> rusqlite::Result<ChunkMetadata> {
        let chunk_id_str: String = row.get(0)?;
        let tenant_id_str: String = row.get(1)?;
        let project_id: Option<String> = row.get(2)?;
        let segment_id: i64 = row.get(3)?;
        let ordinal: i32 = row.get(4)?;
        let chunk_type_str: String = row.get(5)?;
        let status_str: String = row.get(6)?;
        let timestamp_created: i64 = row.get(7)?;
        let hash: String = row.get(8)?;
        let source_uri: Option<String> = row.get(9)?;

        // Parse chunk_id
        let chunk_id = ChunkId::parse(&chunk_id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse tenant_id
        let tenant_id = TenantId::new(&tenant_id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse chunk_type
        let chunk_type = match chunk_type_str.as_str() {
            "code" => ChunkType::Code,
            "doc" => ChunkType::Doc,
            "trace" => ChunkType::Trace,
            "decision" => ChunkType::Decision,
            "plan" => ChunkType::Plan,
            "research" => ChunkType::Research,
            "message" => ChunkType::Message,
            "summary" => ChunkType::Summary,
            _ => ChunkType::Other,
        };

        // Parse status
        let status = match status_str.as_str() {
            "draft" => ChunkStatus::Draft,
            "final" => ChunkStatus::Final,
            "error" => ChunkStatus::Error,
            "deleted" => ChunkStatus::Deleted,
            _ => ChunkStatus::Final,
        };

        Ok(ChunkMetadata {
            chunk_id,
            tenant_id,
            project_id,
            segment_id: segment_id as u64,
            ordinal: ordinal as u32,
            chunk_type,
            status,
            timestamp_created,
            hash,
            source_uri,
        })
    }
}

fn relevance_to_int(relevance: RelevanceLabel) -> i64 {
    match relevance {
        RelevanceLabel::Relevant => 1,
        RelevanceLabel::Irrelevant => -1,
    }
}

fn int_to_relevance(value: i64) -> RelevanceLabel {
    if value < 0 {
        RelevanceLabel::Irrelevant
    } else {
        RelevanceLabel::Relevant
    }
}

impl MetadataStore for SqliteMetadataStore {
    fn insert(&self, metadata: &ChunkMetadata) -> Result<()> {
        self.insert_many(std::slice::from_ref(metadata))
    }

    fn insert_many(&self, metadata: &[ChunkMetadata]) -> Result<()> {
        if metadata.is_empty() {
            return Ok(());
        }

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO chunks (
                    chunk_id, tenant_id, project_id, segment_id, ordinal,
                    chunk_type, status, timestamp_created, hash, source_uri
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for row in metadata {
                stmt.execute(rusqlite::params![
                    row.chunk_id.to_string(),
                    row.tenant_id.as_str(),
                    row.project_id.as_deref(),
                    row.segment_id as i64,
                    row.ordinal as i32,
                    row.chunk_type.to_string(),
                    row.status.to_string(),
                    row.timestamp_created,
                    &row.hash,
                    row.source_uri.as_deref(),
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<ChunkMetadata>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT chunk_id, tenant_id, project_id, segment_id, ordinal,
                    chunk_type, status, timestamp_created, hash, source_uri
             FROM chunks
             WHERE tenant_id = ?1 AND chunk_id = ?2 AND status != 'deleted'",
        )?;

        let result = stmt.query_row(
            rusqlite::params![tenant_id.as_str(), chunk_id.to_string()],
            |row| Self::row_to_metadata(row),
        );

        match result {
            Ok(metadata) => Ok(Some(metadata)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn list(
        &self,
        tenant_id: &TenantId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT chunk_id, tenant_id, project_id, segment_id, ordinal,
                    chunk_type, status, timestamp_created, hash, source_uri
             FROM chunks
             WHERE tenant_id = ?1 AND status != 'deleted'
             ORDER BY timestamp_created DESC
             LIMIT ?2 OFFSET ?3",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), limit as i64, offset as i64],
            |row| Self::row_to_metadata(row),
        )?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    fn mark_deleted(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        let conn = self.conn.lock().unwrap();

        let rows_affected = conn.execute(
            "UPDATE chunks SET status = 'deleted'
             WHERE tenant_id = ?1 AND chunk_id = ?2 AND status != 'deleted'",
            rusqlite::params![tenant_id.as_str(), chunk_id.to_string()],
        )?;

        Ok(rows_affected > 0)
    }

    fn get_by_segment(&self, segment_id: u64) -> Result<Vec<ChunkMetadata>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT chunk_id, tenant_id, project_id, segment_id, ordinal,
                    chunk_type, status, timestamp_created, hash, source_uri
             FROM chunks
             WHERE segment_id = ?1
             ORDER BY ordinal ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![segment_id as i64], |row| {
            Self::row_to_metadata(row)
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    fn count_by_status(&self, tenant_id: &TenantId) -> Result<(usize, usize)> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT status, COUNT(*) as cnt
             FROM chunks
             WHERE tenant_id = ?1
             GROUP BY status",
        )?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str()], |row| {
            let status: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((status, count as usize))
        })?;

        let mut active = 0usize;
        let mut deleted = 0usize;

        for row in rows {
            let (status, count) = row?;
            if status == "deleted" {
                deleted = count;
            } else {
                active += count;
            }
        }

        Ok((active, deleted))
    }

    fn get_deleted_chunk_ids(&self, tenant_id: &TenantId) -> Result<Vec<ChunkId>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn
            .prepare("SELECT chunk_id FROM chunks WHERE tenant_id = ?1 AND status = 'deleted'")?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str()], |row| {
            let chunk_id_str: String = row.get(0)?;
            Ok(chunk_id_str)
        })?;

        let mut chunk_ids = Vec::new();
        for row in rows {
            let chunk_id_str = row?;
            if let Ok(chunk_id) = ChunkId::parse(&chunk_id_str) {
                chunk_ids.push(chunk_id);
            }
        }

        Ok(chunk_ids)
    }

    fn mark_index_pending(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
        now_ms: i64,
    ) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE chunks
                 SET index_state = ?3,
                     index_attempts = 0,
                     index_last_error = NULL,
                     indexed_at_ms = NULL,
                     index_updated_at_ms = ?4
                 WHERE tenant_id = ?1 AND chunk_id = ?2",
            )?;
            for chunk_id in chunk_ids {
                stmt.execute(rusqlite::params![
                    tenant_id.as_str(),
                    chunk_id.to_string(),
                    IndexState::Pending.as_str(),
                    now_ms,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn mark_indexed(&self, tenant_id: &TenantId, chunk_ids: &[ChunkId], now_ms: i64) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE chunks
                 SET index_state = ?3,
                     index_last_error = NULL,
                     indexed_at_ms = ?4,
                     index_updated_at_ms = ?4
                 WHERE tenant_id = ?1 AND chunk_id = ?2",
            )?;
            for chunk_id in chunk_ids {
                stmt.execute(rusqlite::params![
                    tenant_id.as_str(),
                    chunk_id.to_string(),
                    IndexState::Indexed.as_str(),
                    now_ms,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn mark_index_failed(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        error: &str,
        now_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chunks
             SET index_state = ?3,
                 index_attempts = index_attempts + 1,
                 index_last_error = ?4,
                 index_updated_at_ms = ?5
             WHERE tenant_id = ?1 AND chunk_id = ?2",
            rusqlite::params![
                tenant_id.as_str(),
                chunk_id.to_string(),
                IndexState::Failed.as_str(),
                error,
                now_ms,
            ],
        )?;
        Ok(())
    }

    fn list_pending_index_chunk_ids(
        &self,
        tenant_id: &TenantId,
        limit: usize,
    ) -> Result<Vec<ChunkId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT chunk_id
             FROM chunks
             WHERE tenant_id = ?1 AND status != 'deleted' AND index_state = ?2
             ORDER BY timestamp_created ASC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![
                tenant_id.as_str(),
                IndexState::Pending.as_str(),
                limit as i64
            ],
            |row| row.get::<usize, String>(0),
        )?;

        let mut chunk_ids = Vec::new();
        for row in rows {
            if let Ok(chunk_id) = ChunkId::parse(&row?) {
                chunk_ids.push(chunk_id);
            }
        }
        Ok(chunk_ids)
    }

    fn count_by_index_state(&self, tenant_id: &TenantId) -> Result<(usize, usize, usize)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT index_state, COUNT(*) as cnt
             FROM chunks
             WHERE tenant_id = ?1 AND status != 'deleted'
             GROUP BY index_state",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str()], |row| {
            let state: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((state, count as usize))
        })?;

        let mut pending = 0usize;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        for row in rows {
            let (state, count) = row?;
            if state == IndexState::Pending.as_str() {
                pending = count;
            } else if state == IndexState::Indexed.as_str() {
                indexed = count;
            } else if state == IndexState::Failed.as_str() {
                failed = count;
            }
        }
        Ok((pending, indexed, failed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn create_test_metadata(tenant: &str, chunk_id: &ChunkId) -> ChunkMetadata {
        ChunkMetadata {
            chunk_id: chunk_id.clone(),
            tenant_id: TenantId::new(tenant).unwrap(),
            project_id: None,
            segment_id: 1,
            ordinal: 0,
            chunk_type: ChunkType::Doc,
            status: ChunkStatus::Final,
            timestamp_created: 1234567890,
            hash: "abc123".to_string(),
            source_uri: None,
        }
    }

    #[test]
    fn insert_and_get() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        let chunk_id = ChunkId::new();
        let tenant_id = TenantId::new("tenant_a").unwrap();
        let metadata = create_test_metadata("tenant_a", &chunk_id);

        store.insert(&metadata).unwrap();

        let retrieved = store.get(&tenant_id, &chunk_id).unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.chunk_id, chunk_id);
        assert_eq!(retrieved.tenant_id, tenant_id);
        assert_eq!(retrieved.hash, "abc123");
    }

    #[test]
    fn insert_many_round_trip() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();
        let tenant_id = TenantId::new("tenant_batch").unwrap();

        let mut rows = Vec::new();
        for i in 0..3u32 {
            let chunk_id = ChunkId::new();
            let mut row = create_test_metadata("tenant_batch", &chunk_id);
            row.segment_id = 100 + i as u64;
            row.ordinal = i;
            row.timestamp_created = 2000 + i as i64;
            rows.push(row);
        }

        store.insert_many(&rows).unwrap();

        let listed = store.list(&tenant_id, 10, 0).unwrap();
        assert_eq!(listed.len(), 3);
    }

    #[test]
    fn insert_many_empty_is_noop() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();
        let tenant_id = TenantId::new("tenant_empty").unwrap();

        store.insert_many(&[]).unwrap();
        let listed = store.list(&tenant_id, 10, 0).unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn tenant_isolation() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        let chunk_id = ChunkId::new();
        let metadata = create_test_metadata("tenant_a", &chunk_id);
        store.insert(&metadata).unwrap();

        // Tenant A can see their own chunk
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let result_a = store.get(&tenant_a, &chunk_id).unwrap();
        assert!(result_a.is_some());

        // Tenant B cannot see Tenant A's chunk
        let tenant_b = TenantId::new("tenant_b").unwrap();
        let result_b = store.get(&tenant_b, &chunk_id).unwrap();
        assert!(result_b.is_none());
    }

    #[test]
    fn list_pagination() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        let tenant_id = TenantId::new("tenant_a").unwrap();

        // Insert 10 chunks with different timestamps
        for i in 0..10 {
            let chunk_id = ChunkId::new();
            let mut metadata = create_test_metadata("tenant_a", &chunk_id);
            metadata.timestamp_created = 1000 + i;
            metadata.ordinal = i as u32;
            // Different segments to avoid UNIQUE constraint on (segment_id, ordinal)
            metadata.segment_id = i as u64;
            store.insert(&metadata).unwrap();
        }

        // List first 5
        let first_page = store.list(&tenant_id, 5, 0).unwrap();
        assert_eq!(first_page.len(), 5);
        // Should be ordered by timestamp DESC
        assert!(first_page[0].timestamp_created > first_page[4].timestamp_created);

        // List next 5
        let second_page = store.list(&tenant_id, 5, 5).unwrap();
        assert_eq!(second_page.len(), 5);

        // First item of second page should be older than last item of first page
        assert!(second_page[0].timestamp_created < first_page[4].timestamp_created);
    }

    #[test]
    fn soft_delete() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        let chunk_id = ChunkId::new();
        let tenant_id = TenantId::new("tenant_a").unwrap();
        let metadata = create_test_metadata("tenant_a", &chunk_id);

        store.insert(&metadata).unwrap();

        // Chunk exists before delete
        let before = store.get(&tenant_id, &chunk_id).unwrap();
        assert!(before.is_some());

        // Delete the chunk
        let deleted = store.mark_deleted(&tenant_id, &chunk_id).unwrap();
        assert!(deleted);

        // Chunk not visible after delete
        let after = store.get(&tenant_id, &chunk_id).unwrap();
        assert!(after.is_none());

        // Deleting again returns false
        let deleted_again = store.mark_deleted(&tenant_id, &chunk_id).unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn count_by_status() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        let tenant_id = TenantId::new("tenant_a").unwrap();

        // Insert 5 chunks
        let mut chunk_ids = Vec::new();
        for i in 0..5 {
            let chunk_id = ChunkId::new();
            chunk_ids.push(chunk_id.clone());
            let mut metadata = create_test_metadata("tenant_a", &chunk_id);
            metadata.ordinal = i;
            metadata.segment_id = i as u64;
            store.insert(&metadata).unwrap();
        }

        // Before deletion: 5 active, 0 deleted
        let (active, deleted) = store.count_by_status(&tenant_id).unwrap();
        assert_eq!(active, 5);
        assert_eq!(deleted, 0);

        // Delete 2 chunks
        store.mark_deleted(&tenant_id, &chunk_ids[0]).unwrap();
        store.mark_deleted(&tenant_id, &chunk_ids[1]).unwrap();

        // After deletion: 3 active, 2 deleted
        let (active, deleted) = store.count_by_status(&tenant_id).unwrap();
        assert_eq!(active, 3);
        assert_eq!(deleted, 2);
    }

    #[test]
    fn get_by_segment() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        // Insert chunks in different segments
        for seg in 0..3u64 {
            for ord in 0..3u32 {
                let chunk_id = ChunkId::new();
                let mut metadata = create_test_metadata("tenant_a", &chunk_id);
                metadata.segment_id = seg;
                metadata.ordinal = ord;
                store.insert(&metadata).unwrap();
            }
        }

        // Get chunks from segment 1
        let segment_1_chunks = store.get_by_segment(1).unwrap();
        assert_eq!(segment_1_chunks.len(), 3);

        // Verify ordinal ordering
        for (i, chunk) in segment_1_chunks.iter().enumerate() {
            assert_eq!(chunk.ordinal, i as u32);
            assert_eq!(chunk.segment_id, 1);
        }
    }

    #[test]
    fn wal_mode_enabled() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        // Insert something to trigger WAL creation
        let chunk_id = ChunkId::new();
        let metadata = create_test_metadata("test", &chunk_id);
        store.insert(&metadata).unwrap();

        // Database file should exist
        assert!(db_path.exists());

        // Check WAL mode via PRAGMA
        let conn = store.conn.lock().unwrap();
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");
    }

    #[test]
    fn feedback_insert_and_query_roundtrip() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();
        let tenant = TenantId::new("tenant_feedback").unwrap();
        let chunk_id = ChunkId::new();
        let feedback = FeedbackEntry::new(
            tenant.clone(),
            "Find parse config",
            chunk_id.clone(),
            RelevanceLabel::Relevant,
            123456789,
        );

        store.insert_feedback(&feedback).unwrap();

        let loaded = store
            .list_feedback_for_query(&tenant, " find   parse  config ", 10)
            .unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].chunk_id, chunk_id);
        assert_eq!(loaded[0].relevance, RelevanceLabel::Relevant);
    }

    #[test]
    fn index_state_roundtrip_and_counts() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        let tenant_id = TenantId::new("tenant_index").unwrap();
        let chunk_id = ChunkId::new();
        let metadata = create_test_metadata("tenant_index", &chunk_id);
        store.insert(&metadata).unwrap();

        let (pending, indexed, failed) = store.count_by_index_state(&tenant_id).unwrap();
        assert_eq!((pending, indexed, failed), (0, 1, 0));

        store
            .mark_index_pending(&tenant_id, std::slice::from_ref(&chunk_id), 101)
            .unwrap();
        let pending_ids = store.list_pending_index_chunk_ids(&tenant_id, 10).unwrap();
        assert_eq!(pending_ids, vec![chunk_id.clone()]);
        let (pending, indexed, failed) = store.count_by_index_state(&tenant_id).unwrap();
        assert_eq!((pending, indexed, failed), (1, 0, 0));

        store
            .mark_index_failed(&tenant_id, &chunk_id, "boom", 102)
            .unwrap();
        let (pending, indexed, failed) = store.count_by_index_state(&tenant_id).unwrap();
        assert_eq!((pending, indexed, failed), (0, 0, 1));

        store
            .mark_indexed(&tenant_id, std::slice::from_ref(&chunk_id), 103)
            .unwrap();
        let (pending, indexed, failed) = store.count_by_index_state(&tenant_id).unwrap();
        assert_eq!((pending, indexed, failed), (0, 1, 0));
    }

    #[test]
    fn open_migrates_legacy_chunks_schema_with_index_columns() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("legacy.db");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE chunks (
                chunk_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                segment_id INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                chunk_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'final',
                timestamp_created INTEGER NOT NULL,
                hash TEXT NOT NULL,
                source_uri TEXT,
                UNIQUE(segment_id, ordinal)
            )",
            [],
        )
        .unwrap();
        drop(conn);

        let store = SqliteMetadataStore::open(&db_path).unwrap();
        let conn = store.conn.lock().unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(chunks)").unwrap();
        let rows = stmt
            .query_map([], |row| row.get::<usize, String>(1))
            .unwrap();
        let mut names = std::collections::HashSet::new();
        for row in rows {
            names.insert(row.unwrap());
        }
        assert!(names.contains("index_state"));
        assert!(names.contains("index_attempts"));
        assert!(names.contains("index_last_error"));
        assert!(names.contains("indexed_at_ms"));
        assert!(names.contains("index_updated_at_ms"));
    }
}
