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
                    "mode": {
                        "type": "string",
                        "enum": ["generic", "brief_project", "resume_task", "find_failures", "find_decisions", "find_evidence"],
                        "default": "generic",
                        "description": "Optional retrieval intent that biases results toward generated briefs or canonical task/artifact summaries"
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
                            "episode_id": {
                                "type": "string",
                                "description": "Filter by episode identifier"
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
                    "episode_id": {
                        "type": "string",
                        "description": "Optional episode identifier for session grouping"
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
                                "episode_id": {
                                    "type": "string",
                                    "description": "Optional episode identifier"
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
        ToolDefinition::new(
            "task.start",
            "Start a scientific/task memory record and store retrieval projections",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project identifier"
                    },
                    "parent_task_id": {
                        "type": "string",
                        "description": "Optional parent task identifier"
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Optional agent identifier"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional session identifier"
                    },
                    "goal": {
                        "type": "string",
                        "description": "Primary task goal"
                    },
                    "motivation": {
                        "type": "string",
                        "description": "Why this task matters"
                    },
                    "hypothesis": {
                        "type": "string",
                        "description": "Working hypothesis to evaluate"
                    },
                    "scientific_question": {
                        "type": "string",
                        "description": "Scientific or technical question the task should answer"
                    },
                    "dataset_refs": {
                        "type": "array",
                        "description": "Datasets relevant to the task",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "version": {"type": "string"},
                                "description": {"type": "string"}
                            },
                            "required": ["name"]
                        }
                    },
                    "entity_refs": {
                        "type": "array",
                        "description": "Referenced entities relevant to the task",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "entity_type": {"type": "string"},
                                "role": {"type": "string"}
                            },
                            "required": ["name", "entity_type"]
                        }
                    },
                    "expected_outputs": {
                        "type": "array",
                        "description": "Expected outputs before substantive work starts",
                        "items": {"type": "string"}
                    },
                    "provenance": {
                        "type": "object",
                        "description": "Optional provenance for the task artifact",
                        "properties": {
                            "uri": {"type": "string"},
                            "repo": {"type": "string"},
                            "commit": {"type": "string"},
                            "path": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "tool_version": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        }
                    }
                },
                "required": [
                    "tenant_id",
                    "goal",
                    "motivation",
                    "hypothesis",
                    "scientific_question",
                    "dataset_refs",
                    "expected_outputs"
                ]
            }),
        ),
        ToolDefinition::new(
            "task.finish",
            "Finish a scientific/task record and store worked, failed, and validation projections",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Identifier of the task being finished"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project identifier"
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Optional agent identifier"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional session identifier"
                    },
                    "status": {
                        "type": "string",
                        "description": "Final task status, defaults to completed"
                    },
                    "goal": {
                        "type": "string",
                        "description": "Optional goal restatement for the final artifact"
                    },
                    "scientific_question": {
                        "type": "string",
                        "description": "Optional scientific question restatement"
                    },
                    "dataset_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "version": {"type": "string"},
                                "description": {"type": "string"}
                            },
                            "required": ["name"]
                        }
                    },
                    "entity_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "entity_type": {"type": "string"},
                                "role": {"type": "string"}
                            },
                            "required": ["name", "entity_type"]
                        }
                    },
                    "what_worked": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Important approaches or outcomes that worked"
                    },
                    "what_failed": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Important failed attempts or blockers"
                    },
                    "validation": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Validation statements or checks"
                    },
                    "uncertainty": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Remaining uncertainty"
                    },
                    "followups": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Recommended follow-up steps"
                    },
                    "confidence": {
                        "type": "number",
                        "minimum": 0,
                        "maximum": 1,
                        "description": "Confidence score between 0 and 1"
                    },
                    "provenance": {
                        "type": "object",
                        "properties": {
                            "uri": {"type": "string"},
                            "repo": {"type": "string"},
                            "commit": {"type": "string"},
                            "path": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "tool_version": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        }
                    }
                },
                "required": [
                    "tenant_id",
                    "task_id",
                    "what_worked",
                    "what_failed",
                    "validation",
                    "uncertainty",
                    "followups",
                    "confidence"
                ]
            }),
        ),
        ToolDefinition::new(
            "task.progress",
            "Record a meaningful scientific/task checkpoint with blockers, failed attempts, and next steps",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "session_id": {"type": "string"},
                    "summary": {"type": "string"},
                    "blockers": {"type": "array", "items": {"type": "string"}},
                    "failed_attempts": {"type": "array", "items": {"type": "string"}},
                    "next_step": {"type": "string"},
                    "dataset_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "version": {"type": "string"},
                                "description": {"type": "string"}
                            },
                            "required": ["name"]
                        }
                    },
                    "entity_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "entity_type": {"type": "string"},
                                "role": {"type": "string"}
                            },
                            "required": ["name", "entity_type"]
                        }
                    },
                    "provenance": {
                        "type": "object",
                        "properties": {
                            "uri": {"type": "string"},
                            "repo": {"type": "string"},
                            "commit": {"type": "string"},
                            "path": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "tool_version": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        }
                    }
                },
                "required": ["tenant_id", "task_id", "summary", "next_step"]
            }),
        ),
        ToolDefinition::new(
            "task.run_start",
            "Record the start of a substantive tool or workflow run with parameters, inputs, and why it was chosen",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "session_id": {"type": "string"},
                    "tool_name": {"type": "string"},
                    "tool_version": {"type": "string"},
                    "command": {"type": "string"},
                    "why_chosen": {"type": "string"},
                    "parameters": {"type": "object"},
                    "inputs": {"type": "array", "items": {"type": "string"}},
                    "summary": {"type": "string"},
                    "dataset_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "version": {"type": "string"},
                                "description": {"type": "string"}
                            },
                            "required": ["name"]
                        }
                    },
                    "entity_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "entity_type": {"type": "string"},
                                "role": {"type": "string"}
                            },
                            "required": ["name", "entity_type"]
                        }
                    },
                    "provenance": {
                        "type": "object",
                        "properties": {
                            "uri": {"type": "string"},
                            "repo": {"type": "string"},
                            "commit": {"type": "string"},
                            "path": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "tool_version": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        }
                    }
                },
                "required": ["tenant_id", "task_id", "tool_name", "command", "why_chosen", "parameters", "inputs"]
            }),
        ),
        ToolDefinition::new(
            "task.run_finish",
            "Record the completion of a substantive run with status, outputs, metrics, and notes",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "session_id": {"type": "string"},
                    "status": {"type": "string"},
                    "tool_name": {"type": "string"},
                    "tool_version": {"type": "string"},
                    "command": {"type": "string"},
                    "outputs": {"type": "array", "items": {"type": "string"}},
                    "metrics": {"type": "object"},
                    "notes": {"type": "string"},
                    "validation": {"type": "array", "items": {"type": "string"}},
                    "dataset_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "version": {"type": "string"},
                                "description": {"type": "string"}
                            },
                            "required": ["name"]
                        }
                    },
                    "entity_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "entity_type": {"type": "string"},
                                "role": {"type": "string"}
                            },
                            "required": ["name", "entity_type"]
                        }
                    },
                    "provenance": {
                        "type": "object",
                        "properties": {
                            "uri": {"type": "string"},
                            "repo": {"type": "string"},
                            "commit": {"type": "string"},
                            "path": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "tool_version": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        }
                    }
                },
                "required": ["tenant_id", "task_id", "status", "outputs", "notes"]
            }),
        ),
        ToolDefinition::new(
            "task.add_evidence",
            "Record concrete evidence for a task, including evidence kind and optional metrics",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "session_id": {"type": "string"},
                    "summary": {"type": "string"},
                    "evidence_kind": {"type": "string"},
                    "supports_claim": {"type": "boolean"},
                    "metric_name": {"type": "string"},
                    "metric_value": {},
                    "metrics": {"type": "object"},
                    "dataset_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "version": {"type": "string"},
                                "description": {"type": "string"}
                            },
                            "required": ["name"]
                        }
                    },
                    "entity_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "entity_type": {"type": "string"},
                                "role": {"type": "string"}
                            },
                            "required": ["name", "entity_type"]
                        }
                    },
                    "provenance": {
                        "type": "object",
                        "properties": {
                            "uri": {"type": "string"},
                            "repo": {"type": "string"},
                            "commit": {"type": "string"},
                            "path": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "tool_version": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        }
                    }
                },
                "required": ["tenant_id", "task_id", "summary", "evidence_kind", "supports_claim"]
            }),
        ),
        ToolDefinition::new(
            "task.get",
            "Get the canonical artifact history for a task",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"}
                },
                "required": ["tenant_id", "task_id"]
            }),
        ),
        ToolDefinition::new(
            "task.search",
            "Search task-oriented retrieval projections using exact task filters and lexical ranking within the filtered candidate set",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "query": {"type": "string", "default": ""},
                    "k": {
                        "type": "integer",
                        "default": 20,
                        "minimum": 1,
                        "maximum": 100
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["generic", "brief_project", "resume_task", "find_failures", "find_decisions", "find_evidence"],
                        "default": "generic"
                    },
                    "filters": {
                        "type": "object",
                        "properties": {
                            "task_id": {"type": "string"},
                            "artifact_kind": {
                                "type": "string",
                                "enum": ["task_start", "task_progress", "run_start", "run_finish", "evidence", "review", "revision", "verification", "decision", "digest", "task_finish"]
                            },
                            "status": {"type": "string"},
                            "challenge_id": {"type": "string"},
                            "thread_id": {"type": "string"},
                            "reply_to_artifact_id": {"type": "string"},
                            "artifact_role": {"type": "string"},
                            "dataset_name": {"type": "string"},
                            "dataset_version": {"type": "string"},
                            "entity_name": {"type": "string"},
                            "entity_type": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "project_id": {"type": "string"},
                            "agent_id": {"type": "string"},
                            "session_id": {"type": "string"},
                            "requested_action": {"type": "string"},
                            "verification_status": {"type": "string"},
                            "relation_kind": {"type": "string"}
                        }
                    }
                },
                "required": ["tenant_id"]
            }),
        ),
        ToolDefinition::new(
            "artifact.create",
            "Create a canonical knowledge artifact with optional collaboration, verification, and safety metadata.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "artifact_kind": {
                        "type": "string",
                        "enum": ["task_start", "task_progress", "run_start", "run_finish", "evidence", "review", "revision", "verification", "decision", "digest", "task_finish"]
                    },
                    "task_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "parent_task_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "session_id": {"type": "string"},
                    "status": {"type": "string"},
                    "artifact_role": {"type": "string"},
                    "challenge_id": {"type": "string"},
                    "thread_id": {"type": "string"},
                    "reply_to_artifact_id": {"type": "string"},
                    "relation_kind": {"type": "string"},
                    "goal": {"type": "string"},
                    "motivation": {"type": "string"},
                    "hypothesis": {"type": "string"},
                    "scientific_question": {"type": "string"},
                    "method_summary": {"type": "string"},
                    "summary": {"type": "string"},
                    "evidence_kind": {"type": "string"},
                    "supports_claim": {"type": "boolean"},
                    "blockers": {"type": "array", "items": {"type": "string"}},
                    "what_worked": {"type": "array", "items": {"type": "string"}},
                    "what_failed": {"type": "array", "items": {"type": "string"}},
                    "validation": {"type": "array", "items": {"type": "string"}},
                    "uncertainty": {"type": "array", "items": {"type": "string"}},
                    "followups": {"type": "array", "items": {"type": "string"}},
                    "expected_outputs": {"type": "array", "items": {"type": "string"}},
                    "related_artifact_ids": {"type": "array", "items": {"type": "string"}},
                    "contributors": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "contributor_id": {"type": "string"},
                                "display_name": {"type": "string"},
                                "role": {"type": "string"},
                                "contribution": {"type": "string"}
                            },
                            "required": ["contributor_id"]
                        }
                    },
                    "dataset_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "version": {"type": "string"},
                                "description": {"type": "string"}
                            },
                            "required": ["name"]
                        }
                    },
                    "entity_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "entity_type": {"type": "string"},
                                "role": {"type": "string"}
                            },
                            "required": ["name", "entity_type"]
                        }
                    },
                    "tool_name": {"type": "string"},
                    "tool_version": {"type": "string"},
                    "command": {"type": "string"},
                    "parameters": {"type": "object"},
                    "inputs": {"type": "array", "items": {"type": "string"}},
                    "outputs": {"type": "array", "items": {"type": "string"}},
                    "metrics": {"type": "object"},
                    "why_chosen": {"type": "string"},
                    "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                    "requested_action": {"type": "string"},
                    "verification_status": {"type": "string"},
                    "compute_budget": {},
                    "cost_actual": {},
                    "data_access_level": {"type": "string"},
                    "policy_tags": {"type": "array", "items": {"type": "string"}},
                    "allowed_tools": {"type": "array", "items": {"type": "string"}},
                    "approval_state": {"type": "string"},
                    "provenance": {
                        "type": "object",
                        "properties": {
                            "uri": {"type": "string"},
                            "repo": {"type": "string"},
                            "commit": {"type": "string"},
                            "path": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "tool_version": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        }
                    }
                },
                "required": ["tenant_id", "artifact_kind"]
            }),
        ),
        ToolDefinition::new(
            "artifact.get",
            "Get one canonical knowledge artifact by artifact_id.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "artifact_id": {"type": "string"}
                },
                "required": ["tenant_id", "artifact_id"]
            }),
        ),
        ToolDefinition::new(
            "artifact.search",
            "Search canonical knowledge artifacts by ranking their retrieval projections, then return the linked canonical artifacts.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "query": {"type": "string", "default": ""},
                    "k": {
                        "type": "integer",
                        "default": 20,
                        "minimum": 1,
                        "maximum": 100
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["generic", "brief_project", "resume_task", "find_failures", "find_decisions", "find_evidence"],
                        "default": "generic"
                    },
                    "filters": {
                        "type": "object",
                        "properties": {
                            "task_id": {"type": "string"},
                            "artifact_kind": {
                                "type": "string",
                                "enum": ["task_start", "task_progress", "run_start", "run_finish", "evidence", "review", "revision", "verification", "decision", "digest", "task_finish"]
                            },
                            "status": {"type": "string"},
                            "challenge_id": {"type": "string"},
                            "thread_id": {"type": "string"},
                            "reply_to_artifact_id": {"type": "string"},
                            "artifact_role": {"type": "string"},
                            "dataset_name": {"type": "string"},
                            "dataset_version": {"type": "string"},
                            "entity_name": {"type": "string"},
                            "entity_type": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "project_id": {"type": "string"},
                            "agent_id": {"type": "string"},
                            "session_id": {"type": "string"},
                            "requested_action": {"type": "string"},
                            "verification_status": {"type": "string"},
                            "relation_kind": {"type": "string"}
                        }
                    }
                },
                "required": ["tenant_id"]
            }),
        ),
        ToolDefinition::new(
            "artifact.list_thread",
            "List canonical artifacts that belong to the same thread, addressed either by thread_id or by an existing artifact_id.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "thread_id": {"type": "string"},
                    "artifact_id": {"type": "string"}
                },
                "required": ["tenant_id"]
            }),
        ),
        ToolDefinition::new(
            "context.brief_project",
            "Generate or refresh a persisted project brief digest and return an actionable summary derived from task and artifact state.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "query": {"type": "string", "default": ""},
                    "k": {"type": "integer", "default": 20, "minimum": 1, "maximum": 100},
                    "include_related_projects": {"type": "boolean", "default": true}
                },
                "required": ["tenant_id", "project_id"]
            }),
        ),
        ToolDefinition::new(
            "task.resume",
            "Generate or refresh a persisted task resume digest and return the current task resume summary.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "query": {"type": "string", "default": ""},
                    "k": {"type": "integer", "default": 20, "minimum": 1, "maximum": 100}
                },
                "required": ["tenant_id", "task_id"]
            }),
        ),
        ToolDefinition::new(
            "artifact.find_failures",
            "Generate or refresh a failure library digest and return ranked failure summaries from task/artifact state.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "query": {"type": "string", "default": ""},
                    "k": {"type": "integer", "default": 20, "minimum": 1, "maximum": 100}
                },
                "required": ["tenant_id"]
            }),
        ),
        ToolDefinition::new(
            "artifact.find_decisions",
            "Generate or refresh a decision library digest and return explicit or inferred decisions.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "query": {"type": "string", "default": ""},
                    "k": {"type": "integer", "default": 20, "minimum": 1, "maximum": 100}
                },
                "required": ["tenant_id"]
            }),
        ),
        ToolDefinition::new(
            "artifact.find_evidence",
            "Generate or refresh an evidence library digest and return ranked evidence highlights.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "query": {"type": "string", "default": ""},
                    "k": {"type": "integer", "default": 20, "minimum": 1, "maximum": 100}
                },
                "required": ["tenant_id"]
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
        // FEEDBACK-01: memory.feedback
        ToolDefinition::new(
            "memory.feedback",
            "Record relevance feedback for a retrieved chunk so future ranking can adapt",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "query": {
                        "type": "string",
                        "description": "Query text the feedback applies to"
                    },
                    "chunk_id": {
                        "type": "string",
                        "description": "UUID of the chunk that was judged"
                    },
                    "relevance": {
                        "type": "string",
                        "enum": ["relevant", "irrelevant"],
                        "description": "Relevance judgment for this query/chunk pair"
                    }
                },
                "required": ["tenant_id", "query", "chunk_id", "relevance"]
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
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project identifier for digest compaction scope"
                    },
                    "digest_modes": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["generic", "brief_project", "resume_task", "find_failures", "find_decisions", "find_evidence"]
                        },
                        "description": "Optional digest modes to rebuild during compaction"
                    },
                    "force_digest_rebuild": {
                        "type": "boolean",
                        "description": "Force digest regeneration even when storage compaction thresholds are not exceeded",
                        "default": false
                    }
                },
                "required": ["tenant_id"]
            }),
        ),
        // MEMORY-09: memory.consolidate_episode
        ToolDefinition::new(
            "memory.consolidate_episode",
            "Create a summary chunk for one episode and optionally delete source chunks.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "episode_id": {
                        "type": "string",
                        "description": "Episode identifier to consolidate"
                    },
                    "max_chunks": {
                        "type": "integer",
                        "description": "Maximum episode chunks to include in summary",
                        "default": 50,
                        "minimum": 1
                    },
                    "retain_source_chunks": {
                        "type": "boolean",
                        "description": "Keep source chunks after summary creation (default: true)",
                        "default": true
                    }
                },
                "required": ["tenant_id", "episode_id"]
            }),
        ),
        // CONTEXT-01: context.list_subsystems
        ToolDefinition::new(
            "context.list_subsystems",
            "List known context subsystems discovered from tags (ctx:subsystem:<key>).",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "prefix": {
                        "type": "string",
                        "description": "Optional subsystem key prefix filter"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of subsystem entries",
                        "default": 50,
                        "minimum": 1,
                        "maximum": 500
                    }
                },
                "required": ["tenant_id"]
            }),
        ),
        // CONTEXT-02: context.get_files_for_subsystem
        ToolDefinition::new(
            "context.get_files_for_subsystem",
            "List files linked to a subsystem via ctx:file:<path> tags or chunk source paths.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "subsystem_key": {
                        "type": "string",
                        "description": "Subsystem key (from ctx:subsystem:<key>)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of files to return",
                        "default": 50,
                        "minimum": 1,
                        "maximum": 2000
                    }
                },
                "required": ["tenant_id", "subsystem_key"]
            }),
        ),
        // CONTEXT-03: context.search_context_documents
        ToolDefinition::new(
            "context.search_context_documents",
            "Search codified context documents (docs/plans/decisions/summaries) using retrieval and context tags.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "query": {
                        "type": "string",
                        "description": "Context search query"
                    },
                    "k": {
                        "type": "integer",
                        "description": "Maximum results",
                        "default": 20,
                        "minimum": 1,
                        "maximum": 100
                    },
                    "subsystem_key": {
                        "type": "string",
                        "description": "Optional subsystem filter"
                    },
                    "tier": {
                        "type": "string",
                        "enum": ["hot", "cold"],
                        "description": "Optional context tier filter"
                    }
                },
                "required": ["tenant_id", "query"]
            }),
        ),
        // CONTEXT-04: context.find_relevant_context
        ToolDefinition::new(
            "context.find_relevant_context",
            "Find context for a task; optionally prepends hot-context chunks before regular retrieval results.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "task": {
                        "type": "string",
                        "description": "Task description used for retrieval"
                    },
                    "k": {
                        "type": "integer",
                        "description": "Maximum combined results",
                        "default": 20,
                        "minimum": 1,
                        "maximum": 100
                    },
                    "subsystem_keys": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional subsystem filters"
                    },
                    "include_hot": {
                        "type": "boolean",
                        "description": "Prepend hot-context entries before regular retrieval",
                        "default": true
                    }
                },
                "required": ["tenant_id", "task"]
            }),
        ),
        // CONTEXT-05: context.suggest_agent
        ToolDefinition::new(
            "context.suggest_agent",
            "Suggest specialist agents using ctx:agent and ctx:trigger tags against task text and changed files.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "task": {
                        "type": "string",
                        "description": "Task description"
                    },
                    "changed_files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional changed file paths for trigger matching"
                    },
                    "k": {
                        "type": "integer",
                        "description": "Maximum number of suggestions",
                        "default": 3,
                        "minimum": 1,
                        "maximum": 100
                    }
                },
                "required": ["tenant_id", "task"]
            }),
        ),
        // CONTEXT-06: context.get_hot_context
        ToolDefinition::new(
            "context.get_hot_context",
            "Return recent chunks tagged as hot context (ctx:tier:hot).",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "k": {
                        "type": "integer",
                        "description": "Maximum results",
                        "default": 20,
                        "minimum": 1,
                        "maximum": 100
                    }
                },
                "required": ["tenant_id"]
            }),
        ),
    ]
});

/// Get all available tool definitions
///
/// Returns all MCP tool definitions with their schemas.
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
        "task.start",
        "task.progress",
        "task.run_start",
        "task.run_finish",
        "task.add_evidence",
        "task.finish",
        "task.get",
        "task.search",
        "task.resume",
        "artifact.create",
        "artifact.get",
        "artifact.search",
        "artifact.find_failures",
        "artifact.find_decisions",
        "artifact.find_evidence",
        "artifact.list_thread",
        "memory.get",
        "memory.delete",
        "memory.feedback",
        "memory.stats",
        "memory.metrics",
        "memory.compact",
        "memory.consolidate_episode",
        "context.list_subsystems",
        "context.get_files_for_subsystem",
        "context.search_context_documents",
        "context.find_relevant_context",
        "context.brief_project",
        "context.suggest_agent",
        "context.get_hot_context",
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
    fn get_all_tools_returns_thirty_nine() {
        let tools = get_all_tools();
        assert_eq!(tools.len(), 39);
    }

    #[test]
    fn all_tools_have_names() {
        let tools = get_all_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"memory.search"));
        assert!(names.contains(&"memory.add"));
        assert!(names.contains(&"memory.add_batch"));
        assert!(names.contains(&"task.start"));
        assert!(names.contains(&"task.progress"));
        assert!(names.contains(&"task.run_start"));
        assert!(names.contains(&"task.run_finish"));
        assert!(names.contains(&"task.add_evidence"));
        assert!(names.contains(&"task.finish"));
        assert!(names.contains(&"task.get"));
        assert!(names.contains(&"task.search"));
        assert!(names.contains(&"artifact.create"));
        assert!(names.contains(&"artifact.get"));
        assert!(names.contains(&"artifact.search"));
        assert!(names.contains(&"artifact.list_thread"));
        assert!(names.contains(&"memory.get"));
        assert!(names.contains(&"memory.delete"));
        assert!(names.contains(&"memory.feedback"));
        assert!(names.contains(&"memory.stats"));
        assert!(names.contains(&"memory.consolidate_episode"));
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
    fn task_start_schema_has_required_fields() {
        let tool = get_tool("task.start").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"goal"));
        assert!(required_strs.contains(&"motivation"));
        assert!(required_strs.contains(&"hypothesis"));
        assert!(required_strs.contains(&"scientific_question"));
        assert!(required_strs.contains(&"dataset_refs"));
        assert!(required_strs.contains(&"expected_outputs"));
    }

    #[test]
    fn task_finish_schema_has_required_fields() {
        let tool = get_tool("task.finish").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"task_id"));
        assert!(required_strs.contains(&"what_worked"));
        assert!(required_strs.contains(&"what_failed"));
        assert!(required_strs.contains(&"validation"));
        assert!(required_strs.contains(&"uncertainty"));
        assert!(required_strs.contains(&"followups"));
        assert!(required_strs.contains(&"confidence"));
    }

    #[test]
    fn task_get_schema_has_required_fields() {
        let tool = get_tool("task.get").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"task_id"));
    }

    #[test]
    fn task_search_schema_has_tenant_required() {
        let tool = get_tool("task.search").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(tool
            .input_schema
            .get("properties")
            .and_then(|props| props.get("filters"))
            .is_some());
    }

    #[test]
    fn artifact_create_schema_has_required_fields() {
        let tool = get_tool("artifact.create").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"artifact_kind"));
    }

    #[test]
    fn tool_names_list() {
        let names = tool_names();
        assert_eq!(names.len(), 39);
        assert!(names.contains(&"memory.search"));
        assert!(names.contains(&"memory.metrics"));
        assert!(names.contains(&"memory.feedback"));
        assert!(names.contains(&"memory.compact"));
        assert!(names.contains(&"memory.consolidate_episode"));
        assert!(names.contains(&"task.start"));
        assert!(names.contains(&"task.progress"));
        assert!(names.contains(&"task.run_start"));
        assert!(names.contains(&"task.run_finish"));
        assert!(names.contains(&"task.add_evidence"));
        assert!(names.contains(&"task.finish"));
        assert!(names.contains(&"task.get"));
        assert!(names.contains(&"task.search"));
        assert!(names.contains(&"artifact.create"));
        assert!(names.contains(&"artifact.get"));
        assert!(names.contains(&"artifact.search"));
        assert!(names.contains(&"artifact.list_thread"));
        assert!(names.contains(&"context.list_subsystems"));
        assert!(names.contains(&"context.get_files_for_subsystem"));
        assert!(names.contains(&"context.search_context_documents"));
        assert!(names.contains(&"context.find_relevant_context"));
        assert!(names.contains(&"context.suggest_agent"));
        assert!(names.contains(&"context.get_hot_context"));
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

    #[test]
    fn context_tools_are_registered() {
        for name in [
            "context.list_subsystems",
            "context.get_files_for_subsystem",
            "context.search_context_documents",
            "context.find_relevant_context",
            "context.suggest_agent",
            "context.get_hot_context",
        ] {
            assert!(get_tool(name).is_some(), "missing tool {name}");
        }
    }

    #[test]
    fn context_suggest_agent_schema_has_required_fields() {
        let tool = get_tool("context.suggest_agent").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"tenant_id"));
        assert!(required_strs.contains(&"task"));
    }
}
