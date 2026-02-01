//! ONNX-based embedder using sentence transformers
//!
//! Supports multiple embedding models with different pooling strategies:
//! - all-MiniLM-L6-v2: 384-dim, mean pooling
//! - Qwen3-Embedding-0.6B: 1024-dim, last-token pooling

use std::sync::Arc;

use ndarray::{Array2, Array4, ArrayViewD, Axis};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorRef};
use parking_lot::Mutex;
use tokenizers::Tokenizer;

use super::download::{get_model_path_for, get_tokenizer_path_for, EmbeddingModel};
use super::traits::{Embedder, EmbeddingConfig, EmbeddingResult, PoolingStrategy};
use crate::error::{MemdError, Result};

/// Default task instruction for retrieval queries (Qwen3 models)
const RETRIEVAL_INSTRUCTION: &str =
    "Given a search query, retrieve relevant code snippets and documentation";

/// ONNX-based embedder using sentence transformers
pub struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Arc<Mutex<Tokenizer>>,
    config: EmbeddingConfig,
    model: EmbeddingModel,
}

impl OnnxEmbedder {
    /// Create a new ONNX embedder with default model (all-MiniLM-L6-v2)
    ///
    /// Downloads model on first use to ~/.cache/memd/models/
    pub fn new() -> Result<Self> {
        Self::with_model(EmbeddingModel::default())
    }

    /// Create embedder with specific model
    pub fn with_model(model: EmbeddingModel) -> Result<Self> {
        let config = EmbeddingConfig {
            dimension: model.dimension(),
            pooling: model.pooling_strategy(),
            ..Default::default()
        };
        Self::with_model_and_config(model, config)
    }

    /// Create with specific model and custom config overrides
    pub fn with_model_and_config(model: EmbeddingModel, config: EmbeddingConfig) -> Result<Self> {
        let model_path = get_model_path_for(model)?;
        let tokenizer_path = get_tokenizer_path_for(model)?;

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

        tracing::info!(
            model = ?model,
            dimension = config.dimension,
            pooling = ?config.pooling,
            "ONNX embedder initialized"
        );

        Ok(Self {
            session: Mutex::new(session),
            tokenizer: Arc::new(Mutex::new(tokenizer)),
            config,
            model,
        })
    }

    /// Backward compatibility: with_config defaults to default model
    pub fn with_config(config: EmbeddingConfig) -> Result<Self> {
        Self::with_model_and_config(EmbeddingModel::default(), config)
    }

    /// Get the embedding model in use
    pub fn model(&self) -> EmbeddingModel {
        self.model
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

    /// Mean pooling: average all token embeddings weighted by attention mask
    ///
    /// For encoder models (BERT-style, all-MiniLM-L6-v2), averages token embeddings
    /// where attention_mask=1 to produce sentence embeddings.
    fn mean_pooling(
        &self,
        hidden_states: ArrayViewD<f32>,
        attention_mask: &Array2<i64>,
    ) -> Array2<f32> {
        let batch_size = hidden_states.shape()[0];
        let seq_len = hidden_states.shape()[1];
        let hidden_size = hidden_states.shape()[2];

        let mut embeddings = Array2::<f32>::zeros((batch_size, hidden_size));

        for b in 0..batch_size {
            let mut sum = vec![0.0f32; hidden_size];
            let mut count = 0.0f32;

            for s in 0..seq_len {
                if attention_mask[[b, s]] == 1 {
                    for h in 0..hidden_size {
                        sum[h] += hidden_states[[b, s, h]];
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

        embeddings
    }

    /// Last-token pooling: extract embedding at final attended position
    ///
    /// For decoder models (Qwen3, E5-Mistral), the final token aggregates
    /// sequence information. Handles both left-padding and right-padding.
    fn last_token_pooling(
        &self,
        hidden_states: ArrayViewD<f32>, // [batch, seq_len, hidden_size]
        attention_mask: &Array2<i64>,   // [batch, seq_len]
    ) -> Array2<f32> {
        let batch_size = hidden_states.shape()[0];
        let seq_len = hidden_states.shape()[1];
        let hidden_size = hidden_states.shape()[2];

        let mut embeddings = Array2::<f32>::zeros((batch_size, hidden_size));

        // Check for left-padding: all batch items have mask=1 at last position
        let all_attended_at_end = (0..batch_size)
            .all(|b| attention_mask[[b, seq_len - 1]] == 1);

        if all_attended_at_end {
            // Left-padding case: last position is always the token we want
            for b in 0..batch_size {
                for h in 0..hidden_size {
                    embeddings[[b, h]] = hidden_states[[b, seq_len - 1, h]];
                }
            }
        } else {
            // Right-padding case: find last attended position per batch item
            for b in 0..batch_size {
                // Sum attention mask to get sequence length
                let seq_length: i64 = attention_mask.row(b).iter().sum();
                let last_pos = (seq_length - 1).max(0) as usize;

                for h in 0..hidden_size {
                    embeddings[[b, h]] = hidden_states[[b, last_pos, h]];
                }
            }
        }

        embeddings
    }

    /// Format query with instruction prefix for models that require it
    fn format_query(&self, query: &str) -> String {
        if self.model.uses_instruction_format() {
            format!("Instruct: {}\nQuery: {}", RETRIEVAL_INSTRUCTION, query)
        } else {
            query.to_string()
        }
    }

    /// Generate position IDs for decoder models
    ///
    /// Position IDs are simply [0, 1, 2, ..., seq_len-1] for each sequence in the batch.
    fn generate_position_ids(&self, batch_size: usize, seq_len: usize) -> Array2<i64> {
        let mut position_ids = Array2::<i64>::zeros((batch_size, seq_len));
        for b in 0..batch_size {
            for s in 0..seq_len {
                position_ids[[b, s]] = s as i64;
            }
        }
        position_ids
    }

    /// Run inference with KV-cache for decoder models (Qwen3)
    ///
    /// Decoder models require empty past_key_values tensors for the first forward pass.
    /// This helper properly handles the complex input structure and returns the hidden states.
    fn run_with_kv_cache(
        &self,
        session: &mut Session,
        input_ids: &Array2<i64>,
        attention_mask: &Array2<i64>,
        position_ids: &Array2<i64>,
        kv_arrays: Vec<Array4<f32>>,
        num_layers: usize,
    ) -> Result<ndarray::ArrayD<f32>> {
        // Create owned tensors for the core inputs
        let input_ids_tensor = Tensor::from_array(input_ids.clone())
            .map_err(|e| MemdError::StorageError(format!("failed to create input_ids tensor: {}", e)))?;
        let attention_mask_tensor = Tensor::from_array(attention_mask.clone())
            .map_err(|e| MemdError::StorageError(format!("failed to create attention_mask tensor: {}", e)))?;
        let position_ids_tensor = Tensor::from_array(position_ids.clone())
            .map_err(|e| MemdError::StorageError(format!("failed to create position_ids tensor: {}", e)))?;

        // Create owned KV tensors - consuming the arrays
        let mut kv_tensors: Vec<Tensor<f32>> = Vec::with_capacity(num_layers * 2);
        for kv_array in kv_arrays.into_iter() {
            let tensor = Tensor::from_array(kv_array)
                .map_err(|e| MemdError::StorageError(format!("failed to create KV tensor: {}", e)))?;
            kv_tensors.push(tensor);
        }

        // Build inputs vector using owned tensors
        let mut inputs: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = Vec::new();
        inputs.push(("input_ids".into(), SessionInputValue::from(&input_ids_tensor)));
        inputs.push(("attention_mask".into(), SessionInputValue::from(&attention_mask_tensor)));
        inputs.push(("position_ids".into(), SessionInputValue::from(&position_ids_tensor)));

        // Add KV cache tensors with proper naming
        for layer in 0..num_layers {
            let key_idx = layer * 2;
            let value_idx = layer * 2 + 1;
            inputs.push((
                format!("past_key_values.{}.key", layer).into(),
                SessionInputValue::from(&kv_tensors[key_idx]),
            ));
            inputs.push((
                format!("past_key_values.{}.value", layer).into(),
                SessionInputValue::from(&kv_tensors[value_idx]),
            ));
        }

        let outputs = session
            .run(inputs)
            .map_err(|e| MemdError::StorageError(format!("inference failed: {}", e)))?;

        // Extract last_hidden_state from outputs
        let output_array = if let Some(output) = outputs.get("last_hidden_state") {
            output
                .try_extract_array::<f32>()
                .map_err(|e| MemdError::StorageError(format!("failed to extract tensor: {}", e)))?
        } else {
            outputs[0]
                .try_extract_array::<f32>()
                .map_err(|e| MemdError::StorageError(format!("failed to extract tensor: {}", e)))?
        };

        Ok(output_array.into_owned())
    }

    /// Run inference on tokenized inputs
    fn run_inference(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        let (input_ids, attention_mask, token_type_ids) = self.tokenize(texts)?;

        let mut session = self.session.lock();

        // For BERT-style models (all-MiniLM), use token_type_ids
        // For decoder models (Qwen3), use position_ids and empty KV-cache
        let output_array = if let Some(kv_config) = self.model.kv_cache_config() {
            // Qwen3-style: requires position_ids and empty past_key_values
            let batch_size = input_ids.shape()[0];
            let seq_len = input_ids.shape()[1];
            let position_ids = self.generate_position_ids(batch_size, seq_len);

            // Create empty KV-cache tensors: shape (batch, num_kv_heads, 0, head_dim)
            // Using seq_len=0 means no cached keys/values (fresh forward pass)
            // We create one array per layer pair (key+value share the same shape)
            let empty_kv_shape = (batch_size, kv_config.num_kv_heads, 0, kv_config.head_dim);
            let mut kv_arrays: Vec<Array4<f32>> = Vec::with_capacity(kv_config.num_layers * 2);
            for _ in 0..kv_config.num_layers * 2 {
                kv_arrays.push(Array4::<f32>::zeros(empty_kv_shape));
            }

            // Run with KV cache - returns hidden states directly
            self.run_with_kv_cache(
                &mut session,
                &input_ids,
                &attention_mask,
                &position_ids,
                kv_arrays,
                kv_config.num_layers,
            )?
        } else {
            // BERT-style: requires token_type_ids, no position_ids
            let input_ids_tensor = TensorRef::from_array_view(input_ids.view())
                .map_err(|e| MemdError::StorageError(format!("failed to create input_ids tensor: {}", e)))?;
            let attention_mask_tensor = TensorRef::from_array_view(attention_mask.view())
                .map_err(|e| MemdError::StorageError(format!("failed to create attention_mask tensor: {}", e)))?;
            let token_type_ids_tensor = TensorRef::from_array_view(token_type_ids.view())
                .map_err(|e| MemdError::StorageError(format!("failed to create token_type_ids tensor: {}", e)))?;

            let outputs = session
                .run(ort::inputs![
                    "input_ids" => input_ids_tensor,
                    "attention_mask" => attention_mask_tensor,
                    "token_type_ids" => token_type_ids_tensor,
                ])
                .map_err(|e| MemdError::StorageError(format!("inference failed: {}", e)))?;

            // Get the first output (last_hidden_state) - shape: [batch, seq_len, hidden_size]
            // Try by name first, then fall back to index 0
            let arr = if let Some(output) = outputs.get("last_hidden_state") {
                output
                    .try_extract_array::<f32>()
                    .map_err(|e| MemdError::StorageError(format!("failed to extract tensor: {}", e)))?
            } else {
                outputs[0]
                    .try_extract_array::<f32>()
                    .map_err(|e| MemdError::StorageError(format!("failed to extract tensor: {}", e)))?
            };
            arr.into_owned()
        };

        // Apply pooling based on strategy
        let mut embeddings = match self.config.pooling {
            PoolingStrategy::Mean => self.mean_pooling(output_array.view(), &attention_mask),
            PoolingStrategy::LastToken => self.last_token_pooling(output_array.view(), &attention_mask),
        };

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
        // Process in batches using the original run_inference method
        let mut all_results = Vec::with_capacity(texts.len());

        for chunk in texts.chunks(self.config.batch_size) {
            let results = self.run_inference(chunk)?;
            all_results.extend(results);
        }

        Ok(all_results)
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingResult> {
        let formatted = self.format_query(query);
        let results = self.run_inference(&[&formatted])?;
        results
            .into_iter()
            .next()
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

        let embedding = embedder
            .embed_query("Hello, world!")
            .await
            .expect("embed failed");

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
        let embeddings = embedder
            .embed_texts(&texts.iter().map(|s| *s).collect::<Vec<_>>())
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
        let emb2 = embedder
            .embed_query("A cat was sitting on a mat")
            .await
            .unwrap();
        let emb3 = embedder
            .embed_query("Python programming language")
            .await
            .unwrap();

        // Cosine similarity (vectors are normalized, so dot product = cosine)
        let sim_12: f32 = emb1.iter().zip(&emb2).map(|(a, b)| a * b).sum();
        let sim_13: f32 = emb1.iter().zip(&emb3).map(|(a, b)| a * b).sum();

        // Similar sentences should have higher similarity
        assert!(
            sim_12 > sim_13,
            "similar texts should have higher similarity: {} vs {}",
            sim_12,
            sim_13
        );
        assert!(
            sim_12 > 0.7,
            "similar texts should have similarity > 0.7: {}",
            sim_12
        );
    }

    #[test]
    fn test_config_defaults() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.dimension, 384);
        assert!(config.normalize);
        assert_eq!(config.batch_size, 32);
        assert_eq!(config.pooling, PoolingStrategy::Mean);
    }

    #[test]
    fn test_model_defaults() {
        let model = EmbeddingModel::default();
        assert_eq!(model, EmbeddingModel::AllMiniLmL6V2);
        assert_eq!(model.dimension(), 384);
        assert_eq!(model.pooling_strategy(), PoolingStrategy::Mean);
        assert!(!model.uses_instruction_format());
    }

    #[test]
    fn test_qwen3_model_config() {
        let model = EmbeddingModel::Qwen3Embedding0_6B;
        assert_eq!(model.dimension(), 1024);
        assert_eq!(model.pooling_strategy(), PoolingStrategy::LastToken);
        assert!(model.uses_instruction_format());
    }

    #[test]
    fn test_query_formatting_default_model() {
        // For all-MiniLM-L6-v2, queries should not be formatted
        let model = EmbeddingModel::AllMiniLmL6V2;
        assert!(!model.uses_instruction_format());
    }

    #[test]
    fn test_query_formatting_qwen3() {
        // For Qwen3, queries should use instruction format
        let model = EmbeddingModel::Qwen3Embedding0_6B;
        assert!(model.uses_instruction_format());
    }

    #[tokio::test]
    #[ignore = "requires model download (~600MB)"]
    async fn test_qwen3_embed_single_text() {
        let embedder = OnnxEmbedder::with_model(EmbeddingModel::Qwen3Embedding0_6B)
            .expect("failed to create embedder");

        let embedding = embedder
            .embed_query("Hello, world!")
            .await
            .expect("embed failed");

        // Qwen3 produces 1024-dim embeddings
        assert_eq!(embedding.len(), 1024);

        // Check normalized (unit length)
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01, "embedding not normalized: {}", norm);
    }

    #[tokio::test]
    #[ignore = "requires model download (~600MB)"]
    async fn test_qwen3_embed_batch() {
        let embedder = OnnxEmbedder::with_model(EmbeddingModel::Qwen3Embedding0_6B)
            .expect("failed to create embedder");

        let texts = vec!["Hello", "World", "Test"];
        let embeddings = embedder
            .embed_texts(&texts.iter().map(|s| *s).collect::<Vec<_>>())
            .await
            .expect("batch embed failed");

        assert_eq!(embeddings.len(), 3);
        for emb in &embeddings {
            assert_eq!(emb.len(), 1024);
        }
    }
}
