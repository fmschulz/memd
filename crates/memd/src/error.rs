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
}
