//! Mock embedder for testing
//!
//! Produces deterministic embeddings based on text hash.
//! Enables testing without model downloads or GPU.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::traits::{Embedder, EmbeddingConfig, EmbeddingResult};
use crate::error::Result;

/// Mock embedder for testing
///
/// Generates deterministic embeddings by hashing input text.
/// Similar texts will NOT produce similar embeddings (unlike real models).
/// Use for testing index mechanics, not retrieval quality.
pub struct MockEmbedder {
    config: EmbeddingConfig,
}

impl MockEmbedder {
    /// Create a new mock embedder with default config
    pub fn new() -> Self {
        Self::with_config(EmbeddingConfig::default())
    }

    /// Create with custom config
    pub fn with_config(config: EmbeddingConfig) -> Self {
        Self { config }
    }

    /// Generate deterministic embedding from text
    ///
    /// Uses hash-based pseudo-random values for reproducibility.
    fn generate_embedding(&self, text: &str) -> EmbeddingResult {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let seed = hasher.finish();

        let mut embedding = Vec::with_capacity(self.config.dimension);

        // Generate pseudo-random values based on seed
        for i in 0..self.config.dimension {
            let mut h = DefaultHasher::new();
            (seed, i).hash(&mut h);
            let val = h.finish();
            // Map to [-1, 1] range
            let float_val = (val as f64 / u64::MAX as f64) * 2.0 - 1.0;
            embedding.push(float_val as f32);
        }

        // Normalize if configured
        if self.config.normalize {
            let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in embedding.iter_mut() {
                    *x /= norm;
                }
            }
        }

        embedding
    }
}

impl Default for MockEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Embedder for MockEmbedder {
    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        Ok(texts.iter().map(|t| self.generate_embedding(t)).collect())
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingResult> {
        Ok(self.generate_embedding(query))
    }

    fn dimension(&self) -> usize {
        self.config.dimension
    }

    fn config(&self) -> &EmbeddingConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_deterministic() {
        let embedder = MockEmbedder::new();

        let emb1 = embedder.embed_query("hello world").await.unwrap();
        let emb2 = embedder.embed_query("hello world").await.unwrap();

        // Same text should produce same embedding
        assert_eq!(emb1, emb2);
    }

    #[tokio::test]
    async fn test_mock_different_texts() {
        let embedder = MockEmbedder::new();

        let emb1 = embedder.embed_query("hello").await.unwrap();
        let emb2 = embedder.embed_query("world").await.unwrap();

        // Different texts should produce different embeddings
        assert_ne!(emb1, emb2);
    }

    #[tokio::test]
    async fn test_mock_dimension() {
        let embedder = MockEmbedder::new();
        let embedding = embedder.embed_query("test").await.unwrap();

        assert_eq!(embedding.len(), 384);
    }

    #[tokio::test]
    async fn test_mock_normalized() {
        let embedder = MockEmbedder::new();
        let embedding = embedder.embed_query("test").await.unwrap();

        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001, "not normalized: {}", norm);
    }

    #[tokio::test]
    async fn test_mock_batch() {
        let embedder = MockEmbedder::new();
        let texts = vec!["one", "two", "three"];

        let embeddings = embedder.embed_texts(&texts).await.unwrap();

        assert_eq!(embeddings.len(), 3);
        for emb in &embeddings {
            assert_eq!(emb.len(), 384);
        }
    }

    #[test]
    fn test_config_defaults() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.dimension, 1024);  // Qwen3-Embedding-0.6B dimension
        assert!(config.normalize);
        assert_eq!(config.batch_size, 32);
    }

    #[tokio::test]
    async fn test_custom_dimension() {
        let config = EmbeddingConfig {
            dimension: 768,
            normalize: true,
            batch_size: 16,
        };
        let embedder = MockEmbedder::with_config(config);

        let embedding = embedder.embed_query("test").await.unwrap();
        assert_eq!(embedding.len(), 768);
        assert_eq!(embedder.dimension(), 768);
    }

    #[tokio::test]
    async fn test_unnormalized() {
        let config = EmbeddingConfig {
            dimension: 384,
            normalize: false,
            batch_size: 32,
        };
        let embedder = MockEmbedder::with_config(config);

        let embedding = embedder.embed_query("test").await.unwrap();
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();

        // Without normalization, norm should NOT be 1.0
        assert!((norm - 1.0).abs() > 0.1, "unexpectedly normalized: {}", norm);
    }
}
