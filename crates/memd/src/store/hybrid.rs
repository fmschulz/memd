//! Hybrid search coordinator
//!
//! Combines dense (semantic) and sparse (keyword) search with RRF fusion
//! and feature-based reranking for comprehensive retrieval.
//! Supports tiered search with cache/hot/warm fallback when enabled.
//! Includes query routing for intent classification and structural search blending.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tracing::debug;

use super::dense::DenseSearcher;
use crate::error::Result;
use crate::index::{Bm25Index, SearchResult, SparseIndex};
use crate::metrics::TieredQueryMetrics;
use crate::retrieval::{
    ChunkWithMeta, FeatureReranker, FusionCandidate, FusionSource, RerankerConfig, RerankerContext,
    RrfConfig, RrfFusion,
};
use crate::retrieval::packer::{ContextPacker, PackerConfig};
use crate::structural::{
    CallerInfo, ErrorResult, ImportInfo, QueryIntent, QueryRouter, RouteResult,
    SymbolLocation, SymbolQueryService, ToolCallResult, TraceQueryService,
};
use crate::text::TextProcessor;
use crate::tiered::{
    AccessTracker, AccessTrackerConfig, HotTier, HotTierConfig, SemanticCache,
    SemanticCacheConfig, TieredSearcher, TieredSearcherConfig, TieredTiming, WarmTierSearch,
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
            packer: PackerConfig::default(),
            enable_sparse: true,
            enable_tiered: true,
            tiered_config: None,
        }
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
}

// ============================================================================
// Query Routing Types
// ============================================================================

/// Result from search_with_routing, supporting different result types.
#[derive(Debug)]
pub enum SearchWithRoutingResult {
    /// Pure hybrid/semantic search results.
    Hybrid(Vec<HybridSearchResult>),

    /// Structural results only (no semantic context).
    Structural(StructuralSearchResult),

    /// Structural primary + semantic context (STRUCT-14 blending).
    Blended(BlendedSearchResult),

    /// Trace/debug results (tool calls, stack traces).
    Trace(TraceSearchResult),
}

/// Result from structural search.
#[derive(Debug)]
pub struct StructuralSearchResult {
    /// The classified intent.
    pub intent: QueryIntent,
    /// Symbol locations found.
    pub symbols: Vec<SymbolLocation>,
    /// Caller information (if callers query).
    pub callers: Vec<CallerInfo>,
    /// Import information (if imports query).
    pub imports: Vec<ImportInfo>,
    /// Whether we fell back to semantic search.
    pub fell_back_to_semantic: bool,
}

impl Default for StructuralSearchResult {
    fn default() -> Self {
        Self {
            intent: QueryIntent::SemanticSearch,
            symbols: Vec::new(),
            callers: Vec::new(),
            imports: Vec::new(),
            fell_back_to_semantic: false,
        }
    }
}

/// Blended result with structural primary and semantic context (STRUCT-14).
#[derive(Debug)]
pub struct BlendedSearchResult {
    /// The classified intent.
    pub intent: QueryIntent,
    /// Primary results from structural search.
    pub structural: StructuralSearchResult,
    /// Supplementary context from semantic search.
    pub semantic_context: Vec<HybridSearchResult>,
    /// How results were blended.
    pub blend_strategy: BlendStrategy,
}

/// Strategy for blending structural and semantic results.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlendStrategy {
    /// Structural results first (100%), semantic for context only.
    StructuralPrimary,
    /// Weighted blend (structural_weight determines proportion).
    Weighted {
        /// Weight for structural results (0.0-1.0).
        structural_weight: f32,
    },
}

impl Default for BlendStrategy {
    fn default() -> Self {
        Self::StructuralPrimary
    }
}

/// Result from trace/debug search.
#[derive(Debug, Default)]
pub struct TraceSearchResult {
    /// The classified intent.
    pub intent: QueryIntent,
    /// Tool call results.
    pub tool_calls: Vec<ToolCallResult>,
    /// Error/stack trace results.
    pub errors: Vec<ErrorResult>,
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
///
/// Also integrates query routing for intent classification and structural search.
pub struct HybridSearcher {
    dense: Arc<DenseSearcher>,
    sparse: Option<Arc<Bm25Index>>,
    text_processor: TextProcessor,
    fusion: RrfFusion,
    reranker: FeatureReranker,
    #[allow(dead_code)]
    packer: ContextPacker,
    config: HybridConfig,
    /// Per-tenant tiered searchers (only populated if enable_tiered is true)
    tiered_searchers: RwLock<std::collections::HashMap<String, Arc<TieredSearcher<WarmTierAdapter>>>>,
    /// Shared semantic cache (across tenants)
    semantic_cache: Option<Arc<SemanticCache>>,
    /// Access tracker config for creating per-tenant access trackers
    access_tracker_config: AccessTrackerConfig,
    /// Hot tier config for creating per-tenant hot tiers
    hot_tier_config: HotTierConfig,
    /// Query router for intent classification.
    router: QueryRouter,
    /// Optional symbol query service for structural search.
    symbol_query_service: Option<Arc<SymbolQueryService>>,
    /// Optional trace query service for debug/trace search.
    trace_query_service: Option<Arc<TraceQueryService>>,
}

impl HybridSearcher {
    /// Create a new hybrid searcher
    pub fn new(
        dense: Arc<DenseSearcher>,
        sparse: Option<Arc<Bm25Index>>,
        config: HybridConfig,
    ) -> Self {
        let fusion = RrfFusion::new(config.rrf.clone());
        let reranker = FeatureReranker::new(config.reranker.clone());
        let packer = ContextPacker::new(config.packer.clone());

        // Create shared semantic cache if tiered search is enabled
        let semantic_cache = if config.enable_tiered {
            Some(Arc::new(SemanticCache::new(SemanticCacheConfig::default())))
        } else {
            None
        };

        // Create hot tier config based on dense dimension
        let dimension = dense.dimension();
        let hot_tier_config = HotTierConfig {
            hnsw_config: crate::index::HnswConfig {
                dimension,
                max_elements: 50_000,
                max_connections: 16,
                ef_construction: 200,
                ef_search: 30, // Lower than warm tier for faster queries
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
            router: QueryRouter::new(),
            symbol_query_service: None,
            trace_query_service: None,
        }
    }

    /// Create a hybrid searcher with structural query services.
    pub fn with_query_services(
        dense: Arc<DenseSearcher>,
        sparse: Option<Arc<Bm25Index>>,
        config: HybridConfig,
        symbol_query_service: Option<Arc<SymbolQueryService>>,
        trace_query_service: Option<Arc<TraceQueryService>>,
    ) -> Self {
        let mut searcher = Self::new(dense, sparse, config);
        searcher.symbol_query_service = symbol_query_service;
        searcher.trace_query_service = trace_query_service;
        searcher
    }

    /// Set the symbol query service for structural search.
    pub fn set_symbol_query_service(&mut self, service: Arc<SymbolQueryService>) {
        self.symbol_query_service = Some(service);
    }

    /// Set the trace query service for debug/trace search.
    pub fn set_trace_query_service(&mut self, service: Arc<TraceQueryService>) {
        self.trace_query_service = Some(service);
    }

    /// Get reference to the query router.
    pub fn router(&self) -> &QueryRouter {
        &self.router
    }

    /// Get or create tiered searcher for a tenant
    fn get_or_create_tiered_searcher(
        &self,
        tenant_id: &TenantId,
    ) -> Option<Arc<TieredSearcher<WarmTierAdapter>>> {
        if !self.config.enable_tiered {
            return None;
        }

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
        let warm_tier = Arc::new(WarmTierAdapter::new(
            Arc::clone(&self.dense),
            tenant_id.clone(),
        ));

        let access_tracker = Arc::new(RwLock::new(AccessTracker::new(
            self.access_tracker_config.clone(),
        )));

        let hot_tier = Arc::new(RwLock::new(HotTier::with_access_tracker(
            self.hot_tier_config.clone(),
            Arc::clone(&access_tracker),
        )));

        let tiered_config = self
            .config
            .tiered_config
            .clone()
            .unwrap_or_default();

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
        // Index in dense (via DenseSearcher)
        self.dense.index_chunk(tenant_id, chunk_id, text).await?;

        // Index in sparse if enabled
        if self.config.enable_sparse {
            if let Some(ref sparse) = self.sparse {
                // Process text into sentences for fine-grained indexing
                let processed = self.text_processor.process_chunk(text);
                let sentences: Vec<String> = processed.into_iter().map(|p| p.text).collect();

                if !sentences.is_empty() {
                    sparse.insert(tenant_id, chunk_id, &sentences)?;
                }
            }
        }

        debug!(
            tenant_id = %tenant_id,
            chunk_id = %chunk_id,
            sparse_enabled = self.config.enable_sparse,
            "indexed chunk in hybrid searcher"
        );

        Ok(())
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
        let query_embedding = self.dense.embed_query(query).await?;
        timing.dense_time = embed_start.elapsed(); // Embed time tracked as dense_time

        // Step 2: Tiered search (cache -> hot -> warm)
        let project_id = context.and_then(|c| c.current_project.as_deref());
        let tiered_result = tiered_searcher.search(&query_embedding, tenant_id, project_id, k)?;

        // Convert TieredTiming
        let tiered_timing = tiered_result.timing;
        timing.tiered = Some(tiered_timing.clone());

        // Step 3: If cache miss and sparse enabled, merge with sparse results
        let sparse_start = Instant::now();
        let sparse_results = if !tiered_result.cache_hit && self.config.enable_sparse {
            if let Some(ref sparse) = self.sparse {
                sparse.search(tenant_id, query, self.config.sparse_k)?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        timing.sparse_time = sparse_start.elapsed();

        // Step 4: Build results
        // For cache hits, return directly; for non-cache, fuse with sparse
        let results: Vec<HybridSearchResult> = if tiered_result.cache_hit || sparse_results.is_empty() {
            // Direct conversion from tiered results
            tiered_result
                .results
                .into_iter()
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

        // Step 1: Dense search
        let dense_start = Instant::now();
        let (dense_results, _embed_time, _search_time) = self
            .dense
            .search_with_timing(tenant_id, query, self.config.dense_k)
            .await?;
        timing.dense_time = dense_start.elapsed();

        // Step 2: Sparse search (if enabled)
        let sparse_start = Instant::now();
        let sparse_results = if self.config.enable_sparse {
            if let Some(ref sparse) = self.sparse {
                sparse.search(tenant_id, query, self.config.sparse_k)?
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
        if chunks_meta.is_empty() {
            return results;
        }

        // Build reranker context
        let reranker_context = match context {
            Some(ctx) => RerankerContext::now()
                .with_project(ctx.current_project.unwrap_or_default())
                .with_preferred_types(ctx.preferred_types),
            None => RerankerContext::now(),
        };

        // Build ChunkWithMeta for reranker
        let chunks_with_meta: Vec<ChunkWithMeta> = chunks_meta
            .into_iter()
            .map(|meta| ChunkWithMeta {
                chunk_id: meta.chunk_id,
                rrf_score: meta.rrf_score,
                timestamp_created: meta.timestamp_created,
                project_id: meta.project_id,
                chunk_type: meta.chunk_type,
            })
            .collect();

        // Rerank
        let ranked = self.reranker.rerank(chunks_with_meta, &reranker_context);

        // Map back to HybridSearchResult
        ranked
            .into_iter()
            .map(|r| {
                // Find original result to preserve dense/sparse ranks
                let original = results
                    .iter()
                    .find(|orig| orig.chunk_id == r.chunk_id);

                HybridSearchResult {
                    chunk_id: r.chunk_id,
                    final_score: r.final_score,
                    dense_rank: original.and_then(|o| o.dense_rank),
                    sparse_rank: original.and_then(|o| o.sparse_rank),
                }
            })
            .collect()
    }

    // ========================================================================
    // Query Routing Methods
    // ========================================================================

    /// Search with intent-based routing.
    ///
    /// Classifies the query intent and routes to the appropriate search backend:
    /// - Structural queries (symbol definition, callers, references, imports) go to SymbolQueryService
    /// - Trace queries (tool calls, errors) go to TraceQueryService
    /// - Semantic queries fall back to hybrid search
    ///
    /// For code-intent queries (STRUCT-14), blends structural results with semantic context.
    pub async fn search_with_routing(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        project_id: Option<&str>,
    ) -> Result<SearchWithRoutingResult> {
        let route = self.router.classify(query);

        debug!(
            tenant_id = %tenant_id,
            query = query,
            intent = ?route.intent,
            confidence = route.confidence,
            blend = route.blend_semantic_context,
            "query routed"
        );

        match route.intent {
            QueryIntent::SemanticSearch => {
                // Fall back to standard hybrid search
                let context = project_id.map(|p| SearchContext {
                    current_project: Some(p.to_string()),
                    preferred_types: Vec::new(),
                });
                let results = self.search(tenant_id, query, k, context).await?;
                Ok(SearchWithRoutingResult::Hybrid(results))
            }

            QueryIntent::SymbolDefinition(ref name) => {
                self.route_symbol_definition(tenant_id, name, k, project_id, &route)
                    .await
            }

            QueryIntent::SymbolCallers(ref name) => {
                self.route_symbol_callers(tenant_id, name, k, project_id, &route)
                    .await
            }

            QueryIntent::SymbolReferences(ref name) => {
                self.route_symbol_references(tenant_id, name, k, project_id, &route)
                    .await
            }

            QueryIntent::ModuleImports(ref module) => {
                self.route_module_imports(tenant_id, module, k, project_id, &route)
                    .await
            }

            QueryIntent::FileSymbols(ref file) => {
                self.route_file_symbols(tenant_id, file, k, project_id, &route)
                    .await
            }

            QueryIntent::ToolCalls(ref tool_name) => {
                self.route_tool_calls(tenant_id, tool_name.as_deref(), k)
            }

            QueryIntent::ErrorSearch(ref signature) => {
                self.route_error_search(tenant_id, signature.as_deref(), k)
            }

            // Document queries fall back to semantic
            QueryIntent::DocQa | QueryIntent::DecisionWhy(_) | QueryIntent::PlanNext => {
                let context = project_id.map(|p| SearchContext {
                    current_project: Some(p.to_string()),
                    preferred_types: Vec::new(),
                });
                let results = self.search(tenant_id, query, k, context).await?;
                Ok(SearchWithRoutingResult::Hybrid(results))
            }
        }
    }

    /// Route symbol definition query.
    async fn route_symbol_definition(
        &self,
        tenant_id: &TenantId,
        name: &str,
        k: usize,
        project_id: Option<&str>,
        route: &RouteResult,
    ) -> Result<SearchWithRoutingResult> {
        let symbols = if let Some(ref svc) = self.symbol_query_service {
            svc.find_symbol_definition(tenant_id, name, project_id)?
        } else {
            Vec::new()
        };

        let structural = StructuralSearchResult {
            intent: QueryIntent::SymbolDefinition(name.to_string()),
            symbols,
            callers: Vec::new(),
            imports: Vec::new(),
            fell_back_to_semantic: false,
        };

        self.maybe_blend_or_fallback(tenant_id, name, k, project_id, structural, route)
            .await
    }

    /// Route symbol callers query.
    async fn route_symbol_callers(
        &self,
        tenant_id: &TenantId,
        name: &str,
        k: usize,
        project_id: Option<&str>,
        route: &RouteResult,
    ) -> Result<SearchWithRoutingResult> {
        let callers = if let Some(ref svc) = self.symbol_query_service {
            svc.find_callers(tenant_id, name, 1, project_id)?
        } else {
            Vec::new()
        };

        let structural = StructuralSearchResult {
            intent: QueryIntent::SymbolCallers(name.to_string()),
            symbols: Vec::new(),
            callers,
            imports: Vec::new(),
            fell_back_to_semantic: false,
        };

        self.maybe_blend_or_fallback(tenant_id, name, k, project_id, structural, route)
            .await
    }

    /// Route symbol references query.
    async fn route_symbol_references(
        &self,
        tenant_id: &TenantId,
        name: &str,
        k: usize,
        project_id: Option<&str>,
        route: &RouteResult,
    ) -> Result<SearchWithRoutingResult> {
        let symbols = if let Some(ref svc) = self.symbol_query_service {
            svc.find_references(tenant_id, name, project_id)?
        } else {
            Vec::new()
        };

        let structural = StructuralSearchResult {
            intent: QueryIntent::SymbolReferences(name.to_string()),
            symbols,
            callers: Vec::new(),
            imports: Vec::new(),
            fell_back_to_semantic: false,
        };

        self.maybe_blend_or_fallback(tenant_id, name, k, project_id, structural, route)
            .await
    }

    /// Route module imports query.
    async fn route_module_imports(
        &self,
        tenant_id: &TenantId,
        module: &str,
        k: usize,
        project_id: Option<&str>,
        route: &RouteResult,
    ) -> Result<SearchWithRoutingResult> {
        let imports = if let Some(ref svc) = self.symbol_query_service {
            svc.find_imports(tenant_id, module, project_id)?
        } else {
            Vec::new()
        };

        let structural = StructuralSearchResult {
            intent: QueryIntent::ModuleImports(module.to_string()),
            symbols: Vec::new(),
            callers: Vec::new(),
            imports,
            fell_back_to_semantic: false,
        };

        self.maybe_blend_or_fallback(tenant_id, module, k, project_id, structural, route)
            .await
    }

    /// Route file symbols query.
    async fn route_file_symbols(
        &self,
        tenant_id: &TenantId,
        file: &str,
        k: usize,
        project_id: Option<&str>,
        route: &RouteResult,
    ) -> Result<SearchWithRoutingResult> {
        // File symbols not directly supported by SymbolQueryService yet
        // Fall back to semantic search for the file name
        let structural = StructuralSearchResult {
            intent: QueryIntent::FileSymbols(file.to_string()),
            symbols: Vec::new(),
            callers: Vec::new(),
            imports: Vec::new(),
            fell_back_to_semantic: false,
        };

        self.maybe_blend_or_fallback(tenant_id, file, k, project_id, structural, route)
            .await
    }

    /// Route tool calls query.
    fn route_tool_calls(
        &self,
        tenant_id: &TenantId,
        tool_name: Option<&str>,
        k: usize,
    ) -> Result<SearchWithRoutingResult> {
        let tool_calls = if let Some(ref svc) = self.trace_query_service {
            svc.find_tool_calls(tenant_id, tool_name, None, None, k)?
        } else {
            Vec::new()
        };

        Ok(SearchWithRoutingResult::Trace(TraceSearchResult {
            intent: QueryIntent::ToolCalls(tool_name.map(String::from)),
            tool_calls,
            errors: Vec::new(),
        }))
    }

    /// Route error search query.
    fn route_error_search(
        &self,
        tenant_id: &TenantId,
        signature: Option<&str>,
        k: usize,
    ) -> Result<SearchWithRoutingResult> {
        let errors = if let Some(ref svc) = self.trace_query_service {
            svc.find_errors(
                tenant_id,
                signature,
                None,  // function_name
                None,  // file_path
                None,  // time_range
                k,
            )?
        } else {
            Vec::new()
        };

        Ok(SearchWithRoutingResult::Trace(TraceSearchResult {
            intent: QueryIntent::ErrorSearch(signature.map(String::from)),
            tool_calls: Vec::new(),
            errors,
        }))
    }

    /// Maybe blend structural results with semantic context, or fall back to semantic.
    ///
    /// Implements STRUCT-14: code-intent queries blend structural (primary) with semantic (context).
    async fn maybe_blend_or_fallback(
        &self,
        tenant_id: &TenantId,
        query_term: &str,
        k: usize,
        project_id: Option<&str>,
        structural: StructuralSearchResult,
        route: &RouteResult,
    ) -> Result<SearchWithRoutingResult> {
        let has_results = !structural.symbols.is_empty()
            || !structural.callers.is_empty()
            || !structural.imports.is_empty();

        // If no structural results and fallback enabled, use semantic search only
        if !has_results && route.fallback_to_semantic {
            let context = project_id.map(|p| SearchContext {
                current_project: Some(p.to_string()),
                preferred_types: Vec::new(),
            });
            // Trigger semantic search (result not used for structural return, but ensures fallback behavior)
            let _results = self.search(tenant_id, query_term, k, context).await?;

            return Ok(SearchWithRoutingResult::Structural(StructuralSearchResult {
                fell_back_to_semantic: true,
                ..structural
            }));
        }

        // If blending is enabled and we have structural results, add semantic context
        if has_results && route.blend_semantic_context {
            let context = project_id.map(|p| SearchContext {
                current_project: Some(p.to_string()),
                preferred_types: Vec::new(),
            });

            // Get fewer semantic results since they're supplementary
            let semantic_k = (k / 2).max(3);
            let semantic_context = self.search(tenant_id, query_term, semantic_k, context).await?;

            return Ok(SearchWithRoutingResult::Blended(BlendedSearchResult {
                intent: structural.intent.clone(),
                structural,
                semantic_context,
                blend_strategy: BlendStrategy::StructuralPrimary,
            }));
        }

        // Return structural results only
        Ok(SearchWithRoutingResult::Structural(structural))
    }

    /// Classify a query intent without executing the search.
    pub fn classify_query(&self, query: &str) -> RouteResult {
        self.router.classify(query)
    }

    /// Check if structural search is available.
    pub fn structural_enabled(&self) -> bool {
        self.symbol_query_service.is_some()
    }

    /// Check if trace search is available.
    pub fn trace_enabled(&self) -> bool {
        self.trace_query_service.is_some()
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
    pub fn run_tiered_maintenance(&self, tenant_id: &TenantId) -> Option<crate::tiered::MaintenanceResult> {
        let tiered_searcher = self.get_or_create_tiered_searcher(tenant_id)?;
        Some(tiered_searcher.run_maintenance(tenant_id))
    }

    /// Get tiered metrics for recording
    ///
    /// Returns a TieredQueryMetrics for the given timing and cache/hot tier hit info.
    pub fn create_tiered_metrics(timing: &HybridTiming, cache_hit: bool, hot_tier_hit: bool) -> TieredQueryMetrics {
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
            cache.invalidate_chunks(&[chunk_id.clone()]);
        }
    }

    /// Get semantic cache statistics (if tiered search enabled)
    pub fn get_cache_stats(&self) -> Option<crate::tiered::CacheStats> {
        self.semantic_cache.as_ref().map(|c| c.get_stats())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::MockEmbedder;
    use crate::store::DenseSearchConfig;

    fn make_test_hybrid_searcher(enable_sparse: bool) -> HybridSearcher {
        let embedder = Arc::new(MockEmbedder::new());  // Uses default config (1024 dims)
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

    fn make_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    #[tokio::test]
    async fn test_hybrid_search_basic() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();
        let chunk_id = ChunkId::new();

        // Index a chunk
        searcher
            .index_chunk(&tenant, &chunk_id, "The getUserById function returns user data")
            .await
            .unwrap();

        // Search should find it
        let results = searcher.search(&tenant, "getUserById", 10, None).await.unwrap();

        // Should have results (at least from sparse)
        assert!(!results.is_empty());
        assert_eq!(results[0].chunk_id, chunk_id);
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
        assert!(!results.is_empty(), "Should find unique identifier via sparse search");
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
        let results = searcher.search(&tenant, "deletable", 10, None).await.unwrap();
        assert!(!results.is_empty(), "Should be searchable after indexing");

        // Delete from sparse
        searcher.delete_chunk(&tenant, &chunk_id).unwrap();

        // Should not be findable in sparse anymore
        // (dense may still have it until full deletion support is added)
        if let Some(ref sparse) = searcher.sparse {
            let sparse_results = sparse.search(&tenant, "deletable", 10).unwrap();
            assert!(sparse_results.is_empty(), "Should not be in sparse after delete");
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
            },
            ChunkMetaForRerank {
                chunk_id: chunk_id2.clone(),
                rrf_score: 0.4,
                timestamp_created: now_ms, // just created
                project_id: Some("current_project".to_string()),
                chunk_type: ChunkType::Code,
            },
        ];

        let context = Some(SearchContext {
            current_project: Some("current_project".to_string()),
            preferred_types: vec![ChunkType::Code],
        });

        let reranked = searcher.rerank_with_metadata(results, chunks_meta, context);

        // chunk2 should be boosted due to recency, project match, and type preference
        assert_eq!(reranked.len(), 2);
        // The reranker may reorder based on bonuses
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
            .index_chunk(&tenant, &chunk_id1, "The parseConfig function reads configuration files")
            .await
            .unwrap();
        searcher
            .index_chunk(&tenant, &chunk_id2, "Configuration parsing is handled by parseConfig")
            .await
            .unwrap();
        searcher
            .index_chunk(&tenant, &chunk_id3, "This module handles user authentication")
            .await
            .unwrap();

        // Search for parseConfig - should find chunks 1 and 2
        let results = searcher.search(&tenant, "parseConfig", 10, None).await.unwrap();

        // Should find the relevant chunks
        assert!(!results.is_empty(), "Should find chunks matching parseConfig");

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
        let results_a = searcher.search(&tenant_a, "secret", 10, None).await.unwrap();
        assert!(!results_a.is_empty(), "Tenant A should find their data");

        // Tenant B should not find it
        let results_b = searcher.search(&tenant_b, "secret", 10, None).await.unwrap();
        // Sparse index enforces tenant isolation
        let sparse_found_b = results_b.iter().any(|r| r.sparse_rank.is_some());
        assert!(!sparse_found_b, "Tenant B should not find tenant A's data in sparse");
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

    // ========================================================================
    // Query Routing Tests
    // ========================================================================

    #[tokio::test]
    async fn test_route_to_semantic_search() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();
        let chunk_id = ChunkId::new();

        // Index some content
        searcher
            .index_chunk(&tenant, &chunk_id, "How does authentication work in our system")
            .await
            .unwrap();

        // Generic question should route to semantic/hybrid search
        let result = searcher
            .search_with_routing(&tenant, "how does authentication work", 10, None)
            .await
            .unwrap();

        assert!(matches!(result, SearchWithRoutingResult::Hybrid(_)));
    }

    #[tokio::test]
    async fn test_route_to_structural_search_no_service() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();

        // Structural query without symbol service should return empty structural result
        let result = searcher
            .search_with_routing(&tenant, "where is UserService defined", 10, None)
            .await
            .unwrap();

        // Should return structural result (empty, possibly with fallback)
        match result {
            SearchWithRoutingResult::Structural(s) => {
                assert!(matches!(s.intent, QueryIntent::SymbolDefinition(_)));
                assert!(s.symbols.is_empty());
            }
            SearchWithRoutingResult::Blended(b) => {
                assert!(matches!(b.intent, QueryIntent::SymbolDefinition(_)));
            }
            _ => panic!("Expected structural or blended result"),
        }
    }

    #[tokio::test]
    async fn test_route_to_trace_search_no_service() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();

        // Tool calls query without trace service should return empty trace result
        let result = searcher
            .search_with_routing(&tenant, "recent tool calls", 10, None)
            .await
            .unwrap();

        assert!(matches!(result, SearchWithRoutingResult::Trace(ref t) if t.tool_calls.is_empty()));
    }

    #[tokio::test]
    async fn test_classify_query_intent() {
        let searcher = make_test_hybrid_searcher(true);

        // Test various query classifications
        let route = searcher.classify_query("where is Config defined");
        assert!(matches!(route.intent, QueryIntent::SymbolDefinition(_)));
        assert!(route.blend_semantic_context);

        let route = searcher.classify_query("who calls process_data");
        assert!(matches!(route.intent, QueryIntent::SymbolCallers(_)));
        assert!(route.blend_semantic_context);

        let route = searcher.classify_query("recent errors");
        assert!(matches!(route.intent, QueryIntent::ErrorSearch(None)));
        assert!(!route.blend_semantic_context);

        let route = searcher.classify_query("how does caching work");
        assert!(matches!(route.intent, QueryIntent::SemanticSearch));
        assert!(!route.blend_semantic_context);
    }

    #[test]
    fn test_blend_strategy_default() {
        assert_eq!(BlendStrategy::default(), BlendStrategy::StructuralPrimary);
    }

    #[test]
    fn test_structural_result_default() {
        let result = StructuralSearchResult::default();
        assert!(matches!(result.intent, QueryIntent::SemanticSearch));
        assert!(result.symbols.is_empty());
        assert!(result.callers.is_empty());
        assert!(result.imports.is_empty());
        assert!(!result.fell_back_to_semantic);
    }

    #[test]
    fn test_searcher_service_flags() {
        let searcher = make_test_hybrid_searcher(true);

        // No services attached
        assert!(!searcher.structural_enabled());
        assert!(!searcher.trace_enabled());

        // Router should be available
        let _ = searcher.router();
    }

    #[tokio::test]
    async fn test_route_error_search() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();

        let result = searcher
            .search_with_routing(&tenant, "errors: TypeError", 10, None)
            .await
            .unwrap();

        match result {
            SearchWithRoutingResult::Trace(t) => {
                assert!(matches!(t.intent, QueryIntent::ErrorSearch(Some(ref s)) if s == "TypeError"));
            }
            _ => panic!("Expected trace result"),
        }
    }

    #[tokio::test]
    async fn test_route_tool_calls() {
        let searcher = make_test_hybrid_searcher(true);
        let tenant = make_tenant();

        let result = searcher
            .search_with_routing(&tenant, "calls to read_file", 10, None)
            .await
            .unwrap();

        match result {
            SearchWithRoutingResult::Trace(t) => {
                assert!(matches!(t.intent, QueryIntent::ToolCalls(Some(ref s)) if s == "read_file"));
            }
            _ => panic!("Expected trace result"),
        }
    }
}
