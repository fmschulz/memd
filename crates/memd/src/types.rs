// Core types - placeholder for Task 2
// This file will be fully implemented in Task 2

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Tenant identifier - validated string wrapper
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantId(String);

/// Project identifier - optional string wrapper
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(Option<String>);

/// Chunk identifier - UUIDv7 wrapper for time-sortable IDs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkId(Uuid);

/// Type of memory chunk
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChunkType {
    Code,
    Doc,
    Trace,
    Decision,
    Plan,
    Research,
    Message,
    Summary,
    Other,
}

/// Status of a memory chunk
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChunkStatus {
    Draft,
    Final,
    Error,
    Deleted,
}

/// Source information for a chunk
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    pub uri: Option<String>,
    pub repo: Option<String>,
    pub commit: Option<String>,
    pub path: Option<String>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
}

/// Core memory chunk structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryChunk {
    pub chunk_id: ChunkId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub agent_id: Option<String>,
    pub timestamp_created: i64,
    pub timestamp_observed: Option<i64>,
    pub chunk_type: ChunkType,
    pub status: ChunkStatus,
    pub source: Source,
    pub text: String,
    pub tags: Vec<String>,
    pub hash: String,
}
