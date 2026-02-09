//! Metadata store module
//!
//! Handles chunk metadata queries with tenant isolation.
//! Payloads are NOT stored here - only in segment files.

pub mod sqlite;

pub use sqlite::SqliteMetadataStore;

use crate::error::Result;
use crate::types::{ChunkId, ChunkStatus, ChunkType, TenantId};

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
}
