//! MCP server implementation
//!
//! Handles JSON-RPC communication over stdio transport.
//! This is the primary interface for agent integration.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use super::error::McpError;
use super::protocol::{Request, Response};
use super::tools::get_all_tools;
use crate::Config;

/// MCP protocol version supported by this server
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Server name for capability negotiation
const SERVER_NAME: &str = "memd";

/// Server version
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// MCP server that handles JSON-RPC requests over stdio
pub struct McpServer {
    config: Config,
    initialized: bool,
}

impl McpServer {
    /// Create a new MCP server with the given configuration
    pub fn new(config: Config) -> Self {
        Self {
            config,
            initialized: false,
        }
    }

    /// Run the server loop, reading from stdin and writing to stdout
    ///
    /// This is the main event loop. It reads JSON-RPC requests line by line
    /// from stdin, processes them, and writes responses to stdout.
    pub async fn run(&mut self) -> crate::Result<()> {
        info!("MCP server starting");

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    error!("failed to read from stdin: {}", e);
                    break;
                }
            };

            // Skip empty lines
            if line.trim().is_empty() {
                continue;
            }

            debug!(request = %line, "received request");

            // Parse and handle the request
            let response = self.handle_line(&line).await;

            // Serialize and write response
            let json = match response.to_json() {
                Ok(j) => j,
                Err(e) => {
                    error!("failed to serialize response: {}", e);
                    continue;
                }
            };

            debug!(response = %json, "sending response");

            if writeln!(stdout, "{}", json).is_err() {
                error!("failed to write to stdout");
                break;
            }

            if stdout.flush().is_err() {
                error!("failed to flush stdout");
                break;
            }
        }

        info!("MCP server shutting down");
        Ok(())
    }

    /// Handle a single line of input (one JSON-RPC request)
    async fn handle_line(&mut self, line: &str) -> Response {
        // Try to parse the request
        let request = match Request::parse(line) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "failed to parse request");
                return Response::error(None, e.into());
            }
        };

        // Handle the request
        self.handle_request(request).await
    }

    /// Handle a parsed JSON-RPC request
    async fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id.clone();

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params).await,
            "initialized" => {
                // Notification that client is ready - no response needed
                // but we return success for notifications that have an id
                if request.is_notification() {
                    return Response::success(None, Value::Null);
                }
                Ok(Value::Null)
            }
            "tools/list" => self.handle_tools_list().await,
            "tools/call" => self.handle_tools_call(request.params).await,
            "shutdown" => {
                info!("shutdown requested");
                Ok(Value::Null)
            }
            method => {
                warn!(method = %method, "unknown method");
                Err(McpError::MethodNotFound(format!(
                    "method '{}' not found",
                    method
                )))
            }
        };

        match result {
            Ok(value) => Response::success(id, value),
            Err(e) => Response::error(id, e.into()),
        }
    }

    /// Handle the 'initialize' request
    ///
    /// Returns server capabilities and protocol version.
    async fn handle_initialize(&mut self, _params: Option<Value>) -> Result<Value, McpError> {
        if self.initialized {
            warn!("server already initialized");
        }

        self.initialized = true;

        info!(
            protocol_version = PROTOCOL_VERSION,
            server_name = SERVER_NAME,
            server_version = SERVER_VERSION,
            "server initialized"
        );

        Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        }))
    }

    /// Handle the 'tools/list' request
    ///
    /// Returns all available tool definitions.
    async fn handle_tools_list(&self) -> Result<Value, McpError> {
        let tools = get_all_tools();

        let tool_list: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema
                })
            })
            .collect();

        Ok(json!({
            "tools": tool_list
        }))
    }

    /// Handle the 'tools/call' request
    ///
    /// Dispatches to the appropriate tool handler.
    /// Currently returns stub responses - will be implemented with actual storage in later plans.
    async fn handle_tools_call(&mut self, params: Option<Value>) -> Result<Value, McpError> {
        let params = params.ok_or_else(|| McpError::InvalidParams("missing params".to_string()))?;

        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'name' field".to_string()))?;

        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

        info!(tool = %name, "tool call received");

        // Dispatch to tool handlers (stub implementations for now)
        match name {
            "memory.search" => self.handle_tool_search(arguments).await,
            "memory.add" => self.handle_tool_add(arguments).await,
            "memory.add_batch" => self.handle_tool_add_batch(arguments).await,
            "memory.get" => self.handle_tool_get(arguments).await,
            "memory.delete" => self.handle_tool_delete(arguments).await,
            "memory.stats" => self.handle_tool_stats(arguments).await,
            _ => Err(McpError::InvalidParams(format!("unknown tool '{}'", name))),
        }
    }

    // Stub tool handlers - will be implemented with actual storage in later plans

    async fn handle_tool_search(&self, args: Value) -> Result<Value, McpError> {
        let _query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'query' field".to_string()))?;

        let _tenant_id = args
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'tenant_id' field".to_string()))?;

        // Stub: return empty results
        Ok(json!({
            "content": [{
                "type": "text",
                "text": "[]"
            }]
        }))
    }

    async fn handle_tool_add(&self, args: Value) -> Result<Value, McpError> {
        let _tenant_id = args
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'tenant_id' field".to_string()))?;

        let _text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'text' field".to_string()))?;

        let _chunk_type = args
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'type' field".to_string()))?;

        // Stub: return a fake chunk_id
        Ok(json!({
            "content": [{
                "type": "text",
                "text": "{\"chunk_id\": \"00000000-0000-0000-0000-000000000000\"}"
            }]
        }))
    }

    async fn handle_tool_add_batch(&self, args: Value) -> Result<Value, McpError> {
        let _tenant_id = args
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'tenant_id' field".to_string()))?;

        let _chunks = args
            .get("chunks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| McpError::InvalidParams("missing 'chunks' array".to_string()))?;

        // Stub: return empty array of chunk_ids
        Ok(json!({
            "content": [{
                "type": "text",
                "text": "{\"chunk_ids\": []}"
            }]
        }))
    }

    async fn handle_tool_get(&self, args: Value) -> Result<Value, McpError> {
        let _tenant_id = args
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'tenant_id' field".to_string()))?;

        let _chunk_id = args
            .get("chunk_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'chunk_id' field".to_string()))?;

        // Stub: return null (not found)
        Ok(json!({
            "content": [{
                "type": "text",
                "text": "null"
            }]
        }))
    }

    async fn handle_tool_delete(&self, args: Value) -> Result<Value, McpError> {
        let _tenant_id = args
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'tenant_id' field".to_string()))?;

        let _chunk_id = args
            .get("chunk_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'chunk_id' field".to_string()))?;

        // Stub: return success
        Ok(json!({
            "content": [{
                "type": "text",
                "text": "{\"deleted\": true}"
            }]
        }))
    }

    async fn handle_tool_stats(&self, args: Value) -> Result<Value, McpError> {
        let _tenant_id = args
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'tenant_id' field".to_string()))?;

        // Stub: return empty stats
        Ok(json!({
            "content": [{
                "type": "text",
                "text": "{\"total_chunks\": 0, \"total_bytes\": 0}"
            }]
        }))
    }

    /// Get a reference to the config
    #[allow(dead_code)]
    pub fn config(&self) -> &Config {
        &self.config
    }
}

/// Run the MCP server with the given configuration
///
/// This is the main entry point for the MCP server.
pub async fn run_server(config: Config) -> crate::Result<()> {
    let mut server = McpServer::new(config);
    server.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use super::super::protocol::RequestId;

    fn test_config() -> Config {
        Config::default()
    }

    #[tokio::test]
    async fn handle_initialize() {
        let mut server = McpServer::new(test_config());
        let result = server.handle_initialize(None).await.unwrap();

        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn handle_tools_list() {
        let server = McpServer::new(test_config());
        let result = server.handle_tools_list().await.unwrap();

        assert!(result["tools"].is_array());
    }

    #[tokio::test]
    async fn handle_unknown_method() {
        let mut server = McpServer::new(test_config());
        let request = Request {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(1)),
            method: "unknown".to_string(),
            params: None,
        };

        let response = server.handle_request(request).await;
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn handle_tools_call_missing_params() {
        let mut server = McpServer::new(test_config());
        let result = server.handle_tools_call(None).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn handle_tools_call_missing_name() {
        let mut server = McpServer::new(test_config());
        let result = server.handle_tools_call(Some(json!({}))).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn handle_tool_search_stub() {
        let server = McpServer::new(test_config());
        let result = server
            .handle_tool_search(json!({
                "query": "test",
                "tenant_id": "test_tenant"
            }))
            .await
            .unwrap();

        assert!(result["content"].is_array());
    }

    #[tokio::test]
    async fn handle_tool_add_stub() {
        let server = McpServer::new(test_config());
        let result = server
            .handle_tool_add(json!({
                "tenant_id": "test_tenant",
                "text": "test content",
                "type": "doc"
            }))
            .await
            .unwrap();

        assert!(result["content"].is_array());
    }

    #[tokio::test]
    async fn handle_tool_stats_stub() {
        let server = McpServer::new(test_config());
        let result = server
            .handle_tool_stats(json!({
                "tenant_id": "test_tenant"
            }))
            .await
            .unwrap();

        assert!(result["content"].is_array());
    }
}
