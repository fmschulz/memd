//! Error types for memd
//!
//! Provides a unified error type using thiserror for ergonomic error handling
//! throughout the codebase.

use thiserror::Error;

/// Main error type for memd operations
#[derive(Error, Debug)]
pub enum MemdError {
    /// Configuration loading or parsing failures
    #[error("configuration error: {0}")]
    ConfigError(String),

    /// Invalid input validation errors
    #[error("validation error: {0}")]
    ValidationError(String),

    /// Storage operation failures
    #[error("storage error: {0}")]
    StorageError(String),

    /// The dense index is momentarily unavailable because a writer (index
    /// repair or bulk insert) holds its lock. Callers should retry shortly
    /// or fall back to a degraded path rather than block behind the writer.
    /// The display text starts with [`MemdError::INDEX_BUSY_MARKER`] so the
    /// condition stays classifiable after crossing stringifying error
    /// boundaries (ops `McpError` -> CLI -> warm wire).
    #[error("{}: {reason}; retry shortly", MemdError::INDEX_BUSY_MARKER)]
    IndexBusy {
        /// What held the index lock.
        reason: String,
    },

    /// Operation protocol errors.
    #[error("protocol error: {0}")]
    ProtocolError(String),

    /// Warm worker identity does not match this CLI.
    #[error(
        "warm worker is incompatible: worker version {worker_version}, protocol {worker_protocol}; CLI version {cli_version}, protocol {cli_protocol}"
    )]
    IncompatibleWarmWorker {
        worker_version: String,
        worker_protocol: String,
        cli_version: String,
        cli_protocol: String,
    },

    /// Warm worker serves a different embedding model or search variant than
    /// this CLI requested. Triggers the same shutdown+respawn as a version
    /// skew so a resident MiniLM/hybrid worker never answers bge/dense-only
    /// requests (and vice versa).
    #[error(
        "warm worker configuration mismatch: worker serves model {worker_model} / variant {worker_variant}; CLI requested model {cli_model} / variant {cli_variant}"
    )]
    WarmWorkerConfigMismatch {
        worker_model: String,
        worker_variant: String,
        cli_model: String,
        cli_variant: String,
    },

    /// The persisted dense index was built with a different embedding model
    /// (vector dimension) than the one now active — i.e. `--embedding-model`
    /// was changed on an existing store. Refused rather than silently wiping
    /// the index (dense-only search has no sparse fallback).
    #[error(
        "dense index at {path} holds {store_dim}-d embeddings but the active embedding model produces {model_dim}-d vectors; this store was built with a different --embedding-model. Re-open with the original model, or set MEMD_BACKFILL_HNSW_ON_STARTUP=1 to discard the dense index and re-embed from segments."
    )]
    DenseIndexModelMismatch {
        /// Path to the embedding cache that could not be reused.
        path: std::path::PathBuf,
        /// Vector dimension of the persisted index.
        store_dim: usize,
        /// Vector dimension the active embedding model produces.
        model_dim: usize,
    },

    /// Another process currently owns the persistent-store writer lock.
    #[error(
        "writer lock held by another process ({holder}) at {lock_path}; if a memd warm worker is running, route this write through it (--warm auto, the default) or stop it with `memd warm stop`; otherwise stop the other memd process or retry later (MEMD_WRITER_LOCK_TIMEOUT_MS)"
    )]
    WriterLockHeld {
        /// Path to the lock file.
        lock_path: std::path::PathBuf,
        /// Best-effort holder information read from the lock file.
        holder: String,
    },

    /// Mutating operation attempted on a read-only persistent store.
    #[error("store opened read-only: operation '{op}' not permitted")]
    ReadOnlyStore {
        /// Operation name.
        op: String,
    },

    /// Embedding generation errors
    #[error("embedding error: {0}")]
    EmbeddingError(String),

    /// IO errors
    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),

    /// JSON serialization/deserialization errors
    #[error("json error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// TOML deserialization errors (config parsing)
    #[error("toml parse error: {0}")]
    TomlError(#[from] toml::de::Error),

    /// TOML serialization errors
    #[error("toml serialization error: {0}")]
    TomlSerError(#[from] toml::ser::Error),

    /// SQLite database errors
    #[error("database error: {0}")]
    DatabaseError(#[from] rusqlite::Error),

    /// Candle framework errors (for embeddings)
    #[error("candle error: {0}")]
    CandleError(String),
}

// Manual From implementation for candle_core::Error
impl From<candle_core::Error> for MemdError {
    fn from(err: candle_core::Error) -> Self {
        MemdError::CandleError(err.to_string())
    }
}

impl MemdError {
    /// Stable prefix of [`MemdError::IndexBusy`]'s display text. The ops and
    /// CLI layers stringify errors between enums, so the warm worker and its
    /// clients classify busy-ness by this marker rather than by variant.
    /// Deliberately a code-like token: error messages can embed
    /// user-controlled text (chunk bodies, echoed fields), and prose like
    /// "dense index busy" could collide; `memd:dense-index-busy` cannot
    /// plausibly appear in natural content.
    pub const INDEX_BUSY_MARKER: &'static str = "memd:dense-index-busy";

    /// True when an error message (possibly wrapped by other layers)
    /// originated from [`MemdError::IndexBusy`].
    pub fn message_indicates_index_busy(message: &str) -> bool {
        message.contains(Self::INDEX_BUSY_MARKER)
    }
}

/// Result type alias for memd operations
pub type Result<T> = std::result::Result<T, MemdError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let err = MemdError::ValidationError("test error".to_string());
        assert_eq!(err.to_string(), "validation error: test error");
    }

    #[test]
    fn error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: MemdError = io_err.into();
        assert!(matches!(err, MemdError::IoError(_)));
    }

    #[test]
    fn index_busy_classifiable_through_stringification() {
        let err = MemdError::IndexBusy {
            reason: "index repair in flight".to_string(),
        };
        // Direct display carries the marker...
        assert!(MemdError::message_indicates_index_busy(&err.to_string()));
        // ...and survives one more stringly wrap, as done by ops/CLI layers.
        let wrapped = MemdError::ProtocolError(format!("warm worker command failed: {err}"));
        assert!(MemdError::message_indicates_index_busy(
            &wrapped.to_string()
        ));
        // Unrelated errors are not classified busy.
        assert!(!MemdError::message_indicates_index_busy(
            &MemdError::StorageError("disk full".into()).to_string()
        ));
    }
}
