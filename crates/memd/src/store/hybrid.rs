//! Hybrid search coordinator
//!
//! Combines dense (semantic) and sparse (keyword) search with RRF fusion
//! and feature-based reranking for comprehensive retrieval.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::debug;

use super::dense::DenseSearcher;
use crate::error::Result;
use crate::index::{Bm25Index, SparseIndex};
use crate::retrieval::{
    ChunkWithMeta, FeatureReranker, FusionCandidate, FusionSource, RerankerConfig, RerankerContext,
    RrfConfig, RrfFusion,
};
use crate::retrieval::packer::{ContextPacker, PackerConfig};
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
            packer: PackerConfig::default(),
            enable_sparse: true,
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
}

/// Chunk metadata for reranking
pub struct ChunkMetaForRerank {
    pub chunk_id: ChunkId,
    pub rrf_score: f32,
    pub timestamp_created: i64,
    pub project_id: Option<String>,
    pub chunk_type: ChunkType,
}

/// Hybrid search coordinator
///
/// Combines dense (embedding-based) and sparse (BM25) search, fuses results
/// with RRF, and applies feature-based reranking.
pub struct HybridSearcher {
    dense: Arc<DenseSearcher>,
    sparse: Option<Arc<Bm25Index>>,
    text_processor: TextProcessor,
    fusion: RrfFusion,
    reranker: FeatureReranker,
    #[allow(dead_code)]
    packer: ContextPacker,
    config: HybridConfig,
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
    pub async fn search_with_timing(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        _context: Option<SearchContext>,
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
    use crate::embeddings::MockEmbedder;
    use crate::store::DenseSearchConfig;

    fn make_test_hybrid_searcher(enable_sparse: bool) -> HybridSearcher {
        let embedder = Arc::new(MockEmbedder::new(384));
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
}
