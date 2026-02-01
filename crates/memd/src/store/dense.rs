//! Dense vector search coordinator
//!
//! Combines embeddings and HNSW index for semantic search.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::embeddings::{CandleEmbedder, Embedder};
use crate::error::Result;
use crate::index::{HnswConfig, HnswIndex};
use crate::metrics::IndexStats;
use crate::types::{ChunkId, TenantId};

/// Result of a dense search with chunk content
#[derive(Debug, Clone)]
pub struct DenseSearchResult {
    /// Chunk ID
    pub chunk_id: ChunkId,
    /// Cosine similarity score (0.0 to 1.0)
    pub score: f32,
}

/// Configuration for dense search
#[derive(Debug, Clone)]
pub struct DenseSearchConfig {
    /// HNSW configuration
    pub hnsw: HnswConfig,
    /// Whether to persist index
    pub persist: bool,
    // Temporarily removed during Candle migration
    // /// Embedding model to use
    // pub model: EmbeddingModel,
}

impl Default for DenseSearchConfig {
    fn default() -> Self {
        Self {
            hnsw: HnswConfig::default(),
            persist: true,
            // model: EmbeddingModel::default(),
        }
    }
}

/// Dense search coordinator for a tenant
pub struct DenseSearcher {
    /// Embedding model (shared across tenants)
    embedder: Arc<dyn Embedder>,
    /// Per-tenant HNSW indices
    indices: RwLock<HashMap<String, Arc<HnswIndex>>>,
    /// Base path for index persistence
    base_path: Option<PathBuf>,
    /// Configuration
    config: DenseSearchConfig,
}

impl DenseSearcher {
    /// Create a new dense searcher with Candle embedder
    pub fn new(config: DenseSearchConfig) -> Result<Self> {
        let embedder = Arc::new(CandleEmbedder::new()?);

        // Update HNSW config dimension to match model
        let mut updated_config = config.clone();
        updated_config.hnsw.dimension = embedder.dimension();

        Ok(Self {
            embedder,
            indices: RwLock::new(HashMap::new()),
            base_path: None,
            config: updated_config,
        })
    }

    /// Create with custom embedder (for testing with MockEmbedder)
    pub fn with_embedder(embedder: Arc<dyn Embedder>, config: DenseSearchConfig) -> Self {
        Self {
            embedder,
            indices: RwLock::new(HashMap::new()),
            base_path: None,
            config,
        }
    }

    /// Set base path for index persistence
    pub fn with_base_path(mut self, path: PathBuf) -> Self {
        self.base_path = Some(path);
        self
    }

    /// Get or create index for a tenant
    fn get_or_create_index(&self, tenant_id: &TenantId) -> Result<Arc<HnswIndex>> {
        let tenant_str = tenant_id.to_string();

        // Fast path: read lock
        {
            let indices = self.indices.read();
            if let Some(index) = indices.get(&tenant_str) {
                return Ok(Arc::clone(index));
            }
        }

        // Slow path: write lock + create
        let mut indices = self.indices.write();

        // Double-check
        if let Some(index) = indices.get(&tenant_str) {
            return Ok(Arc::clone(index));
        }

        let index = if self.config.persist {
            if let Some(ref base_path) = self.base_path {
                let index_path = base_path
                    .join("tenants")
                    .join(&tenant_str)
                    .join("warm_index");
                HnswIndex::with_persistence(self.config.hnsw.clone(), index_path)?
            } else {
                HnswIndex::new(self.config.hnsw.clone())
            }
        } else {
            HnswIndex::new(self.config.hnsw.clone())
        };

        let index = Arc::new(index);
        indices.insert(tenant_str, Arc::clone(&index));

        Ok(index)
    }

    /// Index a chunk embedding
    pub async fn index_chunk(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        text: &str,
    ) -> Result<()> {
        let embedding = self.embedder.embed_query(text).await?;
        let index = self.get_or_create_index(tenant_id)?;
        index.insert(chunk_id, &embedding)?;

        tracing::debug!(
            tenant_id = %tenant_id,
            chunk_id = %chunk_id,
            "indexed chunk in HNSW"
        );

        Ok(())
    }

    /// Index multiple chunks in batch
    pub async fn index_batch(
        &self,
        tenant_id: &TenantId,
        chunks: &[(ChunkId, String)],
    ) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        // Batch embed all texts
        let texts: Vec<&str> = chunks.iter().map(|(_, text)| text.as_str()).collect();
        let embeddings = self.embedder.embed_texts(&texts).await?;

        // Insert into index
        let index = self.get_or_create_index(tenant_id)?;
        let items: Vec<(ChunkId, Vec<f32>)> = chunks
            .iter()
            .zip(embeddings.into_iter())
            .map(|((chunk_id, _), emb)| (chunk_id.clone(), emb))
            .collect();

        index.insert_batch(&items)?;

        tracing::debug!(
            tenant_id = %tenant_id,
            count = chunks.len(),
            "indexed batch in HNSW"
        );

        Ok(())
    }

    /// Search for similar chunks
    pub async fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<DenseSearchResult>> {
        let (results, _, _) = self.search_with_timing(tenant_id, query, k).await?;
        Ok(results)
    }

    /// Search for similar chunks with timing information
    ///
    /// Returns (results, embed_time, search_time)
    pub async fn search_with_timing(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<(Vec<DenseSearchResult>, Duration, Duration)> {
        let embed_start = Instant::now();
        let query_embedding = self.embedder.embed_query(query).await?;
        let embed_time = embed_start.elapsed();

        let search_start = Instant::now();
        let index = self.get_or_create_index(tenant_id)?;
        let results = index.search(&query_embedding, k)?;
        let search_time = search_start.elapsed();

        let dense_results: Vec<DenseSearchResult> = results
            .into_iter()
            .map(|r| DenseSearchResult {
                chunk_id: r.chunk_id,
                score: r.score,
            })
            .collect();

        tracing::debug!(
            tenant_id = %tenant_id,
            query_len = query.len(),
            results = dense_results.len(),
            embed_ms = embed_time.as_millis(),
            search_ms = search_time.as_millis(),
            "dense search completed"
        );

        Ok((dense_results, embed_time, search_time))
    }

    /// Get index statistics for all tenants
    pub fn get_stats(&self) -> HashMap<String, IndexStats> {
        let indices = self.indices.read();
        let dimension = self.embedder.dimension();

        indices
            .iter()
            .map(|(tenant_id, index)| {
                let count = index.len() as u64;
                // Estimate memory: each embedding is dim * 4 bytes, plus HNSW overhead (~2x)
                let embedding_bytes = count * dimension as u64 * 4;
                let estimated_memory = embedding_bytes * 2;

                (
                    tenant_id.clone(),
                    IndexStats {
                        chunks_indexed: count,
                        embeddings_count: count,
                        embedding_dimension: dimension,
                        index_memory_bytes: estimated_memory,
                    },
                )
            })
            .collect()
    }

    /// Get index statistics for a specific tenant
    pub fn get_tenant_stats(&self, tenant_id: &TenantId) -> Option<IndexStats> {
        let indices = self.indices.read();
        let tenant_str = tenant_id.to_string();
        let dimension = self.embedder.dimension();

        indices.get(&tenant_str).map(|index| {
            let count = index.len() as u64;
            let embedding_bytes = count * dimension as u64 * 4;
            let estimated_memory = embedding_bytes * 2;

            IndexStats {
                chunks_indexed: count,
                embeddings_count: count,
                embedding_dimension: dimension,
                index_memory_bytes: estimated_memory,
            }
        })
    }

    /// Save all indices
    pub fn save_all(&self) -> Result<()> {
        let indices = self.indices.read();
        for (tenant_id, index) in indices.iter() {
            if let Err(e) = index.save() {
                tracing::warn!(tenant_id, error = %e, "failed to save index");
            }
        }
        Ok(())
    }

    /// Get embedding dimension
    pub fn dimension(&self) -> usize {
        self.embedder.dimension()
    }

    /// Search using a pre-computed query embedding (for tiered search)
    ///
    /// This avoids re-embedding when the caller already has the embedding.
    pub fn search_with_embedding(
        &self,
        tenant_id: &TenantId,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<DenseSearchResult>> {
        let index = self.get_or_create_index(tenant_id)?;
        let results = index.search(query_embedding, k)?;

        Ok(results
            .into_iter()
            .map(|r| DenseSearchResult {
                chunk_id: r.chunk_id,
                score: r.score,
            })
            .collect())
    }

    /// Get the number of indexed chunks for a tenant
    pub fn index_len(&self, tenant_id: &TenantId) -> usize {
        let indices = self.indices.read();
        let tenant_str = tenant_id.to_string();
        indices.get(&tenant_str).map(|i| i.len()).unwrap_or(0)
    }

    /// Embed a query text (exposes embedder for tiered search)
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embedder.embed_query(text).await
    }
}

impl Drop for DenseSearcher {
    fn drop(&mut self) {
        if self.config.persist {
            if let Err(e) = self.save_all() {
                tracing::warn!(error = %e, "failed to save indices on drop");
            }
        }
    }
}
