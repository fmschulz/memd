//! Storage module for memd
//!
//! Provides the Store trait and implementations for memory chunk storage.
//! The in-memory store is used as a baseline before persistent storage.

pub mod dense;
pub mod feedback;
pub mod hybrid;
pub mod memory;
pub mod metadata;
pub mod outcome;
pub mod persistent;
pub mod segment;
pub mod shared_add;
pub mod supersession;
pub mod tenant;
pub mod tombstone;
pub mod usage;
pub mod wal;
pub mod writer_lock;

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::compaction::{CompactionMetrics, CompactionResult};
use crate::error::{MemdError, Result};
use crate::metrics::IndexStats;
use crate::task_memory::{
    TaskArtifact, TaskArtifactWriteResult, TaskProjection, TaskRecord, TaskSearchFilters,
};
use crate::tiered::TieredTiming;
use crate::types::lifecycle::{LifecycleMetadata, ResolvedChunk};
use crate::types::{ChunkId, MemoryChunk, TenantId};
pub use feedback::{
    apply_feedback_scores, normalize_query, FeedbackConfig, FeedbackEntry, RelevanceLabel,
};
pub use outcome::{
    apply_rendered_order, decayed_outcome_weight, stable_query_hash, OutcomeEvent, OutcomeEventId,
    OutcomeKind, OutcomePrior, OutcomeVerifier, RankingPolicyMode, RetrievalEpisode,
    RetrievalEpisodeId, RetrievalEpisodeItem, MAX_OUTCOME_ADJUSTMENT, OUTCOME_HALF_LIFE_MS,
    OUTCOME_POLICY_VERSION,
};

/// Statistics for a tenant's store
#[derive(Debug, Clone, Default)]
pub struct StoreStats {
    /// Total number of chunks (including deleted)
    pub total_chunks: usize,
    /// Number of non-deleted, non-candidate storage rows.
    pub active_chunks: usize,
    /// Staged consolidation outputs, hidden from public retrieval.
    pub candidate_chunks: usize,
    /// Number of soft-deleted chunks
    pub deleted_chunks: usize,
    /// Backward-compatible count of active chunks by type.
    pub chunk_types: HashMap<String, usize>,
    /// Count of active chunks by type.
    pub chunk_types_active: HashMap<String, usize>,
    /// Count of deleted chunks by type.
    pub chunk_types_deleted: HashMap<String, usize>,
    /// Count of all chunks by type, including deleted rows.
    pub chunk_types_all: HashMap<String, usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthCounts {
    pub active_chunks: usize,
    /// Staged consolidation outputs. Counted for diagnostics but never as
    /// active/visible memory.
    #[serde(default)]
    pub candidate_chunks: usize,
    pub deleted_chunks: usize,
    pub expired_chunks: usize,
    pub superseded_chunks: usize,
    pub history_chunks: usize,
    pub total_chunks: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DuplicateExample {
    pub canonical_text_preview: String,
    pub count: usize,
    pub byte_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DuplicateHealth {
    pub unique_text_count: usize,
    pub exact_duplicate_group_count: usize,
    pub duplicate_row_count: usize,
    pub duplicate_row_ratio: f64,
    pub duplicate_byte_ratio: f64,
    #[serde(default)]
    pub examples: Vec<DuplicateExample>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexCoverageHealth {
    pub pending: usize,
    pub indexed: usize,
    pub failed: usize,
    pub indexed_percentage: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PayloadHealth {
    pub p50_canonical_text_bytes: usize,
    pub p95_canonical_text_bytes: usize,
    pub max_canonical_text_bytes: usize,
    pub p95_artifact_json_bytes: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoreHealthSnapshot {
    pub counts: HealthCounts,
    pub chunk_types_active: HashMap<String, usize>,
    pub chunk_types_all: HashMap<String, usize>,
    pub duplicates: DuplicateHealth,
    pub index_coverage: IndexCoverageHealth,
    pub payload: PayloadHealth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalMutationOutcome {
    Unavailable,
    Clean,
    OwnWrites,
    /// An external metadata.db mutation was observed. `repaired` is true when
    /// the follow-up HNSW repair completed within the warm request's bounded
    /// foreground budget; it is false when the repair was left to finish in
    /// the background (or one was already running), in which case `warm
    /// status` reports `repair_in_progress`.
    External {
        repaired: bool,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RywProbeStats {
    pub checks: u64,
    pub external_detected: u64,
    pub repairs: u64,
    /// True when a store-owned HNSW repair is currently running.
    pub repair_in_progress: bool,
}

pub(crate) fn score_candidate_chunk(query: &str, chunk: &MemoryChunk) -> f32 {
    if query.trim().is_empty() {
        return 1.0;
    }

    let haystack = format!("{} {}", chunk.text, chunk.tags.join(" ")).to_ascii_lowercase();
    let terms = query
        .split_whitespace()
        .filter_map(|term| {
            let normalized = term
                .trim_matches(|character: char| {
                    !character.is_alphanumeric() && character != '_' && character != '-'
                })
                .to_ascii_lowercase();
            normalized
                .chars()
                .any(char::is_alphanumeric)
                .then_some(normalized)
        })
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return 0.0;
    }
    let natural_haystack_terms = haystack
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();

    let mut score = 0.0f32;
    for term in terms {
        let matched = if term
            .chars()
            .any(|character| !character.is_ascii_alphanumeric())
        {
            haystack.contains(&term)
        } else {
            natural_haystack_terms.iter().any(|candidate| {
                *candidate == term
                    || (candidate.len().min(term.len()) >= 4
                        && (candidate.starts_with(&term) || term.starts_with(*candidate)))
            })
        };
        if matched {
            score += 1.0;
        }
    }

    if haystack.contains(&query.to_ascii_lowercase()) {
        score += 1.5;
    }

    score
}

pub(crate) fn compare_scored_chunks(
    (left_chunk, left_score): &(MemoryChunk, f32),
    (right_chunk, right_score): &(MemoryChunk, f32),
) -> std::cmp::Ordering {
    right_score
        .partial_cmp(left_score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            right_chunk
                .timestamp_created
                .cmp(&left_chunk.timestamp_created)
        })
        .then_with(|| left_chunk.chunk_id.cmp(&right_chunk.chunk_id))
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

    scored.sort_by(compare_scored_chunks);
    if !query.trim().is_empty() {
        scored.retain(|(_, score)| *score > 0.0);
    }
    scored.truncate(k);
    scored
}

/// Marker prefix for [`unsupported_store_capability`] errors so callers can
/// classify optional-capability gaps without matching on free text.
const UNSUPPORTED_STORE_CAPABILITY_PREFIX: &str = "store capability unsupported";

fn unsupported_store_capability(capability: &str) -> MemdError {
    MemdError::StorageError(format!(
        "{UNSUPPORTED_STORE_CAPABILITY_PREFIX}: {capability}"
    ))
}

/// True when `error` marks an optional store capability the active backend
/// does not implement, as produced by `unsupported_store_capability`. Callers
/// that can degrade (e.g. ranking without outcome priors) match on this
/// instead of swallowing every storage error.
pub(crate) fn is_unsupported_store_capability(error: &MemdError) -> bool {
    matches!(
        error,
        MemdError::StorageError(message) if message.starts_with(UNSUPPORTED_STORE_CAPABILITY_PREFIX)
    )
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

    /// Add one logical chunk and return its primary id plus every physical id
    /// created by splitting. Backends that do not split may use the default.
    async fn add_with_stored_ids(&self, chunk: MemoryChunk) -> Result<(ChunkId, Vec<ChunkId>)> {
        let chunk_id = self.add(chunk).await?;
        Ok((chunk_id.clone(), vec![chunk_id]))
    }

    /// Add multiple chunks in a batch
    ///
    /// Returns one primary chunk id for each logical input chunk.
    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>>;

    /// Store a canonical task artifact and its retrieval projections.
    async fn add_task_artifact(
        &self,
        _artifact: TaskArtifact,
        _projections: Vec<TaskProjection>,
    ) -> Result<TaskArtifactWriteResult> {
        Err(unsupported_store_capability("task artifacts"))
    }

    /// Fetch one canonical task artifact by ID.
    async fn get_task_artifact(
        &self,
        _tenant_id: &TenantId,
        _artifact_id: &str,
    ) -> Result<Option<TaskArtifact>> {
        Err(unsupported_store_capability("task artifact lookup"))
    }

    /// List canonical task artifacts for one logical task.
    async fn list_task_artifacts(
        &self,
        _tenant_id: &TenantId,
        _task_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        Err(unsupported_store_capability("task artifact listing"))
    }

    /// List canonical task artifacts for a logical thread.
    async fn list_thread_artifacts(
        &self,
        _tenant_id: &TenantId,
        _thread_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        Err(unsupported_store_capability("thread artifact listing"))
    }

    /// List logical task records for a tenant, optionally scoped to one project.
    async fn list_tasks(
        &self,
        _tenant_id: &TenantId,
        _project_id: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        Err(unsupported_store_capability("task listing"))
    }

    /// List tenants known to this store.
    async fn list_tenants(&self) -> Result<Vec<TenantId>> {
        Err(unsupported_store_capability("tenant listing"))
    }

    /// Resolve candidate projection chunk IDs using exact task filters.
    async fn search_task_projection_chunk_ids(
        &self,
        _tenant_id: &TenantId,
        _filters: &TaskSearchFilters,
        _limit: usize,
    ) -> Result<Vec<ChunkId>> {
        Err(unsupported_store_capability("task projection search"))
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

    /// Rerank a fixed candidate set at a fixed wall-clock reference.
    async fn rerank_chunks_for_query_at(
        &self,
        tenant_id: &TenantId,
        query: &str,
        chunk_ids: &[ChunkId],
        k: usize,
        _ranking_time_ms: i64,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        self.rerank_chunks_for_query(tenant_id, query, chunk_ids, k)
            .await
    }

    /// Resolve canonical artifacts for retrieval projection chunk IDs.
    async fn resolve_artifacts_for_chunks(
        &self,
        _tenant_id: &TenantId,
        _chunk_ids: &[ChunkId],
    ) -> Result<HashMap<String, TaskArtifact>> {
        Err(unsupported_store_capability(
            "artifact projection resolution",
        ))
    }

    /// Record relevance feedback for retrieval quality adaptation.
    async fn add_feedback(&self, _feedback: FeedbackEntry) -> Result<()> {
        Err(unsupported_store_capability("retrieval feedback"))
    }

    /// List feedback events for a query (implementation may cap internally).
    async fn list_feedback(
        &self,
        _tenant_id: &TenantId,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<FeedbackEntry>> {
        Err(unsupported_store_capability("retrieval feedback listing"))
    }

    /// Persist one privacy-safe retrieval episode and its candidate set.
    async fn record_retrieval_episode(
        &self,
        _episode: RetrievalEpisode,
        _items: Vec<RetrievalEpisodeItem>,
    ) -> Result<()> {
        Err(unsupported_store_capability("retrieval episodes"))
    }

    /// Load one retrieval episode by tenant and ID.
    async fn get_retrieval_episode(
        &self,
        _tenant_id: &TenantId,
        _episode_id: &RetrievalEpisodeId,
    ) -> Result<Option<(RetrievalEpisode, Vec<RetrievalEpisodeItem>)>> {
        Err(unsupported_store_capability("retrieval episode lookup"))
    }

    /// Update the observable rendered order after caller-side post-processing.
    async fn finalize_retrieval_episode(
        &self,
        _tenant_id: &TenantId,
        _episode_id: &RetrievalEpisodeId,
        _rendered_chunk_ids: &[ChunkId],
    ) -> Result<()> {
        Err(unsupported_store_capability(
            "retrieval episode finalization",
        ))
    }

    /// Persist an explicit task outcome after validating episode attribution.
    async fn record_outcome(&self, _tenant_id: &TenantId, _event: OutcomeEvent) -> Result<()> {
        Err(unsupported_store_capability("outcome recording"))
    }

    /// List immutable outcomes attached to one retrieval episode.
    async fn list_outcomes_for_episode(
        &self,
        _tenant_id: &TenantId,
        _episode_id: &RetrievalEpisodeId,
    ) -> Result<Vec<OutcomeEvent>> {
        Err(unsupported_store_capability("outcome listing"))
    }

    /// Aggregate ranking-eligible outcomes into requester-scoped decayed priors.
    async fn outcome_priors(
        &self,
        _scope_tenant_id: &TenantId,
        _scope_project_id: Option<&str>,
        _chunk_ids: &[ChunkId],
        _now_ms: i64,
    ) -> Result<Vec<OutcomePrior>> {
        Err(unsupported_store_capability("outcome priors"))
    }

    /// Get chunk by ID (respects tenant isolation)
    ///
    /// Returns None if the chunk doesn't exist or belongs to a different tenant.
    async fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<MemoryChunk>>;

    /// Get a chunk with its lifecycle overlay applied.
    ///
    /// Default impl returns the chunk with `status` derived from the payload
    /// and `lifecycle` defaulted. `PersistentStore` overrides this to join with
    /// the `SqliteMetadataStore` lifecycle columns.
    async fn get_with_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
    ) -> Result<Option<ResolvedChunk>> {
        Ok(self
            .get(tenant_id, chunk_id)
            .await?
            .map(|chunk| ResolvedChunk {
                status: chunk.status,
                chunk,
                lifecycle: LifecycleMetadata::default(),
            }))
    }

    /// Downcast helper for lifecycle-aware tools that need
    /// `PersistentStore`-specific APIs. Default returns `None`;
    /// `PersistentStore` overrides to `Some(self)`.
    fn as_persistent(&self) -> Option<&crate::store::persistent::PersistentStore> {
        None
    }

    /// Best-effort usage-ledger recording. Never fails; implementations
    /// must swallow all errors (debug log) and silently no-op when the
    /// store cannot write (read-only mode, in-memory store).
    fn record_usage_event(&self, _event: crate::store::usage::UsageEvent) {}

    /// Best-effort external metadata mutation probe for warm workers.
    async fn probe_external_mutation(&self) -> ExternalMutationOutcome {
        ExternalMutationOutcome::Unavailable
    }

    /// Read-your-writes probe counters for warm-worker status payloads.
    fn ryw_probe_stats(&self) -> Option<RywProbeStats> {
        None
    }

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

    /// Search with scores at a fixed wall-clock reference.
    ///
    /// Backends without time-sensitive ranking may use the default, which
    /// delegates to `search_with_scores`.
    async fn search_with_scores_at(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        _ranking_time_ms: i64,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        self.search_with_scores(tenant_id, query, k).await
    }

    /// Coarse retrieval capability, reported in-band via `scope_status`
    /// so agents can tell ranked semantic retrieval from degraded
    /// substring matching. Default matches the `search_with_scores`
    /// default above (text matching at constant score); PersistentStore
    /// overrides this when a hybrid/dense searcher is available.
    fn retrieval_mode(&self) -> &'static str {
        "text_fallback"
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

    /// List chunks for a tenant, optionally scoped to a project, with pagination.
    ///
    /// Default implementation pages through `list_chunks` and filters readable
    /// chunks. Persistent stores override this so project filtering happens in
    /// metadata before segment payload reads.
    async fn list_chunks_for_project(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MemoryChunk>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        if project_id.is_none() {
            return self.list_chunks(tenant_id, limit, offset).await;
        }

        let page_size = limit.clamp(1, 500);
        let target = offset.saturating_add(limit);
        let mut raw_offset = 0usize;
        let mut filtered = Vec::new();

        while filtered.len() < target {
            let chunks = self.list_chunks(tenant_id, page_size, raw_offset).await?;
            if chunks.is_empty() {
                break;
            }
            raw_offset = raw_offset.saturating_add(page_size);
            filtered.extend(
                chunks
                    .into_iter()
                    .filter(|chunk| chunk.project_id.as_option() == project_id),
            );
        }

        if offset >= filtered.len() {
            return Ok(Vec::new());
        }
        Ok(filtered.into_iter().skip(offset).take(limit).collect())
    }

    /// Soft delete a chunk
    ///
    /// Returns true if the chunk was found and deleted, false if not found.
    async fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool>;

    /// Get statistics for a tenant
    async fn stats(&self, tenant_id: &TenantId) -> Result<StoreStats>;

    /// Get read-only storage health aggregates for a tenant/project scope.
    async fn health_snapshot(
        &self,
        _tenant_id: &TenantId,
        _project_id: Option<&str>,
        _duplicate_limit: usize,
    ) -> Result<Option<StoreHealthSnapshot>> {
        Ok(None)
    }

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

    /// Search with tier info at a fixed wall-clock reference.
    async fn search_with_tier_info_at(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        ranking_time_ms: i64,
    ) -> Result<(Vec<(MemoryChunk, f32)>, Option<TieredTiming>)> {
        let results = self
            .search_with_scores_at(tenant_id, query, k, ranking_time_ms)
            .await?;
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
        Err(unsupported_store_capability("threshold compaction"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkType;

    #[test]
    fn candidate_ranking_matches_terms_across_boundary_punctuation() {
        let tenant = TenantId::new("candidate_rank_test").unwrap();
        let mut correct = MemoryChunk::new(
            tenant.clone(),
            "For atlas, cache keys must use tenant scope.",
            ChunkType::Decision,
        );
        correct.timestamp_created = 1;
        let correct_id = correct.chunk_id.clone();
        let mut wrong = MemoryChunk::new(
            tenant,
            "For boreal, cache keys must use tenant scope.",
            ChunkType::Decision,
        );
        wrong.timestamp_created = 2;

        let ranked = rank_candidate_chunks(
            vec![correct, wrong],
            "Recall the cache-key namespace scope required by atlas.",
            2,
        );

        assert_eq!(ranked[0].0.chunk_id, correct_id);
    }

    #[test]
    fn candidate_ranking_does_not_match_alphanumeric_term_inside_another_word() {
        let tenant = TenantId::new("candidate_rank_test").unwrap();
        let mut correct = MemoryChunk::new(
            tenant.clone(),
            "For ion, cache keys must use tenant scope.",
            ChunkType::Decision,
        );
        correct.timestamp_created = 1;
        let correct_id = correct.chunk_id.clone();
        let mut wrong = MemoryChunk::new(
            tenant,
            "For atlas, apply the corrected cache-key isolation scope rule.",
            ChunkType::Decision,
        );
        wrong.timestamp_created = 2;

        let ranked = rank_candidate_chunks(vec![correct, wrong], "scope for ion.", 2);

        assert_eq!(ranked[0].0.chunk_id, correct_id);
    }

    #[test]
    fn candidate_ranking_matches_natural_word_suffixes_at_token_boundaries() {
        let tenant = TenantId::new("candidate_rank_test").unwrap();
        let correct = MemoryChunk::new(
            tenant.clone(),
            "Parameters: sensitivity 7.5",
            ChunkType::Trace,
        );
        let wrong = MemoryChunk::new(tenant, "Candidate search", ChunkType::Trace);

        let ranked = rank_candidate_chunks(vec![wrong, correct], "parameter sweeps", 2);

        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].0.text.starts_with("Parameters:"));
    }

    #[test]
    fn candidate_ranking_preserves_dotted_identifier_terms() {
        let tenant = TenantId::new("candidate_rank_test").unwrap();
        let mut correct = MemoryChunk::new(
            tenant.clone(),
            "Release v1.3 fixed the recovery path.",
            ChunkType::Decision,
        );
        correct.timestamp_created = 1;
        let correct_id = correct.chunk_id.clone();
        let mut wrong = MemoryChunk::new(
            tenant,
            "Release v1.2 fixed the recovery path.",
            ChunkType::Decision,
        );
        wrong.timestamp_created = 2;

        let ranked = rank_candidate_chunks(vec![correct, wrong], "v1.3", 2);

        assert_eq!(ranked[0].0.chunk_id, correct_id);
    }

    #[test]
    fn candidate_ranking_rejects_punctuation_only_queries() {
        let tenant = TenantId::new("candidate_rank_test").unwrap();
        let chunk = MemoryChunk::new(tenant, "durable fact", ChunkType::Decision);

        for query in ["???", "---", "___"] {
            assert!(rank_candidate_chunks(vec![chunk.clone()], query, 1).is_empty());
        }
    }
}
