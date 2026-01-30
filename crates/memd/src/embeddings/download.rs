//! Model download utilities
//!
//! Downloads embedding model to ~/.cache/memd/ on first use.

use std::path::PathBuf;

use crate::error::{MemdError, Result};

/// Model info for Qwen3-Embedding-0.6B (quantized)
/// Using ONNX Community's quantized model: https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX
/// Upgraded from all-MiniLM-L6-v2 (56.3 MTEB) to Qwen3 (64.33 MTEB, +15% improvement)
/// Dimensions: 1024 (vs 384), Context: 32K tokens, Languages: 100+, Code: Excellent
const MODEL_URL: &str = "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/onnx/model_quantized.onnx";
const MODEL_FILENAME: &str = "Qwen3-Embedding-0.6B-quantized.onnx";
const TOKENIZER_URL: &str = "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/tokenizer.json";
const TOKENIZER_FILENAME: &str = "qwen3-tokenizer.json";

/// Minimum expected model file size (bytes) - ~150MB for q8 quantized Qwen3
const MIN_MODEL_SIZE: u64 = 100_000_000;
/// Minimum expected tokenizer file size (bytes) - ~700KB
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

/// Download the tokenizer
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_dir() {
        let dir = get_cache_dir().expect("should get cache dir");
        assert!(dir.to_string_lossy().contains("memd"));
        assert!(dir.to_string_lossy().contains("models"));
    }
}
