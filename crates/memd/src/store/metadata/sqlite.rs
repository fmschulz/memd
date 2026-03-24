//! SQLite-backed metadata store
//!
//! Implements MetadataStore using SQLite with WAL mode for crash safety
//! and tenant isolation via indexes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use super::{ChunkMetadata, IndexState, MetadataStore};
use crate::error::Result;
use crate::store::{normalize_query, FeedbackEntry, RelevanceLabel};
use crate::task_memory::{ArtifactKind, TaskArtifact, TaskRecord, TaskSearchFilters};
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

        let mut conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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

        let conn = self.conn.lock().unwrap();
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

        let conn = self.conn.lock().unwrap();
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

        let conn = self.conn.lock().unwrap();
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

        let conn = store.conn.lock().unwrap();
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
