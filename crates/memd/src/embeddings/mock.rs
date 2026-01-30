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
