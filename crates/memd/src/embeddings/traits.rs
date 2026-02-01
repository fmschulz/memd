//! Embedder trait and configuration
//!
//! Defines the interface for embedding generation.

use crate::error::Result;

/// Pooling strategy for sentence embeddings
///
/// Different embedding models require different pooling approaches:
/// - Mean: Average all token embeddings (BERT-style, all-MiniLM-L6-v2)
/// - LastToken: Use final token embedding (decoder-style, Qwen3, E5-Mistral)
/// - Cls: Use CLS token embedding (first token)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PoolingStrategy {
    /// Mean pooling: average all token embeddings weighted by attention mask
    #[default]
    Mean,
    /// Last-token pooling: extract embedding at final attended position
    LastToken,
    /// CLS token pooling: use first token embedding
    Cls,
}

/// Configuration for embedding generation
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Embedding dimension (384 for all-MiniLM-L6-v2, 1024 for Qwen3)
    pub dimension: usize,
    /// Normalize embeddings to unit length
    pub normalize: bool,
    /// Batch size for processing multiple texts
    pub batch_size: usize,
    /// Pooling strategy (determined by model type)
    pub pooling: PoolingStrategy,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            dimension: 384, // all-MiniLM-L6-v2
            normalize: true,
            batch_size: 32,
            pooling: PoolingStrategy::Mean,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pooling_strategy_default() {
        let strategy = PoolingStrategy::default();
        assert_eq!(strategy, PoolingStrategy::Mean);
    }

    #[test]
    fn test_embedding_config_default() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.dimension, 384);
        assert!(config.normalize);
        assert_eq!(config.batch_size, 32);
        assert_eq!(config.pooling, PoolingStrategy::Mean);
    }
}
