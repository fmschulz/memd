// Error types - placeholder for Task 2
// This file will be fully implemented in Task 2

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
    #[error("serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}

/// Result type alias for memd operations
pub type Result<T> = std::result::Result<T, MemdError>;
