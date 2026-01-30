//! ONNX-based embedder using sentence transformers
//!
//! Uses all-MiniLM-L6-v2 quantized model for 384-dim embeddings.

use std::sync::Arc;

use ndarray::{Array2, Axis};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use parking_lot::Mutex;
use tokenizers::Tokenizer;

use super::download::{get_model_path, get_tokenizer_path};
use super::traits::{Embedder, EmbeddingConfig, EmbeddingResult};
use crate::error::{MemdError, Result};

/// ONNX-based embedder using sentence transformers
pub struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Arc<Mutex<Tokenizer>>,
    config: EmbeddingConfig,
}

impl OnnxEmbedder {
    /// Create a new ONNX embedder
    ///
    /// Downloads model on first use to ~/.cache/memd/models/
    pub fn new() -> Result<Self> {
        Self::with_config(EmbeddingConfig::default())
    }

    /// Create with custom config
    pub fn with_config(config: EmbeddingConfig) -> Result<Self> {
        let model_path = get_model_path()?;
        let tokenizer_path = get_tokenizer_path()?;

        let session = Session::builder()
            .map_err(|e| MemdError::StorageError(format!("failed to create ONNX session: {}", e)))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| MemdError::StorageError(format!("failed to set optimization: {}", e)))?
            .with_intra_threads(4)
            .map_err(|e| MemdError::StorageError(format!("failed to set threads: {}", e)))?
            .commit_from_file(&model_path)
            .map_err(|e| MemdError::StorageError(format!("failed to load model: {}", e)))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| MemdError::StorageError(format!("failed to load tokenizer: {}", e)))?;

        Ok(Self {
            session: Mutex::new(session),
            tokenizer: Arc::new(Mutex::new(tokenizer)),
            config,
        })
    }

    /// Tokenize texts and prepare model inputs
    fn tokenize(&self, texts: &[&str]) -> Result<(Array2<i64>, Array2<i64>, Array2<i64>)> {
        let tokenizer = self.tokenizer.lock();

        let encodings = tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| MemdError::StorageError(format!("tokenization failed: {}", e)))?;

        let max_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
        let batch_size = texts.len();

        let mut input_ids = Array2::<i64>::zeros((batch_size, max_len));
        let mut attention_mask = Array2::<i64>::zeros((batch_size, max_len));
        let mut token_type_ids = Array2::<i64>::zeros((batch_size, max_len));

        for (i, encoding) in encodings.iter().enumerate() {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let types = encoding.get_type_ids();

            for (j, &id) in ids.iter().enumerate() {
                input_ids[[i, j]] = id as i64;
                attention_mask[[i, j]] = mask[j] as i64;
                token_type_ids[[i, j]] = types[j] as i64;
            }
        }

        Ok((input_ids, attention_mask, token_type_ids))
    }

    /// Normalize embeddings to unit length
    fn normalize(&self, embeddings: &mut Array2<f32>) {
        for mut row in embeddings.axis_iter_mut(Axis(0)) {
            let norm: f32 = row.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                row.mapv_inplace(|x| x / norm);
            }
        }
    }

    /// Run inference on tokenized inputs
    fn run_inference(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        let (input_ids, attention_mask, token_type_ids) = self.tokenize(texts)?;

        // Create TensorRef from ndarray views
        let input_ids_tensor = TensorRef::from_array_view(input_ids.view())
            .map_err(|e| MemdError::StorageError(format!("failed to create input_ids tensor: {}", e)))?;
        let attention_mask_tensor = TensorRef::from_array_view(attention_mask.view())
            .map_err(|e| MemdError::StorageError(format!("failed to create attention_mask tensor: {}", e)))?;
        let token_type_ids_tensor = TensorRef::from_array_view(token_type_ids.view())
            .map_err(|e| MemdError::StorageError(format!("failed to create token_type_ids tensor: {}", e)))?;

        let mut session = self.session.lock();
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])
            .map_err(|e| MemdError::StorageError(format!("inference failed: {}", e)))?;

        // Get the first output (last_hidden_state) - shape: [batch, seq_len, hidden_size]
        // Try by name first, then fall back to index 0
        let output_array = if let Some(output) = outputs.get("last_hidden_state") {
            output
                .try_extract_array::<f32>()
                .map_err(|e| MemdError::StorageError(format!("failed to extract tensor: {}", e)))?
        } else {
            outputs[0]
                .try_extract_array::<f32>()
                .map_err(|e| MemdError::StorageError(format!("failed to extract tensor: {}", e)))?
        };

        let shape = output_array.shape();
        let batch_size = shape[0];
        let seq_len = shape[1];
        let hidden_size = shape[2];

        // Mean pooling with attention mask
        // For each sample, sum token embeddings where attention_mask=1, then divide by count
        let mut embeddings = Array2::<f32>::zeros((batch_size, hidden_size));

        for b in 0..batch_size {
            let mut sum = vec![0.0f32; hidden_size];
            let mut count = 0.0f32;

            for s in 0..seq_len {
                if attention_mask[[b, s]] == 1 {
                    for h in 0..hidden_size {
                        sum[h] += output_array[[b, s, h]];
                    }
                    count += 1.0;
                }
            }

            if count > 0.0 {
                for h in 0..hidden_size {
                    embeddings[[b, h]] = sum[h] / count;
                }
            }
        }

        // Normalize if configured
        if self.config.normalize {
            self.normalize(&mut embeddings);
        }

        // Convert to Vec<Vec<f32>>
        let results: Vec<EmbeddingResult> = embeddings
            .axis_iter(Axis(0))
            .map(|row| row.to_vec())
            .collect();

        Ok(results)
    }
}

#[async_trait::async_trait]
impl Embedder for OnnxEmbedder {
    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        // Process in batches
        let mut all_results = Vec::with_capacity(texts.len());

        for chunk in texts.chunks(self.config.batch_size) {
            let results = self.run_inference(chunk)?;
            all_results.extend(results);
        }

        Ok(all_results)
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingResult> {
        let results = self.run_inference(&[query])?;
        results.into_iter().next()
            .ok_or_else(|| MemdError::StorageError("no embedding result".into()))
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

    // Note: These tests require network access to download model on first run
    // In CI, model should be pre-cached or these tests skipped

    #[tokio::test]
    #[ignore = "requires model download"]
    async fn test_embed_single_text() {
        let embedder = OnnxEmbedder::new().expect("failed to create embedder");

        let embedding = embedder.embed_query("Hello, world!").await.expect("embed failed");

        assert_eq!(embedding.len(), 384);

        // Check normalized (unit length)
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01, "embedding not normalized: {}", norm);
    }

    #[tokio::test]
    #[ignore = "requires model download"]
    async fn test_embed_batch() {
        let embedder = OnnxEmbedder::new().expect("failed to create embedder");

        let texts = vec!["Hello", "World", "Test"];
        let embeddings = embedder.embed_texts(&texts.iter().map(|s| *s).collect::<Vec<_>>())
            .await
            .expect("batch embed failed");

        assert_eq!(embeddings.len(), 3);
        for emb in &embeddings {
            assert_eq!(emb.len(), 384);
        }
    }

    #[tokio::test]
    #[ignore = "requires model download"]
    async fn test_similar_texts_have_high_similarity() {
        let embedder = OnnxEmbedder::new().expect("failed to create embedder");

        let emb1 = embedder.embed_query("The cat sat on the mat").await.unwrap();
        let emb2 = embedder.embed_query("A cat was sitting on a mat").await.unwrap();
        let emb3 = embedder.embed_query("Python programming language").await.unwrap();

        // Cosine similarity (vectors are normalized, so dot product = cosine)
        let sim_12: f32 = emb1.iter().zip(&emb2).map(|(a, b)| a * b).sum();
        let sim_13: f32 = emb1.iter().zip(&emb3).map(|(a, b)| a * b).sum();

        // Similar sentences should have higher similarity
        assert!(sim_12 > sim_13, "similar texts should have higher similarity: {} vs {}", sim_12, sim_13);
        assert!(sim_12 > 0.7, "similar texts should have similarity > 0.7: {}", sim_12);
    }

    #[test]
    fn test_config_defaults() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.dimension, 384);
        assert!(config.normalize);
        assert_eq!(config.batch_size, 32);
    }
}
