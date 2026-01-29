//! MCP-specific error types
//!
//! Maps MCP error conditions to JSON-RPC error codes.

use super::protocol::{error_codes, RpcError};

/// MCP-specific error variants
#[derive(Debug, Clone)]
pub enum McpError {
    /// Failed to parse JSON (maps to PARSE_ERROR -32700)
    ParseError(String),
    /// Invalid request structure (maps to INVALID_REQUEST -32600)
    InvalidRequest(String),
    /// Method not found (maps to METHOD_NOT_FOUND -32601)
    MethodNotFound(String),
    /// Invalid method parameters (maps to INVALID_PARAMS -32602)
    InvalidParams(String),
    /// Internal server error (maps to INTERNAL_ERROR -32603)
    InternalError(String),
    /// Tool execution error (application-specific, uses -32000)
    ToolError(String),
}

impl McpError {
    /// Get the JSON-RPC error code for this error
    pub fn code(&self) -> i32 {
        match self {
            McpError::ParseError(_) => error_codes::PARSE_ERROR,
            McpError::InvalidRequest(_) => error_codes::INVALID_REQUEST,
            McpError::MethodNotFound(_) => error_codes::METHOD_NOT_FOUND,
            McpError::InvalidParams(_) => error_codes::INVALID_PARAMS,
            McpError::InternalError(_) => error_codes::INTERNAL_ERROR,
            McpError::ToolError(_) => -32000, // Application-defined error range
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
}

impl From<McpError> for RpcError {
    fn from(err: McpError) -> Self {
        RpcError::new(err.code(), err.message())
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
        // Application-defined errors should be in -32000 to -32099 range
        assert!(err.code() >= -32099 && err.code() <= -32000);
    }

    #[test]
    fn error_converts_to_rpc_error() {
        let mcp_err = McpError::MethodNotFound("unknown".into());
        let rpc_err: RpcError = mcp_err.into();
        assert_eq!(rpc_err.code, -32601);
        assert_eq!(rpc_err.message, "unknown");
    }

    #[test]
    fn error_display() {
        let err = McpError::ParseError("bad json".into());
        assert_eq!(err.to_string(), "parse error: bad json");
    }
}
