//! Model download utilities
//!
//! Downloads embedding model to ~/.cache/memd/ on first use.

use std::path::PathBuf;

use super::traits::PoolingStrategy;
use crate::error::{MemdError, Result};

/// Supported embedding models
///
/// Each model has specific configuration for URLs, dimensions, and pooling.
/// Pooling strategy is tied to model architecture, not user-configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmbeddingModel {
    /// all-MiniLM-L6-v2: 384-dim, mean pooling, 23MB quantized
    /// MTEB score: 56.3
    #[default]
    AllMiniLmL6V2,
    /// Qwen3-Embedding-0.6B: 1024-dim, last-token pooling, ~614MB quantized
    /// MTEB score: 64.33 (+15% improvement)
    Qwen3Embedding0_6B,
}

impl EmbeddingModel {
    /// Get embedding dimension for this model
    pub fn dimension(&self) -> usize {
        match self {
            Self::AllMiniLmL6V2 => 384,
            Self::Qwen3Embedding0_6B => 1024,
        }
    }

    /// Get pooling strategy for this model
    pub fn pooling_strategy(&self) -> PoolingStrategy {
        match self {
            Self::AllMiniLmL6V2 => PoolingStrategy::Mean,
            Self::Qwen3Embedding0_6B => PoolingStrategy::LastToken,
        }
    }

    /// Get model ONNX file URL
    pub fn model_url(&self) -> &'static str {
        match self {
            Self::AllMiniLmL6V2 => {
                "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/onnx/model_quantized.onnx"
            }
            Self::Qwen3Embedding0_6B => {
                "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/onnx/model_int8.onnx"
            }
        }
    }

    /// Get tokenizer URL
    pub fn tokenizer_url(&self) -> &'static str {
        match self {
            Self::AllMiniLmL6V2 => {
                "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer.json"
            }
            Self::Qwen3Embedding0_6B => {
                "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/tokenizer.json"
            }
        }
    }

    /// Get model filename for cache
    pub fn model_filename(&self) -> &'static str {
        match self {
            Self::AllMiniLmL6V2 => "all-MiniLM-L6-v2-quantized.onnx",
            Self::Qwen3Embedding0_6B => "qwen3-embedding-0.6b-q8.onnx",
        }
    }

    /// Get tokenizer filename for cache
    pub fn tokenizer_filename(&self) -> &'static str {
        match self {
            Self::AllMiniLmL6V2 => "all-minilm-l6-v2-tokenizer.json",
            Self::Qwen3Embedding0_6B => "qwen3-embedding-0.6b-tokenizer.json",
        }
    }

    /// Get minimum expected model file size (bytes)
    pub fn min_model_size(&self) -> u64 {
        match self {
            Self::AllMiniLmL6V2 => 20_000_000,       // ~23MB
            Self::Qwen3Embedding0_6B => 500_000_000, // ~614MB
        }
    }

    /// Get minimum expected tokenizer file size (bytes)
    pub fn min_tokenizer_size(&self) -> u64 {
        match self {
            Self::AllMiniLmL6V2 => 500_000, // ~700KB
            Self::Qwen3Embedding0_6B => 10_000, // ~varies
        }
    }

    /// Whether this model uses instruction-formatted queries
    pub fn uses_instruction_format(&self) -> bool {
        match self {
            Self::AllMiniLmL6V2 => false,
            Self::Qwen3Embedding0_6B => true,
        }
    }

    /// Whether this model requires position_ids as an input
    ///
    /// Decoder-style models (Qwen3) require explicit position IDs,
    /// while encoder models (BERT, all-MiniLM) compute them internally.
    pub fn requires_position_ids(&self) -> bool {
        match self {
            Self::AllMiniLmL6V2 => false,
            Self::Qwen3Embedding0_6B => true,
        }
    }

    /// Get KV-cache configuration for decoder models
    ///
    /// Returns None for encoder models (BERT-style) that don't use KV-cache.
    /// Returns configuration for decoder models that require empty KV-cache tensors.
    pub fn kv_cache_config(&self) -> Option<KvCacheConfig> {
        match self {
            Self::AllMiniLmL6V2 => None,
            Self::Qwen3Embedding0_6B => Some(KvCacheConfig {
                num_layers: 28,
                num_kv_heads: 8,
                head_dim: 128,
            }),
        }
    }
}

/// KV-cache configuration for decoder models
///
/// Decoder models (like Qwen3) use key-value caching for efficient autoregressive
/// generation. For embedding generation (single forward pass), we pass empty
/// KV-cache tensors with sequence length 0.
#[derive(Debug, Clone, Copy)]
pub struct KvCacheConfig {
    /// Number of transformer layers
    pub num_layers: usize,
    /// Number of key-value attention heads
    pub num_kv_heads: usize,
    /// Dimension of each attention head
    pub head_dim: usize,
}

// Legacy constants for backward compatibility (used by existing get_model_path/get_tokenizer_path)
const MODEL_URL: &str = "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/onnx/model_quantized.onnx";
const MODEL_FILENAME: &str = "all-MiniLM-L6-v2-quantized.onnx";
const TOKENIZER_URL: &str = "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer.json";
const TOKENIZER_FILENAME: &str = "tokenizer.json";
const MIN_MODEL_SIZE: u64 = 20_000_000;
const MIN_TOKENIZER_SIZE: u64 = 500_000;

/// Get the cache directory for memd models
pub fn get_cache_dir() -> Result<PathBuf> {
    let cache_dir = dirs::cache_dir()
        .ok_or_else(|| MemdError::StorageError("cannot determine cache directory".into()))?
        .join("memd")
        .join("models");
    Ok(cache_dir)
}

/// Get path to model file, downloading if needed
pub fn get_model_path() -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    let model_path = cache_dir.join(MODEL_FILENAME);

    if !model_path.exists() {
        download_model(&cache_dir)?;
    }

    // Verify model exists and has expected size
    verify_model_exists(&model_path)?;

    Ok(model_path)
}

/// Get path to tokenizer file, downloading if needed
pub fn get_tokenizer_path() -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    let tokenizer_path = cache_dir.join(TOKENIZER_FILENAME);

    if !tokenizer_path.exists() {
        download_tokenizer(&cache_dir)?;
    }

    // Verify tokenizer exists and has expected size
    verify_tokenizer_exists(&tokenizer_path)?;

    Ok(tokenizer_path)
}

/// Verify model file exists and has reasonable size
pub fn verify_model_exists(path: &PathBuf) -> Result<()> {
    if !path.exists() {
        return Err(MemdError::StorageError(format!(
            "model file not found at {:?}",
            path
        )));
    }

    let metadata = std::fs::metadata(path)?;
    if metadata.len() < MIN_MODEL_SIZE {
        return Err(MemdError::StorageError(format!(
            "model file too small ({} bytes), expected >= {} bytes. File may be corrupted, delete and retry.",
            metadata.len(),
            MIN_MODEL_SIZE
        )));
    }

    Ok(())
}

/// Verify tokenizer file exists and has reasonable size
fn verify_tokenizer_exists(path: &PathBuf) -> Result<()> {
    if !path.exists() {
        return Err(MemdError::StorageError(format!(
            "tokenizer file not found at {:?}",
            path
        )));
    }

    let metadata = std::fs::metadata(path)?;
    if metadata.len() < MIN_TOKENIZER_SIZE {
        return Err(MemdError::StorageError(format!(
            "tokenizer file too small ({} bytes), expected >= {} bytes. File may be corrupted, delete and retry.",
            metadata.len(),
            MIN_TOKENIZER_SIZE
        )));
    }

    Ok(())
}

/// Download the embedding model
pub fn download_model(cache_dir: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let model_path = cache_dir.join(MODEL_FILENAME);

    tracing::info!("Downloading embedding model to {:?}", model_path);

    let response = ureq::get(MODEL_URL)
        .call()
        .map_err(|e| MemdError::StorageError(format!("failed to download model: {}", e)))?;

    let mut file = std::fs::File::create(&model_path)?;
    std::io::copy(&mut response.into_reader(), &mut file)?;

    tracing::info!("Model downloaded successfully");
    Ok(())
}

/// Download the tokenizer (legacy, uses default model)
fn download_tokenizer(cache_dir: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let tokenizer_path = cache_dir.join(TOKENIZER_FILENAME);

    tracing::info!("Downloading tokenizer to {:?}", tokenizer_path);

    let response = ureq::get(TOKENIZER_URL)
        .call()
        .map_err(|e| MemdError::StorageError(format!("failed to download tokenizer: {}", e)))?;

    let mut file = std::fs::File::create(&tokenizer_path)?;
    std::io::copy(&mut response.into_reader(), &mut file)?;

    tracing::info!("Tokenizer downloaded successfully");
    Ok(())
}

// =============================================================================
// Model-aware download functions (new API)
// =============================================================================

/// Get path to model file for specific model, downloading if needed
pub fn get_model_path_for(model: EmbeddingModel) -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    let model_path = cache_dir.join(model.model_filename());

    if !model_path.exists() {
        download_file(model.model_url(), &model_path, model.model_filename())?;
    }

    // Verify model exists and has expected size
    verify_file_size(&model_path, model.min_model_size(), "model")?;

    Ok(model_path)
}

/// Get path to tokenizer file for specific model, downloading if needed
pub fn get_tokenizer_path_for(model: EmbeddingModel) -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    let tokenizer_path = cache_dir.join(model.tokenizer_filename());

    if !tokenizer_path.exists() {
        download_file(model.tokenizer_url(), &tokenizer_path, model.tokenizer_filename())?;
    }

    // Verify tokenizer exists and has expected size
    verify_file_size(&tokenizer_path, model.min_tokenizer_size(), "tokenizer")?;

    Ok(tokenizer_path)
}

/// Generic file download helper
fn download_file(url: &str, path: &PathBuf, name: &str) -> Result<()> {
    let cache_dir = path.parent().unwrap();
    std::fs::create_dir_all(cache_dir)?;

    tracing::info!("Downloading {} to {:?}", name, path);

    let response = ureq::get(url)
        .call()
        .map_err(|e| MemdError::StorageError(format!("failed to download {}: {}", name, e)))?;

    let mut file = std::fs::File::create(path)?;
    std::io::copy(&mut response.into_reader(), &mut file)?;

    tracing::info!("{} downloaded successfully", name);
    Ok(())
}

/// Verify file exists and meets minimum size
fn verify_file_size(path: &PathBuf, min_size: u64, file_type: &str) -> Result<()> {
    if !path.exists() {
        return Err(MemdError::StorageError(format!(
            "{} file not found at {:?}",
            file_type, path
        )));
    }

    let metadata = std::fs::metadata(path)?;
    if metadata.len() < min_size {
        return Err(MemdError::StorageError(format!(
            "{} file too small ({} bytes), expected >= {} bytes. File may be corrupted, delete and retry.",
            file_type, metadata.len(), min_size
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_dir() {
        let dir = get_cache_dir().expect("should get cache dir");
        assert!(dir.to_string_lossy().contains("memd"));
        assert!(dir.to_string_lossy().contains("models"));
    }

    #[test]
    fn test_embedding_model_defaults() {
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
    fn test_model_filenames() {
        assert_eq!(
            EmbeddingModel::AllMiniLmL6V2.model_filename(),
            "all-MiniLM-L6-v2-quantized.onnx"
        );
        assert_eq!(
            EmbeddingModel::Qwen3Embedding0_6B.model_filename(),
            "qwen3-embedding-0.6b-q8.onnx"
        );
    }

    #[test]
    fn test_model_urls() {
        assert!(EmbeddingModel::AllMiniLmL6V2
            .model_url()
            .contains("all-MiniLM-L6-v2"));
        assert!(EmbeddingModel::Qwen3Embedding0_6B
            .model_url()
            .contains("Qwen3-Embedding"));
    }
}
