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
        // MCP-08: memory.metrics
        ToolDefinition::new(
            "memory.metrics",
            "Get system metrics including index sizes and query latency breakdown. Returns: timestamp, per-tenant index stats (chunks, embeddings, memory), latency statistics (avg, p50, p90, p99), recent query breakdown.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Optional: filter to specific tenant"
                    },
                    "include_recent": {
                        "type": "boolean",
                        "description": "Include recent query latency breakdown (default: true)"
                    }
                },
                "required": []
            }),
        ),
        // STRUCT-05: code.find_definition
        ToolDefinition::new(
            "code.find_definition",
            "Find where a symbol (function, class, variable) is defined. Returns file path, line number, signature, and documentation.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "name": {
                        "type": "string",
                        "description": "Symbol name to find"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project scope"
                    }
                },
                "required": ["tenant_id", "name"]
            }),
        ),
        // STRUCT-06: code.find_references
        ToolDefinition::new(
            "code.find_references",
            "Find all usages of a symbol across the codebase. Returns both definitions and call sites.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "name": {
                        "type": "string",
                        "description": "Symbol name to find usages of"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project scope"
                    }
                },
                "required": ["tenant_id", "name"]
            }),
        ),
        // STRUCT-07: code.find_callers
        ToolDefinition::new(
            "code.find_callers",
            "Find all functions that call a given function. Supports multi-hop traversal to find indirect callers.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "name": {
                        "type": "string",
                        "description": "Function name"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "How many hops to traverse (1-3, default 1)",
                        "minimum": 1,
                        "maximum": 3,
                        "default": 1
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project scope"
                    }
                },
                "required": ["tenant_id", "name"]
            }),
        ),
        // STRUCT-08: code.find_imports
        ToolDefinition::new(
            "code.find_imports",
            "Find files that import a given module. Returns file paths and import details.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "module": {
                        "type": "string",
                        "description": "Module name to search for"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project scope"
                    }
                },
                "required": ["tenant_id", "module"]
            }),
        ),
        // STRUCT-11: debug.find_tool_calls
        ToolDefinition::new(
            "debug.find_tool_calls",
            "Find past tool invocations, optionally filtered by name and time range. Returns tool name, input/output, errors, and duration.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "Filter by tool name (e.g., 'memory.search')"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Filter by session ID"
                    },
                    "time_from": {
                        "type": "string",
                        "format": "date-time",
                        "description": "Start of time range (ISO 8601)"
                    },
                    "time_to": {
                        "type": "string",
                        "format": "date-time",
                        "description": "End of time range (ISO 8601)"
                    },
                    "errors_only": {
                        "type": "boolean",
                        "description": "Only return calls that resulted in errors",
                        "default": false
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results",
                        "default": 50,
                        "maximum": 100
                    }
                },
                "required": ["tenant_id"]
            }),
        ),
        // STRUCT-12: debug.find_errors
        ToolDefinition::new(
            "debug.find_errors",
            "Find stack traces and errors, optionally filtered by error signature or function. Returns error type, message, and stack frames.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "error_signature": {
                        "type": "string",
                        "description": "Filter by error type/signature (e.g., 'TypeError')"
                    },
                    "function_name": {
                        "type": "string",
                        "description": "Find errors where function is in stack"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Find errors in this file"
                    },
                    "time_from": {
                        "type": "string",
                        "format": "date-time",
                        "description": "Start of time range"
                    },
                    "time_to": {
                        "type": "string",
                        "format": "date-time",
                        "description": "End of time range"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results",
                        "default": 50,
                        "maximum": 100
                    },
                    "include_frames": {
                        "type": "boolean",
                        "description": "Include stack frames in response",
                        "default": true
                    }
                },
                "required": ["tenant_id"]
            }),
        ),
        // COMPACT-05: memory.compact
        ToolDefinition::new(
            "memory.compact",
            "Run compaction to clean up deleted chunks, merge segments, and rebuild indexes. \
            Use 'force: true' to run regardless of thresholds.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "force": {
                        "type": "boolean",
                        "description": "Force compaction regardless of thresholds (default: false)",
                        "default": false
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
        "memory.metrics",
        "memory.compact",
        "code.find_definition",
        "code.find_references",
        "code.find_callers",
        "code.find_imports",
        "debug.find_tool_calls",
        "debug.find_errors",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_all_tools_returns_fourteen() {
        let tools = get_all_tools();
        assert_eq!(tools.len(), 14);
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
            assert!(
                !tool.description.is_empty(),
                "Tool {} has empty description",
                tool.name
            );
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
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"query"));
        assert!(required_strs.contains(&"tenant_id"));
    }

    #[test]
    fn add_schema_has_required_fields() {
        let tool = get_tool("memory.add").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"text"));
        assert!(required_strs.contains(&"type"));
    }

    #[test]
    fn tool_names_list() {
        let names = tool_names();
        assert_eq!(names.len(), 14);
        assert!(names.contains(&"memory.search"));
        assert!(names.contains(&"memory.metrics"));
        assert!(names.contains(&"memory.compact"));
        assert!(names.contains(&"code.find_definition"));
        assert!(names.contains(&"code.find_references"));
        assert!(names.contains(&"code.find_callers"));
        assert!(names.contains(&"code.find_imports"));
        assert!(names.contains(&"debug.find_tool_calls"));
        assert!(names.contains(&"debug.find_errors"));
    }

    #[test]
    fn code_find_definition_schema_has_required_fields() {
        let tool = get_tool("code.find_definition").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"name"));
    }

    #[test]
    fn code_find_callers_has_depth_property() {
        let tool = get_tool("code.find_callers").unwrap();
        let props = tool.input_schema.get("properties").unwrap();
        let depth = props.get("depth").unwrap();
        assert_eq!(depth.get("minimum").unwrap(), 1);
        assert_eq!(depth.get("maximum").unwrap(), 3);
    }

    #[test]
    fn code_find_imports_schema_has_required_fields() {
        let tool = get_tool("code.find_imports").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"module"));
    }

    #[test]
    fn debug_find_tool_calls_schema_has_required_fields() {
        let tool = get_tool("debug.find_tool_calls").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
    }

    #[test]
    fn debug_find_tool_calls_has_optional_filters() {
        let tool = get_tool("debug.find_tool_calls").unwrap();
        let props = tool.input_schema.get("properties").unwrap();
        assert!(props.get("tool_name").is_some());
        assert!(props.get("session_id").is_some());
        assert!(props.get("time_from").is_some());
        assert!(props.get("time_to").is_some());
        assert!(props.get("errors_only").is_some());
        assert!(props.get("limit").is_some());
    }

    #[test]
    fn debug_find_errors_schema_has_required_fields() {
        let tool = get_tool("debug.find_errors").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
    }

    #[test]
    fn debug_find_errors_has_optional_filters() {
        let tool = get_tool("debug.find_errors").unwrap();
        let props = tool.input_schema.get("properties").unwrap();
        assert!(props.get("error_signature").is_some());
        assert!(props.get("function_name").is_some());
        assert!(props.get("file_path").is_some());
        assert!(props.get("time_from").is_some());
        assert!(props.get("time_to").is_some());
        assert!(props.get("limit").is_some());
        assert!(props.get("include_frames").is_some());
    }
}
