//! Compatibility client for legacy MCP-shaped evaluation suites.
//!
//! The public executable no longer starts an MCP stdio server. This wrapper
//! keeps the older suite call sites intact while routing operation calls
//! through `memd call`.

use serde_json::{json, Value};
use std::path::Path;
use thiserror::Error;

use crate::cli_client::{CliClient, CliClientError};

/// Errors that can occur during compatibility client operations.
#[derive(Debug, Error)]
pub enum McpClientError {
    #[error("failed to spawn process: {0}")]
    SpawnError(#[from] std::io::Error),

    #[error("failed to parse JSON: {0}")]
    ParseError(#[from] serde_json::Error),

    #[error("cli operation failed: {0}")]
    CliError(#[from] CliClientError),

    #[error("rpc error: {0}")]
    RpcError(String),
}

/// MCP-shaped test client backed by the current `memd call` CLI.
pub struct McpClient {
    client: CliClient,
    request_id: i64,
}

impl McpClient {
    /// Start a compatibility client with additional global CLI arguments.
    pub fn start_with_args(memd_path: &Path, extra_args: &[&str]) -> Result<Self, McpClientError> {
        Ok(Self {
            client: CliClient::start_with_args(memd_path, extra_args),
            request_id: 0,
        })
    }

    /// Start a compatibility client with an isolated persistent data directory.
    pub fn start(memd_path: &str) -> Result<Self, McpClientError> {
        Ok(Self {
            client: CliClient::start(memd_path)?,
            request_id: 0,
        })
    }

    /// Send a JSON-RPC-shaped request and get a JSON-RPC-shaped response.
    pub fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, McpClientError> {
        self.request_id += 1;
        match method {
            "initialize" => Ok(self.initialize_response()),
            "tools/list" => Ok(self.tools_list_response()),
            "tools/call" => {
                let params = params.unwrap_or_else(|| json!({}));
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Ok(self.error_response(-32602, "missing tools/call name"));
                };
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Ok(match self.operation_response(name, arguments) {
                    Ok(response) => response,
                    Err(error) => self.error_response(-32000, &error),
                })
            }
            _ => Ok(self.error_response(-32601, &format!("unknown method: {method}"))),
        }
    }

    /// Send raw text for invalid JSON compatibility tests.
    pub fn send_raw(&mut self, text: &str) -> Result<Value, McpClientError> {
        match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                let method = value.get("method").and_then(Value::as_str).unwrap_or("");
                let params = value.get("params").cloned();
                self.request(method, params)
            }
            Err(error) => {
                self.request_id += 1;
                Ok(self.error_response(-32700, &format!("parse error: {error}")))
            }
        }
    }

    /// Send initialize request.
    pub fn initialize(&mut self) -> Result<Value, McpClientError> {
        self.request_id += 1;
        Ok(self.initialize_response())
    }

    /// List available operations as MCP-style tools.
    pub fn tools_list(&mut self) -> Result<Value, McpClientError> {
        self.request_id += 1;
        Ok(self.tools_list_response())
    }

    /// Call an operation and return an MCP-style success envelope.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        self.request_id += 1;
        match self.operation_response(name, arguments) {
            Ok(response) => Ok(response),
            Err(error) => Err(McpClientError::RpcError(error)),
        }
    }

    /// Call an operation and return raw JSON-RPC-style success or error payloads.
    pub fn call_tool_raw(&mut self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        self.request_id += 1;
        Ok(match self.operation_response(name, arguments) {
            Ok(response) => response,
            Err(error) => self.error_response(-32000, &error),
        })
    }

    /// Check whether the backing CLI executable is available.
    pub fn is_running(&self) -> bool {
        self.client.is_available()
    }

    fn operation_response(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let payload = self
            .client
            .call(name, arguments)
            .map_err(|error| error.to_string())?;
        let text = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
        Ok(json!({
            "jsonrpc": "2.0",
            "id": self.request_id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": text
                }]
            }
        }))
    }

    fn initialize_response(&self) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": self.request_id,
            "result": {
                "protocolVersion": "cli-call",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "memd",
                    "version": "cli"
                }
            }
        })
    }

    fn tools_list_response(&self) -> Value {
        let tools = [
            "memory.add",
            "memory.add_batch",
            "memory.search",
            "memory.get",
            "memory.delete",
            "memory.stats",
            "memory.metrics",
            "memory.compact",
            "memory.health",
            "task.start",
            "task.progress",
            "task.finish",
            "artifact.create",
            "artifact.search",
            "code.find_references",
            "code.find_definition",
            "debug.find_tool_calls",
        ]
        .into_iter()
        .map(|name| json!({ "name": name }))
        .collect::<Vec<_>>();

        json!({
            "jsonrpc": "2.0",
            "id": self.request_id,
            "result": {
                "tools": tools
            }
        })
    }

    fn error_response(&self, code: i64, message: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": self.request_id,
            "error": {
                "code": code,
                "message": message
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn request_serialization_keeps_jsonrpc_shape() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": null
        });
        let serialized = serde_json::to_string(&request).unwrap();
        assert!(serialized.contains("jsonrpc"));
        assert!(serialized.contains("initialize"));
    }
}
