//! SQLite-backed metadata store
//!
//! Implements MetadataStore using SQLite with WAL mode for crash safety
//! and tenant isolation via indexes.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use super::{ChunkMetadata, MetadataStore};
use crate::error::Result;
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
                UNIQUE(segment_id, ordinal)
            )",
            [],
        )?;

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

        Ok(())
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

impl MetadataStore for SqliteMetadataStore {
    fn insert(&self, metadata: &ChunkMetadata) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        // Use INSERT OR REPLACE to handle crash recovery scenarios where
        // metadata exists but segment data was lost
        conn.execute(
            "INSERT OR REPLACE INTO chunks (
                chunk_id, tenant_id, project_id, segment_id, ordinal,
                chunk_type, status, timestamp_created, hash, source_uri
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                metadata.chunk_id.to_string(),
                metadata.tenant_id.as_str(),
                metadata.project_id.as_deref(),
                metadata.segment_id as i64,
                metadata.ordinal as i32,
                metadata.chunk_type.to_string(),
                metadata.status.to_string(),
                metadata.timestamp_created,
                &metadata.hash,
                metadata.source_uri.as_deref(),
            ],
        )?;

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
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
