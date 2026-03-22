//! Candle-based embedding implementation
//!
//! Pure Rust embeddings using Candle framework with model pooling for parallelism.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use hf_hub::{api::sync::Api, Repo, RepoType};
use parking_lot::Mutex;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};
use tokio::sync::Semaphore;

use crate::embeddings::traits::{Embedder, EmbeddingConfig, EmbeddingResult, PoolingStrategy};
use crate::error::{MemdError, Result};

const DEFAULT_MODEL: &str = "sentence-transformers/all-MiniLM-L6-v2";
const DEFAULT_DIMENSION: usize = 384;
const DEFAULT_MAX_LENGTH: usize = 512;
const MODEL_POOL_SIZE: usize = 4; // 4 models for parallel inference
                                  // MAX_CONCURRENT derived from MODEL_POOL_SIZE to avoid duplication
const MAX_CONCURRENT: usize = MODEL_POOL_SIZE;

// Global counter for round-robin model selection
static MODEL_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Standalone mean pooling function
fn mean_pool(embeddings: &Tensor, attention_mask: &Tensor) -> Result<Tensor> {
    // embeddings: [batch, seq_len, hidden]
    // attention_mask: [batch, seq_len]

    // Expand attention_mask to match embeddings dimensions [batch, seq_len, hidden]
    // Note: Candle requires explicit expansion, doesn't auto-broadcast in mul()
    let mask = attention_mask
        .unsqueeze(2)?
        .expand(embeddings.shape())?
        .to_dtype(embeddings.dtype())?;

    // Multiply embeddings by attention mask
    let masked_embeddings = embeddings.mul(&mask)?;

    // Sum across sequence dimension
    let sum_embeddings = masked_embeddings.sum(1)?;

    // Count valid tokens per sequence
    let sum_mask = mask.sum(1)?;

    // Avoid division by zero
    let sum_mask = sum_mask.clamp(1e-9, f32::MAX as f64)?;

    // Divide to get mean
    let mean = sum_embeddings.broadcast_div(&sum_mask)?;

    Ok(mean)
}

/// Standalone normalization function (L2 normalize)
fn normalize(tensor: &Tensor) -> Result<Tensor> {
    // Compute L2 norm along last dimension
    let norm = tensor
        .sqr()?
        .sum_keepdim(1)?
        .sqrt()?
        .clamp(1e-12, f32::MAX as f64)?;

    // Divide by norm
    let normalized = tensor.broadcast_div(&norm)?;

    Ok(normalized)
}

/// Candle-based embedder with model pooling
#[derive(Clone)]
pub struct CandleEmbedder {
    /// Pool of BERT models for parallel inference
    model_pool: Vec<Arc<Mutex<BertModel>>>,
    /// Shared tokenizer (thread-safe)
    tokenizer: Arc<Tokenizer>,
    /// Semaphore to limit concurrent inference
    semaphore: Arc<Semaphore>,
    /// Shared device for computation (Arc for thread-safe cloning)
    device: Arc<Device>,
    /// Embedding dimension
    dimension: usize,
    /// Embedding configuration
    config: EmbeddingConfig,
}

impl CandleEmbedder {
    /// Create a new CandleEmbedder with default model (all-MiniLM-L6-v2)
    pub fn new() -> Result<Self> {
        Self::with_config(EmbeddingConfig::default())
    }

    /// Create with custom configuration
    pub fn with_config(config: EmbeddingConfig) -> Result<Self> {
        tracing::info!(
            model = DEFAULT_MODEL,
            dimension = DEFAULT_DIMENSION,
            "initializing Candle embedder"
        );

        // Safe device selection with CPU fallback
        let device = match Device::cuda_if_available(0) {
            Ok(dev) => {
                tracing::info!("using CUDA device for embeddings");
                dev
            }
            Err(e) => {
                tracing::warn!(error = %e, "CUDA not available, falling back to CPU");
                Device::Cpu
            }
        };

        // Download model files from Hugging Face Hub
        let api = Api::new().map_err(|e| {
            MemdError::EmbeddingError(format!("Failed to initialize HF API: {}", e))
        })?;

        let repo = api.repo(Repo::new(DEFAULT_MODEL.to_string(), RepoType::Model));

        let config_path = repo
            .get("config.json")
            .map_err(|e| MemdError::EmbeddingError(format!("Failed to download config: {}", e)))?;

        let tokenizer_path = repo.get("tokenizer.json").map_err(|e| {
            MemdError::EmbeddingError(format!("Failed to download tokenizer: {}", e))
        })?;

        let weights_path = repo
            .get("model.safetensors")
            .map_err(|e| MemdError::EmbeddingError(format!("Failed to download weights: {}", e)))?;

        // Load and validate model config
        let config_str = std::fs::read_to_string(&config_path)
            .map_err(|e| MemdError::ConfigError(format!("Failed to read config: {}", e)))?;

        let bert_config: Config = serde_json::from_str(&config_str)
            .map_err(|e| MemdError::ConfigError(format!("Failed to parse config: {}", e)))?;

        // Validate model architecture
        if bert_config.model_type.as_deref() != Some("bert") {
            return Err(MemdError::ConfigError(format!(
                "Only BERT models supported, got: {:?}",
                bert_config.model_type
            )));
        }

        tracing::info!(
            hidden_size = bert_config.hidden_size,
            num_layers = bert_config.num_hidden_layers,
            "loaded BERT config"
        );

        // Load tokenizer with proper truncation
        let mut tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| MemdError::EmbeddingError(format!("Failed to load tokenizer: {}", e)))?;

        // Set truncation
        if let Err(e) = tokenizer.with_truncation(Some(TruncationParams {
            max_length: DEFAULT_MAX_LENGTH,
            ..Default::default()
        })) {
            tracing::warn!(error = ?e, "Failed to set truncation, using defaults");
        }

        // Set padding (note: with_padding returns &mut Self, not Result)
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));

        // Wrap device in Arc for thread-safe sharing
        let device = Arc::new(device);

        // Create model pool (load same model multiple times for parallelism)
        let mut model_pool = Vec::with_capacity(MODEL_POOL_SIZE);

        for i in 0..MODEL_POOL_SIZE {
            // SAFETY: Memory-mapped safetensors are read-only and immutable.
            // The mmap is valid for the lifetime of the loaded weights file.
            // Multiple models can safely share the same mmaped region.
            let vb = unsafe {
                VarBuilder::from_mmaped_safetensors(&[weights_path.clone()], DTYPE, &device)
                    .map_err(|e| {
                        MemdError::EmbeddingError(format!("Failed to load model {}: {}", i, e))
                    })?
            };

            let model = BertModel::load(vb, &bert_config).map_err(|e| {
                MemdError::EmbeddingError(format!("Failed to create BERT model {}: {}", i, e))
            })?;

            model_pool.push(Arc::new(Mutex::new(model)));

            tracing::debug!(model_id = i, "loaded model into pool");
        }

        let dimension = bert_config.hidden_size;

        tracing::info!(
            pool_size = MODEL_POOL_SIZE,
            dimension = dimension,
            "Candle embedder initialized successfully"
        );

        Ok(Self {
            model_pool,
            tokenizer: Arc::new(tokenizer),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT)),
            device,
            dimension,
            config,
        })
    }

    /// Embed a single text (internal implementation)
    async fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        let texts = vec![text];
        let mut embeddings = self.embed_batch(&texts).await?;
        embeddings
            .pop()
            .ok_or_else(|| MemdError::EmbeddingError("Failed to get embedding".to_string()))
    }

    /// Embed a batch of texts (internal implementation)
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Acquire owned semaphore permit for concurrency control
        // Using acquire_owned() returns OwnedSemaphorePermit which is 'static
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| MemdError::EmbeddingError(format!("Semaphore error: {}", e)))?;

        // Clone all needed values from self before entering spawn_blocking
        // This ensures no references to self remain in the closure
        let model_idx = MODEL_COUNTER.fetch_add(1, Ordering::Relaxed) % MODEL_POOL_SIZE;
        let model: Arc<Mutex<BertModel>> = Arc::clone(&self.model_pool[model_idx]);
        let tokenizer = Arc::clone(&self.tokenizer);

        // Clone Arc<Device> for thread-safe sharing (no device recreation)
        let device = Arc::clone(&self.device);
        let pooling_strategy = self.config.pooling;
        let should_normalize = self.config.normalize;

        // Convert to owned strings for 'static lifetime in spawn_blocking
        let texts_owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();

        // Run inference in blocking task to avoid blocking async runtime
        let embeddings = tokio::task::spawn_blocking(move || {
            let _permit = permit; // Hold permit until task completes

            // Convert back to &str for tokenizer
            let texts_refs: Vec<&str> = texts_owned.iter().map(|s| s.as_str()).collect();

            // Tokenize
            let encodings = tokenizer
                .encode_batch(texts_refs, true)
                .map_err(|e| MemdError::EmbeddingError(format!("Tokenization failed: {}", e)))?;

            // Convert to tensors
            let token_ids: Vec<Vec<u32>> = encodings.iter().map(|e| e.get_ids().to_vec()).collect();

            let attention_mask: Vec<Vec<u32>> = encodings
                .iter()
                .map(|e| e.get_attention_mask().to_vec())
                .collect();

            let token_ids_tensor = Tensor::new(token_ids, &*device).map_err(|e| {
                MemdError::EmbeddingError(format!("Failed to create token tensor: {}", e))
            })?;

            let attention_mask_tensor = Tensor::new(attention_mask, &*device).map_err(|e| {
                MemdError::EmbeddingError(format!("Failed to create mask tensor: {}", e))
            })?;

            // Run model inference
            let model = model.lock();
            let outputs = model
                .forward(&token_ids_tensor, &attention_mask_tensor, None)
                .map_err(|e| MemdError::EmbeddingError(format!("Model forward failed: {}", e)))?;

            // Apply pooling strategy
            let pooled = match pooling_strategy {
                PoolingStrategy::Mean => mean_pool(&outputs, &attention_mask_tensor)?,
                PoolingStrategy::Cls => {
                    // Take CLS token (first token) - select all batches, first sequence position
                    outputs.narrow(1, 0, 1)?.squeeze(1)?
                }
                PoolingStrategy::LastToken => {
                    // Take last non-padding token for each sequence
                    let batch_size = outputs.dim(0)?;
                    let mut last_outputs = Vec::with_capacity(batch_size);

                    for i in 0..batch_size {
                        // Get attention mask for this batch item
                        let mask = attention_mask_tensor.narrow(0, i, 1)?.squeeze(0)?;
                        let mask_vec = mask.to_vec1::<u32>()?;

                        // Find last non-zero position in attention mask
                        let last_pos = mask_vec.iter().rposition(|&x| x != 0).unwrap_or(0);

                        // Extract the token at last_pos for batch item i
                        let batch_item = outputs.narrow(0, i, 1)?;
                        let last_token =
                            batch_item.narrow(1, last_pos, 1)?.squeeze(1)?.squeeze(0)?;
                        last_outputs.push(last_token);
                    }

                    Tensor::stack(&last_outputs, 0)?
                }
            };

            // Normalize embeddings only if configured to do so
            let final_embeddings = if should_normalize {
                normalize(&pooled)?
            } else {
                pooled
            };

            // Convert to Vec<Vec<f32>>
            let embeddings_2d = final_embeddings.to_vec2::<f32>().map_err(|e| {
                MemdError::EmbeddingError(format!("Failed to convert to vec: {}", e))
            })?;

            Ok::<Vec<Vec<f32>>, MemdError>(embeddings_2d)
        })
        .await
        .map_err(|e| MemdError::EmbeddingError(format!("Task join error: {}", e)))??;

        Ok(embeddings)
    }
}

#[async_trait]
impl Embedder for CandleEmbedder {
    async fn embed_query(&self, query: &str) -> Result<EmbeddingResult> {
        self.embed_single(query).await
    }

    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Split into batches and process chunks in parallel to use the model pool.
        let batch_size = self.config.batch_size.max(1);
        let chunk_count = texts.chunks(batch_size).len();
        let mut tasks = tokio::task::JoinSet::new();

        for (chunk_idx, chunk) in texts.chunks(batch_size).enumerate() {
            let embedder = self.clone();
            let chunk_texts: Vec<String> = chunk.iter().map(|text| (*text).to_string()).collect();

            tasks.spawn(async move {
                let chunk_refs: Vec<&str> = chunk_texts.iter().map(|text| text.as_str()).collect();
                let embeddings = embedder.embed_batch(&chunk_refs).await?;
                Ok::<(usize, Vec<Vec<f32>>), MemdError>((chunk_idx, embeddings))
            });
        }

        let mut ordered_batches = vec![None; chunk_count];
        while let Some(task_result) = tasks.join_next().await {
            let (chunk_idx, embeddings) = task_result
                .map_err(|e| MemdError::EmbeddingError(format!("Task join error: {}", e)))??;
            ordered_batches[chunk_idx] = Some(embeddings);
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());
        for batch in ordered_batches {
            let mut embeddings = batch.ok_or_else(|| {
                MemdError::EmbeddingError("missing batch embeddings from worker".into())
            })?;
            all_embeddings.append(&mut embeddings);
        }

        Ok(all_embeddings)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn config(&self) -> &EmbeddingConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires model download and network access"]
    async fn test_embed_basic() {
        let embedder = CandleEmbedder::new().unwrap();
        let embeddings = embedder.embed_texts(&["hello world"]).await.unwrap();
        assert_eq!(embeddings.len(), 1);
        assert_eq!(embeddings[0].len(), 384);

        // Verify normalized (L2 norm should be ~1.0)
        let norm: f32 = embeddings[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01, "embedding should be normalized");
    }

    #[tokio::test]
    #[ignore = "requires model download and network access"]
    async fn test_embed_batch() {
        let embedder = CandleEmbedder::new().unwrap();
        let texts = vec!["hello world", "test embedding", "another example"];
        let embeddings = embedder.embed_texts(&texts).await.unwrap();

        assert_eq!(embeddings.len(), 3);
        for emb in &embeddings {
            assert_eq!(emb.len(), 384);
            let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 0.01, "embedding should be normalized");
        }
    }

    #[tokio::test]
    #[ignore = "requires model download and network access"]
    async fn test_embed_concurrency() {
        let embedder = Arc::new(CandleEmbedder::new().unwrap());
        let mut handles = vec![];

        for i in 0..20 {
            let e = Arc::clone(&embedder);
            handles.push(tokio::spawn(async move {
                e.embed_texts(&[&format!("text {}", i)]).await
            }));
        }

        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    #[ignore = "requires model download and network access"]
    async fn test_semantic_similarity() {
        let embedder = CandleEmbedder::new().unwrap();

        let texts = vec!["the cat sat on the mat", "a feline rested on a rug"];
        let embeddings = embedder.embed_texts(&texts).await.unwrap();

        // Compute cosine similarity
        let dot_product: f32 = embeddings[0]
            .iter()
            .zip(&embeddings[1])
            .map(|(a, b)| a * b)
            .sum();

        // Similar sentences should have high similarity
        assert!(
            dot_product > 0.6,
            "similar sentences should have high similarity, got {}",
            dot_product
        );
    }
}
