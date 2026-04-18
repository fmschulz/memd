//! Core domain types for memd
//!
//! Defines the fundamental data structures used throughout the memory system,
//! including MemoryChunk (the atomic unit of storage), identifiers, and enums.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{MemdError, Result};

/// Tenant identifier - validated string wrapper
///
/// TenantId must be non-empty and contain only alphanumeric characters and underscores.
/// This ensures safe use in file paths and database queries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    /// Create a new TenantId with validation
    ///
    /// # Errors
    /// Returns ValidationError if the id is empty or contains invalid characters.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        Self::validate(&id)?;
        Ok(Self(id))
    }

    /// Validate a tenant id string.
    ///
    /// `tenant_id` is used both as a storage directory name and as a
    /// logical partition key. The allowed charset is intentionally narrow
    /// — ASCII alphanumeric plus underscore — so that tenant values cannot
    /// escape the storage root via `..` / path separators, cannot embed
    /// null bytes, and cannot carry non-UTF-8 garbage.
    ///
    /// Exposed as `pub` (not module-private) so every boundary that turns
    /// a caller-supplied string into a tenant path (MCP handlers,
    /// `discover_and_recover_tenants`, …) can verify consistently.
    pub fn validate(id: &str) -> Result<()> {
        if id.is_empty() {
            return Err(MemdError::ValidationError(
                "tenant_id cannot be empty".to_string(),
            ));
        }

        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(MemdError::ValidationError(format!(
                "tenant_id '{}' contains invalid characters (only alphanumeric and underscore allowed)",
                id
            )));
        }

        Ok(())
    }

    /// Get the inner string value
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TenantId {
    type Error = MemdError;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<TenantId> for String {
    fn from(id: TenantId) -> Self {
        id.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Project identifier - optional string wrapper
///
/// ProjectId can be None (for tenant-level data) or Some(id) for project-scoped data.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ProjectId(Option<String>);

impl ProjectId {
    /// Create a new ProjectId from an optional string
    pub fn new(id: Option<impl Into<String>>) -> Self {
        Self(id.map(|s| s.into()))
    }

    /// Create an empty (None) ProjectId
    pub fn none() -> Self {
        Self(None)
    }

    /// Get the inner optional string value
    pub fn as_option(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// Check if the project id is set
    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(id) => write!(f, "{}", id),
            None => write!(f, "<none>"),
        }
    }
}

impl From<Option<String>> for ProjectId {
    fn from(value: Option<String>) -> Self {
        Self(value)
    }
}

impl From<&str> for ProjectId {
    fn from(value: &str) -> Self {
        Self(Some(value.to_string()))
    }
}

/// Chunk identifier - UUIDv7 wrapper for time-sortable IDs
///
/// Uses UUIDv7 which encodes timestamp for natural chronological ordering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChunkId(Uuid);

impl ChunkId {
    /// Generate a new ChunkId using UUIDv7 (time-sortable)
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Create a ChunkId from an existing UUID
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Parse a ChunkId from a string
    pub fn parse(s: &str) -> Result<Self> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| MemdError::ValidationError(format!("invalid chunk_id: {}", e)))
    }

    /// Get the inner UUID
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for ChunkId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Type of memory chunk
///
/// Categorizes chunks for filtering and routing during retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkType {
    /// Source code snippets, functions, files
    Code,
    /// Documentation, comments, READMEs
    Doc,
    /// Tool call traces, execution logs
    Trace,
    /// Architecture decisions, design choices
    Decision,
    /// Implementation plans, roadmaps
    Plan,
    /// Research notes, investigations
    Research,
    /// Chat messages, conversations
    Message,
    /// Summaries of other chunks or episodes
    Summary,
    /// Uncategorized content
    Other,
}

impl ChunkType {
    /// Get all chunk type variants
    pub fn all() -> &'static [ChunkType] {
        &[
            ChunkType::Code,
            ChunkType::Doc,
            ChunkType::Trace,
            ChunkType::Decision,
            ChunkType::Plan,
            ChunkType::Research,
            ChunkType::Message,
            ChunkType::Summary,
            ChunkType::Other,
        ]
    }
}

impl fmt::Display for ChunkType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ChunkType::Code => "code",
            ChunkType::Doc => "doc",
            ChunkType::Trace => "trace",
            ChunkType::Decision => "decision",
            ChunkType::Plan => "plan",
            ChunkType::Research => "research",
            ChunkType::Message => "message",
            ChunkType::Summary => "summary",
            ChunkType::Other => "other",
        };
        write!(f, "{}", s)
    }
}

impl Default for ChunkType {
    fn default() -> Self {
        ChunkType::Other
    }
}

impl std::str::FromStr for ChunkType {
    type Err = crate::error::MemdError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "code" => Self::Code,
            "doc" => Self::Doc,
            "trace" => Self::Trace,
            "decision" => Self::Decision,
            "plan" => Self::Plan,
            "research" => Self::Research,
            "message" => Self::Message,
            "summary" => Self::Summary,
            "other" => Self::Other,
            _ => {
                return Err(crate::error::MemdError::ValidationError(format!(
                    "unknown chunk_type: {s}"
                )))
            }
        })
    }
}

/// Status of a memory chunk
///
/// Tracks the lifecycle state of a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkStatus {
    /// Work in progress, may be incomplete
    Draft,
    /// Finalized content
    Final,
    /// Contains error information
    Error,
    /// Soft deleted, excluded from retrieval
    Deleted,
}

impl fmt::Display for ChunkStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ChunkStatus::Draft => "draft",
            ChunkStatus::Final => "final",
            ChunkStatus::Error => "error",
            ChunkStatus::Deleted => "deleted",
        };
        write!(f, "{}", s)
    }
}

impl Default for ChunkStatus {
    fn default() -> Self {
        ChunkStatus::Final
    }
}

/// Promotion state for chunks/artifacts in the retrieval hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromotionState {
    /// Raw source material with no higher-level synthesis.
    #[default]
    Raw,
    /// Derived summaries and generated digests.
    Summarized,
    /// Canonical source-of-truth task/artifact records.
    Canonical,
    /// Canonical records or digests that have been explicitly verified.
    Verified,
}

impl fmt::Display for PromotionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PromotionState::Raw => "raw",
            PromotionState::Summarized => "summarized",
            PromotionState::Canonical => "canonical",
            PromotionState::Verified => "verified",
        };
        write!(f, "{}", s)
    }
}

/// Ingestion mode for incoming text.
///
/// Distinguishes between conversational turns (which may need different
/// chunking, retention, and retrieval defaults) and document content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IngestionMode {
    /// Chat/turn-style content from an interactive session.
    Conversation,
    /// File, note, or document-style content (default).
    #[default]
    Document,
}

impl fmt::Display for IngestionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Conversation => "conversation",
            Self::Document => "document",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for IngestionMode {
    type Err = crate::error::MemdError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "conversation" => Ok(Self::Conversation),
            "document" => Ok(Self::Document),
            _ => Err(crate::error::MemdError::ValidationError(format!(
                "unknown ingestion mode: {s}"
            ))),
        }
    }
}

/// Source information for a chunk
///
/// Tracks provenance: where the chunk content originated from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// URI of the source (file://, https://, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Git repository URL or name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Git commit hash
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// File path within the repository
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Name of the tool that generated this chunk
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool call ID for correlation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Source {
    /// Create an empty source
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create a source from a file path
    pub fn from_path(path: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            ..Default::default()
        }
    }

    /// Create a source from a tool call
    pub fn from_tool(name: impl Into<String>, call_id: Option<impl Into<String>>) -> Self {
        Self {
            tool_name: Some(name.into()),
            tool_call_id: call_id.map(|s| s.into()),
            ..Default::default()
        }
    }
}

/// Core memory chunk structure
///
/// The atomic unit of storage in memd. Immutable payload with mutable metadata
/// tracked via side tables. Each chunk represents a piece of context that can
/// be retrieved and used by agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryChunk {
    /// Unique identifier (UUIDv7 for time-sortability)
    pub chunk_id: ChunkId,
    /// Tenant this chunk belongs to (required)
    pub tenant_id: TenantId,
    /// Project within tenant (optional)
    pub project_id: ProjectId,
    /// Agent that created this chunk (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// When this chunk was created (Unix milliseconds)
    pub timestamp_created: i64,
    /// When the underlying event was observed (Unix milliseconds, for bi-temporal support)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_observed: Option<i64>,
    /// Category of this chunk's content
    pub chunk_type: ChunkType,
    /// Lifecycle status
    pub status: ChunkStatus,
    /// Relative retrieval priority tier.
    #[serde(default)]
    pub promotion_state: PromotionState,
    /// Provenance information
    pub source: Source,
    /// The actual content
    pub text: String,
    /// User-defined tags for filtering
    #[serde(default)]
    pub tags: Vec<String>,
    /// Content hash for deduplication
    pub hash: String,
}

impl MemoryChunk {
    /// Create a new MemoryChunk with the given parameters
    ///
    /// Generates a new ChunkId and sets timestamp_created to now.
    pub fn new(tenant_id: TenantId, text: impl Into<String>, chunk_type: ChunkType) -> Self {
        let text = text.into();
        let hash = Self::compute_hash(&text);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        Self {
            chunk_id: ChunkId::new(),
            tenant_id,
            project_id: ProjectId::none(),
            agent_id: None,
            timestamp_created: now_ms,
            timestamp_observed: None,
            chunk_type,
            status: ChunkStatus::Final,
            promotion_state: PromotionState::Raw,
            source: Source::empty(),
            text,
            tags: Vec::new(),
            hash,
        }
    }

    /// Compute a simple hash of the content for deduplication
    fn compute_hash(text: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Builder method to set project_id
    pub fn with_project(mut self, project_id: ProjectId) -> Self {
        self.project_id = project_id;
        self
    }

    /// Builder method to set agent_id
    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Builder method to set source
    pub fn with_source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    /// Builder method to set tags
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Builder method to set status
    pub fn with_status(mut self, status: ChunkStatus) -> Self {
        self.status = status;
        self
    }

    /// Builder method to set promotion state
    pub fn with_promotion_state(mut self, promotion_state: PromotionState) -> Self {
        self.promotion_state = promotion_state;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_valid() {
        assert!(TenantId::new("valid_tenant").is_ok());
        assert!(TenantId::new("tenant123").is_ok());
        assert!(TenantId::new("TENANT").is_ok());
        assert!(TenantId::new("a").is_ok());
    }

    #[test]
    fn tenant_id_empty_rejected() {
        let result = TenantId::new("");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MemdError::ValidationError(_)));
    }

    #[test]
    fn tenant_id_invalid_chars_rejected() {
        assert!(TenantId::new("tenant-name").is_err()); // hyphen
        assert!(TenantId::new("tenant.name").is_err()); // dot
        assert!(TenantId::new("tenant name").is_err()); // space
        assert!(TenantId::new("tenant/name").is_err()); // slash
    }

    #[test]
    fn tenant_id_serde_roundtrip() {
        let id = TenantId::new("test_tenant").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"test_tenant\"");

        let parsed: TenantId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn tenant_id_serde_rejects_invalid() {
        let result: std::result::Result<TenantId, _> = serde_json::from_str("\"\"");
        assert!(result.is_err());
    }

    #[test]
    fn chunk_id_is_unique() {
        let id1 = ChunkId::new();
        let id2 = ChunkId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn chunk_type_display() {
        assert_eq!(ChunkType::Code.to_string(), "code");
        assert_eq!(ChunkType::Decision.to_string(), "decision");
    }

    #[test]
    fn chunk_status_display() {
        assert_eq!(ChunkStatus::Final.to_string(), "final");
        assert_eq!(ChunkStatus::Deleted.to_string(), "deleted");
    }

    #[test]
    fn memory_chunk_serialization() {
        let tenant = TenantId::new("test").unwrap();
        let chunk = MemoryChunk::new(tenant, "Hello, world!", ChunkType::Doc);

        let json = serde_json::to_string(&chunk).unwrap();
        assert!(json.contains("\"text\":\"Hello, world!\""));
        assert!(json.contains("\"chunk_type\":\"doc\""));

        let parsed: MemoryChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.text, "Hello, world!");
    }

    #[test]
    fn memory_chunk_builder_pattern() {
        let tenant = TenantId::new("test").unwrap();
        let chunk = MemoryChunk::new(tenant, "content", ChunkType::Code)
            .with_project("my_project".into())
            .with_agent("claude")
            .with_tags(vec!["rust".to_string(), "api".to_string()])
            .with_status(ChunkStatus::Draft);

        assert!(chunk.project_id.is_some());
        assert_eq!(chunk.agent_id.as_deref(), Some("claude"));
        assert_eq!(chunk.tags.len(), 2);
        assert_eq!(chunk.status, ChunkStatus::Draft);
    }

    #[test]
    fn chunk_type_from_str_round_trip_and_failure() {
        use std::str::FromStr;

        assert_eq!(ChunkType::from_str("code").unwrap(), ChunkType::Code);
        assert_eq!(
            ChunkType::from_str("decision").unwrap(),
            ChunkType::Decision
        );
        let err = ChunkType::from_str("bogus").unwrap_err();
        assert!(matches!(err, MemdError::ValidationError(_)));
    }

    #[test]
    fn ingestion_mode_default_and_parsing() {
        use std::str::FromStr;

        assert_eq!(IngestionMode::default(), IngestionMode::Document);
        assert_eq!(
            IngestionMode::from_str("conversation").unwrap(),
            IngestionMode::Conversation
        );
        let err = IngestionMode::from_str("bogus").unwrap_err();
        assert!(matches!(err, MemdError::ValidationError(_)));
    }

    #[test]
    fn ingestion_mode_serde_round_trip() {
        let json = serde_json::to_string(&IngestionMode::Conversation).unwrap();
        assert_eq!(json, "\"conversation\"");

        let parsed: IngestionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, IngestionMode::Conversation);
    }
}
