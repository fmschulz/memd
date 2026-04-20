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
            Self::AllMiniLmL6V2 => 500_000,     // ~700KB
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
const MODEL_URL: &str =
    "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/onnx/model_quantized.onnx";
const MODEL_FILENAME: &str = "all-MiniLM-L6-v2-quantized.onnx";
const TOKENIZER_URL: &str =
    "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer.json";
const TOKENIZER_FILENAME: &str = "tokenizer.json";
const MIN_MODEL_SIZE: u64 = 20_000_000;
const MIN_TOKENIZER_SIZE: u64 = 500_000;

// Candle BERT (safetensors) files for sentence-transformers/all-MiniLM-L6-v2.
// These URLs are fetched with plain ureq, which follows huggingface.co's
// relative 307 Location headers correctly. Prior versions used hf-hub 0.3.2,
// which mishandled those redirects and failed with RelativeUrlWithoutBase.
// `resolve/main` tracks the repo's head ref — same mutable ref hf-hub 0.3.2
// resolved to by default, so this preserves the prior trust posture rather
// than introducing a stronger (commit-hash) pin.
const CANDLE_BERT_CONFIG_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/config.json";
const CANDLE_BERT_CONFIG_FILENAME: &str = "sentence-transformers-all-MiniLM-L6-v2-config.json";
const CANDLE_BERT_TOKENIZER_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";
const CANDLE_BERT_TOKENIZER_FILENAME: &str =
    "sentence-transformers-all-MiniLM-L6-v2-tokenizer.json";
const CANDLE_BERT_WEIGHTS_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors";
const CANDLE_BERT_WEIGHTS_FILENAME: &str = "sentence-transformers-all-MiniLM-L6-v2.safetensors";
const MIN_CANDLE_BERT_CONFIG_SIZE: u64 = 100; // config.json is ~600 bytes
const MIN_CANDLE_BERT_TOKENIZER_SIZE: u64 = 100_000; // tokenizer.json is ~470KB at main today
const MIN_CANDLE_BERT_WEIGHTS_SIZE: u64 = 80_000_000; // safetensors is ~90MB

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
    let model_path = cache_dir.join(MODEL_FILENAME);
    download_file(MODEL_URL, &model_path, "embedding model")
}

/// Download the tokenizer (legacy, uses default model)
fn download_tokenizer(cache_dir: &PathBuf) -> Result<()> {
    let tokenizer_path = cache_dir.join(TOKENIZER_FILENAME);
    download_file(TOKENIZER_URL, &tokenizer_path, "tokenizer")
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
        download_file(
            model.tokenizer_url(),
            &tokenizer_path,
            model.tokenizer_filename(),
        )?;
    }

    // Verify tokenizer exists and has expected size
    verify_file_size(&tokenizer_path, model.min_tokenizer_size(), "tokenizer")?;

    Ok(tokenizer_path)
}

/// Generic file download helper.
///
/// Streams into a per-invocation sibling temp file
/// (`<path>.partial.<pid>.<thread>.<counter>`), fsyncs, then publishes
/// to the canonical target via `hard_link` + `remove_file`. On Unix
/// `hard_link` fails atomically with `AlreadyExists` if another caller
/// already published — that branch keeps the winner's bytes and drops
/// ours. `rename` would silently clobber the winner (Unix semantics),
/// so we intentionally don't use it. The per-invocation counter in
/// the temp suffix prevents same-process concurrent callers on the
/// same target from sharing a temp file.
///
/// This keeps the cache atomic: an interrupted download, a crash
/// mid-stream, or two racing processes can never leave a half-written
/// file at the canonical cache path where `verify_file_size` would
/// either wedge boot or (worse) let a truncated model through.
fn download_file(url: &str, path: &PathBuf, name: &str) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let cache_dir = path.parent().unwrap();
    std::fs::create_dir_all(cache_dir)?;

    tracing::info!("Downloading {} to {:?}", name, path);

    let response = ureq::get(url)
        .call()
        .map_err(|e| MemdError::StorageError(format!("failed to download {}: {}", name, e)))?;

    let tmp_path = path.with_extension(format!(
        "partial.{}.{:?}.{}",
        std::process::id(),
        std::thread::current().id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::File::create(&tmp_path)?;
    let copy_result = std::io::copy(&mut response.into_reader(), &mut file)
        .and_then(|_| file.sync_all());
    if let Err(e) = copy_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(MemdError::StorageError(format!(
            "failed to stream {} to {:?}: {}",
            name, tmp_path, e
        )));
    }
    drop(file);

    // First-writer-wins publish: hard_link is atomic and fails with
    // AlreadyExists if target already exists. Loser cleans up its tmp
    // and keeps winner's bytes.
    match std::fs::hard_link(&tmp_path, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp_path);
            // Durability fold (v0.8.0 handoff Candle follow-up): after
            // a successful first-writer publish, fsync the parent dir
            // so the new directory entry survives a post-return crash.
            // The earlier file.sync_all() made the inode's data durable;
            // without this, the dirent mapping `path -> inode` can still
            // be lost on recovery. Best-effort: log-and-continue on
            // platforms/filesystems that reject dir fsync.
            fsync_parent_dir(path);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&tmp_path);
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(MemdError::StorageError(format!(
                "failed to publish {} to {:?}: {}",
                name, path, e
            )));
        }
    }

    tracing::info!("{} downloaded successfully", name);
    Ok(())
}

/// Best-effort fsync of the parent directory of ``path``.
///
/// Follows Linux durability semantics: `file.sync_all()` makes the
/// inode's data durable, but a subsequent crash can still lose the
/// directory entry that links the canonical name to that inode.
/// Opening the parent directory and calling `sync_all()` on the
/// directory handle forces the dirent through to stable storage.
///
/// Silent on filesystems that reject directory fsync (rare; mostly
/// networked/synthetic filesystems). We already wrote and link-published
/// the file successfully, so a failed dir-fsync is best-effort — log
/// at debug and continue rather than fail the whole download.
fn fsync_parent_dir(path: &std::path::Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    match std::fs::File::open(parent) {
        Ok(dir) => {
            if let Err(e) = dir.sync_all() {
                tracing::debug!(
                    "fsync on parent dir {:?} failed (best-effort): {}",
                    parent,
                    e
                );
            }
        }
        Err(e) => {
            tracing::debug!(
                "opening parent dir {:?} for fsync failed (best-effort): {}",
                parent,
                e
            );
        }
    }
}

/// Get paths to the Candle BERT model files (config, tokenizer, weights).
///
/// Downloads any missing files from huggingface.co into the memd cache using
/// plain `ureq`, which handles HF's relative 307 redirects correctly. Targets
/// the `sentence-transformers/all-MiniLM-L6-v2` repo used by the Candle BERT
/// embedder. Returns the local paths in (config, tokenizer, weights) order.
pub fn get_candle_bert_paths() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let cache_dir = get_cache_dir()?;
    std::fs::create_dir_all(&cache_dir)?;

    let config_path = cache_dir.join(CANDLE_BERT_CONFIG_FILENAME);
    if !config_path.exists() {
        download_file(CANDLE_BERT_CONFIG_URL, &config_path, "BERT config")?;
    }
    verify_file_size(&config_path, MIN_CANDLE_BERT_CONFIG_SIZE, "BERT config")?;

    let tokenizer_path = cache_dir.join(CANDLE_BERT_TOKENIZER_FILENAME);
    if !tokenizer_path.exists() {
        download_file(
            CANDLE_BERT_TOKENIZER_URL,
            &tokenizer_path,
            "BERT tokenizer",
        )?;
    }
    verify_file_size(
        &tokenizer_path,
        MIN_CANDLE_BERT_TOKENIZER_SIZE,
        "BERT tokenizer",
    )?;

    let weights_path = cache_dir.join(CANDLE_BERT_WEIGHTS_FILENAME);
    if !weights_path.exists() {
        download_file(CANDLE_BERT_WEIGHTS_URL, &weights_path, "BERT weights")?;
    }
    verify_file_size(
        &weights_path,
        MIN_CANDLE_BERT_WEIGHTS_SIZE,
        "BERT weights",
    )?;

    Ok((config_path, tokenizer_path, weights_path))
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
            file_type,
            metadata.len(),
            min_size
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

    #[test]
    fn test_candle_bert_constants() {
        // Contract (repo + file + path, not revision): the Candle BERT embedder
        // pulls config/tokenizer/weights from sentence-transformers/all-MiniLM-L6-v2
        // at the `main` ref, same mutable ref hf-hub 0.3.2 resolved to by default.
        // These asserts lock in the URL shape that plain ureq follows correctly,
        // preventing a silent refactor from reintroducing the RelativeUrlWithoutBase
        // regression. Revision pinning (commit hash) would be a stronger
        // provenance guarantee and is deliberately out of scope here.
        assert_eq!(
            CANDLE_BERT_CONFIG_URL,
            "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/config.json"
        );
        assert_eq!(
            CANDLE_BERT_TOKENIZER_URL,
            "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json"
        );
        assert_eq!(
            CANDLE_BERT_WEIGHTS_URL,
            "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors"
        );
        assert!(CANDLE_BERT_CONFIG_URL.starts_with("https://"));
        assert!(CANDLE_BERT_TOKENIZER_URL.starts_with("https://"));
        assert!(CANDLE_BERT_WEIGHTS_URL.starts_with("https://"));
    }

    #[test]
    fn test_candle_bert_cache_filenames_are_distinct() {
        // Repo-qualified filenames avoid collisions with the Xenova ONNX
        // tokenizer cached under the same directory.
        assert_ne!(CANDLE_BERT_CONFIG_FILENAME, CANDLE_BERT_TOKENIZER_FILENAME);
        assert_ne!(CANDLE_BERT_TOKENIZER_FILENAME, TOKENIZER_FILENAME);
        assert!(CANDLE_BERT_TOKENIZER_FILENAME.contains("sentence-transformers"));
        assert!(CANDLE_BERT_WEIGHTS_FILENAME.ends_with(".safetensors"));
    }

    #[test]
    fn test_download_file_fails_cleanly_on_request_error() {
        // When the ureq .call() fails (connect-refused on a loopback port
        // with no listener), download_file must return an error WITHOUT
        // creating any file on disk — neither the canonical target nor a
        // `.partial.*` sibling. This covers the early-failure branch
        // (before any tmp file exists).
        let tmp_dir = std::env::temp_dir().join(format!(
            "memd-download-req-err-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("atomic-test.bin");

        let result = download_file("http://127.0.0.1:1/never", &target, "test file");
        assert!(result.is_err(), "expected download to error");
        assert!(
            !target.exists(),
            "target should not exist after failed download: {:?}",
            target
        );
        let leftovers: Vec<_> = std::fs::read_dir(&tmp_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp/partial files should remain, found: {:?}",
            leftovers
        );

        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn test_fsync_parent_dir_is_ok_on_real_dir() {
        // Durability fold: `fsync_parent_dir` should quietly succeed
        // on a normal tmpfs/ext4-style directory and never panic. We
        // don't have a way to prove a real fsync reached disk in unit
        // tests, but we can at least prove the code path runs without
        // surfacing an error to the caller (it's best-effort by design).
        let tmp_dir = std::env::temp_dir().join(format!(
            "memd-fsync-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let f = tmp_dir.join("marker.bin");
        std::fs::write(&f, b"x").unwrap();
        // Just exercising the code path; it should not panic or abort
        // even if the underlying FS doesn't implement dir fsync.
        fsync_parent_dir(&f);
        // Also tolerate a path with no parent (e.g., root) without
        // panicking: pass a path with no parent-less pathological shape.
        // `Path::new("foo")`.parent() is Some("") on Unix, so this is
        // already a safe no-op; we just call it to cover.
        fsync_parent_dir(std::path::Path::new("foo"));

        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn test_download_file_fsyncs_parent_on_publish() {
        // End-to-end happy path: download completes, hard_link publishes,
        // and fsync_parent_dir is called on the canonical target's parent.
        // We can't assert the kernel-level fsync, but we can assert the
        // post-publish invariants hold: file present, sized, no stray
        // `.partial.*` siblings left behind.
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let body = b"candle-fsync-payload".to_vec();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body_for_server = body.clone();
        let server_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body_for_server.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&body_for_server);
            }
        });

        let tmp_dir = std::env::temp_dir().join(format!(
            "memd-download-fsync-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("candle.bin");

        let url = format!("http://{}/x", addr);
        let result = download_file(&url, &target, "fsync test");
        server_thread.join().ok();

        assert!(result.is_ok(), "download_file should succeed: {:?}", result);
        let observed = std::fs::read(&target).unwrap();
        assert_eq!(observed, body, "published bytes must match");
        let leftovers: Vec<_> = std::fs::read_dir(&tmp_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("partial"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no partial tmp files should remain, found: {:?}",
            leftovers
        );

        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    /// Network-gated integration test: actually fetch a small real
    /// asset from huggingface.co and verify `download_file` lands it
    /// atomically in a cache dir. Guards against regressions in the
    /// ureq-based HF redirect handling (the RelativeUrlWithoutBase
    /// issue that hf-hub 0.3.2 had; fixed by switching to plain ureq
    /// which follows HF's relative 307 Location headers correctly).
    ///
    /// Opt-in to keep the default `cargo test` hermetic. Enable with
    /// `MEMD_NETWORK_TESTS=1 cargo test -p memd test_download_file_hf_config_integration`.
    /// Uses the ~600-byte `config.json` asset so the test finishes in
    /// well under a second on a normal connection.
    #[test]
    fn test_download_file_hf_config_integration() {
        if std::env::var("MEMD_NETWORK_TESTS").ok().as_deref() != Some("1") {
            eprintln!(
                "skipping HF network test; set MEMD_NETWORK_TESTS=1 to run"
            );
            return;
        }

        let tmp_dir = std::env::temp_dir().join(format!(
            "memd-hf-config-net-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join(CANDLE_BERT_CONFIG_FILENAME);

        let result = download_file(CANDLE_BERT_CONFIG_URL, &target, "BERT config");
        assert!(
            result.is_ok(),
            "HF config download should succeed (network required): {:?}",
            result
        );
        assert!(target.exists(), "config file should exist at target");
        let meta = std::fs::metadata(&target).expect("metadata");
        assert!(
            meta.len() >= MIN_CANDLE_BERT_CONFIG_SIZE,
            "downloaded config is too small ({} bytes, expected >= {})",
            meta.len(),
            MIN_CANDLE_BERT_CONFIG_SIZE
        );
        // No leftover `.partial.*` sibling.
        let leftovers: Vec<_> = std::fs::read_dir(&tmp_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("partial"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no partial tmp files should remain after a clean HF download, found: {:?}",
            leftovers
        );

        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn test_download_file_is_first_writer_wins() {
        // Simulate the race-loser branch: the canonical target already
        // exists when we try to publish. `hard_link` must fail with
        // AlreadyExists, our tmp must be cleaned up, and the pre-existing
        // bytes must be preserved byte-for-byte.
        //
        // A tiny in-process HTTP server on a loopback port serves a real
        // body so `download_file` reaches the publish branch (which is the
        // only place that can observe the pre-existing target). We then
        // assert the target's original bytes are intact and no `.partial.*`
        // sibling is left behind.
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let body = b"fresh-download-payload".to_vec();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body_for_server = body.clone();
        let server_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body_for_server.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&body_for_server);
            }
        });

        let tmp_dir = std::env::temp_dir().join(format!(
            "memd-download-winner-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("winner.bin");

        // Pre-existing target: simulate the winning race sibling.
        let sentinel = b"WINNER-BYTES".to_vec();
        std::fs::write(&target, &sentinel).unwrap();

        let url = format!("http://{}/x", addr);
        let result = download_file(&url, &target, "race test");
        server_thread.join().ok();

        assert!(result.is_ok(), "download_file should succeed: {:?}", result);
        // Target preserved byte-for-byte (winner wins).
        let observed = std::fs::read(&target).unwrap();
        assert_eq!(
            observed, sentinel,
            "pre-existing winner bytes must not be clobbered"
        );
        // Loser's .partial.* tmp file must be cleaned up.
        let leftovers: Vec<_> = std::fs::read_dir(&tmp_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("partial"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "loser's partial tmp must be cleaned up, found: {:?}",
            leftovers
        );

        std::fs::remove_dir_all(&tmp_dir).ok();
    }
}
