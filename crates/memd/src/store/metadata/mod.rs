//! Metadata store module
//!
//! Handles chunk metadata queries with tenant isolation.
//! Payloads are NOT stored here - only in segment files.

pub mod sqlite;

pub use sqlite::SqliteMetadataStore;

use crate::error::Result;
use crate::types::{ChunkId, ChunkStatus, ChunkType, TenantId};

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
}
