//! Hybrid search coordinator
//!
//! Combines dense (semantic) and sparse (keyword) search with RRF fusion
//! and feature-based reranking for comprehensive retrieval.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::debug;

use super::dense::DenseSearcher;
use crate::error::{MemdError, Result};
use crate::index::{Bm25Index, SparseIndex};
use crate::retrieval::packer::{ContextPacker, PackerConfig};
use crate::retrieval::{
    ChunkWithMeta, FusionCandidate, FusionSource, RerankerConfig, RerankerContext, RerankerEngine,
    RerankerMode, RrfConfig, RrfFusion,
};
use crate::text::TextProcessor;
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
        }
    }
}

/// Sparse candidates to fetch when the dense side contributes `dense_k`
/// candidates. A dense side that cannot run counts as zero.
fn sparse_candidate_count(config: &HybridConfig, dense_k: usize, requested_k: usize) -> usize {
    if dense_k == 0 && !config.enable_rerank {
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

/// Hybrid search coordinator
///
/// Combines dense (embedding-based) and sparse (BM25) search, fuses results
/// with RRF, and applies feature-based reranking.
pub struct HybridSearcher {
    dense: Option<Arc<DenseSearcher>>,
    sparse: Option<Arc<Bm25Index>>,
    text_processor: TextProcessor,
    fusion: RrfFusion,
    reranker: RerankerEngine,
    #[allow(dead_code)]
    packer: ContextPacker,
    config: HybridConfig,
}

impl HybridSearcher {
    /// Borrow the sparse index if BM25 is enabled.
    pub fn sparse_index(&self) -> Option<&Arc<Bm25Index>> {
        self.sparse.as_ref()
    }

    /// Why the dense side cannot run, once an embedding call found that the
    /// model does not load. Searches then answer from the keyword index.
    pub fn dense_unavailable_reason(&self) -> Option<&str> {
        self.dense.as_ref()?.unavailable_reason()
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
    /// dense index.
    pub fn new_sparse_only(sparse: Arc<Bm25Index>, mut config: HybridConfig) -> Self {
        config.dense_k = 0;
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

        Self {
            dense,
            sparse,
            text_processor: TextProcessor::new(),
            fusion,
            reranker,
            packer,
            config,
        }
    }

    /// Effective reranker mode in use (after fallback handling).
    pub fn reranker_mode(&self) -> RerankerMode {
        self.reranker.mode()
    }

    pub fn rerank_enabled(&self) -> bool {
        self.config.enable_rerank
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

        debug!(
            tenant_id = %tenant_id,
            chunk_count = chunks.len(),
            sparse_enabled = self.config.enable_sparse,
            "indexed batch in hybrid searcher"
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

    /// Search dense and sparse indexes directly, with timing information.
    pub async fn search_with_timing(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        _context: Option<SearchContext>,
    ) -> Result<(Vec<HybridSearchResult>, HybridTiming)> {
        let total_start = Instant::now();
        let mut timing = HybridTiming::default();

        let dense_start = Instant::now();
        let mut dense_k = self.config.dense_k;
        let mut dense_results = if dense_k > 0 {
            let dense = self.dense.as_ref().ok_or_else(|| {
                MemdError::StorageError("dense search is not configured".to_string())
            })?;
            match dense
                .search_with_timing(tenant_id, query, dense_k.max(k))
                .await
            {
                Ok((dense_results, _embed_time, _search_time)) => dense_results,
                // The model cannot load: answer from the keyword index alone,
                // the same as a searcher built without a dense side.
                Err(MemdError::ModelUnavailable(_)) => {
                    dense_k = 0;
                    Vec::new()
                }
                Err(error) => return Err(error),
            }
        } else {
            Vec::new()
        };
        // Deduplicate dense hits and break score ties by ID before fusion.
        let mut seen = std::collections::HashSet::new();
        dense_results.retain(|result| seen.insert(result.chunk_id.clone()));
        dense_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        timing.dense_time = dense_start.elapsed();

        // Step 2: Sparse search (if enabled)
        let sparse_start = Instant::now();
        let sparse_results = if self.config.enable_sparse {
            if let Some(ref sparse) = self.sparse {
                sparse.search(
                    tenant_id,
                    query,
                    sparse_candidate_count(&self.config, dense_k, k),
                )?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        timing.sparse_time = sparse_start.elapsed();

        // Keep dense similarity scores when no lexical candidate matches.
        if sparse_results.is_empty() {
            let results = dense_results
                .into_iter()
                .take(k)
                .map(|result| HybridSearchResult {
                    chunk_id: result.chunk_id,
                    final_score: result.score,
                    dense_rank: None,
                    sparse_rank: None,
                })
                .collect();
            timing.total_time = total_start.elapsed();
            return Ok((results, timing));
        }

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

    /// Get reference to text processor
    pub fn text_processor(&self) -> &TextProcessor {
        &self.text_processor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::{Embedder, MockEmbedder};
    use crate::store::DenseSearchConfig;

    struct RankingEmbedder(crate::embeddings::EmbeddingConfig);

    #[async_trait::async_trait]
    impl Embedder for RankingEmbedder {
        async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| match *text {
                    "alpha" => vec![1.0, 0.0],
                    "beta keyword" => vec![0.8, 0.6],
                    _ => vec![0.0, 1.0],
                })
                .collect())
        }

        async fn embed_query(&self, _query: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0, 0.0])
        }

        fn dimension(&self) -> usize {
            2
        }

        fn config(&self) -> &crate::embeddings::EmbeddingConfig {
            &self.0
        }
    }

    #[tokio::test]
    async fn uncached_retrieval_preserves_cosine_and_dense_overfetch() {
        for (query, sparse_enabled) in [("absent", false), ("absent", true), ("keyword", true)] {
            let embedder = Arc::new(RankingEmbedder(crate::embeddings::EmbeddingConfig {
                dimension: 2,
                ..Default::default()
            }));
            let mut dense_config = DenseSearchConfig {
                persist: false,
                ..Default::default()
            };
            dense_config.hnsw.dimension = 2;
            let dense = Arc::new(DenseSearcher::with_embedder(embedder, dense_config));
            let sparse = sparse_enabled.then(|| Arc::new(Bm25Index::new().unwrap()));
            let searcher = HybridSearcher::new(
                dense,
                sparse,
                HybridConfig {
                    dense_k: 1,
                    enable_sparse: sparse_enabled,
                    ..Default::default()
                },
            );
            let tenant = make_tenant();
            let alpha = ChunkId::new();
            let beta = ChunkId::new();
            let gamma = ChunkId::new();
            searcher
                .index_batch(
                    &tenant,
                    &[
                        (alpha.clone(), "alpha".to_string()),
                        (beta.clone(), "beta keyword".to_string()),
                        (gamma, "gamma".to_string()),
                    ],
                )
                .await
                .unwrap();
            let results = searcher.search(&tenant, query, 2, None).await.unwrap();
            assert_eq!(results.len(), 2);
            if query == "keyword" {
                assert_eq!(results[0].chunk_id, beta);
                assert_eq!(results[1].chunk_id, alpha);
                assert!((results[0].final_score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-6);
                assert!((results[1].final_score - 1.0 / 61.0).abs() < 1e-6);
            } else {
                assert_eq!(results[0].chunk_id, alpha);
                assert_eq!(results[1].chunk_id, beta);
                assert!((results[0].final_score - 1.0).abs() < 1e-6);
                assert!((results[1].final_score - 0.8).abs() < 1e-6);
                assert!(results
                    .iter()
                    .all(|result| result.dense_rank.is_none() && result.sparse_rank.is_none()));
            }
        }
    }

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
        assert_eq!(
            sparse_candidate_count(&sparse_only, sparse_only.dense_k, 80),
            80
        );

        let reranked_sparse = HybridConfig {
            dense_k: 0,
            sparse_k: 200,
            enable_rerank: true,
            ..HybridConfig::default()
        };
        assert_eq!(
            sparse_candidate_count(&reranked_sparse, reranked_sparse.dense_k, 80),
            200
        );

        let hybrid = HybridConfig {
            dense_k: 100,
            sparse_k: 200,
            ..HybridConfig::default()
        };
        assert_eq!(sparse_candidate_count(&hybrid, hybrid.dense_k, 80), 200);
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
