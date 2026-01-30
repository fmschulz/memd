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
pub mod tenant;
pub mod tombstone;
pub mod wal;

use std::collections::HashMap;

use async_trait::async_trait;

use crate::error::Result;
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
    async fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<MemoryChunk>>;

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
}

pub use dense::{DenseSearchConfig, DenseSearchResult, DenseSearcher};
pub use hybrid::{HybridConfig, HybridSearcher, HybridSearchResult, HybridTiming, SearchContext};
pub use memory::MemoryStore;
pub use persistent::{PersistentStore, PersistentStoreConfig};
pub use tenant::TenantManager;
pub use tombstone::TombstoneSet;
