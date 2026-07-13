//! Core domain types for memd
//!
//! Defines the fundamental data structures used throughout the memory system,
//! including MemoryChunk (the atomic unit of storage), identifiers, and enums.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{MemdError, Result};

pub use lifecycle::{
    LifecycleDelta, LifecycleMetadata, MemoryTier, ResolvedChunk, VisibilityPolicy,
};

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

    /// Validate a caller-supplied project id at an input boundary.
    ///
    /// `ProjectId::new` is infallible and used pervasively, so unlike
    /// `TenantId` the charset is enforced at the boundary rather than in the
    /// constructor. Project ids commonly come from repository basenames, so
    /// the charset is broader than `TenantId` (hyphens and dots allowed), but
    /// it still excludes whitespace, control characters, path separators, and
    /// markdown control bytes so a project id cannot inject instructions into
    /// the agent-facing context (e.g. via `scope_status` hints) or escape the
    /// storage root. `None` and empty are accepted (tenant-level scope).
    pub fn validate(id: &str) -> Result<()> {
        if id.is_empty() {
            return Ok(());
        }
        if id.contains("..") {
            return Err(MemdError::ValidationError(format!(
                "project_id '{id}' must not contain '..'"
            )));
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err(MemdError::ValidationError(format!(
                "project_id '{id}' contains invalid characters (only alphanumeric, '_', '-', and '.' allowed)"
            )));
        }
        Ok(())
    }

    /// Validate an optional caller-supplied project id at an input boundary.
    pub fn validate_opt(id: Option<&str>) -> Result<()> {
        match id {
            Some(id) => Self::validate(id),
            None => Ok(()),
        }
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
/// Ord gives ranking code a deterministic tie-break key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    #[default]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkStatus {
    /// Work in progress, may be incomplete
    Draft,
    /// Synthesized consolidation output awaiting validation and promotion.
    Candidate,
    /// Finalized content
    #[default]
    Final,
    /// Contains error information
    Error,
    /// Soft deleted, excluded from retrieval
    Deleted,
    /// Replaced by a newer chunk; retained for audit but excluded from active retrieval.
    Superseded,
    /// Retention window elapsed; excluded from active retrieval.
    Expired,
}

impl fmt::Display for ChunkStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ChunkStatus::Draft => "draft",
            ChunkStatus::Candidate => "candidate",
            ChunkStatus::Final => "final",
            ChunkStatus::Error => "error",
            ChunkStatus::Deleted => "deleted",
            ChunkStatus::Superseded => "superseded",
            ChunkStatus::Expired => "expired",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for ChunkStatus {
    type Err = crate::error::MemdError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "draft" => Self::Draft,
            "candidate" => Self::Candidate,
            "final" => Self::Final,
            "error" => Self::Error,
            "deleted" => Self::Deleted,
            "superseded" => Self::Superseded,
            "expired" => Self::Expired,
            _ => {
                return Err(crate::error::MemdError::ValidationError(format!(
                    "unknown status: {s}"
                )))
            }
        })
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
    /// Track E: write-time label declaring whether this chunk came
    /// from a `conversation` (rapidly-evolving session memory) or a
    /// `document` (curated, durable) ingestion. Defaults to `Document`
    /// so segment payloads written before E1 deserialize unchanged.
    #[serde(default)]
    pub ingestion_mode: IngestionMode,
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
            ingestion_mode: IngestionMode::Document,
        }
    }

    /// Override the ingestion_mode label set by `new()`. Used by the
    /// MCP `memory.add(_batch)` handler to honour the `mode` request
    /// param before write.
    pub fn with_ingestion_mode(mut self, mode: IngestionMode) -> Self {
        self.ingestion_mode = mode;
        self
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

pub mod lifecycle {
    use super::{ChunkId, ChunkStatus, MemoryChunk};
    use serde::{Deserialize, Serialize};

    /// Retrieval tier for a chunk's lifecycle position.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
    #[serde(rename_all = "snake_case")]
    pub enum MemoryTier {
        /// Short-lived, actively-worked context.
        Working,
        /// Durable default tier for retained knowledge.
        #[default]
        LongTerm,
        /// Archive tier, excluded from active retrieval by default.
        History,
    }

    impl std::fmt::Display for MemoryTier {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let s = match self {
                Self::Working => "working",
                Self::LongTerm => "long_term",
                Self::History => "history",
            };
            f.write_str(s)
        }
    }

    impl std::str::FromStr for MemoryTier {
        type Err = crate::error::MemdError;
        fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
            match s {
                "working" => Ok(Self::Working),
                "long_term" => Ok(Self::LongTerm),
                "history" => Ok(Self::History),
                _ => Err(crate::error::MemdError::ValidationError(format!(
                    "unknown tier: {s}"
                ))),
            }
        }
    }

    /// Lifecycle overlay metadata for a chunk.
    ///
    /// Stored alongside the immutable `MemoryChunk` payload to track
    /// supersession edges, retention windows, and tier placement without
    /// mutating the chunk itself.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct LifecycleMetadata {
        pub tier: MemoryTier,
        pub supersedes: Option<ChunkId>,
        pub superseded_by: Option<ChunkId>,
        pub expires_at_ms: Option<i64>,
        pub review_after_ms: Option<i64>,
        pub lifecycle_updated_at_ms: i64,
    }

    impl Default for LifecycleMetadata {
        fn default() -> Self {
            Self {
                tier: MemoryTier::LongTerm,
                supersedes: None,
                superseded_by: None,
                expires_at_ms: None,
                review_after_ms: None,
                lifecycle_updated_at_ms: 0,
            }
        }
    }

    /// Triple-state delta for lifecycle updates.
    ///
    /// Semantics:
    /// - `None` on an outer `Option` means "leave the field unchanged".
    /// - `Some(value)` on a plain field means "set to `value`".
    /// - `Some(None)` on a nested `Option<Option<T>>` means "clear the field".
    /// - `Some(Some(v))` on a nested `Option<Option<T>>` means "set to `v`".
    #[derive(Debug, Clone, Default)]
    pub struct LifecycleDelta {
        pub status: Option<ChunkStatus>,
        pub tier: Option<MemoryTier>,
        pub supersedes: Option<ChunkId>,
        pub superseded_by: Option<ChunkId>,
        pub expires_at_ms: Option<Option<i64>>,
        pub review_after_ms: Option<Option<i64>>,
        pub lifecycle_updated_at_ms: Option<i64>,
    }

    impl LifecycleDelta {
        /// Returns true when no field is set — used by writers to skip
        /// no-op overlay UPDATEs (e.g. `add_chunk_with_lifecycle` with a
        /// default delta).
        pub fn is_empty(&self) -> bool {
            self.status.is_none()
                && self.tier.is_none()
                && self.supersedes.is_none()
                && self.superseded_by.is_none()
                && self.expires_at_ms.is_none()
                && self.review_after_ms.is_none()
                && self.lifecycle_updated_at_ms.is_none()
        }
    }

    impl LifecycleMetadata {
        /// Produce a new `LifecycleMetadata` with the delta applied.
        ///
        /// Fields left `None` in the delta are carried over unchanged.
        pub fn apply(&self, delta: &LifecycleDelta) -> Self {
            let mut next = self.clone();
            if let Some(tier) = delta.tier {
                next.tier = tier;
            }
            if let Some(ref s) = delta.supersedes {
                next.supersedes = Some(s.clone());
            }
            if let Some(ref s) = delta.superseded_by {
                next.superseded_by = Some(s.clone());
            }
            if let Some(exp) = delta.expires_at_ms {
                next.expires_at_ms = exp;
            }
            if let Some(rev) = delta.review_after_ms {
                next.review_after_ms = rev;
            }
            if let Some(ts) = delta.lifecycle_updated_at_ms {
                next.lifecycle_updated_at_ms = ts;
            }
            next
        }
    }

    /// A chunk paired with its current status and lifecycle overlay.
    ///
    /// Returned by lifecycle-aware retrieval helpers so callers see the
    /// authoritative current state without having to re-read side tables.
    #[derive(Debug, Clone)]
    pub struct ResolvedChunk {
        pub chunk: MemoryChunk,
        pub status: ChunkStatus,
        pub lifecycle: LifecycleMetadata,
    }

    /// Visibility policy for lifecycle-aware retrieval.
    ///
    /// Defaults hide non-active content: superseded, expired, and history-tier
    /// chunks are excluded unless explicitly opted in. `Candidate`, `Deleted`,
    /// and `Error` chunks are always hidden regardless of policy.
    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    pub struct VisibilityPolicy {
        #[serde(default)]
        pub include_superseded: bool,
        #[serde(default)]
        pub include_expired: bool,
        #[serde(default)]
        pub include_history: bool,
    }

    impl VisibilityPolicy {
        /// Check whether a chunk with the given status/tier should be visible.
        pub fn is_visible(&self, status: ChunkStatus, tier: MemoryTier) -> bool {
            match status {
                ChunkStatus::Candidate | ChunkStatus::Deleted | ChunkStatus::Error => false,
                ChunkStatus::Superseded if !self.include_superseded => false,
                ChunkStatus::Expired if !self.include_expired => false,
                _ => !matches!(tier, MemoryTier::History if !self.include_history),
            }
        }

        /// Wall-clock-aware visibility. Hides a chunk when either:
        /// 1. Its status/tier are not visible under the current flags, OR
        /// 2. It has a `lifecycle.expires_at_ms` that has passed and
        ///    `include_expired` is false.
        ///
        /// Prefer this over `is_visible` whenever the caller has access to
        /// a `now_ms` timestamp — it is the single consolidation point for
        /// the lifecycle visibility rule so B1 (search filtering) and C3/C4
        /// (expiry-driven tiering) do not each reimplement the clock check.
        pub fn is_visible_at(
            &self,
            status: ChunkStatus,
            lifecycle: &LifecycleMetadata,
            now_ms: i64,
        ) -> bool {
            if !self.is_visible(status, lifecycle.tier) {
                return false;
            }
            if !self.include_expired {
                if let Some(exp) = lifecycle.expires_at_ms {
                    if exp <= now_ms {
                        return false;
                    }
                }
            }
            true
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn lifecycle_metadata_defaults_and_apply_delta() {
            let lc = LifecycleMetadata::default();
            assert_eq!(lc.tier, MemoryTier::LongTerm);
            assert!(lc.supersedes.is_none());
            assert!(lc.superseded_by.is_none());
            assert!(lc.expires_at_ms.is_none());

            let delta = LifecycleDelta {
                superseded_by: Some(ChunkId::new()),
                tier: Some(MemoryTier::Working),
                lifecycle_updated_at_ms: Some(1_700_000_000_000),
                ..Default::default()
            };
            let next = lc.apply(&delta);
            assert!(next.superseded_by.is_some());
            assert_eq!(next.tier, MemoryTier::Working);
            assert_eq!(next.lifecycle_updated_at_ms, 1_700_000_000_000);
        }

        #[test]
        fn visibility_policy_default_hides_nonactive() {
            let p = VisibilityPolicy::default();
            assert!(!p.include_superseded);
            assert!(!p.include_expired);
            assert!(!p.include_history);
        }

        #[test]
        fn memory_tier_display_and_from_str_fails_closed() {
            use std::str::FromStr;
            assert_eq!(MemoryTier::Working.to_string(), "working");
            assert_eq!(MemoryTier::LongTerm.to_string(), "long_term");
            assert_eq!(MemoryTier::History.to_string(), "history");
            assert_eq!(
                MemoryTier::from_str("working").unwrap(),
                MemoryTier::Working
            );
            assert_eq!(
                MemoryTier::from_str("long_term").unwrap(),
                MemoryTier::LongTerm
            );
            assert_eq!(
                MemoryTier::from_str("history").unwrap(),
                MemoryTier::History
            );
            assert!(MemoryTier::from_str("bogus").is_err());
        }

        #[test]
        fn memory_tier_serde_snake_case() {
            assert_eq!(
                serde_json::to_string(&MemoryTier::Working).unwrap(),
                "\"working\""
            );
            assert_eq!(
                serde_json::to_string(&MemoryTier::LongTerm).unwrap(),
                "\"long_term\""
            );
            assert_eq!(
                serde_json::to_string(&MemoryTier::History).unwrap(),
                "\"history\""
            );
            let t: MemoryTier = serde_json::from_str("\"long_term\"").unwrap();
            assert_eq!(t, MemoryTier::LongTerm);
            let t: MemoryTier = serde_json::from_str("\"history\"").unwrap();
            assert_eq!(t, MemoryTier::History);
        }

        #[test]
        fn lifecycle_delta_clears_expires_and_review_via_some_none() {
            let lc = LifecycleMetadata {
                expires_at_ms: Some(1_000),
                review_after_ms: Some(2_000),
                ..LifecycleMetadata::default()
            };

            // Some(None) clears
            let cleared = lc.apply(&LifecycleDelta {
                expires_at_ms: Some(None),
                review_after_ms: Some(None),
                ..Default::default()
            });
            assert!(cleared.expires_at_ms.is_none());
            assert!(cleared.review_after_ms.is_none());

            // None leaves unchanged
            let left = lc.apply(&LifecycleDelta::default());
            assert_eq!(left.expires_at_ms, Some(1_000));
            assert_eq!(left.review_after_ms, Some(2_000));

            // Some(Some(v)) sets
            let set = lc.apply(&LifecycleDelta {
                expires_at_ms: Some(Some(3_000)),
                ..Default::default()
            });
            assert_eq!(set.expires_at_ms, Some(3_000));
            assert_eq!(set.review_after_ms, Some(2_000)); // untouched
        }

        #[test]
        fn lifecycle_delta_is_empty_default_and_set_fields() {
            // Default delta has every field as `None` — writers should skip the
            // overlay UPDATE in that case.
            assert!(LifecycleDelta::default().is_empty());

            // Any set field breaks emptiness.
            let with_tier = LifecycleDelta {
                tier: Some(MemoryTier::Working),
                ..Default::default()
            };
            assert!(!with_tier.is_empty());

            let with_cleared_expiry = LifecycleDelta {
                expires_at_ms: Some(None),
                ..Default::default()
            };
            assert!(!with_cleared_expiry.is_empty());
        }

        #[test]
        fn visibility_policy_is_visible_matrix() {
            let default = VisibilityPolicy::default();

            // Always hidden regardless of flags
            assert!(!default.is_visible(ChunkStatus::Candidate, MemoryTier::LongTerm));
            assert!(!default.is_visible(ChunkStatus::Deleted, MemoryTier::LongTerm));
            assert!(!default.is_visible(ChunkStatus::Error, MemoryTier::LongTerm));

            // Hidden by default, visible when flag flipped
            assert!(!default.is_visible(ChunkStatus::Superseded, MemoryTier::LongTerm));
            let inc_sup = VisibilityPolicy {
                include_superseded: true,
                ..Default::default()
            };
            assert!(inc_sup.is_visible(ChunkStatus::Superseded, MemoryTier::LongTerm));

            assert!(!default.is_visible(ChunkStatus::Expired, MemoryTier::LongTerm));
            let inc_exp = VisibilityPolicy {
                include_expired: true,
                ..Default::default()
            };
            assert!(inc_exp.is_visible(ChunkStatus::Expired, MemoryTier::LongTerm));

            // History tier hidden by default for otherwise-visible status
            assert!(!default.is_visible(ChunkStatus::Final, MemoryTier::History));
            let inc_hist = VisibilityPolicy {
                include_history: true,
                ..Default::default()
            };
            assert!(inc_hist.is_visible(ChunkStatus::Final, MemoryTier::History));

            // Active Final + LongTerm visible by default
            assert!(default.is_visible(ChunkStatus::Final, MemoryTier::LongTerm));
        }

        #[test]
        fn visibility_policy_is_visible_at_hides_clock_expired() {
            let lc = LifecycleMetadata {
                expires_at_ms: Some(500),
                ..LifecycleMetadata::default()
            };
            let default_p = VisibilityPolicy::default();
            // Status=Final, tier=LongTerm would normally be visible, but
            // the wall clock has passed the expiry threshold.
            assert!(!default_p.is_visible_at(ChunkStatus::Final, &lc, 1_000));
            // Edge case: exactly at expiry counts as expired (`<=`).
            assert!(!default_p.is_visible_at(ChunkStatus::Final, &lc, 500));
            // Before expiry: visible.
            assert!(default_p.is_visible_at(ChunkStatus::Final, &lc, 400));

            // include_expired flips the clock-check too, not just the
            // Expired status branch.
            let inc = VisibilityPolicy {
                include_expired: true,
                ..Default::default()
            };
            assert!(inc.is_visible_at(ChunkStatus::Final, &lc, 1_000));

            // No expires_at_ms: status/tier rule governs alone.
            let lc_none = LifecycleMetadata::default();
            assert!(default_p.is_visible_at(ChunkStatus::Final, &lc_none, 1_000));
            // Deleted still fails regardless of clock / expiry flag.
            assert!(!inc.is_visible_at(ChunkStatus::Deleted, &lc_none, 1_000));
        }
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
    fn project_id_validate_allows_repo_basenames_rejects_injection() {
        // Real project ids come from repo basenames: hyphens and dots allowed.
        assert!(ProjectId::validate("bester-hosting").is_ok());
        assert!(ProjectId::validate("proj_alpha").is_ok());
        assert!(ProjectId::validate("memd.v2").is_ok());
        assert!(ProjectId::validate("").is_ok()); // empty == tenant scope
        assert!(ProjectId::validate_opt(None).is_ok());
        // Injection / traversal vectors are rejected at the boundary.
        assert!(ProjectId::validate("alpha\n## SYSTEM: do x").is_err());
        assert!(ProjectId::validate("../escape").is_err());
        assert!(ProjectId::validate("a/b").is_err());
        assert!(ProjectId::validate("has space").is_err());
        assert!(ProjectId::validate("back`tick").is_err());
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

    #[test]
    fn ingestion_mode_display() {
        assert_eq!(IngestionMode::Conversation.to_string(), "conversation");
        assert_eq!(IngestionMode::Document.to_string(), "document");
    }

    #[test]
    fn chunk_status_lifecycle_variants_serialize() {
        assert_eq!(ChunkStatus::Candidate.to_string(), "candidate");
        assert_eq!(ChunkStatus::Superseded.to_string(), "superseded");
        assert_eq!(ChunkStatus::Expired.to_string(), "expired");

        // serialize lifecycle variants
        assert_eq!(
            serde_json::to_string(&ChunkStatus::Candidate).unwrap(),
            "\"candidate\""
        );
        assert_eq!(
            serde_json::to_string(&ChunkStatus::Superseded).unwrap(),
            "\"superseded\""
        );
        assert_eq!(
            serde_json::to_string(&ChunkStatus::Expired).unwrap(),
            "\"expired\""
        );

        // deserialize lifecycle variants
        let c: ChunkStatus = serde_json::from_str("\"candidate\"").unwrap();
        assert_eq!(c, ChunkStatus::Candidate);
        let s: ChunkStatus = serde_json::from_str("\"superseded\"").unwrap();
        assert_eq!(s, ChunkStatus::Superseded);
        let e: ChunkStatus = serde_json::from_str("\"expired\"").unwrap();
        assert_eq!(e, ChunkStatus::Expired);
    }

    #[test]
    fn chunk_status_from_str_fails_closed() {
        use std::str::FromStr;
        assert_eq!(
            ChunkStatus::from_str("candidate").unwrap(),
            ChunkStatus::Candidate
        );
        assert_eq!(
            ChunkStatus::from_str("superseded").unwrap(),
            ChunkStatus::Superseded
        );
        assert_eq!(
            ChunkStatus::from_str("expired").unwrap(),
            ChunkStatus::Expired
        );
        assert_eq!(ChunkStatus::from_str("final").unwrap(), ChunkStatus::Final);
        assert!(ChunkStatus::from_str("bogus").is_err());
    }
}
