//! MCP client for testing
//!
//! Starts memd as a subprocess and communicates via JSON-RPC over stdio.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;
use thiserror::Error;

/// Errors that can occur during MCP client operations
#[derive(Debug, Error)]
pub enum McpClientError {
    #[error("failed to spawn process: {0}")]
    SpawnError(#[from] std::io::Error),

    #[error("failed to parse JSON: {0}")]
    ParseError(#[from] serde_json::Error),

    #[error("process stdin not available")]
    StdinNotAvailable,

    #[error("process stdout not available")]
    StdoutNotAvailable,

    #[error("read timeout or EOF")]
    ReadError,

    #[error("rpc error: {0}")]
    RpcError(String),
}

/// MCP test client that communicates with memd over stdio
pub struct McpClient {
    process: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    request_id: i64,
    _temp_dir_guard: Option<TempDir>,
}

impl McpClient {
    /// Start memd as a subprocess in MCP mode with additional arguments
    ///
    /// # Arguments
    /// * `memd_path` - Path to the memd binary
    /// * `extra_args` - Additional command-line arguments
    ///
    /// # Returns
    /// An McpClient connected to the memd process
    pub fn start_with_args(
        memd_path: &std::path::PathBuf,
        extra_args: &[&str],
    ) -> Result<Self, McpClientError> {
        let mut cmd = Command::new(memd_path);
        cmd.arg("--mode").arg("mcp");
        for arg in extra_args {
            cmd.arg(arg);
        }

        let mut process = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Show logs for debugging (model downloads, etc)
            .spawn()?;

        let stdin = process
            .stdin
            .take()
            .ok_or(McpClientError::StdinNotAvailable)?;
        let stdout = process
            .stdout
            .take()
            .ok_or(McpClientError::StdoutNotAvailable)?;

        Ok(Self {
            process,
            stdin,
            stdout: BufReader::new(stdout),
            request_id: 0,
            _temp_dir_guard: None,
        })
    }

    /// Start memd as a subprocess in MCP mode
    ///
    /// # Arguments
    /// * `memd_path` - Path to the memd binary
    ///
    /// # Returns
    /// An McpClient connected to the memd process
    pub fn start(memd_path: &str) -> Result<Self, McpClientError> {
        let path = std::path::PathBuf::from(memd_path);
        let data_dir = TempDir::new()?;
        let data_dir_arg = data_dir.path().to_string_lossy().to_string();

        let mut cmd = Command::new(&path);
        cmd.arg("--mode")
            .arg("mcp")
            .arg("--in-memory")
            .arg("--data-dir")
            .arg(data_dir_arg);

        let mut process = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = process
            .stdin
            .take()
            .ok_or(McpClientError::StdinNotAvailable)?;
        let stdout = process
            .stdout
            .take()
            .ok_or(McpClientError::StdoutNotAvailable)?;

        Ok(Self {
            process,
            stdin,
            stdout: BufReader::new(stdout),
            request_id: 0,
            _temp_dir_guard: Some(data_dir),
        })
    }

    /// Send a JSON-RPC request and get the response
    ///
    /// # Arguments
    /// * `method` - The RPC method name
    /// * `params` - Optional parameters
    ///
    /// # Returns
    /// The JSON-RPC response
    pub fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, McpClientError> {
        self.request_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.request_id,
            "method": method,
            "params": params
        });

        let request_str = serde_json::to_string(&request)?;
        writeln!(self.stdin, "{}", request_str)?;
        self.stdin.flush()?;

        let mut line = String::new();
        let bytes_read = self.stdout.read_line(&mut line)?;
        if bytes_read == 0 {
            return Err(McpClientError::ReadError);
        }

        let response: Value = serde_json::from_str(&line)?;
        Ok(response)
    }

    /// Send raw text (for invalid JSON tests)
    ///
    /// # Arguments
    /// * `text` - Raw text to send
    ///
    /// # Returns
    /// The response (may be an error response)
    pub fn send_raw(&mut self, text: &str) -> Result<Value, McpClientError> {
        writeln!(self.stdin, "{}", text)?;
        self.stdin.flush()?;

        let mut line = String::new();
        let bytes_read = self.stdout.read_line(&mut line)?;
        if bytes_read == 0 {
            return Err(McpClientError::ReadError);
        }

        let response: Value = serde_json::from_str(&line)?;
        Ok(response)
    }

    /// Send initialize request
    pub fn initialize(&mut self) -> Result<Value, McpClientError> {
        self.request(
            "initialize",
            Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "memd-evals",
                    "version": "0.1.0"
                }
            })),
        )
    }

    /// List available tools
    pub fn tools_list(&mut self) -> Result<Value, McpClientError> {
        self.request("tools/list", None)
    }

    /// Call a tool
    ///
    /// # Arguments
    /// * `name` - Tool name (e.g., "memory.add")
    /// * `arguments` - Tool arguments as JSON
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        let response = self.request(
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": arguments
            })),
        )?;

        if let Some(error) = response.get("error") {
            return Err(McpClientError::RpcError(error.to_string()));
        }

        Ok(response)
    }

    /// Call a tool and return raw JSON-RPC response, including error payloads.
    ///
    /// Use this for conformance tests that need to validate exact MCP error codes.
    pub fn call_tool_raw(&mut self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        self.request(
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": arguments
            })),
        )
    }

    /// Check if process is still running
    pub fn is_running(&mut self) -> bool {
        self.process
            .try_wait()
            .map(|s| s.is_none())
            .unwrap_or(false)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Kill the process when client is dropped
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require memd to be built first
    // They will be run as part of the eval harness

    #[test]
    fn test_json_serialization() {
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
