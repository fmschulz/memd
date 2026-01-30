//! Embedder trait and configuration
//!
//! Defines the interface for embedding generation.

use crate::error::Result;

/// Configuration for embedding generation
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Embedding dimension (1024 for Qwen3-Embedding-0.6B, was 384 for all-MiniLM-L6-v2)
    pub dimension: usize,
    /// Normalize embeddings to unit length
    pub normalize: bool,
    /// Batch size for processing multiple texts
    pub batch_size: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            dimension: 384,  // all-MiniLM-L6-v2 (TODO: 1024 for Qwen3 when upgraded)
            normalize: true,
            batch_size: 32,
        }
    }
}

/// Result of embedding a text
pub type EmbeddingResult = Vec<f32>;

/// Trait for embedding generation
///
/// Implementations provide dense vector embeddings for text.
/// Used by HNSW index for similarity search.
#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// Embed multiple texts in a batch
    ///
    /// Returns one embedding vector per input text.
    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>>;

    /// Embed a single query text
    ///
    /// May use different preprocessing than embed_texts (e.g., query prefix).
    async fn embed_query(&self, query: &str) -> Result<EmbeddingResult>;

    /// Get the embedding dimension
    fn dimension(&self) -> usize;

    /// Get the configuration
    fn config(&self) -> &EmbeddingConfig;
}
