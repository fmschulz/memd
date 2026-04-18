//! SQLite-backed metadata store
//!
//! Implements MetadataStore using SQLite with WAL mode for crash safety
//! and tenant isolation via indexes.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;

use super::pool::SqliteConnectionPool;
use super::{ChunkMetadata, IndexState, MetadataStore};
use crate::error::Result;
use crate::store::{normalize_query, FeedbackEntry, RelevanceLabel};
use crate::task_memory::{ArtifactKind, TaskArtifact, TaskRecord, TaskSearchFilters};
use crate::types::{
    ChunkId, ChunkStatus, ChunkType, LifecycleDelta, LifecycleMetadata, MemoryTier, TenantId,
};

/// Canonical column list for `chunks` SELECT statements that feed
/// `row_to_metadata`.
///
/// Kept in one place so that every SELECT stays in sync with the row
/// mapper's positional `row.get(N)` calls. When extending the `chunks`
/// table, append the new column to the end of this list *and* to every
/// SELECT that uses it; do not reorder existing columns.
const CHUNK_COLUMNS: &str = "chunk_id, tenant_id, project_id, segment_id, ordinal, \
                             chunk_type, status, timestamp_created, hash, source_uri, \
                             tier, supersedes, superseded_by, expires_at_ms, review_after_ms, \
                             lifecycle_updated_at_ms, canonical_text";

/// SQLite-backed metadata store.
///
/// Phase 4.3: swapped from `Mutex<Connection>` to
/// `SqliteConnectionPool`. Each call acquires a pooled connection
/// (RAII-returned on drop) so readers no longer serialize against
/// each other; writers still serialize naturally at SQLite's own
/// WAL-mode locking.
pub struct SqliteMetadataStore {
    pool: SqliteConnectionPool,
}

impl SqliteMetadataStore {
    /// Open or create a SQLite metadata store. The pool warms one
    /// connection eagerly; subsequent connections grow on demand up
    /// to `MEMD_SQLITE_POOL_MAX` (default 16).
    pub fn open(path: &Path) -> Result<Self> {
        let pool = SqliteConnectionPool::open(path)?;
        let store = Self { pool };
        store.init_schema()?;
        Ok(store)
    }

    /// Initialize the database schema
    fn init_schema(&self) -> Result<()> {
        let conn = self.pool.get();

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
                tier TEXT NOT NULL DEFAULT 'long_term',
                supersedes TEXT,
                superseded_by TEXT,
                expires_at_ms INTEGER,
                review_after_ms INTEGER,
                lifecycle_updated_at_ms INTEGER NOT NULL DEFAULT 0,
                canonical_text TEXT,
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

        Self::init_task_schema(&conn)?;

        Ok(())
    }

    fn init_task_schema(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS task_artifacts (
                artifact_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                task_id TEXT NOT NULL,
                parent_task_id TEXT,
                artifact_kind TEXT NOT NULL,
                status TEXT,
                artifact_role TEXT,
                challenge_id TEXT,
                thread_id TEXT,
                reply_to_artifact_id TEXT,
                agent_id TEXT,
                session_id TEXT,
                goal TEXT,
                summary TEXT,
                tool_name TEXT,
                tool_version TEXT,
                requested_action TEXT,
                verification_status TEXT,
                promotion_state TEXT NOT NULL DEFAULT 'raw',
                digest_key TEXT,
                source_updated_at_ms INTEGER,
                canonical_json TEXT NOT NULL,
                timestamp_created INTEGER NOT NULL,
                timestamp_observed INTEGER
            )",
            [],
        )?;
        Self::ensure_task_artifact_columns(conn)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_task_artifacts_tenant_task
             ON task_artifacts(tenant_id, task_id, artifact_kind, timestamp_created DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_task_artifacts_tool
             ON task_artifacts(tenant_id, tool_name, timestamp_created DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_task_artifacts_thread
             ON task_artifacts(tenant_id, thread_id, timestamp_created DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_task_artifacts_challenge
             ON task_artifacts(tenant_id, challenge_id, timestamp_created DESC)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS tasks (
                task_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                parent_task_id TEXT,
                agent_id TEXT,
                session_id TEXT,
                status TEXT,
                goal TEXT,
                scientific_question TEXT,
                hypothesis TEXT,
                last_artifact_id TEXT NOT NULL,
                started_at_ms INTEGER,
                finished_at_ms INTEGER,
                updated_at_ms INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tasks_tenant_status
             ON tasks(tenant_id, status, updated_at_ms DESC)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS task_events (
                artifact_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                artifact_kind TEXT NOT NULL,
                status TEXT,
                summary TEXT,
                timestamp_created INTEGER NOT NULL,
                timestamp_observed INTEGER
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_task_events_tenant_task
             ON task_events(tenant_id, task_id, timestamp_created DESC)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS runs (
                artifact_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                tool_name TEXT,
                status TEXT,
                command TEXT,
                metrics_json TEXT,
                timestamp_created INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS evidence (
                artifact_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                summary TEXT,
                supports_claim INTEGER,
                metrics_json TEXT,
                timestamp_created INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS datasets (
                dataset_key TEXT PRIMARY KEY,
                dataset_name TEXT NOT NULL,
                dataset_version TEXT,
                description TEXT
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS entities (
                entity_key TEXT PRIMARY KEY,
                entity_name TEXT NOT NULL,
                entity_type TEXT NOT NULL,
                role TEXT
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS task_datasets (
                artifact_id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                dataset_key TEXT NOT NULL,
                dataset_name TEXT NOT NULL,
                dataset_version TEXT,
                PRIMARY KEY (artifact_id, dataset_key)
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_task_datasets_tenant_task
             ON task_datasets(tenant_id, task_id, dataset_name, dataset_version)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS task_entities (
                artifact_id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                entity_key TEXT NOT NULL,
                entity_name TEXT NOT NULL,
                entity_type TEXT NOT NULL,
                PRIMARY KEY (artifact_id, entity_key)
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_task_entities_tenant_task
             ON task_entities(tenant_id, task_id, entity_name, entity_type)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS artifact_links (
                artifact_id TEXT NOT NULL,
                chunk_id TEXT NOT NULL,
                link_kind TEXT NOT NULL,
                projection_kind TEXT,
                PRIMARY KEY (artifact_id, chunk_id, link_kind)
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_artifact_links_chunk
             ON artifact_links(chunk_id)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS artifact_relations (
                artifact_id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                related_artifact_id TEXT NOT NULL,
                relation_kind TEXT NOT NULL,
                PRIMARY KEY (artifact_id, related_artifact_id, relation_kind)
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_artifact_relations_tenant_related
             ON artifact_relations(tenant_id, related_artifact_id, relation_kind)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS artifact_contributors (
                artifact_id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                contributor_id TEXT NOT NULL,
                display_name TEXT,
                role TEXT,
                contribution TEXT,
                PRIMARY KEY (artifact_id, contributor_id)
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_artifact_contributors_tenant
             ON artifact_contributors(tenant_id, contributor_id)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS challenges (
                challenge_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                project_id TEXT,
                summary TEXT,
                status TEXT,
                updated_at_ms INTEGER NOT NULL
            )",
            [],
        )?;

        Ok(())
    }

    fn ensure_task_artifact_columns(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(task_artifacts)")?;
        let rows = stmt.query_map([], |row| row.get::<usize, String>(1))?;
        let mut column_names = std::collections::HashSet::new();
        for name in rows {
            column_names.insert(name?);
        }

        Self::ensure_index_column(
            conn,
            &column_names,
            "tool_name",
            "ALTER TABLE task_artifacts ADD COLUMN tool_name TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "tool_version",
            "ALTER TABLE task_artifacts ADD COLUMN tool_version TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "artifact_role",
            "ALTER TABLE task_artifacts ADD COLUMN artifact_role TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "challenge_id",
            "ALTER TABLE task_artifacts ADD COLUMN challenge_id TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "thread_id",
            "ALTER TABLE task_artifacts ADD COLUMN thread_id TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "reply_to_artifact_id",
            "ALTER TABLE task_artifacts ADD COLUMN reply_to_artifact_id TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "requested_action",
            "ALTER TABLE task_artifacts ADD COLUMN requested_action TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "verification_status",
            "ALTER TABLE task_artifacts ADD COLUMN verification_status TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "promotion_state",
            "ALTER TABLE task_artifacts ADD COLUMN promotion_state TEXT NOT NULL DEFAULT 'raw'",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "digest_key",
            "ALTER TABLE task_artifacts ADD COLUMN digest_key TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "source_updated_at_ms",
            "ALTER TABLE task_artifacts ADD COLUMN source_updated_at_ms INTEGER",
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

        // A3: lifecycle overlay columns.
        Self::ensure_index_column(
            conn,
            &column_names,
            "tier",
            "ALTER TABLE chunks ADD COLUMN tier TEXT NOT NULL DEFAULT 'long_term'",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "supersedes",
            "ALTER TABLE chunks ADD COLUMN supersedes TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "superseded_by",
            "ALTER TABLE chunks ADD COLUMN superseded_by TEXT",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "expires_at_ms",
            "ALTER TABLE chunks ADD COLUMN expires_at_ms INTEGER",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "review_after_ms",
            "ALTER TABLE chunks ADD COLUMN review_after_ms INTEGER",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "lifecycle_updated_at_ms",
            "ALTER TABLE chunks ADD COLUMN lifecycle_updated_at_ms INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::ensure_index_column(
            conn,
            &column_names,
            "canonical_text",
            "ALTER TABLE chunks ADD COLUMN canonical_text TEXT",
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chunks_expiry
             ON chunks(tenant_id, expires_at_ms) WHERE expires_at_ms IS NOT NULL",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chunks_supersedes
             ON chunks(tenant_id, supersedes) WHERE supersedes IS NOT NULL",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chunks_tier_status
             ON chunks(tenant_id, tier, status)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_chunks_canonical
             ON chunks(tenant_id, project_id, canonical_text) WHERE canonical_text IS NOT NULL",
            [],
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
        let conn = self.pool.get();
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

        let conn = self.pool.get();
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

    /// Insert the canonical task artifact envelope plus normalized side rows.
    pub fn insert_task_artifact_bundle(
        &self,
        artifact: &TaskArtifact,
        projection_chunk_ids: &[String],
        projection_kinds: &[String],
    ) -> Result<()> {
        let canonical_json = serde_json::to_string(artifact)?;
        let summary = artifact.event_summary();
        let status = artifact.status.as_deref();
        let project_id = artifact.project_id.as_option();
        let scientific_question = artifact.scientific_question.as_deref();
        let hypothesis = artifact.hypothesis.as_deref();
        let started_at_ms =
            (artifact.artifact_kind.as_str() == "task_start").then_some(artifact.timestamp_created);
        let finished_at_ms = (artifact.artifact_kind.as_str() == "task_finish")
            .then_some(artifact.timestamp_created);

        let mut conn = self.pool.get();
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT OR REPLACE INTO task_artifacts (
                artifact_id, tenant_id, project_id, task_id, parent_task_id,
                artifact_kind, status, artifact_role, challenge_id, thread_id, reply_to_artifact_id,
                agent_id, session_id, goal, summary, tool_name, tool_version,
                requested_action, verification_status, promotion_state, digest_key, source_updated_at_ms,
                canonical_json, timestamp_created, timestamp_observed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            rusqlite::params![
                artifact.artifact_id.as_str(),
                artifact.tenant_id.as_str(),
                project_id,
                artifact.task_id.as_str(),
                artifact.parent_task_id.as_deref(),
                artifact.artifact_kind.as_str(),
                status,
                artifact.artifact_role.as_deref(),
                artifact.challenge_id.as_deref(),
                artifact.thread_id.as_deref(),
                artifact.reply_to_artifact_id.as_deref(),
                artifact.agent_id.as_deref(),
                artifact.session_id.as_deref(),
                artifact.goal.as_deref(),
                summary.as_deref(),
                artifact.tool_name.as_deref(),
                artifact.tool_version.as_deref(),
                artifact.requested_action.as_deref(),
                artifact.verification_status.as_deref(),
                artifact.promotion_state.to_string(),
                artifact.digest_key.as_deref(),
                artifact.source_updated_at_ms,
                canonical_json,
                artifact.timestamp_created,
                artifact.timestamp_observed,
            ],
        )?;

        if artifact.artifact_kind != ArtifactKind::Digest {
            tx.execute(
                "INSERT INTO tasks (
                    task_id, tenant_id, project_id, parent_task_id, agent_id, session_id,
                    status, goal, scientific_question, hypothesis, last_artifact_id,
                    started_at_ms, finished_at_ms, updated_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                ON CONFLICT(task_id) DO UPDATE SET
                    project_id = COALESCE(excluded.project_id, tasks.project_id),
                    parent_task_id = COALESCE(excluded.parent_task_id, tasks.parent_task_id),
                    agent_id = COALESCE(excluded.agent_id, tasks.agent_id),
                    session_id = COALESCE(excluded.session_id, tasks.session_id),
                    status = COALESCE(excluded.status, tasks.status),
                    goal = COALESCE(excluded.goal, tasks.goal),
                    scientific_question = COALESCE(excluded.scientific_question, tasks.scientific_question),
                    hypothesis = COALESCE(excluded.hypothesis, tasks.hypothesis),
                    last_artifact_id = excluded.last_artifact_id,
                    started_at_ms = COALESCE(tasks.started_at_ms, excluded.started_at_ms),
                    finished_at_ms = COALESCE(excluded.finished_at_ms, tasks.finished_at_ms),
                    updated_at_ms = excluded.updated_at_ms",
                rusqlite::params![
                    artifact.task_id.as_str(),
                    artifact.tenant_id.as_str(),
                    project_id,
                    artifact.parent_task_id.as_deref(),
                    artifact.agent_id.as_deref(),
                    artifact.session_id.as_deref(),
                    status,
                    artifact.goal.as_deref(),
                    scientific_question,
                    hypothesis,
                    artifact.artifact_id.as_str(),
                    started_at_ms,
                    finished_at_ms,
                    artifact.timestamp_created,
                ],
            )?;

            tx.execute(
                "INSERT OR REPLACE INTO task_events (
                    artifact_id, tenant_id, task_id, artifact_kind, status, summary,
                    timestamp_created, timestamp_observed
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    artifact.task_id.as_str(),
                    artifact.artifact_kind.as_str(),
                    status,
                    summary.as_deref(),
                    artifact.timestamp_created,
                    artifact.timestamp_observed,
                ],
            )?;
        }

        if let Some(challenge_id) = artifact.challenge_id.as_deref() {
            tx.execute(
                "INSERT INTO challenges (
                    challenge_id, tenant_id, project_id, summary, status, updated_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(challenge_id) DO UPDATE SET
                    project_id = COALESCE(excluded.project_id, challenges.project_id),
                    summary = COALESCE(excluded.summary, challenges.summary),
                    status = COALESCE(excluded.status, challenges.status),
                    updated_at_ms = excluded.updated_at_ms",
                rusqlite::params![
                    challenge_id,
                    artifact.tenant_id.as_str(),
                    project_id,
                    summary.as_deref().or(artifact.goal.as_deref()),
                    status,
                    artifact.timestamp_created,
                ],
            )?;
        }

        for dataset in &artifact.dataset_refs {
            tx.execute(
                "INSERT OR REPLACE INTO datasets (dataset_key, dataset_name, dataset_version, description)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    dataset.key(),
                    dataset.name.as_str(),
                    dataset.version.as_deref(),
                    dataset.description.as_deref(),
                ],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO task_datasets (
                    artifact_id, tenant_id, task_id, dataset_key, dataset_name, dataset_version
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    artifact.task_id.as_str(),
                    dataset.key(),
                    dataset.name.as_str(),
                    dataset.version.as_deref(),
                ],
            )?;
        }

        for entity in &artifact.entity_refs {
            tx.execute(
                "INSERT OR REPLACE INTO entities (entity_key, entity_name, entity_type, role)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    entity.key(),
                    entity.name.as_str(),
                    entity.entity_type.as_str(),
                    entity.role.as_deref(),
                ],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO task_entities (
                    artifact_id, tenant_id, task_id, entity_key, entity_name, entity_type
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    artifact.task_id.as_str(),
                    entity.key(),
                    entity.name.as_str(),
                    entity.entity_type.as_str(),
                ],
            )?;
        }

        for (idx, chunk_id) in projection_chunk_ids.iter().enumerate() {
            tx.execute(
                "INSERT OR REPLACE INTO artifact_links (
                    artifact_id, chunk_id, link_kind, projection_kind
                ) VALUES (?1, ?2, 'retrieval_projection', ?3)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    chunk_id,
                    projection_kinds.get(idx).map(|kind| kind.as_str()),
                ],
            )?;
        }

        if let Some(reply_to_artifact_id) = artifact.reply_to_artifact_id.as_deref() {
            tx.execute(
                "INSERT OR REPLACE INTO artifact_relations (
                    artifact_id, tenant_id, related_artifact_id, relation_kind
                ) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    reply_to_artifact_id,
                    artifact.relation_kind.as_deref().unwrap_or("reply_to"),
                ],
            )?;
        }

        for related_artifact_id in &artifact.related_artifact_ids {
            tx.execute(
                "INSERT OR REPLACE INTO artifact_relations (
                    artifact_id, tenant_id, related_artifact_id, relation_kind
                ) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    related_artifact_id.as_str(),
                    artifact.relation_kind.as_deref().unwrap_or("related"),
                ],
            )?;
        }

        for contributor in &artifact.contributors {
            tx.execute(
                "INSERT OR REPLACE INTO artifact_contributors (
                    artifact_id, tenant_id, contributor_id, display_name, role, contribution
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    artifact.artifact_id.as_str(),
                    artifact.tenant_id.as_str(),
                    contributor.contributor_id.as_str(),
                    contributor.display_name.as_deref(),
                    contributor.role.as_deref(),
                    contributor.contribution.as_deref(),
                ],
            )?;
        }

        match artifact.artifact_kind {
            crate::task_memory::ArtifactKind::RunStart
            | crate::task_memory::ArtifactKind::RunFinish => {
                tx.execute(
                    "INSERT OR REPLACE INTO runs (
                        artifact_id, tenant_id, task_id, tool_name, status, command, metrics_json, timestamp_created
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        artifact.artifact_id.as_str(),
                        artifact.tenant_id.as_str(),
                        artifact.task_id.as_str(),
                        artifact.tool_name.as_deref(),
                        status,
                        artifact.command.as_deref(),
                        artifact.metrics.as_ref().map(|metrics| metrics.to_string()),
                        artifact.timestamp_created,
                    ],
                )?;
            }
            crate::task_memory::ArtifactKind::Evidence => {
                tx.execute(
                    "INSERT OR REPLACE INTO evidence (
                        artifact_id, tenant_id, task_id, summary, supports_claim, metrics_json, timestamp_created
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        artifact.artifact_id.as_str(),
                        artifact.tenant_id.as_str(),
                        artifact.task_id.as_str(),
                        summary.as_deref(),
                        artifact.supports_claim.map(|value| if value { 1 } else { 0 }),
                        artifact.metrics.as_ref().map(|metrics| metrics.to_string()),
                        artifact.timestamp_created,
                    ],
                )?;
            }
            _ => {}
        }

        tx.commit()?;
        Ok(())
    }

    /// Fetch the canonical task artifact envelope by ID.
    pub fn get_task_artifact(
        &self,
        tenant_id: &TenantId,
        artifact_id: &str,
    ) -> Result<Option<TaskArtifact>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT canonical_json
             FROM task_artifacts
             WHERE tenant_id = ?1 AND artifact_id = ?2",
        )?;

        let result = stmt.query_row(rusqlite::params![tenant_id.as_str(), artifact_id], |row| {
            row.get::<usize, String>(0)
        });

        match result {
            Ok(canonical_json) => Ok(Some(serde_json::from_str(&canonical_json)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List canonical task artifacts for one logical task ordered by creation time.
    pub fn list_task_artifacts(
        &self,
        tenant_id: &TenantId,
        task_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT canonical_json
             FROM task_artifacts
             WHERE tenant_id = ?1 AND task_id = ?2
             ORDER BY timestamp_created ASC, artifact_id ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str(), task_id], |row| {
            row.get::<usize, String>(0)
        })?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(serde_json::from_str(&row?)?);
        }
        Ok(artifacts)
    }

    /// List canonical task artifacts for a thread ordered by creation time.
    pub fn list_thread_artifacts(
        &self,
        tenant_id: &TenantId,
        thread_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT canonical_json
             FROM task_artifacts
             WHERE tenant_id = ?1
               AND (thread_id = ?2 OR (thread_id IS NULL AND task_id = ?2))
             ORDER BY timestamp_created ASC, artifact_id ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str(), thread_id], |row| {
            row.get::<usize, String>(0)
        })?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(serde_json::from_str(&row?)?);
        }
        Ok(artifacts)
    }

    pub fn list_tasks(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.pool.get();
        let mut sql = String::from(
            "SELECT task_id, tenant_id, project_id, status, goal, scientific_question,
                    hypothesis, last_artifact_id, started_at_ms, finished_at_ms, updated_at_ms
             FROM tasks
             WHERE tenant_id = ?1",
        );
        let mut params = vec![rusqlite::types::Value::Text(tenant_id.as_str().to_string())];
        if let Some(project_id) = project_id {
            sql.push_str(" AND project_id = ?2");
            params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        sql.push_str(" ORDER BY updated_at_ms DESC");
        sql.push_str(&format!(" LIMIT ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Integer(limit as i64));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(TaskRecord {
                task_id: row.get(0)?,
                tenant_id: TenantId::new(row.get::<usize, String>(1)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                project_id: crate::types::ProjectId::from(row.get::<usize, Option<String>>(2)?),
                status: row.get(3)?,
                goal: row.get(4)?,
                scientific_question: row.get(5)?,
                hypothesis: row.get(6)?,
                last_artifact_id: row.get(7)?,
                started_at_ms: row.get(8)?,
                finished_at_ms: row.get(9)?,
                updated_at_ms: row.get(10)?,
            })
        })?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    /// Resolve projection chunk IDs for exact task filters.
    pub fn search_task_projection_chunk_ids(
        &self,
        tenant_id: &TenantId,
        filters: &TaskSearchFilters,
        limit: usize,
    ) -> Result<Vec<ChunkId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        use rusqlite::types::Value as SqlValue;

        let mut sql = String::from(
            "SELECT DISTINCT l.chunk_id
             FROM task_artifacts a
             JOIN artifact_links l
               ON l.artifact_id = a.artifact_id
              AND l.link_kind = 'retrieval_projection'
             WHERE a.tenant_id = ?1",
        );
        let mut params = vec![SqlValue::Text(tenant_id.as_str().to_string())];
        let mut idx = 2usize;

        if let Some(task_id) = filters.task_id.as_deref() {
            sql.push_str(&format!(" AND a.task_id = ?{}", idx));
            params.push(SqlValue::Text(task_id.to_string()));
            idx += 1;
        }
        if let Some(artifact_kind) = filters.artifact_kind {
            sql.push_str(&format!(" AND a.artifact_kind = ?{}", idx));
            params.push(SqlValue::Text(artifact_kind.as_str().to_string()));
            idx += 1;
        }
        if let Some(status) = filters.status.as_deref() {
            sql.push_str(&format!(" AND a.status = ?{}", idx));
            params.push(SqlValue::Text(status.to_string()));
            idx += 1;
        }
        if let Some(challenge_id) = filters.challenge_id.as_deref() {
            sql.push_str(&format!(" AND a.challenge_id = ?{}", idx));
            params.push(SqlValue::Text(challenge_id.to_string()));
            idx += 1;
        }
        if let Some(thread_id) = filters.thread_id.as_deref() {
            sql.push_str(&format!(
                " AND (a.thread_id = ?{} OR (a.thread_id IS NULL AND a.task_id = ?{}))",
                idx, idx
            ));
            params.push(SqlValue::Text(thread_id.to_string()));
            idx += 1;
        }
        if let Some(reply_to_artifact_id) = filters.reply_to_artifact_id.as_deref() {
            sql.push_str(&format!(" AND a.reply_to_artifact_id = ?{}", idx));
            params.push(SqlValue::Text(reply_to_artifact_id.to_string()));
            idx += 1;
        }
        if let Some(artifact_role) = filters.artifact_role.as_deref() {
            sql.push_str(&format!(" AND a.artifact_role = ?{}", idx));
            params.push(SqlValue::Text(artifact_role.to_string()));
            idx += 1;
        }
        if let Some(project_id) = filters.project_id.as_deref() {
            sql.push_str(&format!(" AND a.project_id = ?{}", idx));
            params.push(SqlValue::Text(project_id.to_string()));
            idx += 1;
        }
        if let Some(agent_id) = filters.agent_id.as_deref() {
            sql.push_str(&format!(" AND a.agent_id = ?{}", idx));
            params.push(SqlValue::Text(agent_id.to_string()));
            idx += 1;
        }
        if let Some(session_id) = filters.session_id.as_deref() {
            sql.push_str(&format!(" AND a.session_id = ?{}", idx));
            params.push(SqlValue::Text(session_id.to_string()));
            idx += 1;
        }
        if let Some(tool_name) = filters.tool_name.as_deref() {
            sql.push_str(&format!(" AND a.tool_name = ?{}", idx));
            params.push(SqlValue::Text(tool_name.to_string()));
            idx += 1;
        }
        if let Some(requested_action) = filters.requested_action.as_deref() {
            sql.push_str(&format!(" AND a.requested_action = ?{}", idx));
            params.push(SqlValue::Text(requested_action.to_string()));
            idx += 1;
        }
        if let Some(verification_status) = filters.verification_status.as_deref() {
            sql.push_str(&format!(" AND a.verification_status = ?{}", idx));
            params.push(SqlValue::Text(verification_status.to_string()));
            idx += 1;
        }
        if let Some(relation_kind) = filters.relation_kind.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM artifact_relations ar
                    WHERE ar.artifact_id = a.artifact_id
                      AND ar.relation_kind = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(relation_kind.to_string()));
            idx += 1;
        }
        if let Some(dataset_name) = filters.dataset_name.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM task_datasets d
                    WHERE d.artifact_id = a.artifact_id
                      AND d.dataset_name = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(dataset_name.to_string()));
            idx += 1;
            if let Some(dataset_version) = filters.dataset_version.as_deref() {
                sql.push_str(&format!(
                    " AND EXISTS (
                        SELECT 1
                        FROM task_datasets dv
                        WHERE dv.artifact_id = a.artifact_id
                          AND dv.dataset_name = ?{}
                          AND dv.dataset_version = ?{}
                    )",
                    idx,
                    idx + 1
                ));
                params.push(SqlValue::Text(dataset_name.to_string()));
                params.push(SqlValue::Text(dataset_version.to_string()));
                idx += 2;
            }
        } else if let Some(dataset_version) = filters.dataset_version.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM task_datasets d
                    WHERE d.artifact_id = a.artifact_id
                      AND d.dataset_version = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(dataset_version.to_string()));
            idx += 1;
        }
        if let Some(entity_name) = filters.entity_name.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM task_entities e
                    WHERE e.artifact_id = a.artifact_id
                      AND e.entity_name = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(entity_name.to_string()));
            idx += 1;
            if let Some(entity_type) = filters.entity_type.as_deref() {
                sql.push_str(&format!(
                    " AND EXISTS (
                        SELECT 1
                        FROM task_entities et
                        WHERE et.artifact_id = a.artifact_id
                          AND et.entity_name = ?{}
                          AND et.entity_type = ?{}
                    )",
                    idx,
                    idx + 1
                ));
                params.push(SqlValue::Text(entity_name.to_string()));
                params.push(SqlValue::Text(entity_type.to_string()));
                idx += 2;
            }
        } else if let Some(entity_type) = filters.entity_type.as_deref() {
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM task_entities e
                    WHERE e.artifact_id = a.artifact_id
                      AND e.entity_type = ?{}
                )",
                idx
            ));
            params.push(SqlValue::Text(entity_type.to_string()));
            idx += 1;
        }

        sql.push_str(" ORDER BY a.timestamp_created DESC");
        sql.push_str(&format!(" LIMIT ?{}", idx));
        params.push(SqlValue::Integer(limit as i64));

        let conn = self.pool.get();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<usize, String>(0)
        })?;

        let mut chunk_ids = Vec::new();
        for row in rows {
            if let Ok(chunk_id) = ChunkId::parse(&row?) {
                chunk_ids.push(chunk_id);
            }
        }
        Ok(chunk_ids)
    }

    /// Resolve canonical artifacts for retrieval projection chunk IDs.
    pub fn resolve_artifacts_for_chunks(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
    ) -> Result<HashMap<String, TaskArtifact>> {
        let mut resolved = HashMap::new();
        if chunk_ids.is_empty() {
            return Ok(resolved);
        }

        let placeholders = (0..chunk_ids.len())
            .map(|idx| format!("?{}", idx + 2))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "SELECT l.chunk_id, a.canonical_json
             FROM artifact_links l
             JOIN task_artifacts a
               ON a.artifact_id = l.artifact_id
             WHERE a.tenant_id = ?1
               AND l.link_kind = 'retrieval_projection'
               AND l.chunk_id IN ({})",
            placeholders
        );

        let mut params = Vec::with_capacity(chunk_ids.len() + 1);
        params.push(rusqlite::types::Value::Text(tenant_id.as_str().to_string()));
        for chunk_id in chunk_ids {
            params.push(rusqlite::types::Value::Text(chunk_id.to_string()));
        }

        let conn = self.pool.get();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok((row.get::<usize, String>(0)?, row.get::<usize, String>(1)?))
        })?;

        for row in rows {
            let (chunk_id, canonical_json) = row?;
            resolved.insert(chunk_id, serde_json::from_str(&canonical_json)?);
        }

        Ok(resolved)
    }

    /// Convert a database row to [`ChunkMetadata`].
    ///
    /// **Invariant:** the SELECT that produced `row` must project
    /// columns in the order defined by [`CHUNK_COLUMNS`]. Any mismatch
    /// will silently corrupt the parsed record.
    ///
    /// Fail-closed on unknown `status` / `tier` strings: A3 treats an
    /// unrecognized enum value as a schema-drift bug rather than
    /// silently defaulting, so bad rows surface as conversion errors
    /// instead of masquerading as `Final`/`LongTerm`.
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
        let tier_str: String = row.get(10)?;
        let supersedes_str: Option<String> = row.get(11)?;
        let superseded_by_str: Option<String> = row.get(12)?;
        let expires_at_ms: Option<i64> = row.get(13)?;
        let review_after_ms: Option<i64> = row.get(14)?;
        let lifecycle_updated_at_ms: i64 = row.get(15)?;
        let canonical_text: Option<String> = row.get(16)?;

        // Parse chunk_id
        let chunk_id = ChunkId::parse(&chunk_id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse tenant_id
        let tenant_id = TenantId::new(&tenant_id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse chunk_type — fail-closed via FromStr.
        let chunk_type = chunk_type_str.parse::<ChunkType>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse status — fail-closed via FromStr (A1).
        let status = status_str.parse::<ChunkStatus>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse tier — fail-closed via FromStr (A2).
        let tier = tier_str.parse::<MemoryTier>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
        })?;

        let supersedes = supersedes_str
            .map(|s| ChunkId::parse(&s))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    11,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let superseded_by = superseded_by_str
            .map(|s| ChunkId::parse(&s))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    12,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

        let lifecycle = LifecycleMetadata {
            tier,
            supersedes,
            superseded_by,
            expires_at_ms,
            review_after_ms,
            lifecycle_updated_at_ms,
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
            lifecycle,
            canonical_text,
        })
    }

    /// Back-date a chunk's `timestamp_created` via direct SQL.
    ///
    /// TEST-ONLY: used by integration tests that need a deterministic
    /// clock when exercising history promotion / age-based behavior.
    /// Gated behind `cfg(any(test, feature = "test-support"))` so it
    /// never ships in release builds. Allowed-dead until Track C tests
    /// wire it up through the `common` test-helpers module.
    ///
    /// Visibility note: spec called for `pub(crate)`, but the integration
    /// tests under `crates/memd/tests/` live in a separate crate and
    /// cannot see crate-private items. Exposing this as `pub` behind the
    /// `test-support` feature keeps it off release builds while letting
    /// integration tests reach it.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub fn force_timestamp_created(
        &self,
        chunk_id: &ChunkId,
        ts_ms: i64,
    ) -> Result<()> {
        let conn = self.pool.get();
        conn.execute(
            "UPDATE chunks SET timestamp_created = ?1 WHERE chunk_id = ?2",
            rusqlite::params![ts_ms, chunk_id.to_string()],
        )?;
        Ok(())
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

        let mut conn = self.pool.get();
        let tx = conn.transaction()?;

        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO chunks (
                    chunk_id, tenant_id, project_id, segment_id, ordinal,
                    chunk_type, status, timestamp_created, hash, source_uri,
                    tier, supersedes, superseded_by, expires_at_ms, review_after_ms,
                    lifecycle_updated_at_ms, canonical_text
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
                    row.lifecycle.tier.to_string(),
                    row.lifecycle.supersedes.as_ref().map(|c| c.to_string()),
                    row.lifecycle.superseded_by.as_ref().map(|c| c.to_string()),
                    row.lifecycle.expires_at_ms,
                    row.lifecycle.review_after_ms,
                    row.lifecycle.lifecycle_updated_at_ms,
                    row.canonical_text.as_deref(),
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<ChunkMetadata>> {
        let conn = self.pool.get();

        let sql = format!(
            "SELECT {CHUNK_COLUMNS}
             FROM chunks
             WHERE tenant_id = ?1 AND chunk_id = ?2 AND status != 'deleted'"
        );
        let mut stmt = conn.prepare(&sql)?;

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
        let conn = self.pool.get();

        let sql = format!(
            "SELECT {CHUNK_COLUMNS}
             FROM chunks
             WHERE tenant_id = ?1 AND status != 'deleted'
             ORDER BY timestamp_created DESC
             LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = conn.prepare(&sql)?;

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
        let conn = self.pool.get();

        let rows_affected = conn.execute(
            "UPDATE chunks SET status = 'deleted'
             WHERE tenant_id = ?1 AND chunk_id = ?2 AND status != 'deleted'",
            rusqlite::params![tenant_id.as_str(), chunk_id.to_string()],
        )?;

        Ok(rows_affected > 0)
    }

    fn get_by_segment(&self, segment_id: u64) -> Result<Vec<ChunkMetadata>> {
        let conn = self.pool.get();

        let sql = format!(
            "SELECT {CHUNK_COLUMNS}
             FROM chunks
             WHERE segment_id = ?1
             ORDER BY ordinal ASC"
        );
        let mut stmt = conn.prepare(&sql)?;

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
        let conn = self.pool.get();

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
        let conn = self.pool.get();

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

        let mut conn = self.pool.get();
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

        let mut conn = self.pool.get();
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
        let conn = self.pool.get();
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
        let conn = self.pool.get();
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
        let conn = self.pool.get();
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

    fn update_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        delta: &LifecycleDelta,
    ) -> Result<()> {
        let conn = self.pool.get();
        conn.execute(
            "UPDATE chunks SET
                status                  = COALESCE(:status, status),
                tier                    = COALESCE(:tier, tier),
                supersedes              = COALESCE(:supersedes, supersedes),
                superseded_by           = COALESCE(:superseded_by, superseded_by),
                expires_at_ms           = CASE WHEN :set_expires = 1 THEN :expires_at ELSE expires_at_ms END,
                review_after_ms         = CASE WHEN :set_review  = 1 THEN :review_at  ELSE review_after_ms END,
                lifecycle_updated_at_ms = COALESCE(:lifecycle_at, lifecycle_updated_at_ms)
             WHERE tenant_id = :tenant AND chunk_id = :chunk",
            rusqlite::named_params! {
                ":status":        delta.status.map(|s| s.to_string()),
                ":tier":          delta.tier.map(|t| t.to_string()),
                ":supersedes":    delta.supersedes.as_ref().map(|c| c.to_string()),
                ":superseded_by": delta.superseded_by.as_ref().map(|c| c.to_string()),
                ":set_expires":   i64::from(delta.expires_at_ms.is_some()),
                ":expires_at":    delta.expires_at_ms.flatten(),
                ":set_review":    i64::from(delta.review_after_ms.is_some()),
                ":review_at":     delta.review_after_ms.flatten(),
                ":lifecycle_at":  delta.lifecycle_updated_at_ms,
                ":tenant":        tenant_id.as_str(),
                ":chunk":         chunk_id.to_string(),
            },
        )?;
        Ok(())
    }

    fn atomic_supersede(
        &self,
        tenant_id: &TenantId,
        old_id: &ChunkId,
        new_id: &ChunkId,
        now_ms: i64,
    ) -> Result<()> {
        let mut conn = self.pool.get();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE chunks SET status = 'superseded', superseded_by = :new,
                lifecycle_updated_at_ms = :now
             WHERE tenant_id = :tenant AND chunk_id = :old",
            rusqlite::named_params! {
                ":new": new_id.to_string(),
                ":now": now_ms,
                ":tenant": tenant_id.as_str(),
                ":old": old_id.to_string(),
            },
        )?;
        tx.execute(
            "UPDATE chunks SET supersedes = :old, lifecycle_updated_at_ms = :now
             WHERE tenant_id = :tenant AND chunk_id = :new",
            rusqlite::named_params! {
                ":old": old_id.to_string(),
                ":now": now_ms,
                ":tenant": tenant_id.as_str(),
                ":new": new_id.to_string(),
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    fn set_canonical_text(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        canonical: &str,
    ) -> Result<()> {
        let conn = self.pool.get();
        conn.execute(
            "UPDATE chunks SET canonical_text = ?1 WHERE tenant_id = ?2 AND chunk_id = ?3",
            rusqlite::params![canonical, tenant_id.as_str(), chunk_id.to_string()],
        )?;
        Ok(())
    }

    fn list_expired_before(&self, tenant_id: &TenantId, now_ms: i64) -> Result<Vec<ChunkId>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT chunk_id FROM chunks
             WHERE tenant_id = ?1
               AND expires_at_ms IS NOT NULL
               AND expires_at_ms < ?2
               AND status NOT IN ('deleted', 'expired')",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), now_ms],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = Vec::new();
        for row in rows {
            let s = row?;
            let id = ChunkId::parse(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            ids.push(id);
        }
        Ok(ids)
    }

    fn list_stale_superseded(
        &self,
        tenant_id: &TenantId,
        older_than_ms: i64,
    ) -> Result<Vec<ChunkId>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT chunk_id FROM chunks
             WHERE tenant_id = ?1
               AND status IN ('superseded', 'expired')
               AND tier != 'history'
               AND lifecycle_updated_at_ms < ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![tenant_id.as_str(), older_than_ms],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = Vec::new();
        for row in rows {
            let s = row?;
            let id = ChunkId::parse(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            ids.push(id);
        }
        Ok(ids)
    }

    fn list_lifecycle_hidden(&self, tenant_id: &TenantId) -> Result<Vec<ChunkId>> {
        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT chunk_id FROM chunks
             WHERE tenant_id = ?1
               AND (status IN ('superseded', 'expired') OR tier = 'history')",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut ids = Vec::new();
        for row in rows {
            let s = row?;
            let id = ChunkId::parse(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            ids.push(id);
        }
        Ok(ids)
    }

    fn list_by_canonical_text(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        canonical: &str,
    ) -> Result<Vec<ChunkMetadata>> {
        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = :tenant
               AND (:project IS NULL OR project_id = :project)
               AND canonical_text = :canonical"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":canonical": canonical,
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn list_recent_for_project(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = :tenant
               AND (:project IS NULL OR project_id = :project)
             ORDER BY timestamp_created DESC
             LIMIT :limit"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":limit": limit as i64,
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn list_for_export(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        include_history: bool,
    ) -> Result<Vec<ChunkMetadata>> {
        let conn = self.pool.get();
        let sql = format!(
            "SELECT {CHUNK_COLUMNS} FROM chunks
             WHERE tenant_id = :tenant
               AND (:project IS NULL OR project_id = :project)
               AND (:include_history = 1 OR tier != 'history')
               AND status NOT IN ('deleted', 'error')
             ORDER BY timestamp_created ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":tenant": tenant_id.as_str(),
                ":project": project_id,
                ":include_history": i64::from(include_history),
            },
            Self::row_to_metadata,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_memory::{ArtifactKind, DatasetRef, TaskArtifact, TaskSearchFilters};
    use crate::types::ProjectId;
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
            lifecycle: LifecycleMetadata::default(),
            canonical_text: None,
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
        let conn = store.pool.get();
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
    fn task_artifact_bundle_roundtrip() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("task.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        let tenant_id = TenantId::new("tenant_task").unwrap();
        let mut artifact = TaskArtifact::new_task_start(tenant_id.clone());
        artifact.project_id = ProjectId::new(Some("project_alpha".to_string()));
        artifact.goal = Some("Profile the perturbation response".to_string());
        artifact.motivation = Some("The regulator remains unresolved".to_string());
        artifact.hypothesis = Some("RpoS drives the induced genes".to_string());
        artifact.scientific_question =
            Some("Which genes increase after the perturbation?".to_string());
        artifact.dataset_refs = vec![DatasetRef {
            name: "rna_seq".to_string(),
            version: Some("v1".to_string()),
            description: Some("Count matrix".to_string()),
        }];

        store
            .insert_task_artifact_bundle(
                &artifact,
                &["chunk-1".to_string(), "chunk-2".to_string()],
                &["task_goal".to_string(), "task_summary".to_string()],
            )
            .unwrap();

        let loaded = store
            .get_task_artifact(&tenant_id, &artifact.artifact_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.task_id, artifact.task_id);
        assert_eq!(loaded.goal, artifact.goal);
        assert_eq!(loaded.dataset_refs, artifact.dataset_refs);

        let conn = store.pool.get();
        let link_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM artifact_links WHERE artifact_id = ?1",
                rusqlite::params![artifact.artifact_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(link_count, 2);
    }

    #[test]
    fn task_projection_search_filters_by_exact_dimensions() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("task_search.db");
        let store = SqliteMetadataStore::open(&db_path).unwrap();
        let tenant_id = TenantId::new("tenant_task").unwrap();
        let chunk_a = ChunkId::new().to_string();
        let chunk_b = ChunkId::new().to_string();

        let mut run_artifact = TaskArtifact::new_run_start(tenant_id.clone(), "task-a");
        run_artifact.project_id = ProjectId::new(Some("project_alpha".to_string()));
        run_artifact.status = Some("started".to_string());
        run_artifact.tool_name = Some("mmseqs".to_string());
        run_artifact.dataset_refs = vec![DatasetRef {
            name: "rna_seq".to_string(),
            version: Some("v1".to_string()),
            description: None,
        }];
        store
            .insert_task_artifact_bundle(&run_artifact, &[chunk_a.clone()], &["run".to_string()])
            .unwrap();

        let mut other_artifact = TaskArtifact::new_run_start(tenant_id.clone(), "task-b");
        other_artifact.project_id = ProjectId::new(Some("project_beta".to_string()));
        other_artifact.status = Some("started".to_string());
        other_artifact.tool_name = Some("blast".to_string());
        other_artifact.dataset_refs = vec![DatasetRef {
            name: "proteomics".to_string(),
            version: Some("v2".to_string()),
            description: None,
        }];
        store
            .insert_task_artifact_bundle(&other_artifact, &[chunk_b], &["run".to_string()])
            .unwrap();

        let chunk_ids = store
            .search_task_projection_chunk_ids(
                &tenant_id,
                &TaskSearchFilters {
                    task_id: Some("task-a".to_string()),
                    artifact_kind: Some(ArtifactKind::RunStart),
                    status: Some("started".to_string()),
                    challenge_id: None,
                    thread_id: None,
                    reply_to_artifact_id: None,
                    artifact_role: None,
                    dataset_name: Some("rna_seq".to_string()),
                    dataset_version: Some("v1".to_string()),
                    entity_name: None,
                    entity_type: None,
                    tool_name: Some("mmseqs".to_string()),
                    project_id: Some("project_alpha".to_string()),
                    agent_id: None,
                    session_id: None,
                    requested_action: None,
                    verification_status: None,
                    relation_kind: None,
                },
                20,
            )
            .unwrap();

        assert_eq!(chunk_ids.len(), 1);
        assert_eq!(chunk_ids[0].to_string(), chunk_a);
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
        let conn = store.pool.get();
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

    #[test]
    fn chunks_table_has_lifecycle_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
        let conn = store.pool.get();
        let mut stmt = conn.prepare("PRAGMA table_info(chunks)").unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<usize, String>(1))
            .unwrap();
        let mut cols: Vec<String> = Vec::new();
        for row in rows {
            cols.push(row.unwrap());
        }
        for c in &[
            "tier",
            "supersedes",
            "superseded_by",
            "expires_at_ms",
            "review_after_ms",
            "lifecycle_updated_at_ms",
            "canonical_text",
        ] {
            assert!(cols.iter().any(|x| x == c), "missing column: {c}");
        }
    }

    #[test]
    fn row_to_metadata_fails_closed_on_unknown_status() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
        let conn = store.pool.get();
        conn.execute(
            "INSERT INTO chunks (chunk_id, tenant_id, segment_id, ordinal, chunk_type, status,
                                 timestamp_created, hash, tier, lifecycle_updated_at_ms)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                "019d0000-0000-7000-8000-000000000001",
                "t",
                0_i64,
                0_i32,
                "doc",
                "bogus_status",
                1_i64,
                "h",
                "long_term",
                0_i64
            ],
        )
        .unwrap();
        let result = store.get(
            &TenantId::new("t").unwrap(),
            &ChunkId::parse("019d0000-0000-7000-8000-000000000001").unwrap(),
        );
        assert!(result.is_err(), "expected error on unknown status");
    }

    #[test]
    fn legacy_db_gains_lifecycle_columns_on_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("m.db");

        // Simulate a DB created before A3 by opening a raw rusqlite connection and
        // creating chunks WITHOUT the 7 new columns.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
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
                    source_uri TEXT
                )",
                [],
            )
            .unwrap();
            // Insert a row with the pre-A3 column set only.
            conn.execute(
                "INSERT INTO chunks (chunk_id, tenant_id, segment_id, ordinal, chunk_type, status, timestamp_created, hash)
                 VALUES (?1, ?2, 0, 0, 'doc', 'final', 1, 'h')",
                rusqlite::params![
                    "019d0000-0000-7000-8000-000000000010",
                    "t",
                ],
            )
            .unwrap();
        }

        // Now open via SqliteMetadataStore — this should trigger
        // ensure_index_columns to backfill the 7 new columns and
        // indexes.
        let store = SqliteMetadataStore::open(&db_path).unwrap();

        // Verify the columns now exist.
        let conn = store.pool.get();
        let cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(chunks)").unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<usize, String>(1))
                .unwrap();
            let mut names = Vec::new();
            for row in rows {
                names.push(row.unwrap());
            }
            names
        };
        for c in &[
            "tier",
            "supersedes",
            "superseded_by",
            "expires_at_ms",
            "review_after_ms",
            "lifecycle_updated_at_ms",
            "canonical_text",
        ] {
            assert!(
                cols.iter().any(|x| x == c),
                "missing column after migration: {c}"
            );
        }
        drop(conn);

        // Verify the pre-existing row is still readable AND gains default lifecycle.
        let meta = store
            .get(
                &TenantId::new("t").unwrap(),
                &ChunkId::parse("019d0000-0000-7000-8000-000000000010").unwrap(),
            )
            .unwrap()
            .expect("row survives migration");
        assert_eq!(meta.status, ChunkStatus::Final);
        assert_eq!(meta.lifecycle.tier, crate::types::MemoryTier::LongTerm);
        assert_eq!(meta.lifecycle.lifecycle_updated_at_ms, 0);
        assert!(meta.lifecycle.supersedes.is_none());
        assert!(meta.lifecycle.superseded_by.is_none());
        assert!(meta.lifecycle.expires_at_ms.is_none());
        assert!(meta.lifecycle.review_after_ms.is_none());
        assert!(meta.canonical_text.is_none());
    }

    /// Seed a fresh chunk row for the given tenant and return its
    /// `ChunkMetadata`. Used by the A4 lifecycle tests to avoid
    /// repeating insert boilerplate.
    fn seed_chunk(store: &SqliteMetadataStore, tenant: &str) -> ChunkMetadata {
        use std::sync::atomic::{AtomicU64, Ordering};
        // Unique (segment_id, ordinal) pair per call so we do not
        // collide with prior seeds in the same test.
        static NEXT_SEGMENT: AtomicU64 = AtomicU64::new(1_000);
        let segment_id = NEXT_SEGMENT.fetch_add(1, Ordering::SeqCst);

        let chunk_id = ChunkId::new();
        let mut meta = create_test_metadata(tenant, &chunk_id);
        meta.segment_id = segment_id;
        meta.ordinal = 0;
        store.insert(&meta).unwrap();
        meta
    }

    #[test]
    fn update_lifecycle_writes_overlay_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
        let meta = seed_chunk(&store, "t");
        let new_id = ChunkId::new();

        store
            .update_lifecycle(
                &meta.tenant_id,
                &meta.chunk_id,
                &crate::types::LifecycleDelta {
                    status: Some(ChunkStatus::Superseded),
                    superseded_by: Some(new_id.clone()),
                    lifecycle_updated_at_ms: Some(1_700_000_000_000),
                    ..Default::default()
                },
            )
            .unwrap();

        let reloaded = store.get(&meta.tenant_id, &meta.chunk_id).unwrap().unwrap();
        assert_eq!(reloaded.status, ChunkStatus::Superseded);
        assert_eq!(
            reloaded.lifecycle.superseded_by.as_ref().unwrap(),
            &new_id
        );
        assert_eq!(reloaded.lifecycle.lifecycle_updated_at_ms, 1_700_000_000_000);
    }

    #[test]
    fn update_lifecycle_triple_state_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
        let meta = seed_chunk(&store, "t");

        // Set expires_at_ms via Some(Some(value)).
        store
            .update_lifecycle(
                &meta.tenant_id,
                &meta.chunk_id,
                &crate::types::LifecycleDelta {
                    expires_at_ms: Some(Some(1_000_000)),
                    ..Default::default()
                },
            )
            .unwrap();
        let r1 = store.get(&meta.tenant_id, &meta.chunk_id).unwrap().unwrap();
        assert_eq!(r1.lifecycle.expires_at_ms, Some(1_000_000));

        // Clear expires_at_ms via Some(None).
        store
            .update_lifecycle(
                &meta.tenant_id,
                &meta.chunk_id,
                &crate::types::LifecycleDelta {
                    expires_at_ms: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        let r2 = store.get(&meta.tenant_id, &meta.chunk_id).unwrap().unwrap();
        assert!(r2.lifecycle.expires_at_ms.is_none());
    }

    #[test]
    fn atomic_supersede_links_both_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
        let old = seed_chunk(&store, "t");
        let new = seed_chunk(&store, "t");
        assert_ne!(old.chunk_id, new.chunk_id);

        store
            .atomic_supersede(
                &old.tenant_id,
                &old.chunk_id,
                &new.chunk_id,
                1_800_000_000_000,
            )
            .unwrap();

        let old_r = store.get(&old.tenant_id, &old.chunk_id).unwrap().unwrap();
        assert_eq!(old_r.status, ChunkStatus::Superseded);
        assert_eq!(
            old_r.lifecycle.superseded_by.as_ref().unwrap(),
            &new.chunk_id
        );
        assert_eq!(old_r.lifecycle.lifecycle_updated_at_ms, 1_800_000_000_000);

        let new_r = store.get(&new.tenant_id, &new.chunk_id).unwrap().unwrap();
        assert_eq!(
            new_r.lifecycle.supersedes.as_ref().unwrap(),
            &old.chunk_id
        );
        assert_eq!(new_r.lifecycle.lifecycle_updated_at_ms, 1_800_000_000_000);
    }

    #[test]
    fn list_expired_before_returns_only_old_expiring_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
        let tenant = TenantId::new("t").unwrap();

        // One expires at 500 (before 1000).
        let early = seed_chunk(&store, "t");
        store
            .update_lifecycle(
                &tenant,
                &early.chunk_id,
                &crate::types::LifecycleDelta {
                    expires_at_ms: Some(Some(500)),
                    ..Default::default()
                },
            )
            .unwrap();

        // One expires at 2000 (after 1000).
        let later = seed_chunk(&store, "t");
        store
            .update_lifecycle(
                &tenant,
                &later.chunk_id,
                &crate::types::LifecycleDelta {
                    expires_at_ms: Some(Some(2000)),
                    ..Default::default()
                },
            )
            .unwrap();

        // One with no expiry.
        let never = seed_chunk(&store, "t");

        let expired = store.list_expired_before(&tenant, 1000).unwrap();
        assert_eq!(
            expired.len(),
            1,
            "only the row expiring before 1000 should appear"
        );
        assert_eq!(expired[0], early.chunk_id);
        assert!(!expired.contains(&later.chunk_id));
        assert!(!expired.contains(&never.chunk_id));
    }

    #[test]
    fn list_by_canonical_text_filters_by_project_and_text() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SqliteMetadataStore::open(&tmp.path().join("m.db")).unwrap();
        let tenant = TenantId::new("t").unwrap();

        // Three rows: two share canonical='alpha' but differ by project;
        // one has canonical='beta'.
        let mut a = seed_chunk(&store, "t");
        a.project_id = Some("proj_a".to_string());
        a.canonical_text = Some("alpha".to_string());
        store.insert(&a).unwrap();

        let mut b = seed_chunk(&store, "t");
        b.project_id = Some("proj_b".to_string());
        b.canonical_text = Some("alpha".to_string());
        store.insert(&b).unwrap();

        let mut c = seed_chunk(&store, "t");
        c.project_id = Some("proj_a".to_string());
        c.canonical_text = Some("beta".to_string());
        store.insert(&c).unwrap();

        // Filter by project=proj_a + canonical=alpha → only `a`.
        let scoped = store
            .list_by_canonical_text(&tenant, Some("proj_a"), "alpha")
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].chunk_id, a.chunk_id);

        // Project=None matches across all projects → both `a` and `b`.
        let all_projects = store
            .list_by_canonical_text(&tenant, None, "alpha")
            .unwrap();
        assert_eq!(all_projects.len(), 2);
        let ids: std::collections::HashSet<_> =
            all_projects.iter().map(|m| m.chunk_id.clone()).collect();
        assert!(ids.contains(&a.chunk_id));
        assert!(ids.contains(&b.chunk_id));

        // Non-matching canonical returns empty.
        let empty = store
            .list_by_canonical_text(&tenant, None, "gamma")
            .unwrap();
        assert!(empty.is_empty());
    }
}
