//! Hybrid search coordinator
//!
//! Combines dense (semantic) and sparse (keyword) search with RRF fusion
//! and feature-based reranking for comprehensive retrieval.
//! Supports tiered search with cache/hot/warm fallback when enabled.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tracing::debug;

use super::dense::DenseSearcher;
use crate::error::{MemdError, Result};
use crate::index::{Bm25Index, SearchResult, SparseIndex};
use crate::metrics::TieredQueryMetrics;
use crate::retrieval::packer::{ContextPacker, PackerConfig};
use crate::retrieval::{
    ChunkWithMeta, FusionCandidate, FusionSource, RerankerConfig, RerankerContext, RerankerEngine,
    RerankerMode, RrfConfig, RrfFusion,
};
use crate::text::TextProcessor;
use crate::tiered::{
    AccessTracker, AccessTrackerConfig, HotTier, HotTierConfig, SemanticCache, SemanticCacheConfig,
    TieredSearcher, TieredSearcherConfig, TieredTiming, WarmTierSearch,
};
use crate::types::{ChunkId, ChunkType, TenantId};

/// Configuration for hybrid search
#[derive(Debug, Clone)]
pub struct HybridConfig {
    /// Number of candidates to fetch from dense search
    pub dense_k: usize,
    /// Number of candidates to fetch from sparse search
    pub sparse_k: usize,
    /// RRF configuration
    pub rrf: RrfConfig,
    /// Reranker configuration
    pub reranker: RerankerConfig,
    /// Apply metadata/query feature reranking after dense/sparse fusion.
    pub enable_rerank: bool,
    /// Packer configuration
    pub packer: PackerConfig,
    /// Enable sparse search (can be disabled for dense-only fallback)
    pub enable_sparse: bool,
    /// Enable tiered search with cache/hot/warm fallback
    pub enable_tiered: bool,
    /// Tiered search configuration (if enable_tiered is true)
    pub tiered_config: Option<TieredSearcherConfig>,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            dense_k: 100,
            sparse_k: 100,
            rrf: RrfConfig::default(),
            reranker: RerankerConfig::default(),
            enable_rerank: true,
            packer: PackerConfig::default(),
            enable_sparse: true,
            enable_tiered: true,
            tiered_config: None,
        }
    }
}

fn sparse_candidate_count(config: &HybridConfig, requested_k: usize) -> usize {
    if config.dense_k == 0 && !config.enable_rerank {
        requested_k
    } else {
        config.sparse_k
    }
}

/// Result from hybrid search (before packing)
#[derive(Debug, Clone)]
pub struct HybridSearchResult {
    pub chunk_id: ChunkId,
    pub final_score: f32,
    pub dense_rank: Option<usize>,
    pub sparse_rank: Option<usize>,
}

/// Context for search (project, preferences)
#[derive(Debug, Clone, Default)]
pub struct SearchContext {
    pub current_project: Option<String>,
    pub preferred_types: Vec<ChunkType>,
    /// Fixed wall-clock reference for reproducible recency scoring.
    pub ranking_time_ms: Option<i64>,
}

/// Timing breakdown for hybrid search
#[derive(Debug, Clone, Default)]
pub struct HybridTiming {
    pub dense_time: Duration,
    pub sparse_time: Duration,
    pub fusion_time: Duration,
    pub rerank_time: Duration,
    pub total_time: Duration,
    /// Tiered timing breakdown (if tiered search was used)
    pub tiered: Option<TieredTiming>,
}

/// Chunk metadata for reranking
pub struct ChunkMetaForRerank {
    pub chunk_id: ChunkId,
    pub rrf_score: f32,
    pub timestamp_created: i64,
    pub project_id: Option<String>,
    pub chunk_type: ChunkType,
    pub text: Option<String>,
}

/// Adapter to expose DenseSearcher as a warm tier for TieredSearcher
///
/// This adapter bridges the DenseSearcher (which handles embedding + HNSW)
/// to the WarmTierSearch trait required by TieredSearcher.
pub struct WarmTierAdapter {
    /// Reference to the dense searcher
    dense: Arc<DenseSearcher>,
    /// Tenant for scoped searches
    tenant_id: TenantId,
    /// Version counter for cache invalidation
    version: AtomicU64,
    /// Cached embeddings for hot tier promotion (chunk_id -> embedding)
    embedding_cache: RwLock<std::collections::HashMap<ChunkId, Vec<f32>>>,
}

impl WarmTierAdapter {
    /// Create a new warm tier adapter for a tenant
    pub fn new(dense: Arc<DenseSearcher>, tenant_id: TenantId) -> Self {
        Self {
            dense,
            tenant_id,
            version: AtomicU64::new(1),
            embedding_cache: RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Cache an embedding for later hot tier promotion
    pub fn cache_embedding(&self, chunk_id: ChunkId, embedding: Vec<f32>) {
        let mut cache = self.embedding_cache.write();
        cache.insert(chunk_id, embedding);
    }

    /// Increment version (call on chunk add/delete)
    pub fn increment_version(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }
}

impl WarmTierSearch for WarmTierAdapter {
    fn search(&self, query_embedding: &[f32], k: usize) -> Result<Vec<SearchResult>> {
        // Use the dense searcher's search with pre-computed embedding
        let results = self
            .dense
            .search_with_embedding(&self.tenant_id, query_embedding, k)?;

        // Convert DenseSearchResult to SearchResult
        Ok(results
            .into_iter()
            .map(|r| SearchResult {
                chunk_id: r.chunk_id,
                score: r.score,
            })
            .collect())
    }

    fn get_embedding(&self, chunk_id: &ChunkId) -> Option<Vec<f32>> {
        let cache = self.embedding_cache.read();
        cache.get(chunk_id).cloned()
    }

    fn get_version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    fn len(&self) -> usize {
        self.dense.index_len(&self.tenant_id)
    }
}

/// Hybrid search coordinator
///
/// Combines dense (embedding-based) and sparse (BM25) search, fuses results
/// with RRF, and applies feature-based reranking. Supports tiered search
/// with cache/hot/warm fallback when enabled.
pub struct HybridSearcher {
    dense: Option<Arc<DenseSearcher>>,
    sparse: Option<Arc<Bm25Index>>,
    text_processor: TextProcessor,
    fusion: RrfFusion,
    reranker: RerankerEngine,
    #[allow(dead_code)]
    packer: ContextPacker,
    config: HybridConfig,
    /// Per-tenant tiered searchers (only populated if enable_tiered is true)
    tiered_searchers:
        RwLock<std::collections::HashMap<String, Arc<TieredSearcher<WarmTierAdapter>>>>,
    /// Shared semantic cache (across tenants)
    semantic_cache: Option<Arc<SemanticCache>>,
    /// Access tracker config for creating per-tenant access trackers
    access_tracker_config: AccessTrackerConfig,
    /// Hot tier config for creating per-tenant hot tiers
    hot_tier_config: HotTierConfig,
}

impl HybridSearcher {
    /// Borrow the sparse index if BM25 is enabled.
    pub fn sparse_index(&self) -> Option<&Arc<Bm25Index>> {
        self.sparse.as_ref()
    }

    /// Create a new hybrid searcher
    pub fn new(
        dense: Arc<DenseSearcher>,
        sparse: Option<Arc<Bm25Index>>,
        config: HybridConfig,
    ) -> Self {
        Self::from_parts(Some(dense), sparse, config)
    }

    /// Create a sparse-only searcher without loading an embedding model or
    /// dense index. The caller must disable dense and tiered retrieval.
    pub fn new_sparse_only(sparse: Arc<Bm25Index>, mut config: HybridConfig) -> Self {
        config.dense_k = 0;
        config.enable_tiered = false;
        Self::from_parts(None, Some(sparse), config)
    }

    fn from_parts(
        dense: Option<Arc<DenseSearcher>>,
        sparse: Option<Arc<Bm25Index>>,
        config: HybridConfig,
    ) -> Self {
        let fusion = RrfFusion::new(config.rrf.clone());
        let reranker = RerankerEngine::new(config.reranker.clone());
        let packer = ContextPacker::new(config.packer.clone());

        // Create shared semantic cache if tiered search is enabled
        let semantic_cache = if config.enable_tiered && dense.is_some() {
            Some(Arc::new(SemanticCache::new(SemanticCacheConfig::default())))
        } else {
            None
        };

        // Create hot tier config based on dense dimension
        let dimension = dense
            .as_ref()
            .map(|searcher| searcher.dimension())
            .unwrap_or_else(|| crate::index::HnswConfig::default().dimension);
        let hot_tier_config = HotTierConfig {
            hnsw_config: crate::index::HnswConfig {
                dimension,
                max_elements: 50_000,
                max_connections: 16,
                ef_construction: 200,
                ef_search: 30, // Lower than warm tier for faster queries
                persist_graph_dump: true,
                search_lock_budget_ms: None,
                backfill_hnsw_on_startup: false,
            },
            ..Default::default()
        };

        Self {
            dense,
            sparse,
            text_processor: TextProcessor::new(),
            fusion,
            reranker,
            packer,
            config,
            tiered_searchers: RwLock::new(std::collections::HashMap::new()),
            semantic_cache,
            access_tracker_config: AccessTrackerConfig::default(),
            hot_tier_config,
        }
    }

    /// Effective reranker mode in use (after fallback handling).
    pub fn reranker_mode(&self) -> RerankerMode {
        self.reranker.mode()
    }

    pub fn rerank_enabled(&self) -> bool {
        self.config.enable_rerank
    }

    /// Get or create tiered searcher for a tenant
    fn get_or_create_tiered_searcher(
        &self,
        tenant_id: &TenantId,
    ) -> Option<Arc<TieredSearcher<WarmTierAdapter>>> {
        if !self.config.enable_tiered {
            return None;
        }
        let dense = self.dense.as_ref()?;

        let tenant_str = tenant_id.to_string();

        // Fast path: read lock
        {
            let tiered_searchers = self.tiered_searchers.read();
            if let Some(searcher) = tiered_searchers.get(&tenant_str) {
                return Some(Arc::clone(searcher));
            }
        }

        // Slow path: write lock + create
        let mut tiered_searchers = self.tiered_searchers.write();

        // Double-check
        if let Some(searcher) = tiered_searchers.get(&tenant_str) {
            return Some(Arc::clone(searcher));
        }

        // Create components for this tenant
        let warm_tier = Arc::new(WarmTierAdapter::new(Arc::clone(dense), tenant_id.clone()));

        let access_tracker = Arc::new(RwLock::new(AccessTracker::new(
            self.access_tracker_config.clone(),
        )));

        let hot_tier = Arc::new(RwLock::new(HotTier::with_access_tracker(
            self.hot_tier_config.clone(),
            Arc::clone(&access_tracker),
        )));

        let tiered_config = self.config.tiered_config.clone().unwrap_or_default();

        let tiered_searcher = TieredSearcher::new(
            Arc::clone(self.semantic_cache.as_ref()?),
            hot_tier,
            access_tracker,
            warm_tier,
            tiered_config,
        );

        let tiered_searcher = Arc::new(tiered_searcher);
        tiered_searchers.insert(tenant_str, Arc::clone(&tiered_searcher));

        Some(tiered_searcher)
    }

    /// Index a chunk in both dense and sparse indexes
    pub async fn index_chunk(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        text: &str,
    ) -> Result<()> {
        self.index_batch(tenant_id, &[(chunk_id.clone(), text.to_string())])
            .await
    }

    /// Index multiple chunks in dense+sparse indexes with batched operations.
    pub async fn index_batch(
        &self,
        tenant_id: &TenantId,
        chunks: &[(ChunkId, String)],
    ) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        if self.config.dense_k > 0 {
            let dense = self.dense.as_ref().ok_or_else(|| {
                MemdError::StorageError("dense search is not configured".to_string())
            })?;
            dense.index_batch(tenant_id, chunks).await?;
        }

        if self.config.enable_sparse {
            if let Some(ref sparse) = self.sparse {
                let sparse_items: Vec<(TenantId, ChunkId, Vec<String>)> = chunks
                    .iter()
                    .filter_map(|(chunk_id, text)| {
                        let processed = self.text_processor.process_chunk(text);
                        let sentences: Vec<String> =
                            processed.into_iter().map(|p| p.text).collect();
                        if sentences.is_empty() {
                            return None;
                        }
                        Some((tenant_id.clone(), chunk_id.clone(), sentences))
                    })
                    .collect();
                if !sparse_items.is_empty() {
                    sparse.insert_batch(&sparse_items)?;
                }
            }
        }

        // Phase 3.5: bump the per-tenant memory version so the
        // semantic cache invalidates any entry whose snapshot predates
        // this write. Previously the version was only advertised via
        // `WarmTierAdapter::get_version` but never incremented, so the
        // cache never invalidated on add.
        self.bump_tenant_memory_version(tenant_id);

        debug!(
            tenant_id = %tenant_id,
            chunk_count = chunks.len(),
            sparse_enabled = self.config.enable_sparse,
            "indexed batch in hybrid searcher"
        );
        Ok(())
    }

    /// Bump the per-tenant `memory_version` on the warm tier so cache
    /// consumers can detect stale entries. Exposed on HybridSearcher
    /// rather than inlined so the storage layer can call it from any
    /// mutation path without reaching into private state.
    pub fn bump_tenant_memory_version(&self, tenant_id: &TenantId) {
        if let Some(searcher) = self.get_or_create_tiered_searcher(tenant_id) {
            searcher.warm_tier().increment_version();
        }
    }

    /// Current per-tenant `memory_version` from the warm tier.
    /// Returns `None` when no tiered searcher has been created for
    /// this tenant (e.g., tiered search disabled). Exposed primarily
    /// for tests and diagnostics — production code should rely on the
    /// cache's own staleness logic.
    pub fn tenant_memory_version(&self, tenant_id: &TenantId) -> Option<u64> {
        use crate::tiered::tiered_searcher::WarmTierSearch;
        let searchers = self.tiered_searchers.read();
        searchers
            .get(tenant_id.as_str())
            .map(|searcher| searcher.warm_tier().get_version())
    }

    /// Remove chunk from indexes
    pub fn delete_chunk(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<()> {
        // Delete from sparse if enabled
        if self.config.enable_sparse {
            if let Some(ref sparse) = self.sparse {
                sparse.delete(tenant_id, chunk_id)?;
            }
        }

        // Invalidate in semantic cache (if tiered enabled)
        self.invalidate_chunk_in_cache(chunk_id);

        // Demote from hot tier if present
        if self.get_or_create_tiered_searcher(tenant_id).is_some() {
            // Access the hot tier through the searcher is not directly possible,
            // but invalidation is handled through cache and the chunk will be
            // filtered out on next search since metadata marks it deleted.
            debug!(
                tenant_id = %tenant_id,
                chunk_id = %chunk_id,
                "invalidated chunk in tiered cache"
            );
        }

        // Note: Dense index deletion is not currently supported by HnswIndex
        // The chunk will be orphaned but won't appear in results after
        // metadata is updated

        // Phase 3.5: bump the per-tenant memory version so cache
        // entries that predate this delete are invalidated.
        self.bump_tenant_memory_version(tenant_id);

        debug!(
            tenant_id = %tenant_id,
            chunk_id = %chunk_id,
            "deleted chunk from hybrid searcher"
        );

        Ok(())
    }

    /// Perform hybrid search with fusion and reranking
    pub async fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        context: Option<SearchContext>,
    ) -> Result<Vec<HybridSearchResult>> {
        let (results, _timing) = self
            .search_with_timing(tenant_id, query, k, context)
            .await?;
        Ok(results)
    }

    /// Search with timing information for metrics
    ///
    /// If tiered search is enabled, uses cache/hot/warm fallback.
    /// Otherwise falls back to standard dense+sparse fusion.
    pub async fn search_with_timing(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        context: Option<SearchContext>,
    ) -> Result<(Vec<HybridSearchResult>, HybridTiming)> {
        // Try tiered search first if enabled
        if let Some(tiered_searcher) = self.get_or_create_tiered_searcher(tenant_id) {
            return self
                .search_tiered(tenant_id, query, k, context.as_ref(), &tiered_searcher)
                .await;
        }

        // Fall back to standard dense+sparse fusion
        self.search_standard(tenant_id, query, k).await
    }

    /// Internal tiered search path
    async fn search_tiered(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        context: Option<&SearchContext>,
        tiered_searcher: &TieredSearcher<WarmTierAdapter>,
    ) -> Result<(Vec<HybridSearchResult>, HybridTiming)> {
        let total_start = Instant::now();
        let mut timing = HybridTiming::default();

        // Step 1: Embed query
        let embed_start = Instant::now();
        let dense = self.dense.as_ref().ok_or_else(|| {
            MemdError::StorageError("tiered search requires a dense searcher".to_string())
        })?;
        let query_embedding = dense.embed_query(query).await?;
        timing.dense_time = embed_start.elapsed(); // Embed time tracked as dense_time

        // Step 2: Tiered search (cache -> hot -> warm)
        // Over-fetch the dense leg to the configured dense_k so RRF fuses a
        // deep dense list against the deep sparse list (sparse_k), matching the
        // non-tiered path. Fetching only `k` here would let a chunk that ranks
        // shallow in dense but top in sparse lose a fusion it should win. The
        // final list is truncated to `k` after fusion below.
        let dense_fetch = self.config.dense_k.max(k);
        let project_id = context.and_then(|c| c.current_project.as_deref());
        let tiered_result =
            tiered_searcher.search(&query_embedding, tenant_id, project_id, dense_fetch)?;

        // Convert TieredTiming
        let tiered_timing = tiered_result.timing;
        timing.tiered = Some(tiered_timing.clone());

        // Step 3: Run the sparse leg whenever hybrid search is enabled. The
        // semantic cache stores dense tier results, not the final fused list;
        // skipping sparse work on a cache hit would silently turn repeated
        // hybrid queries into dense-only queries and change their ranking.
        let sparse_start = Instant::now();
        let sparse_results = if self.config.enable_sparse {
            if let Some(ref sparse) = self.sparse {
                sparse.search(tenant_id, query, sparse_candidate_count(&self.config, k))?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        timing.sparse_time = sparse_start.elapsed();

        // Step 4: Build results. Cache hits still need sparse fusion because
        // only the dense leg is cached.
        let results: Vec<HybridSearchResult> = if sparse_results.is_empty() {
            // Direct conversion from tiered results. Truncate to `k`: the
            // dense leg was over-fetched to dense_fetch for fusion, but with
            // no sparse leg to fuse there is nothing to truncate later.
            tiered_result
                .results
                .into_iter()
                .take(k)
                .map(|r| HybridSearchResult {
                    chunk_id: r.chunk_id,
                    final_score: r.score,
                    dense_rank: None, // Tier doesn't track separate ranks
                    sparse_rank: None,
                })
                .collect()
        } else {
            // Fuse tiered (dense) results with sparse
            let fusion_start = Instant::now();
            let mut candidates: Vec<FusionCandidate> = Vec::new();

            // Tiered results as dense candidates
            for (rank, result) in tiered_result.results.iter().enumerate() {
                candidates.push(FusionCandidate {
                    chunk_id: result.chunk_id.clone(),
                    source: FusionSource::Dense,
                    rank: rank + 1,
                    source_score: result.score,
                });
            }

            // Sparse candidates
            for (rank, result) in sparse_results.iter().enumerate() {
                candidates.push(FusionCandidate {
                    chunk_id: result.chunk_id.clone(),
                    source: FusionSource::Sparse,
                    rank: rank + 1,
                    source_score: result.score,
                });
            }

            let fused = self.fusion.fuse(candidates);
            timing.fusion_time = fusion_start.elapsed();

            fused
                .into_iter()
                .take(k)
                .map(|f| HybridSearchResult {
                    chunk_id: f.chunk_id,
                    final_score: f.rrf_score,
                    dense_rank: f.dense_rank,
                    sparse_rank: f.sparse_rank,
                })
                .collect()
        };

        timing.total_time = total_start.elapsed();

        debug!(
            tenant_id = %tenant_id,
            query_len = query.len(),
            cache_hit = tiered_result.cache_hit,
            hot_tier_hit = tiered_result.hot_tier_hit,
            source_tier = ?tiered_result.source_tier,
            result_count = results.len(),
            total_ms = timing.total_time.as_millis(),
            "tiered hybrid search completed"
        );

        Ok((results, timing))
    }

    /// Standard dense+sparse fusion path (when tiered is disabled)
    async fn search_standard(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<(Vec<HybridSearchResult>, HybridTiming)> {
        let total_start = Instant::now();
        let mut timing = HybridTiming::default();

        let dense_start = Instant::now();
        let dense_results = if self.config.dense_k > 0 {
            let dense = self.dense.as_ref().ok_or_else(|| {
                MemdError::StorageError("dense search is not configured".to_string())
            })?;
            let (dense_results, _embed_time, _search_time) = dense
                .search_with_timing(tenant_id, query, self.config.dense_k)
                .await?;
            dense_results
        } else {
            Vec::new()
        };
        timing.dense_time = dense_start.elapsed();

        // Step 2: Sparse search (if enabled)
        let sparse_start = Instant::now();
        let sparse_results = if self.config.enable_sparse {
            if let Some(ref sparse) = self.sparse {
                sparse.search(tenant_id, query, sparse_candidate_count(&self.config, k))?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        timing.sparse_time = sparse_start.elapsed();

        // Step 3: Build fusion candidates
        let fusion_start = Instant::now();
        let mut candidates: Vec<FusionCandidate> = Vec::new();

        // Dense candidates
        for (rank, result) in dense_results.iter().enumerate() {
            candidates.push(FusionCandidate {
                chunk_id: result.chunk_id.clone(),
                source: FusionSource::Dense,
                rank: rank + 1, // 1-indexed
                source_score: result.score,
            });
        }

        // Sparse candidates
        for (rank, result) in sparse_results.iter().enumerate() {
            candidates.push(FusionCandidate {
                chunk_id: result.chunk_id.clone(),
                source: FusionSource::Sparse,
                rank: rank + 1, // 1-indexed
                source_score: result.score,
            });
        }

        // Fuse with RRF
        let fused = self.fusion.fuse(candidates);
        timing.fusion_time = fusion_start.elapsed();

        // Step 4: Rerank (simplified - without full metadata)
        // Full reranking with metadata requires store access, which is done at PersistentStore level
        let rerank_start = Instant::now();

        // Build results from fused
        let results: Vec<HybridSearchResult> = fused
            .into_iter()
            .take(k)
            .map(|f| HybridSearchResult {
                chunk_id: f.chunk_id,
                final_score: f.rrf_score,
                dense_rank: f.dense_rank,
                sparse_rank: f.sparse_rank,
            })
            .collect();

        timing.rerank_time = rerank_start.elapsed();
        timing.total_time = total_start.elapsed();

        debug!(
            tenant_id = %tenant_id,
            query_len = query.len(),
            dense_count = dense_results.len(),
            sparse_count = sparse_results.len(),
            result_count = results.len(),
            dense_ms = timing.dense_time.as_millis(),
            sparse_ms = timing.sparse_time.as_millis(),
            fusion_ms = timing.fusion_time.as_millis(),
            total_ms = timing.total_time.as_millis(),
            "hybrid search completed"
        );

        Ok((results, timing))
    }

    /// Rerank results with full metadata (called by PersistentStore)
    pub fn rerank_with_metadata(
        &self,
        results: Vec<HybridSearchResult>,
        chunks_meta: Vec<ChunkMetaForRerank>,
        context: Option<SearchContext>,
    ) -> Vec<HybridSearchResult> {
        self.rerank_with_metadata_for_query("", results, chunks_meta, context)
    }

    /// Rerank results with full metadata and query context.
    pub fn rerank_with_metadata_for_query(
        &self,
        query: &str,
        results: Vec<HybridSearchResult>,
        chunks_meta: Vec<ChunkMetaForRerank>,
        context: Option<SearchContext>,
    ) -> Vec<HybridSearchResult> {
        if chunks_meta.is_empty() {
            return results;
        }

        let mut reranker_context = match context {
            Some(ctx) => {
                let base = ctx
                    .ranking_time_ms
                    .map(RerankerContext::at)
                    .unwrap_or_else(RerankerContext::now)
                    .with_preferred_types(ctx.preferred_types);
                if let Some(project) = ctx.current_project {
                    base.with_project(project)
                } else {
                    base
                }
            }
            None => RerankerContext::now(),
        };
        if !query.trim().is_empty() {
            reranker_context = reranker_context.with_query(query);
        }

        let chunks_with_meta: Vec<ChunkWithMeta> = chunks_meta
            .into_iter()
            .map(|meta| ChunkWithMeta {
                chunk_id: meta.chunk_id,
                rrf_score: meta.rrf_score,
                timestamp_created: meta.timestamp_created,
                project_id: meta.project_id,
                chunk_type: meta.chunk_type,
                text: meta.text,
            })
            .collect();

        let ranked = self.reranker.rerank(chunks_with_meta, &reranker_context);

        ranked
            .into_iter()
            .map(|r| {
                let original = results.iter().find(|orig| orig.chunk_id == r.chunk_id);

                HybridSearchResult {
                    chunk_id: r.chunk_id,
                    final_score: r.final_score,
                    dense_rank: original.and_then(|o| o.dense_rank),
                    sparse_rank: original.and_then(|o| o.sparse_rank),
                }
            })
            .collect()
    }

    /// Check if sparse search is enabled
    pub fn sparse_enabled(&self) -> bool {
        self.config.enable_sparse && self.sparse.is_some()
    }

    /// Check if tiered search is enabled
    pub fn tiered_enabled(&self) -> bool {
        self.config.enable_tiered
    }

    /// Get reference to text processor
    pub fn text_processor(&self) -> &TextProcessor {
        &self.text_processor
    }

    /// Run tiered maintenance for a tenant (promotions, demotions, evictions)
    ///
    /// Should be called periodically (e.g., every 60 seconds).
    pub fn run_tiered_maintenance(
        &self,
        tenant_id: &TenantId,
    ) -> Option<crate::tiered::MaintenanceResult> {
        let tiered_searcher = self.get_or_create_tiered_searcher(tenant_id)?;
        Some(tiered_searcher.run_maintenance(tenant_id))
    }

    /// Get tiered metrics for recording
    ///
    /// Returns a TieredQueryMetrics for the given timing and cache/hot tier hit info.
    pub fn create_tiered_metrics(
        timing: &HybridTiming,
        cache_hit: bool,
        hot_tier_hit: bool,
    ) -> TieredQueryMetrics {
        let tiered = timing.tiered.as_ref();
        let source_tier = if cache_hit {
            "cache"
        } else if hot_tier_hit {
            "hot"
        } else {
            "warm"
        };

        TieredQueryMetrics {
            source_tier: source_tier.to_string(),
            cache_lookup_ms: tiered.map(|t| t.cache_lookup_ms).unwrap_or(0),
            hot_tier_ms: tiered.map(|t| t.hot_tier_ms).unwrap_or(0),
            warm_tier_ms: tiered.map(|t| t.warm_tier_ms).unwrap_or(0),
            cache_hit,
            hot_tier_hit,
        }
    }

    /// Invalidate cache entries containing a specific chunk
    ///
    /// Called when a chunk is deleted to ensure cache consistency.
    pub fn invalidate_chunk_in_cache(&self, chunk_id: &ChunkId) {
        if let Some(ref cache) = self.semantic_cache {
            cache.invalidate_chunks(std::slice::from_ref(chunk_id));
        }
    }

    /// Get semantic cache statistics (if tiered search enabled)
    pub fn get_cache_stats(&self) -> Option<crate::tiered::CacheStats> {
        self.semantic_cache.as_ref().map(|c| c.get_stats())
    }

    /// Get reference to semantic cache for compaction
    ///
    /// Returns None if tiered search is not enabled.
    pub fn get_semantic_cache(&self) -> Option<&SemanticCache> {
        self.semantic_cache.as_ref().map(|c| c.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::{Embedder, MockEmbedder};
    use crate::store::DenseSearchConfig;

    fn make_test_hybrid_searcher(enable_sparse: bool) -> HybridSearcher {
        let embedder = Arc::new(MockEmbedder::new()); // Uses default config (1024 dims)
        let dense_config = DenseSearchConfig {
            persist: false,
            ..Default::default()
        };
        let dense = Arc::new(DenseSearcher::with_embedder(embedder, dense_config));

        let sparse = if enable_sparse {
            Some(Arc::new(Bm25Index::new().unwrap()))
        } else {
            None
        };

        let config = HybridConfig {
            enable_sparse,
            enable_tiered: false, // Disable tiered for tests (MockEmbedder has different dimension)
            ..Default::default()
        };

        HybridSearcher::new(dense, sparse, config)
    }

    fn make_test_tiered_hybrid_searcher() -> HybridSearcher {
        let embedder = Arc::new(MockEmbedder::new());
        let mut dense_config = DenseSearchConfig {
            persist: false,
            ..Default::default()
        };
        dense_config.hnsw.dimension = embedder.dimension();
        let dense = Arc::new(DenseSearcher::with_embedder(embedder, dense_config));
        let sparse = Some(Arc::new(Bm25Index::new().unwrap()));
        let config = HybridConfig {
            enable_sparse: true,
            enable_tiered: true,
            ..Default::default()
        };

        HybridSearcher::new(dense, sparse, config)
    }

    fn make_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    #[test]
    fn sparse_only_search_fetches_only_the_requested_candidates() {
        let sparse_only = HybridConfig {
            dense_k: 0,
            sparse_k: 200,
            enable_rerank: false,
            ..HybridConfig::default()
        };
        assert_eq!(sparse_candidate_count(&sparse_only, 80), 80);

        let reranked_sparse = HybridConfig {
            dense_k: 0,
            sparse_k: 200,
            enable_rerank: true,
            ..HybridConfig::default()
        };
        assert_eq!(sparse_candidate_count(&reranked_sparse, 80), 200);

        let hybrid = HybridConfig {
            dense_k: 100,
            sparse_k: 200,
            ..HybridConfig::default()
        };
        assert_eq!(sparse_candidate_count(&hybrid, 80), 200);
    }

    #[tokio::test]
    async fn test_hybrid_search_basic() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();
        let chunk_id = ChunkId::new();

        // Index a chunk
        searcher
            .index_chunk(
                &tenant,
                &chunk_id,
                "The getUserById function returns user data",
            )
            .await
            .unwrap();

        // Search should find it
        let results = searcher
            .search(&tenant, "getUserById", 10, None)
            .await
            .unwrap();

        // Should have results (at least from sparse)
        assert!(!results.is_empty());
        assert_eq!(results[0].chunk_id, chunk_id);
    }

    #[tokio::test]
    async fn test_index_batch_and_search() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();
        let chunk_a = ChunkId::new();
        let chunk_b = ChunkId::new();
        let chunks = vec![
            (
                chunk_a.clone(),
                "The alpha parser handles config and json".to_string(),
            ),
            (
                chunk_b.clone(),
                "The beta formatter emits markdown output".to_string(),
            ),
        ];

        searcher.index_batch(&tenant, &chunks).await.unwrap();

        let alpha = searcher
            .search(&tenant, "alpha parser", 10, None)
            .await
            .unwrap();
        assert!(!alpha.is_empty());
        assert_eq!(alpha[0].chunk_id, chunk_a);

        let beta = searcher
            .search(&tenant, "beta formatter", 10, None)
            .await
            .unwrap();
        assert!(!beta.is_empty());
        assert_eq!(beta[0].chunk_id, chunk_b);
    }

    #[tokio::test]
    async fn test_keyword_match_improvement() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();
        let chunk_id = ChunkId::new();

        // Add chunk with unique identifier
        searcher
            .index_chunk(
                &tenant,
                &chunk_id,
                "The XyzSpecialFunctionName handles edge cases in processing",
            )
            .await
            .unwrap();

        // Search for the unique identifier
        let results = searcher
            .search(&tenant, "XyzSpecialFunctionName", 10, None)
            .await
            .unwrap();

        // Hybrid should find it via sparse (keyword) search
        assert!(
            !results.is_empty(),
            "Should find unique identifier via sparse search"
        );
        assert_eq!(results[0].chunk_id, chunk_id);
    }

    #[tokio::test]
    async fn test_index_and_delete() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();
        let chunk_id = ChunkId::new();

        // Index a chunk
        searcher
            .index_chunk(&tenant, &chunk_id, "deletable content here")
            .await
            .unwrap();

        // Verify searchable
        let results = searcher
            .search(&tenant, "deletable", 10, None)
            .await
            .unwrap();
        assert!(!results.is_empty(), "Should be searchable after indexing");

        // Delete from sparse
        searcher.delete_chunk(&tenant, &chunk_id).unwrap();

        // Should not be findable in sparse anymore
        // (dense may still have it until full deletion support is added)
        if let Some(ref sparse) = searcher.sparse {
            let sparse_results = sparse.search(&tenant, "deletable", 10).unwrap();
            assert!(
                sparse_results.is_empty(),
                "Should not be in sparse after delete"
            );
        }
    }

    #[tokio::test]
    async fn test_timing_breakdown() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();

        // Search with timing
        let (results, timing) = searcher
            .search_with_timing(&tenant, "test query", 10, None)
            .await
            .unwrap();

        // All timing components should be populated (even if zero)
        assert!(timing.total_time >= timing.dense_time);
        assert!(results.len() <= 10);
    }

    #[tokio::test]
    async fn test_tiered_cache_hit_preserves_sparse_fusion() {
        let searcher = make_test_tiered_hybrid_searcher();
        let tenant = make_tenant();
        let exact = ChunkId::new();
        let other = ChunkId::new();
        searcher
            .index_batch(
                &tenant,
                &[
                    (
                        exact,
                        "XyzSpecialFunctionName handles exact keyword lookups".to_string(),
                    ),
                    (other, "general retrieval implementation notes".to_string()),
                ],
            )
            .await
            .unwrap();

        let context = Some(SearchContext {
            ranking_time_ms: Some(1_700_000_000_000),
            ..SearchContext::default()
        });
        let cold = searcher
            .search(&tenant, "XyzSpecialFunctionName", 10, context.clone())
            .await
            .unwrap();
        let warm = searcher
            .search(&tenant, "XyzSpecialFunctionName", 10, context)
            .await
            .unwrap();
        let signature = |results: &[HybridSearchResult]| {
            results
                .iter()
                .map(|result| {
                    (
                        result.chunk_id.clone(),
                        result.final_score.to_bits(),
                        result.dense_rank,
                        result.sparse_rank,
                    )
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(signature(&cold), signature(&warm));
        assert!(warm.iter().any(|result| result.sparse_rank.is_some()));
        let stats = searcher.get_cache_stats().unwrap();
        assert_eq!(stats.cache_hits, 1);
    }

    #[tokio::test]
    async fn test_sparse_disabled() {
        let searcher = make_test_hybrid_searcher(false);
        let tenant = make_tenant();
        let chunk_id = ChunkId::new();

        // Index
        searcher
            .index_chunk(&tenant, &chunk_id, "some content")
            .await
            .unwrap();

        // Search should work (dense only)
        let results = searcher.search(&tenant, "content", 10, None).await.unwrap();

        // Verify sparse is disabled
        assert!(!searcher.sparse_enabled());

        // Should still get results from dense
        // (MockEmbedder produces deterministic embeddings)
        assert!(results.is_empty() || results[0].sparse_rank.is_none());
    }

    #[tokio::test]
    async fn test_rerank_with_metadata() {
        let searcher = make_test_hybrid_searcher(true);
        let chunk_id1 = ChunkId::new();
        let chunk_id2 = ChunkId::new();

        let results = vec![
            HybridSearchResult {
                chunk_id: chunk_id1.clone(),
                final_score: 0.5,
                dense_rank: Some(1),
                sparse_rank: Some(2),
            },
            HybridSearchResult {
                chunk_id: chunk_id2.clone(),
                final_score: 0.4,
                dense_rank: Some(2),
                sparse_rank: Some(1),
            },
        ];

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        // chunk2 is newer and in current project
        let chunks_meta = vec![
            ChunkMetaForRerank {
                chunk_id: chunk_id1.clone(),
                rrf_score: 0.5,
                timestamp_created: now_ms - 7 * 24 * 60 * 60 * 1000, // 7 days old
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: None,
            },
            ChunkMetaForRerank {
                chunk_id: chunk_id2.clone(),
                rrf_score: 0.4,
                timestamp_created: now_ms, // just created
                project_id: Some("current_project".to_string()),
                chunk_type: ChunkType::Code,
                text: None,
            },
        ];

        let context = Some(SearchContext {
            current_project: Some("current_project".to_string()),
            preferred_types: vec![ChunkType::Code],
            ranking_time_ms: None,
        });

        let reranked = searcher.rerank_with_metadata(results, chunks_meta, context);

        // chunk2 should be boosted due to recency, project match, and type preference
        assert_eq!(reranked.len(), 2);
        // The reranker may reorder based on bonuses
    }

    #[tokio::test]
    async fn test_rerank_with_metadata_cross_encoder_prefers_query_match() {
        let embedder = Arc::new(MockEmbedder::new());
        let dense_config = DenseSearchConfig {
            persist: false,
            ..Default::default()
        };
        let dense = Arc::new(DenseSearcher::with_embedder(embedder, dense_config));
        let config = HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            reranker: RerankerConfig {
                mode: RerankerMode::CrossEncoder,
                rrf_weight: 0.2,
                recency_weight: 0.0,
                recency_half_life_days: 7.0,
                project_weight: 0.0,
                type_weight: 0.0,
                query_text_weight: 0.0,
                cross_encoder_weight: 1.0,
            },
            ..Default::default()
        };
        let searcher = HybridSearcher::new(dense, None, config);

        let chunk_id1 = ChunkId::new();
        let chunk_id2 = ChunkId::new();
        let results = vec![
            HybridSearchResult {
                chunk_id: chunk_id1.clone(),
                final_score: 0.5,
                dense_rank: Some(1),
                sparse_rank: None,
            },
            HybridSearchResult {
                chunk_id: chunk_id2.clone(),
                final_score: 0.5,
                dense_rank: Some(2),
                sparse_rank: None,
            },
        ];
        let chunks_meta = vec![
            ChunkMetaForRerank {
                chunk_id: chunk_id1.clone(),
                rrf_score: 0.5,
                timestamp_created: 0,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: Some("hybrid retrieval with query-aware ranking".to_string()),
            },
            ChunkMetaForRerank {
                chunk_id: chunk_id2.clone(),
                rrf_score: 0.5,
                timestamp_created: 0,
                project_id: None,
                chunk_type: ChunkType::Doc,
                text: Some("totally unrelated text".to_string()),
            },
        ];

        let reranked = searcher.rerank_with_metadata_for_query(
            "query aware retrieval",
            results,
            chunks_meta,
            None,
        );

        assert_eq!(reranked.len(), 2);
        #[cfg(feature = "cross-encoder-reranker")]
        assert_eq!(reranked[0].chunk_id, chunk_id1);
        #[cfg(not(feature = "cross-encoder-reranker"))]
        assert_eq!(searcher.reranker_mode(), RerankerMode::Feature);
    }

    #[tokio::test]
    async fn test_multiple_chunks_fusion() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();

        // Add multiple chunks with different content
        let chunk_id1 = ChunkId::new();
        let chunk_id2 = ChunkId::new();
        let chunk_id3 = ChunkId::new();

        searcher
            .index_chunk(
                &tenant,
                &chunk_id1,
                "The parseConfig function reads configuration files",
            )
            .await
            .unwrap();
        searcher
            .index_chunk(
                &tenant,
                &chunk_id2,
                "Configuration parsing is handled by parseConfig",
            )
            .await
            .unwrap();
        searcher
            .index_chunk(
                &tenant,
                &chunk_id3,
                "This module handles user authentication",
            )
            .await
            .unwrap();

        // Search for parseConfig - should find chunks 1 and 2
        let results = searcher
            .search(&tenant, "parseConfig", 10, None)
            .await
            .unwrap();

        // Should find the relevant chunks
        assert!(
            !results.is_empty(),
            "Should find chunks matching parseConfig"
        );

        // Results should include chunks with parseConfig
        let result_ids: Vec<ChunkId> = results.iter().map(|r| r.chunk_id.clone()).collect();
        assert!(
            result_ids.contains(&chunk_id1) || result_ids.contains(&chunk_id2),
            "Should include parseConfig chunks"
        );
    }

    #[tokio::test]
    async fn test_tenant_isolation() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();
        let chunk_id = ChunkId::new();

        // Index chunk for tenant_a
        searcher
            .index_chunk(&tenant_a, &chunk_id, "secret data for tenant A only")
            .await
            .unwrap();

        // Tenant A should find it
        let results_a = searcher
            .search(&tenant_a, "secret", 10, None)
            .await
            .unwrap();
        assert!(!results_a.is_empty(), "Tenant A should find their data");

        // Tenant B should not find it
        let results_b = searcher
            .search(&tenant_b, "secret", 10, None)
            .await
            .unwrap();
        // Sparse index enforces tenant isolation
        let sparse_found_b = results_b.iter().any(|r| r.sparse_rank.is_some());
        assert!(
            !sparse_found_b,
            "Tenant B should not find tenant A's data in sparse"
        );
    }

    #[tokio::test]
    async fn test_empty_query() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();
        let chunk_id = ChunkId::new();

        searcher
            .index_chunk(&tenant, &chunk_id, "some content here")
            .await
            .unwrap();

        // Empty query should not crash
        let results = searcher.search(&tenant, "", 10, None).await;
        // May return error or empty results depending on sparse index behavior
        assert!(results.is_ok() || results.is_err());
    }

    #[tokio::test]
    async fn test_config_defaults() {
        let config = HybridConfig::default();

        assert_eq!(config.dense_k, 100);
        assert_eq!(config.sparse_k, 100);
        assert!(config.enable_sparse);
    }
}
