//! MCP tool definitions
//!
//! Defines the memory tools exposed via MCP.
//! Placeholder for Task 3 implementation.

use serde_json::Value;

/// Definition of an MCP tool
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Tool name (e.g., "memory.search")
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// JSON Schema for input parameters
    pub input_schema: Value,
}

/// Get all available tool definitions
///
/// Placeholder - will be implemented in Task 3.
pub fn get_all_tools() -> Vec<ToolDefinition> {
    Vec::new()
}

/// Get a tool definition by name
///
/// Placeholder - will be implemented in Task 3.
pub fn get_tool(_name: &str) -> Option<ToolDefinition> {
    None
}
