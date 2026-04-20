//! MCP (Model Context Protocol) module
//!
//! Implements the MCP server for agent integration via JSON-RPC over stdio
//! and streamable HTTP.

pub mod dedup;
pub mod digest_sweeper;
pub mod error;
pub mod handlers;
pub mod markdown_export;
pub mod post_write_hooks;
pub mod protocol;
pub mod server;
pub mod tools;

pub use digest_sweeper::{spawn_digest_sweeper, DigestSweeperHandle};
pub use error::McpError;
pub use handlers::*;
pub use post_write_hooks::PostWriteEvent;
pub use protocol::{Request, RequestId, Response, RpcError};
pub use server::{run_http_server, run_server, serve_http_server, McpServer};
pub use tools::{get_all_tools, get_tool, tool_names, ToolDefinition};
