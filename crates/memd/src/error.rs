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

    /// MCP protocol errors
    #[error("protocol error: {0}")]
    ProtocolError(String),

    /// IO errors
    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),

    /// JSON serialization/deserialization errors
    #[error("json error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// TOML deserialization errors (config parsing)
    #[error("toml parse error: {0}")]
    TomlError(#[from] toml::de::Error),
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
