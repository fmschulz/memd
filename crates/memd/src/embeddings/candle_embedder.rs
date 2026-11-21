//! Candle-based embedding implementation
//!
//! Pure Rust embeddings using the Candle framework. One copy of the model
//! serves every batch, and it loads on the first embedding call.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use parking_lot::Mutex;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};
use tokio::sync::Semaphore;

use crate::embeddings::download::{get_candle_model_paths, CandleModel};
use crate::embeddings::traits::{Embedder, EmbeddingConfig, EmbeddingResult, PoolingStrategy};
use crate::error::{MemdError, Result};

const DEFAULT_MAX_LENGTH: usize = 512;
/// Most forward passes that run at the same time. Each runs on one thread,
/// the weights are shared, and `BertModel::forward` takes `&self`.
const MAX_INFERENCE_WORKERS: usize = 32;
/// Most texts in one forward pass. With `MAX_INFERENCE_WORKERS` this bounds
/// the activation memory of a large call to 256 texts at a time.
const MAX_FORWARD_ROWS: usize = 8;

/// Forward passes that run at once on this machine.
fn inference_workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |cores| cores.get())
        .min(MAX_INFERENCE_WORKERS)
}

/// Texts per forward pass for a call that embeds `texts` texts on `workers`
/// threads. A pass holds at most `configured` and `MAX_FORWARD_ROWS` texts,
/// and a call is split finely enough that every worker has a pass to run.
fn forward_batch_size(configured: usize, texts: usize, workers: usize) -> usize {
    configured
        .clamp(1, MAX_FORWARD_ROWS)
        .min(texts.div_ceil(workers.max(1)))
        .max(1)
}

/// Run `work` on a fresh pool of one thread and wait for it.
fn on_one_thread<T: Send>(work: impl FnOnce() -> T + Send) -> Result<T> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .thread_name(|_| "memd-embed".to_string())
        .build()
        .map_err(|e| MemdError::EmbeddingError(format!("Failed to start inference thread: {e}")))?;
    Ok(pool.install(work))
}

/// Parse a CUDA ordinal from a `MEMD_EMBED_DEVICE` value such as `cuda:2`,
/// `gpu:1`, or a bare `3`. Falls back to device 0 when no index is present
/// (e.g. plain `cuda`/`gpu`). Input is expected pre-trimmed + lowercased.
fn parse_cuda_index(pref: &str) -> usize {
    pref.rsplit(':')
        .next()
        .and_then(|tail| tail.parse::<usize>().ok())
        .or_else(|| pref.parse::<usize>().ok())
        .unwrap_or(0)
}

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

/// Pick the compute device.
///
/// `MEMD_EMBED_DEVICE` lets a host pin embeddings to a specific device
/// WITHOUT touching the global `CUDA_VISIBLE_DEVICES` (which affects every
/// other GPU program on the box, and which Candle/cudarc does not reliably
/// honor here anyway):
///   cpu              -> always CPU (best on shared/contended GPU hosts,
///                       where grabbing a busy GPU makes embedding, and
///                       the warm-worker HNSW backfill, wildly slow)
///   cuda | gpu       -> CUDA device 0
///   cuda:N | gpu:N | N -> CUDA device N (e.g. an idle GPU)
///   unset | auto     -> historical behavior: CUDA 0 if available, else CPU
fn select_device() -> Device {
    match std::env::var("MEMD_EMBED_DEVICE")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
    {
        Some(pref) if pref == "cpu" => {
            tracing::info!("MEMD_EMBED_DEVICE=cpu — using CPU for embeddings");
            Device::Cpu
        }
        Some(pref) if !pref.is_empty() && pref != "auto" => {
            let idx = parse_cuda_index(&pref);
            match Device::new_cuda(idx) {
                Ok(dev) => {
                    tracing::info!(
                        cuda_index = idx,
                        "MEMD_EMBED_DEVICE — using CUDA device for embeddings"
                    );
                    dev
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        cuda_index = idx,
                        "MEMD_EMBED_DEVICE requested CUDA device unavailable, falling back to CPU"
                    );
                    Device::Cpu
                }
            }
        }
        _ => match Device::cuda_if_available(0) {
            Ok(dev) => {
                tracing::info!("using CUDA device for embeddings");
                dev
            }
            Err(e) => {
                tracing::warn!(error = %e, "CUDA not available, falling back to CPU");
                Device::Cpu
            }
        },
    }
}

/// Weights, tokenizer and device, built by the first embedding call.
struct LoadedModel {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl LoadedModel {
    fn load(model: CandleModel) -> Result<Self> {
        tracing::info!(model = model.hf_id(), "initializing Candle embedder");

        let device = select_device();

        // Download model files from Hugging Face via the memd download helper.
        // Historically this used `hf-hub` 0.3.2, which fed huggingface.co's
        // relative 307 Location URLs verbatim to `ureq::get` and bubbled up
        // `Bad URL: RelativeUrlWithoutBase`. Plain `ureq::get` (inside
        // `download_file`) resolves relative redirects against the request
        // URL, so the Candle embedder now boots in environments where hf-hub
        // used to crash the harness.
        //
        // Files land under `~/.cache/memd/models/`, not the hf-hub cache
        // (`~/.cache/huggingface/hub` or `$HF_HOME/hub`). Hosts with a
        // populated hf-hub cache but no network will re-download on first
        // upgrade — see CHANGELOG.
        let (config_path, tokenizer_path, weights_path) = get_candle_model_paths(model)?;

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

        // The embedder reports its dimension before it loads, from the model
        // choice. The config file must agree.
        if bert_config.hidden_size != model.dimension() {
            return Err(MemdError::ConfigError(format!(
                "Model config hidden size {} does not match the {} dimension of {}",
                bert_config.hidden_size,
                model.dimension(),
                model.hf_id()
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

        // SAFETY: Memory-mapped safetensors are read-only and immutable.
        // The mmap is valid for the lifetime of the loaded weights file.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(std::slice::from_ref(&weights_path), DTYPE, &device)
                .map_err(|e| MemdError::EmbeddingError(format!("Failed to load model: {}", e)))?
        };

        let bert = BertModel::load(vb, &bert_config).map_err(|e| {
            MemdError::EmbeddingError(format!("Failed to create BERT model: {}", e))
        })?;

        tracing::info!(
            dimension = bert_config.hidden_size,
            "Candle embedder initialized successfully"
        );

        Ok(Self {
            model: bert,
            tokenizer,
            device,
        })
    }
}

/// The model of one embedder, shared by its clones and loaded on first use.
struct LazyModel {
    choice: CandleModel,
    loaded: OnceLock<LoadedModel>,
    /// Makes concurrent first calls load once. A failed load leaves the cell
    /// empty, so the next call tries again.
    loading: Mutex<()>,
}

impl LazyModel {
    fn get(&self) -> Result<&LoadedModel> {
        if let Some(loaded) = self.loaded.get() {
            return Ok(loaded);
        }
        let _loading = self.loading.lock();
        if let Some(loaded) = self.loaded.get() {
            return Ok(loaded);
        }
        let loaded = LoadedModel::load(self.choice)?;
        Ok(self.loaded.get_or_init(|| loaded))
    }
}

/// Candle-based embedder. Cloning shares the model.
#[derive(Clone)]
pub struct CandleEmbedder {
    /// The model, loaded by the first `embed_texts` or `embed_query` call
    model: Arc<LazyModel>,
    /// Semaphore to limit concurrent inference to one pass per worker
    semaphore: Arc<Semaphore>,
    /// Forward passes that may run at once
    workers: usize,
    /// Embedding dimension, a property of the model choice
    dimension: usize,
    /// Embedding configuration
    config: EmbeddingConfig,
    /// Retrieval query prefix (model-specific; applied to queries only)
    query_prefix: Option<&'static str>,
}

impl CandleEmbedder {
    /// Create a new CandleEmbedder with the default model (all-MiniLM-L6-v2).
    pub fn new() -> Self {
        Self::with_config(EmbeddingConfig::default())
    }

    /// Create with custom configuration and the default model
    /// (all-MiniLM-L6-v2). Use `with_config_and_model` to select a model.
    pub fn with_config(config: EmbeddingConfig) -> Self {
        Self::with_config_and_model(config, CandleModel::default())
    }

    /// Create with custom configuration and an explicit Candle model.
    ///
    /// Pooling and the query prefix are properties of the model's training
    /// recipe, so they override whatever the caller's config carried. This
    /// does not touch the model files: the first `embed_texts` or
    /// `embed_query` call loads them, and a load failure is that call's
    /// error.
    pub fn with_config_and_model(mut config: EmbeddingConfig, model: CandleModel) -> Self {
        config.pooling = model.pooling_strategy();
        let workers = inference_workers();

        Self {
            model: Arc::new(LazyModel {
                choice: model,
                loaded: OnceLock::new(),
                loading: Mutex::new(()),
            }),
            semaphore: Arc::new(Semaphore::new(workers)),
            workers,
            dimension: model.dimension(),
            config,
            query_prefix: model.query_prefix(),
        }
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
        let lazy_model = Arc::clone(&self.model);
        let pooling_strategy = self.config.pooling;
        let should_normalize = self.config.normalize;

        // Convert to owned strings for 'static lifetime in spawn_blocking
        let texts_owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();

        // Run inference in blocking task to avoid blocking async runtime
        let embeddings = tokio::task::spawn_blocking(move || {
            let _permit = permit; // Hold permit until task completes

            // The first call loads the model; a failure is this call's error.
            let loaded = lazy_model.get()?;

            // Tokenizing and the forward pass run on one thread. Candle and
            // the tokenizer size their rayon use from the pool they run in,
            // so the forward passes of different batches run side by side
            // instead of splitting every small matrix over all cores.
            on_one_thread(|| {
                let LoadedModel {
                    model,
                    tokenizer,
                    device,
                } = loaded;

                // Convert back to &str for tokenizer
                let texts_refs: Vec<&str> = texts_owned.iter().map(|s| s.as_str()).collect();

                // Tokenize
                let encodings = tokenizer.encode_batch(texts_refs, true).map_err(|e| {
                    MemdError::EmbeddingError(format!("Tokenization failed: {}", e))
                })?;

                // Convert to tensors
                let token_ids: Vec<Vec<u32>> =
                    encodings.iter().map(|e| e.get_ids().to_vec()).collect();

                let attention_mask: Vec<Vec<u32>> = encodings
                    .iter()
                    .map(|e| e.get_attention_mask().to_vec())
                    .collect();

                let token_ids_tensor = Tensor::new(token_ids, device).map_err(|e| {
                    MemdError::EmbeddingError(format!("Failed to create token tensor: {}", e))
                })?;

                let attention_mask_tensor = Tensor::new(attention_mask, device).map_err(|e| {
                    MemdError::EmbeddingError(format!("Failed to create mask tensor: {}", e))
                })?;

                // Run model inference
                // `BertModel::forward(input_ids, token_type_ids, attention_mask)`:
                // single-segment input has token type 0 everywhere, and the mask
                // keeps padding out of self-attention so a text's vector does not
                // depend on the other texts in its batch. See `EMBEDDING_REVISION`.
                let token_type_ids = token_ids_tensor.zeros_like().map_err(|e| {
                    MemdError::EmbeddingError(format!("Failed to create token type tensor: {}", e))
                })?;
                let outputs = model
                    .forward(
                        &token_ids_tensor,
                        &token_type_ids,
                        Some(&attention_mask_tensor),
                    )
                    .map_err(|e| {
                        MemdError::EmbeddingError(format!("Model forward failed: {}", e))
                    })?;
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
            })?
        })
        .await
        .map_err(|e| MemdError::EmbeddingError(format!("Task join error: {}", e)))??;

        Ok(embeddings)
    }
}

impl Default for CandleEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Embedder for CandleEmbedder {
    async fn embed_query(&self, query: &str) -> Result<EmbeddingResult> {
        // Retrieval-tuned models (BGE) expect an instruction prefix on the
        // query side only; documents are embedded verbatim by embed_texts.
        match self.query_prefix {
            Some(prefix) => {
                let prefixed = format!("{prefix}{query}");
                self.embed_single(&prefixed).await
            }
            None => self.embed_single(query).await,
        }
    }

    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Split into forward passes and run them side by side, one per worker.
        let batch_size = forward_batch_size(self.config.batch_size, texts.len(), self.workers);
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

    fn is_loaded(&self) -> bool {
        self.model.loaded.get().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires pinned MiniLM and BGE-small model files in the model cache"]
    async fn bge_small_reference_and_same_dimension_rebuild() {
        use crate::index::EmbeddingCache;
        use crate::store::Store;
        use crate::types::{ChunkType, MemoryChunk, TenantId};
        use crate::{PersistentStore, PersistentStoreConfig};

        let sentence = "the cat sat on the mat";
        // Independent float64 NumPy BERT forward pass, CLS + L2, pinned
        // revision 5c38ec7c405ec4b44b94cc5a9bb96e735b38267a.
        let document_reference = [
            0.030454374,
            -0.010436759,
            0.0030197523,
            0.061055217,
            -0.03193069,
            -0.00399272,
            0.08802044,
            0.09531104,
            0.06485733,
            0.043397103,
            -0.020592682,
            -0.054777026,
        ];
        let query_reference = [
            0.023824256,
            -0.0016893448,
            -0.010143985,
            0.07980437,
            -0.01056636,
            -0.00067347835,
            0.08750715,
            0.10632792,
            0.03941586,
            0.046982303,
            -0.005884339,
            -0.051092632,
        ];
        let embedder = CandleEmbedder::with_config_and_model(
            EmbeddingConfig::default(),
            CandleModel::BgeSmall,
        );
        let document = embedder.embed_texts(&[sentence]).await.unwrap().remove(0);
        let query = embedder.embed_query(sentence).await.unwrap();
        for (actual, expected) in [(&document, document_reference), (&query, query_reference)] {
            assert_eq!(actual.len(), 384);
            assert!((actual.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-5);
            for (value, reference) in actual.iter().zip(expected) {
                assert!(
                    (value - reference).abs() < 1e-4,
                    "got {value}, expected {reference}"
                );
            }
        }
        let cosine: f32 = document.iter().zip(&query).map(|(a, b)| a * b).sum();
        assert!((cosine - 0.9492462).abs() < 1e-4, "got {cosine}");
        drop(embedder);

        let dir = tempfile::tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().into(),
            enable_dense_search: true,
            enable_hybrid_search: false,
            enable_async_indexing: false,
            backfill_hnsw_on_startup: false,
            ..Default::default()
        };
        let tenants = [
            TenantId::new("model_a").unwrap(),
            TenantId::new("model_b").unwrap(),
        ];
        let cache_path = |tenant: &TenantId| {
            dir.path()
                .join("tenants")
                .join(tenant.to_string())
                .join("warm_index/embeddings.bin")
        };
        {
            let store = PersistentStore::open(config.clone()).unwrap();
            for tenant in &tenants {
                store
                    .add(MemoryChunk::new(tenant.clone(), sentence, ChunkType::Doc))
                    .await
                    .unwrap();
            }
            store.shutdown().unwrap();
        }
        let original = std::fs::read(cache_path(&tenants[0])).unwrap();
        let mut switched = config.clone();
        switched.candle_model = CandleModel::BgeSmall;
        let error = PersistentStore::open(switched.clone())
            .err()
            .expect("same-dimension switch must fail");
        assert!(error.to_string().contains("all-MiniLM-L6-v2"));
        assert!(error.to_string().contains("bge-small-en-v1.5"));
        assert_eq!(std::fs::read(cache_path(&tenants[0])).unwrap(), original);
        switched.read_only = true;
        assert!(PersistentStore::open(switched.clone()).is_err());
        switched.read_only = false;
        switched.backfill_hnsw_on_startup = true;
        {
            let store = PersistentStore::open(switched.clone()).unwrap();
            // shutdown waits for the startup rebuild and saves every tenant.
            store.shutdown().unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("embedding_model"))
                .unwrap()
                .trim(),
            CandleModel::BgeSmall.hf_id()
        );
        for tenant in &tenants {
            let cache = EmbeddingCache::load_from(&cache_path(tenant)).unwrap();
            let rebuilt = cache.get(0).unwrap();
            assert_eq!(cache.len(), 1);
            for (actual, expected) in rebuilt.iter().zip(&document) {
                assert!(
                    (actual - expected).abs() < 1e-5,
                    "old model vector survived the rebuild"
                );
            }
        }
        assert!(
            PersistentStore::open(config).is_err(),
            "reverse switch must fail too"
        );
        switched.backfill_hnsw_on_startup = false;
        {
            let store = PersistentStore::open(switched).unwrap();
            // Extend the rebuilt cache, then verify both rows use BGE-small.
            store
                .add(MemoryChunk::new(
                    tenants[0].clone(),
                    "a feline rested on a rug",
                    ChunkType::Doc,
                ))
                .await
                .unwrap();
            store.shutdown().unwrap();
        }
        assert_eq!(
            EmbeddingCache::load_from(&cache_path(&tenants[0]))
                .unwrap()
                .len(),
            2
        );
    }

    /// Building an embedder reads no model file, so it works without the
    /// model on disk and reports the dimension of the chosen model.
    #[test]
    fn construction_reads_no_model_files() {
        let embedder =
            CandleEmbedder::with_config_and_model(EmbeddingConfig::default(), CandleModel::BgeBase);
        assert_eq!(embedder.dimension(), 768);
        assert_eq!(embedder.config().pooling, PoolingStrategy::Cls);
        assert!(!embedder.is_loaded());
    }

    #[test]
    fn forward_batches_spread_texts_over_the_workers() {
        // One text is one forward.
        assert_eq!(forward_batch_size(32, 1, 16), 1);
        // Fewer texts than the configured size still give each worker a share.
        assert_eq!(forward_batch_size(32, 32, 16), 2);
        assert_eq!(forward_batch_size(32, 256, 32), 8);
        // A big call is capped by the pass limit and by the configured size.
        assert_eq!(forward_batch_size(32, 100_000, 16), MAX_FORWARD_ROWS);
        assert_eq!(forward_batch_size(4, 256, 8), 4);
        // One worker still honors both caps.
        assert_eq!(forward_batch_size(32, 256, 1), MAX_FORWARD_ROWS);
        assert_eq!(forward_batch_size(3, 256, 1), 3);
        // A configured size of zero still makes progress.
        assert_eq!(forward_batch_size(0, 10, 4), 1);
    }

    #[test]
    fn parse_cuda_index_handles_all_forms() {
        assert_eq!(parse_cuda_index("cuda"), 0);
        assert_eq!(parse_cuda_index("gpu"), 0);
        assert_eq!(parse_cuda_index("cuda:2"), 2);
        assert_eq!(parse_cuda_index("gpu:1"), 1);
        assert_eq!(parse_cuda_index("3"), 3);
        // Unparseable index falls back to device 0.
        assert_eq!(parse_cuda_index("cuda:x"), 0);
    }

    #[tokio::test]
    #[ignore = "requires model download and network access"]
    async fn test_embed_basic() {
        let embedder = CandleEmbedder::new();
        // The dimension is known before the model loads; the first call loads.
        assert_eq!(embedder.dimension(), 384);
        assert!(!embedder.is_loaded());
        let embeddings = embedder.embed_texts(&["hello world"]).await.unwrap();
        assert!(embedder.is_loaded());
        assert_eq!(embeddings.len(), 1);
        assert_eq!(embeddings[0].len(), 384);

        // Verify normalized (L2 norm should be ~1.0)
        let norm: f32 = embeddings[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01, "embedding should be normalized");
    }

    /// A text's vector must not depend on the texts it was batched with.
    /// Padding to the longest text in the batch used to leak into
    /// self-attention (see `EMBEDDING_REVISION`).
    #[tokio::test]
    #[ignore = "requires model download and network access"]
    async fn test_embedding_is_independent_of_batch_neighbours() {
        let embedder = CandleEmbedder::new();
        let short = "scheduler quota exceeded";
        let long = "The second login node rejects every job above the group quota, \
                    so submit to the debug queue first and check the account limits \
                    before starting a long array job on the shared partition."
            .repeat(3);

        let alone = embedder.embed_texts(&[short]).await.unwrap();
        let batched = embedder.embed_texts(&[short, long.as_str()]).await.unwrap();

        let cosine: f32 = alone[0].iter().zip(&batched[0]).map(|(a, b)| a * b).sum();
        assert!(
            cosine > 0.9999,
            "the same text embedded alone and in a batch must match; cosine was {cosine}"
        );
    }

    /// A vector must not depend on how a call is split into forward passes.
    /// Texts of mixed length go through one padded forward pass, and one by
    /// one without any padding; the vectors agree to 1e-6 per component.
    #[tokio::test]
    #[ignore = "requires the cached MiniLM model files"]
    async fn vectors_do_not_depend_on_the_split_into_forward_passes() {
        let embedder = CandleEmbedder::new();
        let texts: Vec<String> = (0..20)
            .map(|i| format!("{} item {i}", "scheduler quota ".repeat(1 + (i * 7) % 12)))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();

        let together = embedder.embed_batch(&refs).await.unwrap();
        let mut worst = 0.0f32;
        for (text, joint) in refs.iter().zip(&together) {
            let alone = embedder.embed_batch(&[*text]).await.unwrap().remove(0);
            for (a, b) in alone.iter().zip(joint) {
                worst = worst.max((a - b).abs());
            }
        }
        eprintln!("largest per-component difference: {worst:e}");
        assert!(worst < 1e-6, "split changed a vector by {worst:e}");

        // `embed_texts` splits the call over its workers and keeps the order.
        let split = embedder.embed_texts(&refs).await.unwrap();
        assert_eq!(split.len(), together.len());
        for (joint, piece) in together.iter().zip(&split) {
            for (a, b) in joint.iter().zip(piece) {
                assert!((a - b).abs() < 1e-6, "embed_texts changed a vector");
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires model download and network access"]
    async fn test_embed_batch() {
        let embedder = CandleEmbedder::new();
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
        let embedder = Arc::new(CandleEmbedder::new());
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
        let embedder = CandleEmbedder::new();

        let texts = vec![
            "the cat sat on the mat",
            "a feline rested on a rug",
            "quarterly tax filings are due in april",
        ];
        let embeddings = embedder.embed_texts(&texts).await.unwrap();
        let cosine = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let similar = cosine(&embeddings[0], &embeddings[1]);
        let unrelated = cosine(&embeddings[0], &embeddings[2]);

        // Reference values for the pinned all-MiniLM-L6-v2 weights, from an
        // independent implementation of the model (token type 0, padding
        // masked out of attention, mean pooling): 0.5612 and -0.0300. The
        // revision 1 inputs scored the paraphrase 0.60 to 0.62 depending on
        // the batch, which is what the old `> 0.6` bound was fitted to.
        assert!(
            (similar - 0.5612).abs() < 0.01,
            "paraphrase similarity should match the reference model, got {similar}"
        );
        // The value this pair scored before the model was shared and loaded
        // lazily. A change in how vectors are computed moves it.
        assert!(
            (similar - 0.561162).abs() < 1e-5,
            "paraphrase similarity moved from 0.561162, got {similar}"
        );
        assert!(
            (unrelated + 0.0300).abs() < 0.01,
            "unrelated similarity should match the reference model, got {unrelated}"
        );
    }
}
