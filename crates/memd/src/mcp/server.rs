//! MCP server implementation
//!
//! Handles JSON-RPC communication over stdio transport.
//! Placeholder for Task 2 implementation.

use crate::Config;

/// MCP server that handles JSON-RPC requests over stdio
pub struct McpServer {
    #[allow(dead_code)]
    config: Config,
}

impl McpServer {
    /// Create a new MCP server with the given configuration
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

/// Run the MCP server with the given configuration
///
/// This is the main entry point for the MCP server.
/// Placeholder for Task 2 implementation.
pub async fn run_server(_config: Config) -> crate::Result<()> {
    // Will be implemented in Task 2
    Ok(())
}
