//! SQLite-backed metadata store
//!
//! Implements MetadataStore using SQLite with WAL mode for crash safety
//! and tenant isolation via indexes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::pool::SqliteConnectionPool;
use super::{ChunkMetadata, IndexState, MetadataStore};
use crate::consolidate::journal::{
    ConsolidationEntryRecord, ConsolidationRun, ConsolidationRunId, ConsolidationState,
    LineageRelation, MemoryLineage, PromotionOutcome,
};
use crate::error::{MemdError, Result};
use crate::store::outcome::{validate_outcome_event, validate_retrieval_episode};
use crate::store::usage::{UsageEvent, UsageEventRecord};
use crate::store::{
    decayed_outcome_weight, normalize_query, DuplicateExample, DuplicateHealth, FeedbackEntry,
    HealthCounts, IndexCoverageHealth, OutcomeEvent, OutcomeEventId, OutcomeKind, OutcomePrior,
    OutcomeVerifier, PayloadHealth, RankingPolicyMode, RelevanceLabel, RetrievalEpisode,
    RetrievalEpisodeId, RetrievalEpisodeItem, StoreHealthSnapshot,
};
use crate::task_memory::{ArtifactKind, TaskArtifact, TaskRecord, TaskSearchFilters};
use crate::types::{
    ChunkId, ChunkStatus, ChunkType, LifecycleDelta, LifecycleMetadata, MemoryTier, TenantId,
};

mod chunks;
mod consolidation;
mod episodes;
mod feedback;
mod lifecycle;
mod schema;
mod tasks;

#[cfg(test)]
mod tests;

fn sql_decode_error(
    column: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(error))
}

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

    /// Open a shared-cache in-memory metadata store and run normal migrations.
    pub fn open_in_memory() -> Result<Self> {
        static NEXT_IN_MEMORY_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_IN_MEMORY_ID.fetch_add(1, Ordering::Relaxed);
        let uri = format!(
            "file:memd_ro_missing_{}_{}?mode=memory&cache=shared",
            std::process::id(),
            id
        );
        let pool = SqliteConnectionPool::open_uri_with_max(
            &uri,
            crate::store::metadata::pool::DEFAULT_POOL_MAX_SIZE,
        )?;
        let store = Self { pool };
        store.init_schema()?;
        Ok(store)
    }

    /// Return SQLite page and freelist counts for diagnostics/tests.
    pub fn page_count_snapshot(&self) -> Result<(u64, u64)> {
        let conn = self.pool.get();
        let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let freelist_count: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        Ok((page_count.max(0) as u64, freelist_count.max(0) as u64))
    }

    /// Checkpoint the metadata WAL. `TRUNCATE` is used only for SQLite's own
    /// metadata WAL, not the tenant append-only WAL.
    pub fn checkpoint_wal(&self) -> Result<()> {
        let conn = self.pool.get();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    /// Run SQLite VACUUM on the metadata database.
    pub fn vacuum(&self) -> Result<()> {
        let conn = self.pool.get();
        conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Return active chunk counts by project for a tenant.
    pub fn project_counts(
        &self,
        tenant_id: &TenantId,
        limit: usize,
    ) -> Result<Vec<(Option<String>, usize)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.pool.get();
        let mut stmt = conn.prepare(
            "SELECT project_id, COUNT(*) AS active_chunks
             FROM chunks
             WHERE tenant_id = ?1 AND status NOT IN ('candidate', 'deleted')
             GROUP BY project_id
             ORDER BY active_chunks DESC
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(rusqlite::params![tenant_id.as_str(), limit as i64], |row| {
            let project_id: Option<String> = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((project_id, count.max(0) as usize))
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}
