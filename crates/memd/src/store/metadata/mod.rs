//! Metadata store module
//!
//! Handles chunk metadata queries with tenant isolation.
//! Payloads are NOT stored here - only in segment files.

pub mod pool;
pub mod sqlite;

pub use pool::{PooledConnection, SqliteConnectionPool};
pub use sqlite::SqliteMetadataStore;

use crate::error::Result;
use crate::types::{ChunkId, ChunkStatus, ChunkType, LifecycleDelta, LifecycleMetadata, TenantId};

/// Index lifecycle state for a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Pending,
    Indexed,
    Failed,
}

impl IndexState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Indexed => "indexed",
            Self::Failed => "failed",
        }
    }
}

/// Metadata record for a chunk (no payload)
#[derive(Debug, Clone)]
pub struct ChunkMetadata {
    pub chunk_id: ChunkId,
    pub tenant_id: TenantId,
    pub project_id: Option<String>,
    pub segment_id: u64,
    pub ordinal: u32,
    pub chunk_type: ChunkType,
    pub status: ChunkStatus,
    pub timestamp_created: i64,
    pub hash: String,
    pub source_uri: Option<String>,
    /// Lifecycle overlay (tier, supersession edges, retention window).
    ///
    /// Added in A3. For chunks inserted before the lifecycle columns
    /// existed, this falls back to `LifecycleMetadata::default()` at
    /// read time (long-term tier, no supersession, no expiry).
    pub lifecycle: LifecycleMetadata,
    /// Optional canonical text for writer-driven digests / supersession
    /// by content identity. Orthogonal to the chunk payload; empty for
    /// regular chunks.
    pub canonical_text: Option<String>,
}

/// Metadata store trait
pub trait MetadataStore: Send + Sync {
    /// Insert metadata for a new chunk
    fn insert(&self, metadata: &ChunkMetadata) -> Result<()>;

    /// Insert multiple metadata rows atomically.
    ///
    /// Implementations should treat this as all-or-nothing.
    fn insert_many(&self, metadata: &[ChunkMetadata]) -> Result<()>;

    /// Get metadata by chunk_id (tenant_id required for isolation)
    fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<ChunkMetadata>>;

    /// Check for a chunk_id across all tenants.
    ///
    /// Chunk ids are globally unique in the SQLite schema today. Importers
    /// use this before preserving externally supplied ids so a cross-tenant
    /// import cannot replace an existing row through `INSERT OR REPLACE`.
    fn chunk_id_exists(&self, chunk_id: &ChunkId) -> Result<bool>;

    /// List chunks for a tenant (non-deleted only)
    fn list(&self, tenant_id: &TenantId, limit: usize, offset: usize)
        -> Result<Vec<ChunkMetadata>>;

    /// Mark chunk as deleted (soft delete)
    fn mark_deleted(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool>;

    /// Get all chunk_ids for a segment (for tombstone sync)
    fn get_by_segment(&self, segment_id: u64) -> Result<Vec<ChunkMetadata>>;

    /// Count chunks by status for a tenant
    fn count_by_status(&self, tenant_id: &TenantId) -> Result<(usize, usize)>;

    /// Get all deleted chunk IDs for a tenant (for compaction)
    fn get_deleted_chunk_ids(&self, tenant_id: &TenantId) -> Result<Vec<ChunkId>>;

    /// Mark chunks as pending indexing.
    fn mark_index_pending(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
        now_ms: i64,
    ) -> Result<()>;

    /// Mark chunks as successfully indexed.
    fn mark_indexed(&self, tenant_id: &TenantId, chunk_ids: &[ChunkId], now_ms: i64) -> Result<()>;

    /// Mark one chunk as failed indexing and increment attempt count.
    fn mark_index_failed(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        error: &str,
        now_ms: i64,
    ) -> Result<()>;

    /// List pending index chunk IDs for one tenant.
    fn list_pending_index_chunk_ids(
        &self,
        tenant_id: &TenantId,
        limit: usize,
    ) -> Result<Vec<ChunkId>>;

    /// Count pending/indexed/failed chunks for one tenant.
    fn count_by_index_state(&self, tenant_id: &TenantId) -> Result<(usize, usize, usize)>;

    /// Apply a lifecycle delta to one chunk row.
    ///
    /// Triple-state semantics match `LifecycleDelta`:
    /// - `None` leaves the field unchanged.
    /// - `Some(value)` sets (for simple fields) or sets-or-clears (for
    ///   nested `Option<Option<T>>` fields such as `expires_at_ms`).
    fn update_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        delta: &LifecycleDelta,
    ) -> Result<()>;

    /// Atomically link a supersession pair inside one SQL transaction.
    ///
    /// Marks `old_id` as `Superseded` with `superseded_by = new_id`, and
    /// sets `supersedes = old_id` on `new_id`. Both rows receive the
    /// same `lifecycle_updated_at_ms = now_ms`.
    fn atomic_supersede(
        &self,
        tenant_id: &TenantId,
        old_id: &ChunkId,
        new_id: &ChunkId,
        now_ms: i64,
    ) -> Result<()>;

    /// Set the optional canonical text used by writer-driven digest /
    /// supersession-by-content-identity flows.
    fn set_canonical_text(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        canonical: &str,
    ) -> Result<()>;

    /// List chunk IDs whose retention window has elapsed before `now_ms`.
    ///
    /// Skips rows already in terminal lifecycle states (`deleted`,
    /// `expired`) so sweeps do not retouch previously expired rows.
    fn list_expired_before(&self, tenant_id: &TenantId, now_ms: i64) -> Result<Vec<ChunkId>>;

    /// List superseded/expired chunk IDs older than the given cutoff
    /// that are not yet demoted to the history tier. Feeds history
    /// promotion (C4).
    fn list_stale_superseded(
        &self,
        tenant_id: &TenantId,
        older_than_ms: i64,
    ) -> Result<Vec<ChunkId>>;

    /// List chunk IDs hidden by lifecycle (superseded, expired, or
    /// history-tier). Used by compaction (B2) to extend the HNSW
    /// excluded-ID set beyond soft-deleted rows.
    fn list_lifecycle_hidden(&self, tenant_id: &TenantId) -> Result<Vec<ChunkId>>;

    /// List chunks whose `canonical_text` equals `canonical`, optionally
    /// scoped to `project_id`. Feeds conflict-aware ingest (D3).
    fn list_by_canonical_text(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        canonical: &str,
    ) -> Result<Vec<ChunkMetadata>>;

    /// List the most recently created chunks for a tenant, optionally
    /// scoped to a project.
    fn list_recent_for_project(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChunkMetadata>>;

    /// List chunks for OMF export (F2).
    ///
    /// Excludes `deleted` and `error` rows unconditionally; includes
    /// `superseded` and `expired` so callers can decide whether to emit
    /// them. Excludes `history`-tier rows unless `include_history` is
    /// set. Ordered by creation time ascending for stable export.
    fn list_for_export(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        include_history: bool,
    ) -> Result<Vec<ChunkMetadata>>;
}
