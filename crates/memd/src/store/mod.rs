//! Storage module for memd
//!
//! Provides the Store trait and implementations for memory chunk storage.
//! The in-memory store is used as a baseline before persistent storage.

pub mod dense;
pub mod feedback;
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
use crate::task_memory::{
    TaskArtifact, TaskArtifactWriteResult, TaskProjection, TaskSearchFilters,
};
use crate::tiered::TieredTiming;
use crate::types::{ChunkId, MemoryChunk, TenantId};
pub use feedback::{
    apply_feedback_scores, normalize_query, FeedbackConfig, FeedbackEntry, RelevanceLabel,
};

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

pub(crate) fn score_candidate_chunk(query: &str, chunk: &MemoryChunk) -> f32 {
    if query.trim().is_empty() {
        return 1.0;
    }

    let haystack = format!("{} {}", chunk.text, chunk.tags.join(" ")).to_ascii_lowercase();
    let terms = query
        .split_whitespace()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return 1.0;
    }

    let mut score = 0.0f32;
    for term in terms {
        if haystack.contains(&term) {
            score += 1.0;
        }
    }

    if haystack.contains(&query.to_ascii_lowercase()) {
        score += 1.5;
    }

    score
}

pub(crate) fn rank_candidate_chunks(
    mut chunks: Vec<MemoryChunk>,
    query: &str,
    k: usize,
) -> Vec<(MemoryChunk, f32)> {
    let mut scored = chunks
        .drain(..)
        .map(|chunk| {
            let score = score_candidate_chunk(query, &chunk);
            (chunk, score)
        })
        .collect::<Vec<_>>();

    scored.sort_by(|(left_chunk, left_score), (right_chunk, right_score)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right_chunk
                    .timestamp_created
                    .cmp(&left_chunk.timestamp_created)
            })
    });
    if !query.trim().is_empty() {
        scored.retain(|(_, score)| *score > 0.0);
    }
    scored.truncate(k);
    scored
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

    /// Store a canonical task artifact and its retrieval projections.
    async fn add_task_artifact(
        &self,
        _artifact: TaskArtifact,
        _projections: Vec<TaskProjection>,
    ) -> Result<TaskArtifactWriteResult> {
        Err(MemdError::StorageError(
            "task artifacts not supported by this store".into(),
        ))
    }

    /// Fetch one canonical task artifact by ID.
    async fn get_task_artifact(
        &self,
        _tenant_id: &TenantId,
        _artifact_id: &str,
    ) -> Result<Option<TaskArtifact>> {
        Ok(None)
    }

    /// List canonical task artifacts for one logical task.
    async fn list_task_artifacts(
        &self,
        _tenant_id: &TenantId,
        _task_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        Ok(Vec::new())
    }

    /// Resolve candidate projection chunk IDs using exact task filters.
    async fn search_task_projection_chunk_ids(
        &self,
        _tenant_id: &TenantId,
        _filters: &TaskSearchFilters,
        _limit: usize,
    ) -> Result<Vec<ChunkId>> {
        Ok(Vec::new())
    }

    /// Rerank a prefiltered set of candidate chunk IDs for one query.
    async fn rerank_chunks_for_query(
        &self,
        tenant_id: &TenantId,
        query: &str,
        chunk_ids: &[ChunkId],
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        let mut chunks = Vec::with_capacity(chunk_ids.len());
        for chunk_id in chunk_ids {
            if let Some(chunk) = self.get(tenant_id, chunk_id).await? {
                chunks.push(chunk);
            }
        }
        Ok(rank_candidate_chunks(chunks, query, k))
    }

    /// Record relevance feedback for retrieval quality adaptation.
    async fn add_feedback(&self, _feedback: FeedbackEntry) -> Result<()> {
        Ok(())
    }

    /// List feedback events for a query (implementation may cap internally).
    async fn list_feedback(
        &self,
        _tenant_id: &TenantId,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<FeedbackEntry>> {
        Ok(Vec::new())
    }

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

    /// List chunks for a tenant with pagination semantics.
    ///
    /// Default implementation uses `search` with an empty query and applies
    /// offset slicing. Backends with metadata indexes should override this.
    async fn list_chunks(
        &self,
        tenant_id: &TenantId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MemoryChunk>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let fetch = limit.saturating_add(offset);
        let mut chunks = self.search(tenant_id, "", fetch).await?;
        if offset >= chunks.len() {
            return Ok(Vec::new());
        }
        Ok(chunks.drain(offset..).take(limit).collect())
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
