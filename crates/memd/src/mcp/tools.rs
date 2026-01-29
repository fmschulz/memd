//! MCP tool definitions
//!
//! Defines the memory tools exposed via MCP following the MCP tool schema format.
//! Each tool has a name, description, and JSON Schema for input parameters.

use serde_json::{json, Value};
use std::sync::LazyLock;

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

impl ToolDefinition {
    /// Create a new tool definition
    fn new(name: impl Into<String>, description: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// Static list of all memory tools
static MEMORY_TOOLS: LazyLock<Vec<ToolDefinition>> = LazyLock::new(|| {
    vec![
        // MCP-02: memory.search
        ToolDefinition::new(
            "memory.search",
            "Search memory for relevant chunks using semantic and lexical matching",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query text"
                    },
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project identifier to scope the search"
                    },
                    "k": {
                        "type": "integer",
                        "description": "Maximum number of results to return",
                        "default": 20,
                        "minimum": 1,
                        "maximum": 100
                    },
                    "filters": {
                        "type": "object",
                        "description": "Optional filters to narrow results",
                        "properties": {
                            "types": {
                                "type": "array",
                                "items": {
                                    "type": "string",
                                    "enum": ["code", "doc", "trace", "decision", "plan", "research", "message", "summary", "other"]
                                },
                                "description": "Filter by chunk types"
                            },
                            "time_range": {
                                "type": "object",
                                "properties": {
                                    "from": {
                                        "type": "string",
                                        "format": "date-time",
                                        "description": "Start of time range (ISO 8601)"
                                    },
                                    "to": {
                                        "type": "string",
                                        "format": "date-time",
                                        "description": "End of time range (ISO 8601)"
                                    }
                                }
                            }
                        }
                    }
                },
                "required": ["query", "tenant_id"]
            }),
        ),
        // MCP-03: memory.add
        ToolDefinition::new(
            "memory.add",
            "Add a memory chunk to storage",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "text": {
                        "type": "string",
                        "description": "Content of the memory chunk"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["code", "doc", "trace", "decision", "plan", "research", "message", "summary", "other"],
                        "description": "Type of memory chunk"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project identifier"
                    },
                    "source": {
                        "type": "object",
                        "description": "Optional provenance information",
                        "properties": {
                            "uri": {
                                "type": "string",
                                "description": "Source URI"
                            },
                            "repo": {
                                "type": "string",
                                "description": "Git repository"
                            },
                            "commit": {
                                "type": "string",
                                "description": "Git commit hash"
                            },
                            "path": {
                                "type": "string",
                                "description": "File path"
                            },
                            "tool_name": {
                                "type": "string",
                                "description": "Name of tool that generated this"
                            },
                            "tool_call_id": {
                                "type": "string",
                                "description": "Tool call ID for correlation"
                            }
                        }
                    },
                    "tags": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "description": "Optional tags for filtering"
                    }
                },
                "required": ["tenant_id", "text", "type"]
            }),
        ),
        // MCP-04: memory.add_batch
        ToolDefinition::new(
            "memory.add_batch",
            "Add multiple memory chunks in a single operation",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "chunks": {
                        "type": "array",
                        "description": "Array of chunks to add",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": {
                                    "type": "string",
                                    "description": "Content of the memory chunk"
                                },
                                "type": {
                                    "type": "string",
                                    "enum": ["code", "doc", "trace", "decision", "plan", "research", "message", "summary", "other"],
                                    "description": "Type of memory chunk"
                                },
                                "project_id": {
                                    "type": "string",
                                    "description": "Optional project identifier"
                                },
                                "source": {
                                    "type": "object",
                                    "description": "Optional provenance information"
                                },
                                "tags": {
                                    "type": "array",
                                    "items": {
                                        "type": "string"
                                    },
                                    "description": "Optional tags"
                                }
                            },
                            "required": ["text", "type"]
                        }
                    }
                },
                "required": ["tenant_id", "chunks"]
            }),
        ),
        // MCP-05: memory.get
        ToolDefinition::new(
            "memory.get",
            "Get a memory chunk by its ID",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "chunk_id": {
                        "type": "string",
                        "description": "UUID of the chunk to retrieve"
                    }
                },
                "required": ["tenant_id", "chunk_id"]
            }),
        ),
        // MCP-06: memory.delete
        ToolDefinition::new(
            "memory.delete",
            "Delete a memory chunk (soft delete)",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "chunk_id": {
                        "type": "string",
                        "description": "UUID of the chunk to delete"
                    }
                },
                "required": ["tenant_id", "chunk_id"]
            }),
        ),
        // MCP-07: memory.stats
        ToolDefinition::new(
            "memory.stats",
            "Get memory statistics for a tenant",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    }
                },
                "required": ["tenant_id"]
            }),
        ),
    ]
});

/// Get all available tool definitions
///
/// Returns all 6 memory tools with their schemas.
pub fn get_all_tools() -> Vec<ToolDefinition> {
    MEMORY_TOOLS.clone()
}

/// Get a tool definition by name
///
/// Returns None if the tool name is not found.
pub fn get_tool(name: &str) -> Option<ToolDefinition> {
    MEMORY_TOOLS.iter().find(|t| t.name == name).cloned()
}

/// Get tool names as a list
pub fn tool_names() -> Vec<&'static str> {
    vec![
        "memory.search",
        "memory.add",
        "memory.add_batch",
        "memory.get",
        "memory.delete",
        "memory.stats",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_all_tools_returns_six() {
        let tools = get_all_tools();
        assert_eq!(tools.len(), 6);
    }

    #[test]
    fn all_tools_have_names() {
        let tools = get_all_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"memory.search"));
        assert!(names.contains(&"memory.add"));
        assert!(names.contains(&"memory.add_batch"));
        assert!(names.contains(&"memory.get"));
        assert!(names.contains(&"memory.delete"));
        assert!(names.contains(&"memory.stats"));
    }

    #[test]
    fn all_tools_have_descriptions() {
        let tools = get_all_tools();
        for tool in tools {
            assert!(!tool.description.is_empty(), "Tool {} has empty description", tool.name);
        }
    }

    #[test]
    fn all_tools_have_valid_schemas() {
        let tools = get_all_tools();
        for tool in tools {
            assert!(
                tool.input_schema.is_object(),
                "Tool {} schema is not an object",
                tool.name
            );
            assert!(
                tool.input_schema.get("type").is_some(),
                "Tool {} schema missing 'type'",
                tool.name
            );
            assert!(
                tool.input_schema.get("properties").is_some(),
                "Tool {} schema missing 'properties'",
                tool.name
            );
        }
    }

    #[test]
    fn get_tool_by_name() {
        let tool = get_tool("memory.search").expect("memory.search should exist");
        assert_eq!(tool.name, "memory.search");
        assert!(tool.description.contains("Search"));
    }

    #[test]
    fn get_tool_unknown_returns_none() {
        assert!(get_tool("unknown.tool").is_none());
    }

    #[test]
    fn search_schema_has_required_fields() {
        let tool = get_tool("memory.search").unwrap();
        let required = tool.input_schema.get("required").unwrap().as_array().unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"query"));
        assert!(required_strs.contains(&"tenant_id"));
    }

    #[test]
    fn add_schema_has_required_fields() {
        let tool = get_tool("memory.add").unwrap();
        let required = tool.input_schema.get("required").unwrap().as_array().unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"text"));
        assert!(required_strs.contains(&"type"));
    }

    #[test]
    fn tool_names_list() {
        let names = tool_names();
        assert_eq!(names.len(), 6);
        assert!(names.contains(&"memory.search"));
    }
}
