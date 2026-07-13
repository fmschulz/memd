//! Local operation error types.
//!
//! The executable reports these errors through CLI output. `McpError` remains
//! a type alias for downstream compatibility.

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// Error returned by the protocol-neutral operation layer.
#[derive(Debug, Clone)]
pub enum OperationError {
    /// Failed to parse JSON.
    ParseError(String),
    /// Invalid request structure.
    InvalidRequest(String),
    /// Method not found.
    MethodNotFound(String),
    /// Invalid method parameters.
    InvalidParams(String),
    /// Internal operation error.
    InternalError(String),
    /// Operation execution error.
    ToolError(String),
}

impl OperationError {
    /// Get the stable legacy numeric error code for this error.
    pub fn code(&self) -> i32 {
        match self {
            OperationError::ParseError(_) => PARSE_ERROR,
            OperationError::InvalidRequest(_) => INVALID_REQUEST,
            OperationError::MethodNotFound(_) => METHOD_NOT_FOUND,
            OperationError::InvalidParams(_) => INVALID_PARAMS,
            OperationError::InternalError(_) => INTERNAL_ERROR,
            OperationError::ToolError(_) => -32000,
        }
    }

    /// Get the error message
    pub fn message(&self) -> &str {
        match self {
            OperationError::ParseError(msg) => msg,
            OperationError::InvalidRequest(msg) => msg,
            OperationError::MethodNotFound(msg) => msg,
            OperationError::InvalidParams(msg) => msg,
            OperationError::InternalError(msg) => msg,
            OperationError::ToolError(msg) => msg,
        }
    }

    /// Short kebab-case label describing the variant — used as the
    /// `reason` bucket in Phase 4.4 rejection metrics.
    pub fn reason_label(&self) -> &'static str {
        match self {
            OperationError::ParseError(_) => "parse-error",
            OperationError::InvalidRequest(_) => "invalid-request",
            OperationError::MethodNotFound(_) => "method-not-found",
            OperationError::InvalidParams(_) => "invalid-params",
            OperationError::InternalError(_) => "internal-error",
            OperationError::ToolError(_) => "tool-error",
        }
    }
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationError::ParseError(msg) => write!(f, "parse error: {}", msg),
            OperationError::InvalidRequest(msg) => write!(f, "invalid request: {}", msg),
            OperationError::MethodNotFound(msg) => write!(f, "method not found: {}", msg),
            OperationError::InvalidParams(msg) => write!(f, "invalid params: {}", msg),
            OperationError::InternalError(msg) => write!(f, "internal error: {}", msg),
            OperationError::ToolError(msg) => write!(f, "tool error: {}", msg),
        }
    }
}

impl std::error::Error for OperationError {}

/// Historical name retained for one compatibility release.
pub type McpError = OperationError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_jsonrpc_spec() {
        assert_eq!(McpError::ParseError("test".into()).code(), -32700);
        assert_eq!(McpError::InvalidRequest("test".into()).code(), -32600);
        assert_eq!(McpError::MethodNotFound("test".into()).code(), -32601);
        assert_eq!(McpError::InvalidParams("test".into()).code(), -32602);
        assert_eq!(McpError::InternalError("test".into()).code(), -32603);
    }

    #[test]
    fn tool_error_uses_application_range() {
        let err = McpError::ToolError("failed".into());
        assert!(err.code() >= -32099 && err.code() <= -32000);
    }

    #[test]
    fn error_display() {
        let err = McpError::ParseError("bad json".into());
        assert_eq!(err.to_string(), "parse error: bad json");
    }
}
