//! JSON-RPC 2.0 protocol types for MCP
//!
//! Implements the message format specified by JSON-RPC 2.0 and MCP protocol.
//! Handles request parsing, response building, and error objects.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::McpError;

/// JSON-RPC 2.0 standard error codes
pub mod error_codes {
    /// Invalid JSON was received by the server
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON sent is not a valid Request object
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist / is not available
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameter(s)
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal JSON-RPC error
    pub const INTERNAL_ERROR: i32 = -32603;
}

/// Request ID can be either a number or a string
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// JSON-RPC 2.0 Request object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Must be "2.0"
    pub jsonrpc: String,
    /// Request identifier (None for notifications)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// Method name to invoke
    pub method: String,
    /// Method parameters (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Parse a JSON-RPC request from a line of text
    ///
    /// # Errors
    /// Returns McpError::ParseError if the JSON is invalid or doesn't match the schema.
    pub fn parse(line: &str) -> Result<Self, McpError> {
        let request: Request =
            serde_json::from_str(line).map_err(|e| McpError::ParseError(e.to_string()))?;

        // Validate jsonrpc version
        if request.jsonrpc != "2.0" {
            return Err(McpError::InvalidRequest(format!(
                "invalid jsonrpc version '{}', must be '2.0'",
                request.jsonrpc
            )));
        }

        Ok(request)
    }

    /// Check if this is a notification (no id)
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// JSON-RPC 2.0 Error object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Error code
    pub code: i32,
    /// Human-readable error message
    pub message: String,
    /// Additional error data (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Create a new error with code and message
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Create an error with additional data
    pub fn with_data(code: i32, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    /// Create a parse error (-32700)
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(error_codes::PARSE_ERROR, message)
    }

    /// Create an invalid request error (-32600)
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_REQUEST, message)
    }

    /// Create a method not found error (-32601)
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            error_codes::METHOD_NOT_FOUND,
            format!("method '{}' not found", method),
        )
    }

    /// Create an invalid params error (-32602)
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_PARAMS, message)
    }

    /// Create an internal error (-32603)
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new(error_codes::INTERNAL_ERROR, message)
    }
}

/// JSON-RPC 2.0 Response object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Must be "2.0"
    pub jsonrpc: String,
    /// Request identifier (must match request)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// Result on success (mutually exclusive with error)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error on failure (mutually exclusive with result)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// Create a successful response with a result
    pub fn success(id: Option<RequestId>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response
    pub fn error(id: Option<RequestId>, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// Create an error response from code and message
    pub fn error_from_code(id: Option<RequestId>, code: i32, message: impl Into<String>) -> Self {
        Self::error(id, RpcError::new(code, message))
    }

    /// Serialize response to JSON string
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let request = Request::parse(json).unwrap();
        assert_eq!(request.method, "initialize");
        assert_eq!(request.id, Some(RequestId::Number(1)));
        assert!(request.params.is_none());
    }

    #[test]
    fn parse_request_with_params() {
        let json = r#"{"jsonrpc":"2.0","id":"abc","method":"test","params":{"key":"value"}}"#;
        let request = Request::parse(json).unwrap();
        assert_eq!(request.id, Some(RequestId::String("abc".to_string())));
        assert!(request.params.is_some());
    }

    #[test]
    fn parse_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"notify"}"#;
        let request = Request::parse(json).unwrap();
        assert!(request.is_notification());
    }

    #[test]
    fn parse_invalid_json() {
        let result = Request::parse("not json");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::ParseError(_)));
    }

    #[test]
    fn parse_wrong_version() {
        let json = r#"{"jsonrpc":"1.0","id":1,"method":"test"}"#;
        let result = Request::parse(json);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidRequest(_)));
    }

    #[test]
    fn success_response_serialization() {
        let response =
            Response::success(Some(RequestId::Number(1)), serde_json::json!({"ok": true}));
        let json = response.to_json().unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn error_response_serialization() {
        let response = Response::error(
            Some(RequestId::Number(1)),
            RpcError::method_not_found("unknown"),
        );
        let json = response.to_json().unwrap();
        assert!(json.contains("\"error\""));
        assert!(json.contains("-32601"));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn error_codes_are_correct() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
    }

    #[test]
    fn rpc_error_with_data() {
        let error = RpcError::with_data(
            error_codes::INVALID_PARAMS,
            "missing field",
            serde_json::json!({"field": "tenant_id"}),
        );
        assert_eq!(error.code, -32602);
        assert!(error.data.is_some());
    }
}
