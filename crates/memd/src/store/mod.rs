//! Storage module for memd
//!
//! Provides the Store trait and implementations for memory chunk storage.
//! The in-memory store is used as a baseline before persistent storage.

pub mod dense;
pub mod hybrid;
pub mod memory;
pub mod metadata;
pub mod persistent;
pub mod segment;
pub mod shared_add;
pub mod tenant;
pub mod tombstone;
pub mod wal;

use std::collections::HashMap;

use async_trait::async_trait;

use crate::compaction::{CompactionMetrics, CompactionResult};
use crate::error::{MemdError, Result};
use crate::metrics::IndexStats;
use crate::tiered::TieredTiming;
use crate::types::{ChunkId, MemoryChunk, TenantId};

/// Statistics for a tenant's store
#[derive(Debug, Clone, Default)]
pub struct StoreStats {
    /// Total number of chunks (including deleted)
    pub total_chunks: usize,
    /// Number of soft-deleted chunks
    pub deleted_chunks: usize,
    /// Count of chunks by type
    pub chunk_types: HashMap<String, usize>,
}

/// Store trait for memory operations
///
/// Defines the interface for all storage backends (in-memory, persistent, etc.)
#[async_trait]
pub trait Store: Send + Sync {
    /// Add a chunk to the store
    ///
    /// Returns the chunk_id of the stored chunk.
    async fn add(&self, chunk: MemoryChunk) -> Result<ChunkId>;

    /// Add multiple chunks in a batch
    ///
    /// Returns the chunk_ids of all stored chunks.
    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>>;

    /// Get chunk by ID (respects tenant isolation)
    ///
    /// Returns None if the chunk doesn't exist or belongs to a different tenant.
    async fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<MemoryChunk>>;

    /// Search chunks (stub: returns all non-deleted chunks matching tenant)
    ///
    /// The search is currently a simple substring match - real vector search
    /// comes in Phase 3.
    async fn search(&self, tenant_id: &TenantId, query: &str, k: usize)
        -> Result<Vec<MemoryChunk>>;

    /// Search with scores (default: calls search with score 1.0)
    ///
    /// Returns chunks with their relevance scores.
    /// Default implementation calls search() and assigns score 1.0 to all results.
    /// PersistentStore overrides this with real dense search using HNSW.
    async fn search_with_scores(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        let chunks = self.search(tenant_id, query, k).await?;
        Ok(chunks.into_iter().map(|c| (c, 1.0)).collect())
    }

    /// Soft delete a chunk
    ///
    /// Returns true if the chunk was found and deleted, false if not found.
    async fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool>;

    /// Get statistics for a tenant
    async fn stats(&self, tenant_id: &TenantId) -> Result<StoreStats>;

    /// Search with tier info for debugging
    ///
    /// Returns results with tiered timing and optional tier decisions.
    /// Default implementation calls search_with_scores and returns None for timing.
    /// PersistentStore overrides this with real tiered search info.
    async fn search_with_tier_info(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<(Vec<(MemoryChunk, f32)>, Option<TieredTiming>)> {
        let results = self.search_with_scores(tenant_id, query, k).await?;
        Ok((results, None))
    }

    /// Get tiered search statistics
    ///
    /// Returns None if tiered search is not enabled.
    /// PersistentStore overrides this with real tiered stats.
    fn get_tiered_stats(&self) -> Option<persistent::TieredStats> {
        None
    }

    /// Get dense index statistics
    ///
    /// Returns per-tenant index stats when available.
    /// Default implementation returns empty stats.
    fn get_index_stats(&self, _tenant_id: Option<&TenantId>) -> HashMap<String, IndexStats> {
        HashMap::new()
    }

    /// Run compaction for a tenant regardless of thresholds
    ///
    /// Forces compaction to run even if no thresholds are exceeded.
    /// Default implementation returns error (compaction not supported).
    /// PersistentStore overrides with real implementation.
    fn run_compaction(&self, _tenant_id: &TenantId) -> Result<CompactionResult> {
        Err(MemdError::StorageError("compaction not supported".into()))
    }

    /// Run compaction for a tenant if thresholds are exceeded
    ///
    /// Returns None if no compaction needed (all thresholds below limits).
    /// Returns Some(CompactionResult) if compaction was performed.
    /// Default implementation returns Ok(None).
    /// PersistentStore overrides with real implementation.
    fn run_compaction_if_needed(&self, _tenant_id: &TenantId) -> Result<Option<CompactionResult>> {
        Ok(None)
    }

    /// Get compaction metrics for a tenant
    ///
    /// Returns metrics about tombstone ratio, segment count, HNSW staleness.
    /// Default implementation returns error (not available).
    /// PersistentStore overrides with real implementation.
    fn get_compaction_metrics(&self, _tenant_id: &TenantId) -> Result<CompactionMetrics> {
        Err(MemdError::StorageError(
            "compaction metrics not available".into(),
        ))
    }
}

pub use dense::{DenseSearchConfig, DenseSearchResult, DenseSearcher};
pub use hybrid::{HybridConfig, HybridSearchResult, HybridSearcher, HybridTiming, SearchContext};
pub use memory::MemoryStore;
pub use persistent::{PersistentStore, PersistentStoreConfig, TieredStats};
pub use shared_add::{split_for_add, ADD_CHUNK_THRESHOLD};
pub use tenant::TenantManager;
pub use tombstone::TombstoneSet;
