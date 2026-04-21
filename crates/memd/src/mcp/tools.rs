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
                        "enum": ["generic", "brief_project", "resume_task", "find_failures", "find_decisions", "find_evidence", "find_highlights"],
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
                    },
                    "include_superseded": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, include chunks with status=Superseded in the results instead of hiding them. Best-effort on dense-only tenants: compaction evicts lifecycle-hidden rows from the HNSW index, so include_superseded=true only surfaces rows that have not yet been evicted. For guaranteed access to a specific superseded chunk, use memory.get(chunk_id, include_superseded=true)."
                    },
                    "include_expired": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, include chunks with status=Expired or a past expires_at_ms instead of hiding them. Same best-effort caveat as include_superseded."
                    },
                    "include_history": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, include chunks in MemoryTier::History instead of hiding them. Same best-effort caveat as include_superseded."
                    },
                    "oversample_factor": {
                        "type": "integer",
                        "default": 3,
                        "minimum": 1,
                        "maximum": 10,
                        "description": "Multiplier applied to k when pulling candidates from the ranker before visibility filtering. Larger values give more headroom to refill to k when many top hits are hidden; smaller values are cheaper but may under-fill. Ignored when all three include_* flags are true."
                    }
                },
                "required": ["query"]
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
                    },
                    "expires_at_ms": {
                        "type": "integer",
                        "description": "Optional wall-clock expiry (ms since epoch). When set, the chunk is lazy-hidden from retrieval once expired (Track C2) and materialised to status=Expired by the compaction sweep (Track C3). Requires a persistent store."
                    },
                    "review_after_ms": {
                        "type": "integer",
                        "description": "Optional review reminder (ms since epoch). Informational only — does not hide the chunk. Requires a persistent store."
                    },
                    "mode": {
                        "type": "string",
                        "description": "Optional ingestion mode label (e.g. \"conversation\", \"document\"). Accepted now for Track C/E forward-compat; consumed by Track E."
                    },
                    "supersede_near_duplicates": {
                        "description": "Track D: when set, prior chunks in the same (tenant, project) that match the new chunk's canonical form (exact mode) or trigram-Jaccard similarity (fuzzy mode) are atomically superseded with a back-edge to the new chunk. Accepts either `true` (shorthand for {mode: 'exact', scope: 'project'}) or a config object {mode, threshold, scope}. Requires a persistent store. Response gains a `superseded_ids` array.",
                        "oneOf": [
                            { "type": "boolean" },
                            {
                                "type": "object",
                                "properties": {
                                    "mode": { "type": "string", "enum": ["exact", "fuzzy"] },
                                    "threshold": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                                    "scope": { "type": "string", "enum": ["project", "tenant"] }
                                }
                            }
                        ]
                    }
                },
                "required": ["text", "type"]
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
                                },
                                "expires_at_ms": {
                                    "type": "integer",
                                    "description": "Optional wall-clock expiry (ms since epoch) for this chunk. Same semantics as memory.add.expires_at_ms."
                                },
                                "review_after_ms": {
                                    "type": "integer",
                                    "description": "Optional review reminder (ms since epoch) for this chunk. Informational only."
                                },
                                "mode": {
                                    "type": "string",
                                    "description": "Optional ingestion mode label for this chunk (Track E forward-compat)."
                                }
                            },
                            "required": ["text", "type"]
                        }
                    },
                    "supersede_near_duplicates": {
                        "description": "Track D: when set, applied per-chunk in the batch with the same semantics as memory.add.supersede_near_duplicates. Response gains a `superseded_ids: [[...], ...]` parallel array (one inner array per input chunk).",
                        "oneOf": [
                            { "type": "boolean" },
                            {
                                "type": "object",
                                "properties": {
                                    "mode": { "type": "string", "enum": ["exact", "fuzzy"] },
                                    "threshold": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                                    "scope": { "type": "string", "enum": ["project", "tenant"] }
                                }
                            }
                        ]
                    }
                },
                "required": ["chunks"]
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
                "required": ["goal"]
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
                "required": ["task_id"]
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
                "required": ["task_id", "summary"]
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
                "required": ["task_id", "tool_name"]
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
                "required": ["task_id", "status"]
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
                "required": ["task_id", "evidence_kind"]
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
                "required": ["task_id"]
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
                        "enum": ["generic", "brief_project", "resume_task", "find_failures", "find_decisions", "find_evidence", "find_highlights"],
                        "default": "generic"
                    },
                    "filters": {
                        "type": "object",
                        "properties": {
                            "task_id": {"type": "string"},
                            "artifact_kind": {
                                "type": "string",
                                "enum": ["task_start", "task_progress", "run_start", "run_finish", "evidence", "review", "revision", "verification", "decision", "digest", "task_finish", "wiki_page"]
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
                "required": []
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
                        "enum": ["task_start", "task_progress", "run_start", "run_finish", "evidence", "review", "revision", "verification", "decision", "digest", "task_finish", "wiki_page"]
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
                    "content": {"type": "string"},
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
                "required": ["artifact_kind"]
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
                "required": ["artifact_id"]
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
                        "enum": ["generic", "brief_project", "resume_task", "find_failures", "find_decisions", "find_evidence", "find_highlights"],
                        "default": "generic"
                    },
                    "filters": {
                        "type": "object",
                        "properties": {
                            "task_id": {"type": "string"},
                            "artifact_kind": {
                                "type": "string",
                                "enum": ["task_start", "task_progress", "run_start", "run_finish", "evidence", "review", "revision", "verification", "decision", "digest", "task_finish", "wiki_page"]
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
                "required": []
            }),
        ),
        // ---------- Phase 2.3 focused artifact tools ----------
        // These tools wrap `artifact.create` with a fixed `artifact_kind`
        // and a small schema — 3-5 fields each — so agents do not have
        // to fight the 50-field mega-schema when they just want to
        // record a review, a revision, a decision, or a verification.
        // `artifact.create` remains registered (deprecated) for
        // backwards compatibility.
        ToolDefinition::new(
            "artifact.review",
            "Record a review artifact: an agent's assessment of an existing artifact \
             (usually a task.start / task.progress / task.finish). Use reply_to_artifact_id \
             to attach the review to a specific parent. Supply agent_id to enable distinct-writer \
             countersignature promotion if supports_claim is also set.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "summary": {"type": "string"},
                    "reply_to_artifact_id": {"type": "string"},
                    "supports_claim": {"type": "boolean"},
                    "agent_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "requested_action": {"type": "string"}
                },
                "required": ["task_id", "summary"]
            }),
        ),
        ToolDefinition::new(
            "artifact.revision",
            "Record a revision artifact: a follow-up that supersedes or amends a prior \
             artifact. reply_to_artifact_id is the artifact being revised.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "summary": {"type": "string"},
                    "reply_to_artifact_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "project_id": {"type": "string"}
                },
                "required": ["task_id", "summary", "reply_to_artifact_id"]
            }),
        ),
        ToolDefinition::new(
            "artifact.decision",
            "Record a decision artifact: an explicit choice between alternatives with a \
             rationale. why_chosen captures the rationale; optional reply_to_artifact_id \
             chains the decision to the prior context.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "summary": {"type": "string"},
                    "why_chosen": {"type": "string"},
                    "reply_to_artifact_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "project_id": {"type": "string"}
                },
                "required": ["task_id", "summary"]
            }),
        ),
        ToolDefinition::new(
            "artifact.verification",
            "Record a verification artifact: a distinct agent's countersignature of a prior \
             claim. supports_claim=true with a different agent_id than the parent's promotes \
             the parent to VerifiedRecord.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "summary": {"type": "string"},
                    "reply_to_artifact_id": {"type": "string"},
                    "supports_claim": {"type": "boolean"},
                    "agent_id": {"type": "string"},
                    "project_id": {"type": "string"}
                },
                "required": ["task_id", "supports_claim", "reply_to_artifact_id"]
            }),
        ),
        ToolDefinition::new(
            "artifact.find_related",
            "Find canonical artifacts whose text overlaps with a claim. \
             This is a retrieval helper, not a trust primitive: a returned artifact \
             supports a claim only if it has an independent countersignature \
             (distinct agent_id) and survives your own review. Prefer this over \
             `artifact.verify`, which is kept as a deprecated alias.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "claim": {"type": "string"},
                    "project_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "thread_id": {"type": "string"},
                    "candidate_artifact_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional artifact ids to inspect first before falling back to search"
                    },
                    "k": {
                        "type": "integer",
                        "default": 8,
                        "minimum": 1,
                        "maximum": 100
                    },
                    "include_digests": {
                        "type": "boolean",
                        "default": false,
                        "description": "Include consulted digest hints in the response even when canonical hits exist"
                    },
                    "create_artifact": {
                        "type": "boolean",
                        "default": false,
                        "description": "Persist a verification-style artifact recording the retrieval result"
                    },
                    "record_task_id": {
                        "type": "string",
                        "description": "Optional task id to own a persisted record artifact"
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Required for distinct-writer countersignature promotion when create_artifact=true. Omit to write an anonymous verification record (cannot upgrade trust)."
                    }
                },
                "required": ["claim"]
            }),
        ),
        ToolDefinition::new(
            "artifact.verify",
            "DEPRECATED alias for `artifact.find_related`. The historical \
             `artifact.verify` naming implied grounding against canonical \
             artifacts, but the underlying implementation is substring overlap \
             + retrieval, not cryptographic or countersignature-based \
             verification. Use `artifact.find_related` instead; this alias \
             forwards with a warning and will be removed in a future release.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "claim": {"type": "string"},
                    "project_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "thread_id": {"type": "string"},
                    "candidate_artifact_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional artifact ids to inspect first"
                    },
                    "k": {
                        "type": "integer",
                        "default": 8,
                        "minimum": 1,
                        "maximum": 100
                    },
                    "include_digests": {"type": "boolean", "default": false},
                    "create_artifact": {"type": "boolean", "default": false},
                    "record_task_id": {"type": "string"},
                    "agent_id": {"type": "string"}
                },
                "required": ["claim"]
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
                "required": []
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
                "required": ["project_id"]
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
                "required": ["task_id"]
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
                "required": []
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
                "required": []
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
                "required": []
            }),
        ),
        ToolDefinition::new(
            "artifact.find_highlights",
            "Generate or refresh a highlight library digest and return ranked, high-uplift lessons for future agents.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {"type": "string"},
                    "project_id": {"type": "string"},
                    "query": {"type": "string", "default": ""},
                    "k": {"type": "integer", "default": 20, "minimum": 1, "maximum": 100}
                },
                "required": []
            }),
        ),
        // MCP-05: memory.get
        ToolDefinition::new(
            "memory.get",
            "Fetch a chunk by id with its lifecycle overlay. Response shapes: \
             `{found: false}` when the chunk does not exist OR is Deleted; \
             `{found: true, hidden: true, status, tier, hidden_reason}` when the \
             chunk is hidden by the visibility policy. `hidden_reason` is one of \
             `superseded`, `expired`, `history`, `error` and matches \
             VisibilityPolicy::is_visible_at precedence (status → tier → wall-clock \
             expiry). Caller can opt in to the first three via \
             include_superseded/include_expired/include_history; `error` has no \
             include knob. `{found: true, chunk, lifecycle, status}` when the chunk \
             is visible.",
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
                    },
                    "include_superseded": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return chunks marked Superseded instead of hiding them."
                    },
                    "include_expired": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return chunks marked Expired (by status or expires_at_ms) instead of hiding them."
                    },
                    "include_history": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return chunks in the History tier instead of hiding them."
                    }
                },
                "required": ["chunk_id"]
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
                "required": ["chunk_id"]
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
                "required": ["query", "chunk_id", "relevance"]
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
                "required": []
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
                "required": ["name"]
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
                "required": ["name"]
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
                "required": ["name"]
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
                "required": ["module"]
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
                "required": []
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
                "required": []
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
                            "enum": ["generic", "brief_project", "resume_task", "find_failures", "find_decisions", "find_evidence", "find_highlights"]
                        },
                        "description": "Optional digest modes to rebuild during compaction"
                    },
                    "force_digest_rebuild": {
                        "type": "boolean",
                        "description": "Force digest regeneration even when storage compaction thresholds are not exceeded",
                        "default": false
                    }
                },
                "required": []
            }),
        ),
        // LIFECYCLE-01: memory.supersede (Track A — A7)
        ToolDefinition::new(
            "memory.supersede",
            "Atomically supersede an existing chunk with a new version. Old chunk keeps \
             provenance but is filtered from default retrieval. Bumps tenant cache version.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "old_chunk_id": {
                        "type": "string",
                        "description": "UUID of the chunk being superseded"
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Content of the replacement chunk"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["code", "doc", "trace", "decision", "plan", "research", "message", "summary", "other"],
                        "description": "Chunk type (code, doc, trace, etc.) of the replacement"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project identifier"
                    },
                    "source": {
                        "type": "object",
                        "description": "Optional provenance information",
                        "properties": {
                            "uri": {"type": "string"},
                            "repo": {"type": "string"},
                            "commit": {"type": "string"},
                            "path": {"type": "string"},
                            "tool_name": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        }
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional tags for filtering"
                    }
                },
                "required": ["old_chunk_id", "new_text", "type"]
            }),
        ),
        // LIFECYCLE-02: memory.set_expiry (Track C6)
        ToolDefinition::new(
            "memory.set_expiry",
            "Update the expires_at_ms and/or review_after_ms overlay fields on an existing chunk. \
             Pass `null` to clear a field, omit to leave it unchanged, pass a number to set it. \
             Bumps the tenant cache version when at least one field changed.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "chunk_id": {
                        "type": "string",
                        "description": "UUID of the chunk whose overlay is being updated"
                    },
                    "expires_at_ms": {
                        "type": ["integer", "null"],
                        "description": "New wall-clock expiry (ms since epoch). `null` clears the field; omit to leave it unchanged."
                    },
                    "review_after_ms": {
                        "type": ["integer", "null"],
                        "description": "New review reminder (ms since epoch). `null` clears the field; omit to leave it unchanged."
                    }
                },
                "required": ["chunk_id"]
            }),
        ),
        // EXPORT-01: memory.export_markdown (Track G2)
        ToolDefinition::new(
            "memory.export_markdown",
            "Render the tenant's chunks as a tree of markdown files. Returns `{files: [{path, content}]}` — \
             never writes to disk; the CLI consumes the payload and writes locally. Files are grouped \
             one-per-(project, chunk_type) bucket with stable sort order. Requires a persistent store.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project filter — only chunks under this project are exported"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 10000,
                        "description": "Max chunks to read from metadata before grouping"
                    }
                }
            }),
        ),
        // LIFECYCLE-03: memory.find_near_duplicates (Track D5)
        ToolDefinition::new(
            "memory.find_near_duplicates",
            "Read-only Track D preview. Returns existing live-head chunks that the same text would supersede via memory.add(supersede_near_duplicates=...). \
             Always reports exact-canonical matches; when `fuzzy_threshold` is set, also returns trigram-Jaccard candidates with `similarity` scores. \
             No writes, no cache bumps. Requires a persistent store.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": {
                        "type": "string",
                        "description": "Tenant identifier for data isolation"
                    },
                    "text": {
                        "type": "string",
                        "description": "Probe text — canonicalised and matched against the dedup index"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["code", "doc", "trace", "decision", "plan", "research", "message", "summary", "other"],
                        "default": "doc"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Optional project identifier — combined with `scope` to bound the candidate pool"
                    },
                    "fuzzy_threshold": {
                        "type": "number",
                        "minimum": 0.0,
                        "maximum": 1.0,
                        "description": "Optional padded char-trigram Jaccard threshold. When omitted, only exact-canonical matches are returned."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["project", "tenant"],
                        "default": "project"
                    }
                },
                "required": ["text"]
            }),
        ),
        // OMF-01: memory.export_omf (Track F5)
        ToolDefinition::new(
            "memory.export_omf",
            "Export the tenant's memory as an OMF 1.0 document. Each item's `extensions.memd` namespace round-trips \
             chunk_id, project_id, chunk_type, ingestion_mode, and the lifecycle overlay (status, tier, \
             supersedes, superseded_by, expires_at_ms, review_after_ms, lifecycle_updated_at_ms) so a subsequent \
             memd↔memd import can preserve state under the F3 trust gate. Returns `{document: OmfDocument}`. \
             Read-only. Requires a persistent store.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": { "type": "string", "description": "Tenant identifier" },
                    "project_id": { "type": "string", "description": "Optional project filter" },
                    "include_history": {
                        "type": "boolean",
                        "default": false,
                        "description": "Include history-tier rows (default: live-only)"
                    },
                    "include_superseded": {
                        "type": "boolean",
                        "default": true,
                        "description": "Include status=Superseded rows"
                    },
                    "include_expired": {
                        "type": "boolean",
                        "default": true,
                        "description": "Include status=Expired and lazily-expired rows"
                    }
                }
            }),
        ),
        // OMF-02: memory.preview_omf_import (Track F5)
        ToolDefinition::new(
            "memory.preview_omf_import",
            "Dry-run an OMF 1.0 import. Walks the same dedup + filter + trust-gate path as memory.import_omf \
             and reports `{total, to_import, duplicates, filtered, unscoped, by_project}`. Never writes, \
             never bumps cache versions. Requires a persistent store.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": { "type": "string", "description": "Tenant identifier" },
                    "document": {
                        "type": "object",
                        "description": "OMF 1.0 document to preview (same shape as memory.export_omf output)"
                    },
                    "include_archived": {
                        "type": "boolean",
                        "default": true,
                        "description": "Include items whose top-level status is 'archived' or 'expired'"
                    },
                    "fuzzy_threshold": {
                        "type": "number",
                        "minimum": 0.0,
                        "maximum": 1.0,
                        "description": "Optional trigram Jaccard threshold. Absent = exact-only."
                    }
                },
                "required": ["document"]
            }),
        ),
        // OMF-03: memory.import_omf (Track F5)
        ToolDefinition::new(
            "memory.import_omf",
            "Import an OMF 1.0 document into the tenant. Exact-canonical dedup by default; optional fuzzy \
             (`fuzzy_threshold`). Lifecycle overlay fields are honoured only when `document.source.app == \"memd\"` \
             and `extensions.memd.v == 1` (F3 trust gate). Returns `{total, imported, duplicates, skipped}`. \
             Requires a persistent store.",
            json!({
                "type": "object",
                "properties": {
                    "tenant_id": { "type": "string", "description": "Tenant identifier" },
                    "document": {
                        "type": "object",
                        "description": "OMF 1.0 document to import"
                    },
                    "include_archived": {
                        "type": "boolean",
                        "default": true,
                        "description": "Include items whose top-level status is 'archived' or 'expired'"
                    },
                    "fuzzy_threshold": {
                        "type": "number",
                        "minimum": 0.0,
                        "maximum": 1.0,
                        "description": "Optional trigram Jaccard threshold. Absent = exact-only."
                    }
                },
                "required": ["document"]
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
                "required": ["episode_id"]
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
                "required": []
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
                "required": ["subsystem_key"]
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
                "required": ["query"]
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
                "required": ["task"]
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
                "required": ["task"]
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
                "required": []
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
        "artifact.review",
        "artifact.revision",
        "artifact.decision",
        "artifact.verification",
        "artifact.get",
        "artifact.search",
        "artifact.find_related",
        "artifact.verify",
        "artifact.find_failures",
        "artifact.find_decisions",
        "artifact.find_evidence",
        "artifact.find_highlights",
        "artifact.list_thread",
        "memory.get",
        "memory.delete",
        "memory.feedback",
        "memory.stats",
        "memory.metrics",
        "memory.compact",
        "memory.supersede",
        "memory.set_expiry",
        "memory.find_near_duplicates",
        "memory.export_markdown",
        "memory.export_omf",
        "memory.preview_omf_import",
        "memory.import_omf",
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
    fn get_all_tools_returns_expected_count() {
        let tools = get_all_tools();
        // Phase 2.3: 42 legacy tools + four focused artifact tools
        // (artifact.review / revision / decision / verification).
        // Track A (A7): + memory.supersede.
        // Track C (C6): + memory.set_expiry.
        // Track D (D5): + memory.find_near_duplicates.
        // Track G (G2): + memory.export_markdown.
        // Track F (F5): + memory.export_omf + memory.preview_omf_import + memory.import_omf.
        assert_eq!(tools.len(), 53);
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
        assert!(names.contains(&"artifact.find_related"));
        assert!(names.contains(&"artifact.verify"));
        assert!(names.contains(&"artifact.find_highlights"));
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
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
        // Phase 2.2: task.start requires only `goal`. All other fields
        // (motivation, hypothesis, scientific_question, dataset_refs,
        // expected_outputs, etc.) became optional so agents can log
        // minimal progress without inventing content. The legacy
        // fields must NOT appear in `required`.
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+"
        );
        assert!(required_strs.contains(&"goal"));
        for legacy in [
            "motivation",
            "hypothesis",
            "scientific_question",
            "dataset_refs",
            "expected_outputs",
        ] {
            assert!(
                !required_strs.contains(&legacy),
                "`{}` must be optional on task.start in v0.3.1+",
                legacy
            );
        }
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
        // Phase 2.2: task.finish requires only `task_id`.
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+"
        );
        assert!(required_strs.contains(&"task_id"));
        for legacy in [
            "what_worked",
            "what_failed",
            "validation",
            "uncertainty",
            "followups",
            "confidence",
        ] {
            assert!(
                !required_strs.contains(&legacy),
                "`{}` must be optional on task.finish in v0.3.1+",
                legacy
            );
        }
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
        assert!(required_strs.contains(&"artifact_kind"));
    }

    #[test]
    fn artifact_verify_schema_has_required_fields() {
        let tool = get_tool("artifact.verify").unwrap();
        let required = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
        assert!(required_strs.contains(&"claim"));
    }

    #[test]
    fn artifact_find_related_schema_mirrors_verify() {
        let find_related = get_tool("artifact.find_related").expect("find_related must exist");
        let verify = get_tool("artifact.verify").expect("verify alias must still exist");

        // Both expose the same required fields and both accept the same
        // core parameters — the deprecated alias forwards to the same
        // handler. Compare the `required` lists and the property key set
        // (ignore property-level descriptions so the deprecated entry can
        // stay slimmer).
        let find_required = find_related
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let verify_required = verify
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(find_required, verify_required);

        let find_props: Vec<String> = find_related
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|m| {
                let mut keys: Vec<String> = m.keys().cloned().collect();
                keys.sort();
                keys
            })
            .unwrap_or_default();
        let verify_props: Vec<String> = verify
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|m| {
                let mut keys: Vec<String> = m.keys().cloned().collect();
                keys.sort();
                keys
            })
            .unwrap_or_default();
        assert_eq!(
            find_props, verify_props,
            "alias must accept the same top-level parameters"
        );

        assert!(
            verify.description.to_lowercase().contains("deprecated"),
            "artifact.verify description must flag itself as deprecated, got: {}",
            verify.description
        );
        assert!(
            !find_related
                .description
                .to_lowercase()
                .contains("ground a claim"),
            "find_related description must not re-assert the grounding-as-trust story"
        );
    }

    #[test]
    fn tool_names_list() {
        let names = tool_names();
        // Phase 2.3: 42 legacy + 4 focused artifact tools.
        // Track A (A7): + memory.supersede.
        // Track C (C6): + memory.set_expiry.
        // Track D (D5): + memory.find_near_duplicates.
        // Track G (G2): + memory.export_markdown.
        // Track F (F5): + memory.export_omf + memory.preview_omf_import + memory.import_omf.
        assert_eq!(names.len(), 53);
        assert!(names.contains(&"memory.supersede"));
        assert!(names.contains(&"memory.set_expiry"));
        assert!(names.contains(&"memory.find_near_duplicates"));
        assert!(names.contains(&"memory.export_markdown"));
        assert!(names.contains(&"memory.export_omf"));
        assert!(names.contains(&"memory.preview_omf_import"));
        assert!(names.contains(&"memory.import_omf"));
        assert!(names.contains(&"artifact.find_related"));
        assert!(names.contains(&"artifact.review"));
        assert!(names.contains(&"artifact.revision"));
        assert!(names.contains(&"artifact.decision"));
        assert!(names.contains(&"artifact.verification"));
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
        assert!(names.contains(&"artifact.verify"));
        assert!(names.contains(&"artifact.find_highlights"));
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
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
        assert!(
            !required_strs.contains(&"tenant_id"),
            "tenant_id is optional in v0.3.1+; it must NOT appear in required"
        );
        assert!(required_strs.contains(&"task"));
    }
}
