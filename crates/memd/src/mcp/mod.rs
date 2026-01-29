//! MCP (Model Context Protocol) module
//!
//! Implements the MCP server for agent integration via JSON-RPC over stdio.
//! This is the primary interface for tools like Claude Code and Codex CLI.

pub mod error;
pub mod protocol;
pub mod server;
pub mod tools;

pub use error::McpError;
pub use protocol::{Request, RequestId, Response, RpcError};
pub use server::{run_server, McpServer};
pub use tools::{get_all_tools, get_tool, ToolDefinition};
