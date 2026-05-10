//! Local operation error types.
//!
//! The enum name is retained for API compatibility with the existing handler
//! layer; the executable reports these errors through CLI output.

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// Operation-specific error variants.
#[derive(Debug, Clone)]
pub enum McpError {
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

impl McpError {
    /// Get the stable legacy numeric error code for this error.
    pub fn code(&self) -> i32 {
        match self {
            McpError::ParseError(_) => PARSE_ERROR,
            McpError::InvalidRequest(_) => INVALID_REQUEST,
            McpError::MethodNotFound(_) => METHOD_NOT_FOUND,
            McpError::InvalidParams(_) => INVALID_PARAMS,
            McpError::InternalError(_) => INTERNAL_ERROR,
            McpError::ToolError(_) => -32000,
        }
    }

    /// Get the error message
    pub fn message(&self) -> &str {
        match self {
            McpError::ParseError(msg) => msg,
            McpError::InvalidRequest(msg) => msg,
            McpError::MethodNotFound(msg) => msg,
            McpError::InvalidParams(msg) => msg,
            McpError::InternalError(msg) => msg,
            McpError::ToolError(msg) => msg,
        }
    }

    /// Short kebab-case label describing the variant — used as the
    /// `reason` bucket in Phase 4.4 rejection metrics.
    pub fn reason_label(&self) -> &'static str {
        match self {
            McpError::ParseError(_) => "parse-error",
            McpError::InvalidRequest(_) => "invalid-request",
            McpError::MethodNotFound(_) => "method-not-found",
            McpError::InvalidParams(_) => "invalid-params",
            McpError::InternalError(_) => "internal-error",
            McpError::ToolError(_) => "tool-error",
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::ParseError(msg) => write!(f, "parse error: {}", msg),
            McpError::InvalidRequest(msg) => write!(f, "invalid request: {}", msg),
            McpError::MethodNotFound(msg) => write!(f, "method not found: {}", msg),
            McpError::InvalidParams(msg) => write!(f, "invalid params: {}", msg),
            McpError::InternalError(msg) => write!(f, "internal error: {}", msg),
            McpError::ToolError(msg) => write!(f, "tool error: {}", msg),
        }
    }
}

impl std::error::Error for McpError {}

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
