//! MCP (Model Context Protocol) module
//!
//! Implements the MCP server for agent integration via JSON-RPC over stdio
//! and streamable HTTP.

pub mod error;
pub mod handlers;
pub mod protocol;
pub mod server;
pub mod tools;

pub use error::McpError;
pub use handlers::*;
pub use protocol::{Request, RequestId, Response, RpcError};
pub use server::{McpServer, run_http_server, run_server};
pub use tools::{ToolDefinition, get_all_tools, get_tool, tool_names};
