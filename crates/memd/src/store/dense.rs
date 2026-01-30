//! Dense vector search coordinator
//!
//! Combines embeddings and HNSW index for semantic search.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::embeddings::{Embedder, OnnxEmbedder};
use crate::error::Result;
use crate::index::{HnswConfig, HnswIndex};
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
}

impl Default for DenseSearchConfig {
    fn default() -> Self {
        Self {
            hnsw: HnswConfig::default(),
            persist: true,
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
    /// Create a new dense searcher with ONNX embedder
    pub fn new(config: DenseSearchConfig) -> Result<Self> {
        let embedder = Arc::new(OnnxEmbedder::new()?);

        Ok(Self {
            embedder,
            indices: RwLock::new(HashMap::new()),
            base_path: None,
            config,
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
        let query_embedding = self.embedder.embed_query(query).await?;
        let index = self.get_or_create_index(tenant_id)?;

        let results = index.search(&query_embedding, k)?;

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
            "dense search completed"
        );

        Ok(dense_results)
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
