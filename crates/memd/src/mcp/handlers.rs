//! Tool call handlers for MCP
//!
//! Bridges MCP tool calls to store operations.
//! Each handler validates parameters, calls the store, and formats the response.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

/// Whether retrieval that carries a `project_id` may widen across every
/// tenant on this daemon that contains the same project string.
///
/// Off by default: tenant isolation is the expected behavior, the
/// fallback is a migration shim only. `McpServer::new` sets this from
/// `ServerConfig.allow_cross_tenant_project_fallback`.
static ALLOW_CROSS_TENANT_PROJECT_FALLBACK: AtomicBool = AtomicBool::new(false);

/// Enable or disable the cross-tenant project fallback. Called from
/// `McpServer::new` / `McpServer::with_metrics` to honour the server
/// config. Exposed as `pub(crate)` so integration tests can flip it.
pub(crate) fn set_cross_tenant_project_fallback(enabled: bool) {
    ALLOW_CROSS_TENANT_PROJECT_FALLBACK.store(enabled, Ordering::Relaxed);
}

fn cross_tenant_project_fallback_enabled() -> bool {
    ALLOW_CROSS_TENANT_PROJECT_FALLBACK.load(Ordering::Relaxed)
}

/// Resolve an `agent_id` for an artifact write from an explicit param.
///
/// Rationale — the v0.3.0 prototype maintained a process-global default
/// derived from `initialize.clientInfo` in a `static RwLock<Option<String>>`.
/// That was unsound: `McpServer` is shared across every HTTP client
/// behind `Arc<AsyncMutex<_>>`, and `handle_initialize` rewrote the
/// default on each call. One session could overwrite another's
/// identity and bypass the distinct-writer countersignature rule, or a
/// single client could reinitialize as a different persona between
/// writes and forge a false countersignature.
///
/// v0.3.1 therefore keeps agent identity **explicit**: callers supply
/// `agent_id` on artifact writes when they want countersignature
/// promotion. The trust-tier check in `promote_if_countersigned` already
/// requires both the current and the parent artifact to have a
/// non-empty `agent_id`, so anonymous writes simply cannot produce a
/// false `VerifiedRecord`.
///
/// Per-session auto-population (without the bleed hazard) lands in
/// Phase 2 alongside the HTTP session model.
pub(crate) fn resolved_agent_id(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

use super::error::McpError;
// `pub use` (not plain `use`) so the legacy public path
// `memd::mcp::handlers::PostWriteEvent` keeps resolving for downstream
// code that named it directly before the Item 6 relocation. The
// canonical home is now `memd::mcp::post_write_hooks::PostWriteEvent`
// (re-exported at `memd::mcp::PostWriteEvent`), but dropping the
// nested path would be a silent semver shrink.
pub use super::post_write_hooks::PostWriteEvent;
use crate::metrics::{IndexStats, MetricsCollector};
use crate::store::metadata::MetadataStore;
use crate::store::{FeedbackEntry, RelevanceLabel, Store, StoreStats, TenantManager};
use crate::task_memory::{
    build_library_digest_artifact, build_project_brief_digest_artifact, build_project_brief_view,
    build_task_projections, build_task_projections_minimal, build_task_resume_digest_artifact,
    build_task_resume_view, derive_artifact_promotion_state, derive_artifact_trust_tier,
    derive_chunk_trust_tier, infer_decision_items, infer_evidence_items, infer_failure_items,
    infer_highlight_items, ArtifactKind, ContributorRef, DatasetRef, DecisionViewItem, EntityRef,
    EvidenceViewItem, FailureViewItem, HighlightViewItem, ProjectBriefView, TaskArtifact,
    TaskProvenance, TaskResumeView, TaskSearchFilters, TrustTier, DIGEST_ROLE_DECISION_LIBRARY,
    DIGEST_ROLE_EVIDENCE_LIBRARY, DIGEST_ROLE_FAILURE_LIBRARY, DIGEST_ROLE_HIGHLIGHT_LIBRARY,
    DIGEST_ROLE_PROJECT_BRIEF, DIGEST_ROLE_TASK_RESUME,
};
use crate::tiered::TieredTiming;
use crate::types::{
    ChunkId, ChunkStatus, ChunkType, LifecycleDelta, MemoryChunk, ProjectId, Source, TenantId,
    VisibilityPolicy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    #[default]
    Generic,
    BriefProject,
    ResumeTask,
    FindFailures,
    FindDecisions,
    FindEvidence,
    FindHighlights,
}

// ---------- Request Types ----------

/// Parameters for memory.search
#[derive(Debug, Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    pub tenant_id: String,
    pub query: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default)]
    pub filters: Option<SearchFilters>,
    /// Enable debug output showing tier source for each result
    #[serde(default)]
    pub debug_tiers: Option<bool>,
    #[serde(default)]
    pub mode: Option<QueryMode>,
    /// Return chunks with `status=Superseded` in results instead of hiding
    /// them. Maps 1:1 to `VisibilityPolicy::include_superseded`.
    ///
    /// Best-effort on dense-only tenants: compaction evicts lifecycle-
    /// hidden rows from the HNSW index on each rebuild (see Track B2), so
    /// `include_superseded=true` only surfaces rows that have not yet
    /// been evicted. For guaranteed access to a specific superseded
    /// chunk's payload, use `memory.get(chunk_id, include_superseded=true)`
    /// — that path queries the metadata overlay directly and does not
    /// depend on index retention.
    #[serde(default)]
    pub include_superseded: Option<bool>,
    /// Return chunks with `status=Expired` or a past `expires_at_ms` instead
    /// of hiding them. Maps 1:1 to `VisibilityPolicy::include_expired`.
    ///
    /// Same best-effort caveat as `include_superseded`: post-compaction,
    /// use `memory.get` for deterministic access to a specific expired
    /// chunk.
    #[serde(default)]
    pub include_expired: Option<bool>,
    /// Return chunks in `MemoryTier::History` instead of hiding them.
    /// Maps 1:1 to `VisibilityPolicy::include_history`.
    ///
    /// Same best-effort caveat as `include_superseded`.
    #[serde(default)]
    pub include_history: Option<bool>,
    /// Multiplier applied to `k` when deciding how many candidates to pull
    /// from the ranker before visibility filtering. Larger values give the
    /// visibility filter more headroom to refill to `k`; smaller values
    /// reduce cost but may under-fill when many top hits are hidden.
    /// Default 3, capped at 10, ignored when no visibility flag flips any
    /// row (i.e. all three include_* are true).
    #[serde(default)]
    pub oversample_factor: Option<usize>,
}

fn default_k() -> usize {
    20
}

impl Default for SearchParams {
    fn default() -> Self {
        // Mirrors the serde defaults so in-file `#[cfg(test)]` callers can
        // use `SearchParams { query: ..., ..Default::default() }` instead of
        // enumerating every optional field. Keep in sync with the serde
        // `default = "default_k"` and `#[serde(default)]` attributes on
        // the struct.
        Self {
            tenant_id: String::new(),
            query: String::new(),
            project_id: None,
            k: default_k(),
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
        }
    }
}

/// Optional filters for search
#[derive(Debug, Deserialize, Default)]
pub struct SearchFilters {
    #[serde(default)]
    pub types: Option<Vec<String>>,
    #[serde(default)]
    pub episode_id: Option<String>,
    #[serde(default)]
    pub time_range: Option<TimeRange>,
}

/// Time range filter
#[derive(Debug, Deserialize)]
pub struct TimeRange {
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Parameters for memory.add
#[derive(Debug, Deserialize, Default)]
pub struct AddParams {
    #[serde(default)]
    pub tenant_id: String,
    pub text: String,
    #[serde(rename = "type")]
    pub chunk_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub episode_id: Option<String>,
    #[serde(default)]
    pub source: Option<SourceParams>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional wall-clock expiry (ms since epoch). When set, the chunk is
    /// hidden at retrieval after this time (C2) and materialised to
    /// `status=Expired` by the compaction sweep (C3).
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    /// Optional review reminder (ms since epoch). Purely informational at
    /// the retrieval layer — does NOT hide the chunk — but callers may
    /// surface it to prompt review.
    #[serde(default)]
    pub review_after_ms: Option<i64>,
    /// Optional ingestion mode label (e.g. `"conversation"`, `"document"`).
    /// Accepted as part of the C1 surface so Track E can consume it
    /// without a second schema churn. No behaviour wired to it yet at the
    /// C1 layer — the field is persisted verbatim by Track E.
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional Track D conflict-aware ingestion knob. When set, prior
    /// rows in the same `(tenant, project)` (or whole tenant if
    /// `scope: "tenant"`) that match the new chunk's canonical form
    /// (exact mode) or trigram-Jaccard similarity (fuzzy mode) are
    /// atomically superseded with a back-edge to the new row. Absent or
    /// `false` keeps the legacy "always insert, never supersede"
    /// behaviour and the response shape stays backwards-compatible.
    #[serde(default)]
    pub supersede_near_duplicates: Option<DedupSpec>,
}

/// Track D conflict-aware ingestion descriptor. Accepts either:
/// * a bare boolean — `true` means "exact mode, scope: project, no
///   threshold" (matches the most common shorthand);
/// * a structured config that picks mode / threshold / scope.
///
/// Untagged so callers can pass the bare boolean form without naming
/// the struct.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum DedupSpec {
    Bool(bool),
    Config(DedupConfig),
}

/// Structured Track D dedup configuration.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct DedupConfig {
    /// `"exact"` (default) — match on canonical_text equality.
    /// `"fuzzy"` — match on trigram Jaccard ≥ `threshold` over the
    /// most recent N rows for the same scope.
    #[serde(default)]
    pub mode: Option<String>,
    /// Required when `mode == "fuzzy"`; ignored otherwise. If absent in
    /// fuzzy mode the handler picks 0.92 (paraphrase tier).
    #[serde(default)]
    pub threshold: Option<f32>,
    /// `"project"` (default) restricts the candidate pool to rows with
    /// the same project_id. `"tenant"` widens to the whole tenant.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Source information for a chunk
#[derive(Debug, Deserialize, Default)]
pub struct SourceParams {
    pub uri: Option<String>,
    pub repo: Option<String>,
    pub commit: Option<String>,
    pub path: Option<String>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
}

/// Single chunk for batch add
#[derive(Debug, Deserialize, Default)]
pub struct BatchChunkParams {
    pub text: String,
    #[serde(rename = "type")]
    pub chunk_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub episode_id: Option<String>,
    #[serde(default)]
    pub source: Option<SourceParams>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional wall-clock expiry (ms since epoch) for this chunk. Same
    /// semantics as `AddParams::expires_at_ms`.
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    /// Optional review reminder (ms since epoch) for this chunk. Same
    /// semantics as `AddParams::review_after_ms`.
    #[serde(default)]
    pub review_after_ms: Option<i64>,
    /// Optional ingestion mode label for this chunk. Same semantics as
    /// `AddParams::mode` — accepted now, consumed by Track E.
    #[serde(default)]
    pub mode: Option<String>,
}

impl BatchChunkParams {
    /// True when the chunk carries any Track C temporal overlay field.
    fn has_lifecycle_overlay(&self) -> bool {
        self.expires_at_ms.is_some() || self.review_after_ms.is_some()
    }
}

/// Parameters for memory.add_batch
#[derive(Debug, Deserialize)]
pub struct AddBatchParams {
    #[serde(default)]
    pub tenant_id: String,
    pub chunks: Vec<BatchChunkParams>,
    /// Optional Track D conflict-aware ingestion knob, applied per
    /// chunk in the batch. Same shape and semantics as
    /// `AddParams::supersede_near_duplicates`. When set, the response
    /// gains a `superseded_ids: [[...], ...]` parallel array (one
    /// inner array per input chunk).
    #[serde(default)]
    pub supersede_near_duplicates: Option<DedupSpec>,
}

/// Dataset reference supplied to task tools.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskDatasetRefParams {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Entity reference supplied to task tools.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskEntityRefParams {
    pub name: String,
    pub entity_type: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// Contributor metadata supplied to artifact tools.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskContributorParams {
    pub contributor_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub contribution: Option<String>,
}

/// Provenance supplied to task tools.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TaskProvenanceParams {
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_version: Option<String>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// Parameters for task.start
#[derive(Debug, Deserialize)]
pub struct TaskStartParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// The only hard-required field — everything else is an optional
    /// enrichment that the agent can backfill when it has the
    /// information. Phase 2.2 shrinks the required surface so callers
    /// are not forced to invent fields like `hypothesis` just to log
    /// "I started work on X".
    pub goal: String,
    #[serde(default)]
    pub motivation: String,
    #[serde(default)]
    pub hypothesis: String,
    #[serde(default)]
    pub scientific_question: String,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub expected_outputs: Vec<String>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.finish
#[derive(Debug, Deserialize)]
pub struct TaskFinishParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub scientific_question: Option<String>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    // Phase 2.2: the summary fields are now optional. Agents that just
    // want to close a task can do so without inventing content for
    // every axis; richer finishes populate what they know.
    #[serde(default)]
    pub what_worked: Vec<String>,
    #[serde(default)]
    pub what_failed: Vec<String>,
    #[serde(default)]
    pub validation: Vec<String>,
    #[serde(default)]
    pub uncertainty: Vec<String>,
    #[serde(default)]
    pub followups: Vec<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.progress
#[derive(Debug, Deserialize)]
pub struct TaskProgressParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub failed_attempts: Vec<String>,
    #[serde(default)]
    pub next_step: String,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.run_start
#[derive(Debug, Deserialize)]
pub struct TaskRunStartParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub tool_name: String,
    #[serde(default)]
    pub tool_version: Option<String>,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub why_chosen: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.run_finish
#[derive(Debug, Deserialize)]
pub struct TaskRunFinishParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_version: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub metrics: Option<Value>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub validation: Vec<String>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.add_evidence
#[derive(Debug, Deserialize)]
pub struct TaskAddEvidenceParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub summary: String,
    pub evidence_kind: String,
    #[serde(default)]
    pub supports_claim: Option<bool>,
    #[serde(default)]
    pub metric_name: Option<String>,
    #[serde(default)]
    pub metric_value: Option<Value>,
    #[serde(default)]
    pub metrics: Option<Value>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.get
#[derive(Debug, Deserialize)]
pub struct TaskGetParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task_id: String,
}

/// Exact task-aware filters for task.search.
#[derive(Debug, Deserialize, Default)]
pub struct TaskSearchFiltersParams {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub artifact_kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub challenge_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub reply_to_artifact_id: Option<String>,
    #[serde(default)]
    pub artifact_role: Option<String>,
    #[serde(default)]
    pub dataset_name: Option<String>,
    #[serde(default)]
    pub dataset_version: Option<String>,
    #[serde(default)]
    pub entity_name: Option<String>,
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub requested_action: Option<String>,
    #[serde(default)]
    pub verification_status: Option<String>,
    #[serde(default)]
    pub relation_kind: Option<String>,
}

/// Parameters for task.search
#[derive(Debug, Deserialize)]
pub struct TaskSearchParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default)]
    pub filters: Option<TaskSearchFiltersParams>,
    #[serde(default)]
    pub mode: Option<QueryMode>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectBriefParams {
    #[serde(default)]
    pub tenant_id: String,
    pub project_id: String,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default = "default_true")]
    pub include_related_projects: bool,
}

#[derive(Debug, Deserialize)]
pub struct TaskResumeParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_k")]
    pub k: usize,
}

#[derive(Debug, Deserialize)]
pub struct ArtifactLibraryParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_k")]
    pub k: usize,
}

/// Parameters for artifact.create
#[derive(Debug, Deserialize)]
pub struct ArtifactCreateParams {
    #[serde(default)]
    pub tenant_id: String,
    pub artifact_kind: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub artifact_role: Option<String>,
    #[serde(default)]
    pub challenge_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub reply_to_artifact_id: Option<String>,
    #[serde(default)]
    pub relation_kind: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub motivation: Option<String>,
    #[serde(default)]
    pub hypothesis: Option<String>,
    #[serde(default)]
    pub scientific_question: Option<String>,
    #[serde(default)]
    pub method_summary: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// Full-markdown body for `wiki_page` artifacts. Rejected at the
    /// MCP boundary on every other kind — see `handle_artifact_create`.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub evidence_kind: Option<String>,
    #[serde(default)]
    pub supports_claim: Option<bool>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub what_worked: Vec<String>,
    #[serde(default)]
    pub what_failed: Vec<String>,
    #[serde(default)]
    pub validation: Vec<String>,
    #[serde(default)]
    pub uncertainty: Vec<String>,
    #[serde(default)]
    pub followups: Vec<String>,
    #[serde(default)]
    pub expected_outputs: Vec<String>,
    #[serde(default)]
    pub related_artifact_ids: Vec<String>,
    #[serde(default)]
    pub contributors: Vec<TaskContributorParams>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_version: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub metrics: Option<Value>,
    #[serde(default)]
    pub why_chosen: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub requested_action: Option<String>,
    #[serde(default)]
    pub verification_status: Option<String>,
    #[serde(default)]
    pub compute_budget: Option<Value>,
    #[serde(default)]
    pub cost_actual: Option<Value>,
    #[serde(default)]
    pub data_access_level: Option<String>,
    #[serde(default)]
    pub policy_tags: Vec<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub approval_state: Option<String>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for artifact.get
#[derive(Debug, Deserialize)]
pub struct ArtifactGetParams {
    #[serde(default)]
    pub tenant_id: String,
    pub artifact_id: String,
}

/// Parameters for artifact.list_thread
#[derive(Debug, Deserialize)]
pub struct ArtifactListThreadParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub artifact_id: Option<String>,
}

/// Parameters for artifact.verify / artifact.find_related.
#[derive(Debug, Deserialize)]
pub struct ArtifactVerifyParams {
    #[serde(default)]
    pub tenant_id: String,
    pub claim: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub candidate_artifact_ids: Vec<String>,
    #[serde(default = "default_verify_k")]
    pub k: usize,
    #[serde(default)]
    pub include_digests: bool,
    #[serde(default)]
    pub create_artifact: bool,
    #[serde(default)]
    pub record_task_id: Option<String>,
    /// Optional `agent_id` for the verification record produced when
    /// `create_artifact = true`. Supplying this is required for
    /// distinct-writer countersignature promotion (see trust-tier
    /// rules) — anonymous verification artifacts can never upgrade
    /// trust, by design.
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// Parameters for memory.get
///
/// The three `include_*` flags map 1:1 onto `VisibilityPolicy`. They
/// default to `false` so memory.get hides non-active content (superseded
/// / expired / history) unless the caller explicitly opts in — mirroring
/// the overlay semantics documented on `VisibilityPolicy` in types.rs.
#[derive(Debug, Deserialize)]
pub struct GetParams {
    #[serde(default)]
    pub tenant_id: String,
    pub chunk_id: String,
    #[serde(default)]
    pub include_superseded: Option<bool>,
    #[serde(default)]
    pub include_expired: Option<bool>,
    #[serde(default)]
    pub include_history: Option<bool>,
}

/// Parameters for memory.delete
#[derive(Debug, Deserialize)]
pub struct DeleteParams {
    #[serde(default)]
    pub tenant_id: String,
    pub chunk_id: String,
}

/// Parameters for memory.stats
#[derive(Debug, Deserialize)]
pub struct StatsParams {
    #[serde(default)]
    pub tenant_id: String,
}

/// Parameters for memory.metrics
#[derive(Debug, Deserialize, Default)]
pub struct MetricsParams {
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default = "default_true")]
    pub include_recent: bool,
    /// Include tiered stats (cache, hot tier, promotions) - default true
    #[serde(default = "default_true")]
    pub include_tiered: bool,
}

fn default_verify_k() -> usize {
    8
}

/// Parameters for memory.compact
#[derive(Debug, Deserialize)]
pub struct CompactParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub digest_modes: Option<Vec<QueryMode>>,
    #[serde(default)]
    pub force_digest_rebuild: bool,
}

/// Parameters for memory.feedback
#[derive(Debug, Deserialize)]
pub struct FeedbackParams {
    #[serde(default)]
    pub tenant_id: String,
    pub query: String,
    pub chunk_id: String,
    pub relevance: String,
}

/// Parameters for memory.consolidate_episode
#[derive(Debug, Deserialize)]
pub struct ConsolidateEpisodeParams {
    #[serde(default)]
    pub tenant_id: String,
    pub episode_id: String,
    #[serde(default = "default_episode_limit")]
    pub max_chunks: usize,
    #[serde(default = "default_true")]
    pub retain_source_chunks: bool,
}

/// Parameters for context.list_subsystems
#[derive(Debug, Deserialize)]
pub struct ContextListSubsystemsParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// Parameters for context.get_files_for_subsystem
#[derive(Debug, Deserialize)]
pub struct ContextGetFilesForSubsystemParams {
    #[serde(default)]
    pub tenant_id: String,
    pub subsystem_key: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// Parameters for context.search_context_documents
#[derive(Debug, Deserialize)]
pub struct ContextSearchDocumentsParams {
    #[serde(default)]
    pub tenant_id: String,
    pub query: String,
    #[serde(default = "default_context_limit")]
    pub k: usize,
    #[serde(default)]
    pub subsystem_key: Option<String>,
    /// Optional tier filter: "hot" | "cold"
    #[serde(default)]
    pub tier: Option<String>,
}

/// Parameters for context.find_relevant_context
#[derive(Debug, Deserialize)]
pub struct ContextFindRelevantContextParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task: String,
    #[serde(default = "default_context_limit")]
    pub k: usize,
    #[serde(default)]
    pub subsystem_keys: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub include_hot: bool,
}

/// Parameters for context.suggest_agent
#[derive(Debug, Deserialize)]
pub struct ContextSuggestAgentParams {
    #[serde(default)]
    pub tenant_id: String,
    pub task: String,
    #[serde(default)]
    pub changed_files: Option<Vec<String>>,
    #[serde(default = "default_context_agent_limit")]
    pub k: usize,
}

/// Parameters for context.get_hot_context
#[derive(Debug, Deserialize)]
pub struct ContextGetHotContextParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default = "default_context_limit")]
    pub k: usize,
}

fn default_episode_limit() -> usize {
    50
}

fn default_context_limit() -> usize {
    20
}

fn default_context_agent_limit() -> usize {
    3
}

fn default_true() -> bool {
    true
}

fn default_depth() -> u32 {
    1
}

/// Parameters for code.find_definition
#[derive(Debug, Deserialize)]
pub struct FindDefinitionParams {
    #[serde(default)]
    pub tenant_id: String,
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Parameters for code.find_references
#[derive(Debug, Deserialize)]
pub struct FindReferencesParams {
    #[serde(default)]
    pub tenant_id: String,
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Parameters for code.find_callers
#[derive(Debug, Deserialize)]
pub struct FindCallersParams {
    #[serde(default)]
    pub tenant_id: String,
    pub name: String,
    #[serde(default = "default_depth")]
    pub depth: u32,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Parameters for code.find_imports
#[derive(Debug, Deserialize)]
pub struct FindImportsParams {
    #[serde(default)]
    pub tenant_id: String,
    pub module: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

fn default_limit() -> usize {
    50
}

/// Parameters for debug.find_tool_calls
#[derive(Debug, Deserialize)]
pub struct FindToolCallsParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub time_from: Option<String>,
    #[serde(default)]
    pub time_to: Option<String>,
    #[serde(default)]
    pub errors_only: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// Parameters for debug.find_errors
#[derive(Debug, Deserialize)]
pub struct FindErrorsParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub error_signature: Option<String>,
    #[serde(default)]
    pub function_name: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub time_from: Option<String>,
    #[serde(default)]
    pub time_to: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_true")]
    pub include_frames: bool,
}

// ---------- Response Types ----------

/// Result of a search operation
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub results: Vec<ChunkResult>,
    /// Tier debug info (only present when debug_tiers=true)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tier_info: Option<TierDebugInfo>,
    /// Repair-loop diagnostics when a fallback query rewrite was attempted
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repair_info: Option<RepairInfo>,
}

/// Debug information about tier performance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierDebugInfo {
    /// Primary source tier ("cache" | "hot" | "warm" | "hybrid")
    pub source_tier: String,
    /// Whether cache was hit
    pub cache_hit: bool,
    /// Whether hot tier returned results
    pub hot_tier_hit: bool,
    /// Cache lookup latency (ms)
    pub cache_lookup_ms: u64,
    /// Hot tier search latency (ms)
    pub hot_tier_ms: u64,
    /// Warm tier search latency (ms)
    pub warm_tier_ms: u64,
}

/// Diagnostics for query repair behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairInfo {
    pub attempted: bool,
    pub repaired: bool,
    pub original_query: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repaired_query: Option<String>,
}

/// Single chunk in search results
#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkResult {
    pub chunk_id: String,
    pub text: String,
    pub score: f32, // Stub: 1.0 for all results
    pub chunk_type: String,
    pub promotion_state: String,
    pub source: SourceResult,
    pub timestamp_created: i64,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub episode_id: Option<String>,
    /// Provenance-first citation details for this result
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub citation: Option<CitationResult>,
    #[serde(default)]
    pub trust_tier: TrustTier,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grounding_refs: Vec<GroundingRef>,
    #[serde(default)]
    pub verification_hint: VerificationHint,
    /// Which tier this result came from (only present when debug_tiers=true)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_tier: Option<String>,
    /// Canonical artifact linked to this projection chunk when available.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact: Option<TaskArtifact>,
}

/// Source information in results
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct SourceResult {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

impl From<&Source> for SourceResult {
    fn from(s: &Source) -> Self {
        Self {
            uri: s.uri.clone(),
            repo: s.repo.clone(),
            commit: s.commit.clone(),
            path: s.path.clone(),
            tool_name: s.tool_name.clone(),
            tool_call_id: s.tool_call_id.clone(),
        }
    }
}

/// Citation metadata for a search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationResult {
    pub citation_id: String,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chunk_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub total_chunks: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub char_start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub char_end: Option<usize>,
}

/// Canonical artifact pointer that can be used to ground a retrieved claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundingRef {
    pub artifact_id: String,
    pub task_id: String,
    pub thread_id: String,
    pub artifact_kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact_role: Option<String>,
    pub promotion_state: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub citation: Option<CitationResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerificationHint {
    pub requires_verification: bool,
    #[serde(default)]
    pub reason: String,
}

/// Result of an add operation
#[derive(Debug, Serialize, Deserialize)]
pub struct AddResult {
    pub chunk_id: String,
}

/// Result of a batch add operation
#[derive(Debug, Serialize, Deserialize)]
pub struct AddBatchResult {
    pub chunk_ids: Vec<String>,
}

/// Result of a task artifact write operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskArtifactResult {
    pub task_id: String,
    pub artifact_id: String,
    pub projection_chunk_ids: Vec<String>,
}

/// Result of task.get.
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskGetResult {
    pub task_id: String,
    pub artifacts: Vec<TaskArtifact>,
}

/// Result of artifact.get.
#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactGetResult {
    pub artifact: Option<TaskArtifact>,
}

/// One artifact returned from artifact.search.
#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactSearchHit {
    pub artifact: TaskArtifact,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub matched_chunk_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub matched_text: Option<String>,
    #[serde(default)]
    pub trust_tier: TrustTier,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grounding_refs: Vec<GroundingRef>,
    #[serde(default)]
    pub verification_hint: VerificationHint,
}

/// Result of artifact.search.
#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactSearchResult {
    pub results: Vec<ArtifactSearchHit>,
}

/// Result of artifact.list_thread.
#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactThreadResult {
    pub thread_id: String,
    pub artifacts: Vec<TaskArtifact>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectBriefResult {
    pub artifact: TaskArtifact,
    pub brief: ProjectBriefView,
    #[serde(default)]
    pub trust_tier: TrustTier,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grounding_refs: Vec<GroundingRef>,
    #[serde(default)]
    pub verification_hint: VerificationHint,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskResumeResult {
    pub artifact: TaskArtifact,
    pub resume: TaskResumeView,
    #[serde(default)]
    pub trust_tier: TrustTier,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grounding_refs: Vec<GroundingRef>,
    #[serde(default)]
    pub verification_hint: VerificationHint,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FailureSearchResult {
    pub artifact: TaskArtifact,
    pub results: Vec<FailureViewItem>,
    #[serde(default)]
    pub trust_tier: TrustTier,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grounding_refs: Vec<GroundingRef>,
    #[serde(default)]
    pub verification_hint: VerificationHint,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DecisionSearchViewResult {
    pub artifact: TaskArtifact,
    pub results: Vec<DecisionViewItem>,
    #[serde(default)]
    pub trust_tier: TrustTier,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grounding_refs: Vec<GroundingRef>,
    #[serde(default)]
    pub verification_hint: VerificationHint,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EvidenceSearchViewResult {
    pub artifact: TaskArtifact,
    pub results: Vec<EvidenceViewItem>,
    #[serde(default)]
    pub trust_tier: TrustTier,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grounding_refs: Vec<GroundingRef>,
    #[serde(default)]
    pub verification_hint: VerificationHint,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HighlightSearchViewResult {
    pub artifact: TaskArtifact,
    pub results: Vec<HighlightViewItem>,
    #[serde(default)]
    pub trust_tier: TrustTier,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grounding_refs: Vec<GroundingRef>,
    #[serde(default)]
    pub verification_hint: VerificationHint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingStatus {
    VerifiedRecord,
    CanonicallyGrounded,
    DigestOnly,
    InsufficientGrounding,
    Conflicted,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactVerifyResult {
    pub claim: String,
    pub grounding_status: GroundingStatus,
    pub confidence: f32,
    pub supporting_artifacts: Vec<GroundingRef>,
    pub conflicting_artifacts: Vec<GroundingRef>,
    pub consulted_digests: Vec<GroundingRef>,
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verification_artifact: Option<TaskArtifact>,
}

/// Result of a delete operation
#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteResult {
    pub deleted: bool,
}

/// Result of a feedback operation
#[derive(Debug, Serialize, Deserialize)]
pub struct FeedbackResult {
    pub stored: bool,
}

/// Result of memory.consolidate_episode
#[derive(Debug, Serialize, Deserialize)]
pub struct ConsolidateEpisodeResult {
    pub summary_chunk_id: String,
    pub source_chunk_count: usize,
    pub retained_source_chunks: bool,
}

/// Result of context.list_subsystems
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextListSubsystemsResult {
    pub subsystems: Vec<SubsystemSummary>,
}

/// Subsystem summary
#[derive(Debug, Serialize, Deserialize)]
pub struct SubsystemSummary {
    pub key: String,
    pub chunk_count: usize,
    pub file_count: usize,
}

/// Result of context.get_files_for_subsystem
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextGetFilesForSubsystemResult {
    pub subsystem_key: String,
    pub files: Vec<String>,
}

/// Result of context.search_context_documents
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextSearchDocumentsResult {
    pub results: Vec<ChunkResult>,
}

/// Result of context.find_relevant_context
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextFindRelevantContextResult {
    pub results: Vec<ChunkResult>,
    pub hot_included: bool,
}

/// Agent recommendation entry
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentSuggestion {
    pub agent_name: String,
    pub score: f32,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub matched_triggers: Vec<String>,
}

/// Result of context.suggest_agent
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextSuggestAgentResult {
    pub recommendations: Vec<AgentSuggestion>,
    pub considered_agents: usize,
}

/// Result of context.get_hot_context
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextGetHotContextResult {
    pub results: Vec<ChunkResult>,
}

/// Result of a stats operation
#[derive(Debug, Serialize, Deserialize)]
pub struct StatsResult {
    pub total_chunks: usize,
    pub deleted_chunks: usize,
    pub chunk_types: HashMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub disk_stats: Option<DiskStatsResult>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub compaction: Option<CompactionStatsResult>,
}

/// Disk statistics in stats result
#[derive(Debug, Serialize, Deserialize)]
pub struct DiskStatsResult {
    pub total_bytes: u64,
    pub segment_count: usize,
}

/// Compaction statistics in stats result
#[derive(Debug, Serialize, Deserialize)]
pub struct CompactionStatsResult {
    /// Ratio of deleted to total chunks (0.0 to 1.0)
    pub tombstone_ratio: f32,
    /// Number of active (non-deleted) chunks
    pub active_chunks: usize,
    /// Number of deleted chunks
    pub deleted_chunks: usize,
    /// Number of sparse index segments
    pub segment_count: usize,
    /// HNSW index staleness (0.0 to 1.0)
    pub hnsw_staleness: f32,
    /// Number of embeddings in HNSW cache
    pub hnsw_cache_size: usize,
    /// Number of embeddings in HNSW index
    pub hnsw_index_size: usize,
    /// Whether compaction is needed based on default thresholds
    pub needs_compaction: bool,
}

/// Combined tiered search statistics result
#[derive(Debug, Serialize, Deserialize)]
pub struct TieredStatsResult {
    /// Semantic cache statistics
    pub cache_stats: CacheStatsResult,
    /// Hot tier statistics
    pub hot_tier_stats: HotTierStatsResult,
    /// Tiered performance metrics
    pub metrics: TieredMetricsResult,
}

/// Cache statistics
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheStatsResult {
    /// Total cache lookups
    pub total_lookups: u64,
    /// Cache hits
    pub hits: u64,
    /// Cache misses
    pub misses: u64,
    /// Hit rate (0.0-1.0)
    pub hit_rate: f32,
    /// Number of entries in cache
    pub entry_count: usize,
    /// Average confidence of cached entries
    pub avg_confidence: f32,
}

/// Hot tier statistics
#[derive(Debug, Serialize, Deserialize)]
pub struct HotTierStatsResult {
    /// Number of chunks in hot tier
    pub chunk_count: usize,
    /// Capacity used (0.0-1.0)
    pub capacity_used: f32,
    /// Hot tier version
    pub version: u64,
    /// Average promotion score of chunks in hot tier
    pub avg_promotion_score: f32,
}

/// Tiered performance metrics
#[derive(Debug, Serialize, Deserialize)]
pub struct TieredMetricsResult {
    /// Total promotions
    pub promotions: u64,
    /// Total demotions
    pub demotions: u64,
    /// Average cache lookup latency (ms)
    pub avg_cache_ms: f64,
    /// Average hot tier search latency (ms)
    pub avg_hot_tier_ms: f64,
    /// Average warm tier search latency (ms)
    pub avg_warm_tier_ms: f64,
}

/// Result of code.find_definition
#[derive(Debug, Serialize, Deserialize)]
pub struct FindDefinitionResult {
    pub definitions: Vec<SymbolLocationResult>,
}

/// A symbol location in the codebase
#[derive(Debug, Serialize, Deserialize)]
pub struct SymbolLocationResult {
    pub file_path: String,
    pub name: String,
    pub kind: String,
    pub line_start: u32,
    pub line_end: u32,
    pub col_start: u32,
    pub col_end: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    pub language: String,
}

/// Result of code.find_references
#[derive(Debug, Serialize, Deserialize)]
pub struct FindReferencesResult {
    pub references: Vec<SymbolLocationResult>,
}

/// Result of code.find_callers
#[derive(Debug, Serialize, Deserialize)]
pub struct FindCallersResult {
    pub callers: Vec<CallerInfoResult>,
}

/// Information about a caller
#[derive(Debug, Serialize, Deserialize)]
pub struct CallerInfoResult {
    pub caller_name: String,
    pub caller_file: String,
    pub call_line: u32,
    pub call_col: u32,
    pub caller_kind: String,
    pub depth: u32,
}

/// Result of code.find_imports
#[derive(Debug, Serialize, Deserialize)]
pub struct FindImportsResult {
    pub imports: Vec<ImportInfoResult>,
}

/// Information about an import
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportInfoResult {
    pub importing_file: String,
    pub import_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

// ---------- Helper Functions ----------

fn validate_search_k(k: usize) -> Result<(), McpError> {
    if (1..=100).contains(&k) {
        return Ok(());
    }

    Err(McpError::InvalidParams(
        "invalid 'k': must be between 1 and 100".to_string(),
    ))
}

fn validate_search_time_range(
    filters: Option<&SearchFilters>,
) -> Result<(Option<i64>, Option<i64>), McpError> {
    let Some(time_range) = filters.and_then(|f| f.time_range.as_ref()) else {
        return Ok((None, None));
    };

    let from_ms = time_range
        .from
        .as_deref()
        .map(|s| {
            crate::structural::parse_iso_datetime(s).map_err(|e| {
                McpError::InvalidParams(format!("invalid filters.time_range.from: {}", e))
            })
        })
        .transpose()?;

    let to_ms = time_range
        .to
        .as_deref()
        .map(|s| {
            crate::structural::parse_iso_datetime(s).map_err(|e| {
                McpError::InvalidParams(format!("invalid filters.time_range.to: {}", e))
            })
        })
        .transpose()?;

    if let (Some(from_ms), Some(to_ms)) = (from_ms, to_ms) {
        if from_ms > to_ms {
            return Err(McpError::InvalidParams(
                "invalid filters.time_range: 'from' must be <= 'to'".to_string(),
            ));
        }
    }

    Ok((from_ms, to_ms))
}

#[derive(Debug, Default)]
struct ParsedSearchFilters {
    chunk_types: Option<HashSet<ChunkType>>,
    episode_id: Option<String>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
}

fn parse_search_filters(filters: Option<&SearchFilters>) -> Result<ParsedSearchFilters, McpError> {
    let (from_ms, to_ms) = validate_search_time_range(filters)?;

    let chunk_types = filters
        .and_then(|f| f.types.as_ref())
        .map(|types| {
            types
                .iter()
                .map(|t| parse_chunk_type(t))
                .collect::<Result<HashSet<_>, _>>()
        })
        .transpose()?;

    Ok(ParsedSearchFilters {
        chunk_types,
        episode_id: filters.and_then(|f| f.episode_id.clone()),
        from_ms,
        to_ms,
    })
}

fn apply_search_filters(
    scored_chunks: Vec<(MemoryChunk, f32)>,
    project_id: Option<&str>,
    filters: &ParsedSearchFilters,
    k: usize,
) -> Vec<(MemoryChunk, f32)> {
    scored_chunks
        .into_iter()
        .filter(|(chunk, _)| {
            if let Some(project_id) = project_id {
                if chunk.project_id.as_option() != Some(project_id) {
                    return false;
                }
            }

            if let Some(types) = filters.chunk_types.as_ref() {
                if !types.contains(&chunk.chunk_type) {
                    return false;
                }
            }

            if let Some(episode_id) = filters.episode_id.as_deref() {
                let expected_tag = format!("episode:{}", episode_id);
                if !chunk.tags.iter().any(|tag| tag == &expected_tag) {
                    return false;
                }
            }

            if let Some(from_ms) = filters.from_ms {
                if chunk.timestamp_created < from_ms {
                    return false;
                }
            }

            if let Some(to_ms) = filters.to_ms {
                if chunk.timestamp_created > to_ms {
                    return false;
                }
            }

            true
        })
        .take(k)
        .collect()
}

/// Apply the lifecycle visibility policy to an over-sampled ranked list and
/// trim to `k`. Superseded, Expired, and History-tier chunks are dropped
/// unless the corresponding `include_*` flag is set; rows with an
/// `expires_at_ms` that has already passed are dropped unless
/// `include_expired` is set; Deleted and Error rows are always dropped
/// regardless of flags (the `Error` hide is the reason this loop cannot
/// be short-circuited when all three `include_*` are true — the ranker
/// backends only filter `Deleted`, so `Error` can still reach the
/// handler and must be caught here).
///
/// "Oversample-and-refill" is the whole point: callers request more than
/// `k` candidates from the ranker so that even when the top hits are
/// hidden we can still return a full page of visible results.
///
/// Cross-tenant correctness: `memory.search` can return hits across
/// tenants when `project_id` is set. The visibility lookup must use the
/// hit row's own `chunk.tenant_id`, not an outer tenant parameter, or a
/// project-scoped search across tenants would point at the wrong overlay
/// rows.
///
/// Cost: one `get_with_lifecycle` per kept candidate. With the default
/// `oversample_factor=3` and `k=20`, this is up to 60 metadata reads per
/// query. This is a known tail-latency cost of the visibility overlay;
/// a cheaper design that carries `ResolvedChunk` from the ranker is a
/// future optimisation (tracked as a followup) but would require
/// changing the search return shape.
async fn apply_visibility_filter<S: Store>(
    store: &S,
    ranked: Vec<(MemoryChunk, f32)>,
    policy: &VisibilityPolicy,
    k: usize,
) -> Vec<(MemoryChunk, f32)> {
    let now_ms = current_time_ms();
    let mut out: Vec<(MemoryChunk, f32)> = Vec::with_capacity(k.min(ranked.len()));
    for (chunk, score) in ranked {
        if out.len() >= k {
            break;
        }
        match store
            .get_with_lifecycle(&chunk.tenant_id, &chunk.chunk_id)
            .await
        {
            Ok(Some(resolved)) => {
                if policy.is_visible_at(resolved.status, &resolved.lifecycle, now_ms) {
                    // Use the resolved chunk payload (same content, but
                    // from the overlay path — keeps any future overlay-
                    // side payload annotations consistent with memory.get).
                    out.push((resolved.chunk, score));
                }
            }
            Ok(None) => {
                // Row was deleted between the ranker pull and the
                // visibility check — drop it.
            }
            Err(e) => {
                // Transient overlay lookup failure: log and drop this
                // row rather than failing the whole search. Fail-closed
                // (drop) is safer than leaking a row whose status we
                // couldn't verify.
                warn!(
                    chunk_id = %chunk.chunk_id,
                    tenant_id = %chunk.tenant_id,
                    error = %e,
                    "visibility filter: get_with_lifecycle failed, dropping hit"
                );
            }
        }
    }
    out
}

/// Resolve the effective `VisibilityPolicy` and oversample factor for a
/// search call. The oversample factor is capped at 10 so a pathological
/// caller can't force a 100x ranker pull by setting it to 1000.
fn resolve_visibility_and_oversample(params: &SearchParams) -> (VisibilityPolicy, usize) {
    let policy = VisibilityPolicy {
        include_superseded: params.include_superseded.unwrap_or(false),
        include_expired: params.include_expired.unwrap_or(false),
        include_history: params.include_history.unwrap_or(false),
    };
    // When every include_* is true, the filter is effectively a no-op —
    // don't oversample.
    let all_permissive =
        policy.include_superseded && policy.include_expired && policy.include_history;
    let oversample = if all_permissive {
        1
    } else {
        params.oversample_factor.unwrap_or(3).clamp(1, 10)
    };
    (policy, oversample)
}

fn parse_tag_usize(tags: &[String], prefix: &str) -> Option<usize> {
    tags.iter().find_map(|tag| {
        tag.strip_prefix(prefix)
            .and_then(|value| value.parse().ok())
    })
}

fn extract_episode_id(tags: &[String]) -> Option<String> {
    tags.iter()
        .find_map(|tag| tag.strip_prefix("episode:").map(|value| value.to_string()))
}

fn make_episode_tag(episode_id: &str) -> String {
    format!("episode:{}", episode_id)
}

fn validate_episode_id(episode_id: &str) -> Result<(), McpError> {
    if episode_id.is_empty() {
        return Err(McpError::InvalidParams(
            "episode_id must not be empty".to_string(),
        ));
    }

    if episode_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(());
    }

    Err(McpError::InvalidParams(
        "episode_id must contain only letters, digits, '_' or '-'".to_string(),
    ))
}

fn parse_relevance_label(value: &str) -> Result<RelevanceLabel, McpError> {
    match value.to_ascii_lowercase().as_str() {
        "relevant" | "positive" => Ok(RelevanceLabel::Relevant),
        "irrelevant" | "negative" => Ok(RelevanceLabel::Irrelevant),
        _ => Err(McpError::InvalidParams(
            "invalid relevance: must be one of [relevant, irrelevant]".to_string(),
        )),
    }
}

fn build_citation(chunk: &MemoryChunk) -> CitationResult {
    let hash_prefix = chunk.hash.get(..12).unwrap_or(&chunk.hash);
    CitationResult {
        citation_id: format!("{}:{}", chunk.chunk_id, hash_prefix),
        content_hash: chunk.hash.clone(),
        source_uri: chunk.source.uri.clone(),
        source_repo: chunk.source.repo.clone(),
        source_commit: chunk.source.commit.clone(),
        source_path: chunk.source.path.clone(),
        source_tool_name: chunk.source.tool_name.clone(),
        source_tool_call_id: chunk.source.tool_call_id.clone(),
        chunk_index: parse_tag_usize(&chunk.tags, "chunk_index:"),
        total_chunks: parse_tag_usize(&chunk.tags, "total_chunks:"),
        char_start: parse_tag_usize(&chunk.tags, "char_start:"),
        char_end: parse_tag_usize(&chunk.tags, "char_end:"),
    }
}

fn build_grounding_ref(artifact: &TaskArtifact, citation: Option<CitationResult>) -> GroundingRef {
    GroundingRef {
        artifact_id: artifact.artifact_id.clone(),
        task_id: artifact.task_id.clone(),
        thread_id: artifact.thread_key().to_string(),
        artifact_kind: artifact.artifact_kind.as_str().to_string(),
        artifact_role: artifact.artifact_role.clone(),
        promotion_state: artifact.promotion_state.to_string(),
        citation,
    }
}

fn verification_hint_for_trust_tier(trust_tier: TrustTier) -> VerificationHint {
    match trust_tier {
        TrustTier::SemanticCandidate => VerificationHint {
            requires_verification: true,
            reason: "semantic candidate without canonical artifact grounding".to_string(),
        },
        TrustTier::CanonicalRecord => VerificationHint {
            requires_verification: false,
            reason: "linked to a canonical non-digest artifact".to_string(),
        },
        TrustTier::CompiledDigestHint => VerificationHint {
            requires_verification: true,
            reason:
                "compiled digest hint; re-ground against canonical artifacts before trusting claims"
                    .to_string(),
        },
        TrustTier::VerifiedRecord => VerificationHint {
            requires_verification: false,
            reason: "linked to an explicit verification or otherwise verified record".to_string(),
        },
    }
}

fn artifact_text_for_grounding(artifact: &TaskArtifact) -> String {
    let mut parts = Vec::new();
    if let Some(summary) = artifact
        .summary
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(summary.clone());
    }
    if let Some(goal) = artifact
        .goal
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(goal.clone());
    }
    if let Some(question) = artifact
        .scientific_question
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(question.clone());
    }
    if let Some(method) = artifact
        .method_summary
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(method.clone());
    }
    if let Some(command) = artifact
        .command
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(command.clone());
    }
    parts.extend(artifact.validation.clone());
    parts.extend(artifact.what_worked.clone());
    parts.extend(artifact.what_failed.clone());
    parts.extend(artifact.outputs.clone());
    parts.extend(artifact.followups.clone());
    if let Some(event_summary) = artifact.event_summary() {
        parts.push(event_summary);
    }
    parts.join(" ")
}

fn artifact_claim_score(artifact: &TaskArtifact, claim: &str) -> f32 {
    score_text_candidate(
        claim,
        &artifact_text_for_grounding(artifact),
        artifact.timestamp_created,
    )
}

fn artifact_has_negative_marker(artifact: &TaskArtifact) -> bool {
    if artifact.supports_claim == Some(false) {
        return true;
    }

    matches!(
        artifact
            .verification_status
            .as_deref()
            .map(|value| value.trim().to_ascii_lowercase()),
        Some(value)
            if matches!(
                value.as_str(),
                "rejected"
                    | "failed"
                    | "conflicted"
                    | "unsupported"
                    | "insufficient_grounding"
                    | "invalid"
            )
    )
}

fn artifact_supports_claim(artifact: &TaskArtifact, claim: &str, score: f32) -> bool {
    if matches!(
        derive_artifact_trust_tier(artifact),
        TrustTier::SemanticCandidate | TrustTier::CompiledDigestHint
    ) {
        return false;
    }
    if artifact_has_negative_marker(artifact) {
        return false;
    }

    score > 0.0
        || artifact_claim_score(artifact, claim) > 0.0
        || artifact.supports_claim == Some(true)
        || !artifact.validation.is_empty()
}

fn result_metadata(
    artifact: Option<&TaskArtifact>,
    citation: Option<CitationResult>,
) -> (TrustTier, Vec<GroundingRef>, VerificationHint) {
    let trust_tier = derive_chunk_trust_tier(artifact);
    let grounding_refs = artifact
        .map(|artifact| vec![build_grounding_ref(artifact, citation.clone())])
        .unwrap_or_default();
    let verification_hint = verification_hint_for_trust_tier(trust_tier);
    (trust_tier, grounding_refs, verification_hint)
}

fn build_artifact_search_hit(
    artifact: TaskArtifact,
    score: f32,
    matched_chunk: Option<&MemoryChunk>,
) -> ArtifactSearchHit {
    let trust_tier = derive_artifact_trust_tier(&artifact);
    let grounding_refs = vec![build_grounding_ref(
        &artifact,
        matched_chunk.map(build_citation),
    )];
    ArtifactSearchHit {
        artifact,
        score,
        matched_chunk_id: matched_chunk.map(|chunk| chunk.chunk_id.to_string()),
        matched_text: matched_chunk.map(|chunk| chunk.text.clone()),
        trust_tier,
        grounding_refs,
        verification_hint: verification_hint_for_trust_tier(trust_tier),
    }
}

async fn artifact_lookup_tenants<S: Store>(
    store: &S,
    primary_tenant: &TenantId,
    project_id: Option<&str>,
) -> Result<Vec<TenantId>, McpError> {
    if let Some(project_id) = project_id.filter(|value| !value.trim().is_empty()) {
        return scoped_tenants_for_project(store, primary_tenant, Some(project_id)).await;
    }

    // Without a project_id filter, looking up an artifact by id normally
    // stays within the caller's tenant. The daemon-wide sweep is only
    // available when the operator has opted into the cross-tenant
    // compatibility fallback.
    if !cross_tenant_project_fallback_enabled() {
        return Ok(vec![primary_tenant.clone()]);
    }

    let mut tenants = vec![primary_tenant.clone()];
    let mut seen = HashSet::from([primary_tenant.to_string()]);
    for tenant in store
        .list_tenants()
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        if seen.insert(tenant.to_string()) {
            tenants.push(tenant);
        }
    }
    Ok(tenants)
}

async fn get_artifact_by_id_in_scope<S: Store>(
    store: &S,
    lookup_tenants: &[TenantId],
    artifact_id: &str,
) -> Result<Option<TaskArtifact>, McpError> {
    for tenant in lookup_tenants {
        if let Some(artifact) = store
            .get_task_artifact(tenant, artifact_id)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
        {
            return Ok(Some(artifact));
        }
    }
    Ok(None)
}

async fn resolve_grounding_refs_by_artifact_ids<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    artifact_ids: &[String],
    limit: usize,
) -> Result<Vec<GroundingRef>, McpError> {
    let lookup_tenants = artifact_lookup_tenants(store, tenant_id, project_id).await?;
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for artifact_id in artifact_ids {
        if !seen.insert(artifact_id.clone()) {
            continue;
        }
        if let Some(artifact) =
            get_artifact_by_id_in_scope(store, &lookup_tenants, artifact_id).await?
        {
            refs.push(build_grounding_ref(&artifact, None));
            if refs.len() >= limit {
                break;
            }
        }
    }
    Ok(refs)
}

const TAG_CTX_TIER_HOT: &str = "ctx:tier:hot";
const TAG_CTX_TIER_COLD: &str = "ctx:tier:cold";
const TAG_CTX_DOC: &str = "ctx:doc";
const TAG_CTX_SUBSYSTEM_PREFIX: &str = "ctx:subsystem:";
const TAG_CTX_FILE_PREFIX: &str = "ctx:file:";
const TAG_CTX_TRIGGER_PREFIX: &str = "ctx:trigger:";
const TAG_CTX_AGENT_PREFIX: &str = "ctx:agent:";

fn has_exact_tag(tags: &[String], expected: &str) -> bool {
    tags.iter().any(|tag| tag == expected)
}

fn tag_values(tags: &[String], prefix: &str) -> Vec<String> {
    tags.iter()
        .filter_map(|tag| tag.strip_prefix(prefix).map(str::to_string))
        .collect()
}

fn chunk_matches_subsystem(chunk: &MemoryChunk, subsystem_key: &str) -> bool {
    tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX)
        .iter()
        .any(|value| value == subsystem_key)
}

fn chunk_matches_any_subsystem(chunk: &MemoryChunk, subsystem_keys: &[String]) -> bool {
    if subsystem_keys.is_empty() {
        return true;
    }
    subsystem_keys
        .iter()
        .any(|key| chunk_matches_subsystem(chunk, key))
}

fn chunk_matches_tier(chunk: &MemoryChunk, tier: Option<&str>) -> bool {
    match tier {
        Some("hot") => has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT),
        Some("cold") => has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD),
        Some(_) => false,
        None => true,
    }
}

fn is_context_chunk(chunk: &MemoryChunk) -> bool {
    if has_exact_tag(&chunk.tags, TAG_CTX_DOC)
        || has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT)
        || has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD)
        || !tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX).is_empty()
    {
        return true;
    }

    matches!(
        chunk.chunk_type,
        ChunkType::Doc
            | ChunkType::Research
            | ChunkType::Decision
            | ChunkType::Plan
            | ChunkType::Summary
    )
}

fn chunk_to_result(
    chunk: &MemoryChunk,
    score: f32,
    source_tier: Option<String>,
    artifact: Option<TaskArtifact>,
) -> ChunkResult {
    let citation = Some(build_citation(chunk));
    let (trust_tier, grounding_refs, verification_hint) =
        result_metadata(artifact.as_ref(), citation.clone());
    ChunkResult {
        chunk_id: chunk.chunk_id.to_string(),
        text: chunk.text.clone(),
        score,
        chunk_type: chunk.chunk_type.to_string(),
        promotion_state: chunk.promotion_state.to_string(),
        source: SourceResult::from(&chunk.source),
        timestamp_created: chunk.timestamp_created,
        tags: chunk.tags.clone(),
        episode_id: extract_episode_id(&chunk.tags),
        citation,
        trust_tier,
        grounding_refs,
        verification_hint,
        source_tier,
        artifact,
    }
}

async fn collect_all_chunks<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    max_chunks: usize,
) -> Result<Vec<MemoryChunk>, McpError> {
    if max_chunks == 0 {
        return Ok(Vec::new());
    }

    let page_size = 200usize.min(max_chunks.max(1));
    let mut offset = 0usize;
    let mut chunks = Vec::new();

    loop {
        let page = store
            .list_chunks(tenant_id, page_size, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if page.is_empty() {
            break;
        }

        for chunk in page {
            chunks.push(chunk);
            if chunks.len() >= max_chunks {
                return Ok(chunks);
            }
        }

        offset = offset.saturating_add(page_size);
    }

    Ok(chunks)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();

    if pattern == "*" {
        return true;
    }

    if !pattern.contains('*') {
        return text.contains(&pattern);
    }

    let parts: Vec<&str> = pattern.split('*').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return true;
    }

    let mut cursor = 0usize;
    for (idx, part) in parts.iter().enumerate() {
        let slice = &text[cursor..];
        let Some(found) = slice.find(part) else {
            return false;
        };

        if idx == 0 && !pattern.starts_with('*') && found != 0 {
            return false;
        }

        cursor += found + part.len();
    }

    if !pattern.ends_with('*') {
        if let Some(last) = parts.last() {
            return text.ends_with(last);
        }
    }

    true
}

fn has_active_search_filters(project_id: Option<&str>, filters: &ParsedSearchFilters) -> bool {
    project_id.is_some()
        || filters.chunk_types.is_some()
        || filters.episode_id.is_some()
        || filters.from_ms.is_some()
        || filters.to_ms.is_some()
}

fn adaptive_fetch_k(k: usize, query: &str, has_filters: bool) -> usize {
    if has_filters {
        return 100;
    }

    let token_count = query.split_whitespace().count();
    let is_complex = token_count >= 6 || query.len() >= 80;
    if is_complex {
        return (k.saturating_mul(2)).clamp(1, 100);
    }

    k
}

fn normalize_query_for_repair(query: &str) -> Option<String> {
    let normalized = query
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    if normalized.is_empty() {
        return None;
    }

    let original = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized == original.to_lowercase() {
        None
    } else {
        Some(normalized)
    }
}

fn build_episode_summary_text(episode_id: &str, chunks: &[MemoryChunk]) -> String {
    let mut lines = Vec::with_capacity(chunks.len() + 1);
    lines.push(format!(
        "Episode {} summary ({} chunks)",
        episode_id,
        chunks.len()
    ));

    for chunk in chunks {
        let snippet = chunk
            .text
            .replace('\n', " ")
            .chars()
            .take(180)
            .collect::<String>();
        lines.push(format!("- [{}] {}", chunk.chunk_type, snippet));
    }

    lines.join("\n")
}

/// Parse a chunk type string into ChunkType enum
/// Parse the `mode` request param into an `IngestionMode`. Empty / None
/// returns the default (`Document`). Unknown values fail-closed with a
/// clear MCP error so callers learn about the typo immediately.
pub(crate) fn parse_ingestion_mode(s: Option<&str>) -> Result<crate::types::IngestionMode, McpError> {
    use crate::types::IngestionMode;
    let trimmed = s.map(|x| x.trim()).filter(|x| !x.is_empty());
    match trimmed {
        None => Ok(IngestionMode::default()),
        Some(value) => value.parse::<IngestionMode>().map_err(|e| {
            McpError::InvalidParams(format!(
                "invalid ingestion mode '{}': {}; expected 'conversation' or 'document'",
                value, e
            ))
        }),
    }
}

/// E2: when ingestion_mode is Conversation and the caller did not pass
/// an explicit `review_after_ms`, default to `now() + 14 days` so the
/// chunk surfaces in the review stream after roughly two weeks.
/// Document-mode writes and Conversation-mode writes that already
/// carry an explicit `review_after_ms` are passed through unchanged.
pub(crate) fn apply_conversation_review_default(
    mode: crate::types::IngestionMode,
    review_after_ms: Option<i64>,
) -> Option<i64> {
    use crate::types::IngestionMode;
    if review_after_ms.is_some() || mode != IngestionMode::Conversation {
        return review_after_ms;
    }
    const FOURTEEN_DAYS_MS: i64 = 14 * 24 * 60 * 60 * 1000;
    Some(current_time_ms() + FOURTEEN_DAYS_MS)
}

fn parse_chunk_type(s: &str) -> Result<ChunkType, McpError> {
    match s.to_lowercase().as_str() {
        "code" => Ok(ChunkType::Code),
        "doc" | "scientific" => Ok(ChunkType::Doc), // Map scientific documents to Doc type
        "trace" => Ok(ChunkType::Trace),
        "decision" => Ok(ChunkType::Decision),
        "plan" => Ok(ChunkType::Plan),
        "research" => Ok(ChunkType::Research),
        "message" => Ok(ChunkType::Message),
        "summary" => Ok(ChunkType::Summary),
        "general" | "other" => Ok(ChunkType::Other),
        _ => Err(McpError::InvalidParams(format!(
            "invalid chunk type '{}', must be one of: code, doc, scientific, trace, decision, plan, research, message, summary, general, other",
            s
        ))),
    }
}

/// Resolve and validate `tenant_id` from a tool call.
///
/// Resolution order — the first non-empty value wins:
///   1. explicit value from the call params
///   2. `$MEMD_DEFAULT_TENANT` environment variable
///   3. `~/.memd/default_tenant` file (single line, trimmed)
///   4. the literal string `"default"`
///
/// This is the Phase 2.1 adoption fix: `tenant_id` became optional on
/// every tool schema, and agents that do not know their tenant
/// (typical: a fresh Claude Code session) still end up writing to a
/// stable local tenant instead of failing the call. Operators who run
/// one daemon for multiple logical spaces can pin the default via the
/// env var or file.
///
/// The returned `TenantId` is always validated against
/// `TenantId::validate`, so even operator-supplied defaults cannot
/// escape the storage layout.
fn resolve_tenant_id(explicit: &str) -> Result<TenantId, McpError> {
    fn try_build(value: &str, source: &'static str) -> Option<Result<TenantId, McpError>> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(TenantId::new(trimmed).map_err(|e| {
            McpError::InvalidParams(format!("invalid tenant_id from {}: {}", source, e))
        }))
    }

    if let Some(result) = try_build(explicit, "call params") {
        return result;
    }

    if let Ok(env_value) = std::env::var("MEMD_DEFAULT_TENANT") {
        if let Some(result) = try_build(&env_value, "$MEMD_DEFAULT_TENANT") {
            return result;
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let pinned = std::path::PathBuf::from(home)
            .join(".memd")
            .join("default_tenant");
        if let Ok(contents) = std::fs::read_to_string(&pinned) {
            if let Some(result) = try_build(&contents, "~/.memd/default_tenant") {
                return result;
            }
        }
    }

    // Final fallback: a literal "default" tenant. Always valid per
    // `TenantId::validate` (ASCII alphanumeric).
    TenantId::new("default").map_err(|e| McpError::InvalidParams(e.to_string()))
}

/// Legacy alias. Kept so older call sites that want the strict "caller
/// supplied a tenant_id" semantics do not accidentally pick up the
/// file/env/default fallback. Prefer `resolve_tenant_id` for new code.
#[allow(dead_code)]
fn validate_tenant_id(tenant_id: &str) -> Result<TenantId, McpError> {
    resolve_tenant_id(tenant_id)
}

/// Validate chunk_id and return ChunkId
fn validate_chunk_id(chunk_id: &str) -> Result<ChunkId, McpError> {
    ChunkId::parse(chunk_id).map_err(|e| McpError::InvalidParams(e.to_string()))
}

fn validate_identifier(name: &str, value: &str) -> Result<(), McpError> {
    if value.trim().is_empty() {
        return Err(McpError::InvalidParams(format!(
            "{} must not be empty",
            name
        )));
    }
    Ok(())
}

fn validate_confidence(confidence: f32) -> Result<(), McpError> {
    if !(0.0..=1.0).contains(&confidence) {
        return Err(McpError::InvalidParams(
            "confidence must be between 0.0 and 1.0".to_string(),
        ));
    }
    Ok(())
}

fn dataset_params_to_refs(params: Vec<TaskDatasetRefParams>) -> Result<Vec<DatasetRef>, McpError> {
    let mut refs = Vec::with_capacity(params.len());
    for dataset in params {
        validate_identifier("dataset_refs[].name", &dataset.name)?;
        refs.push(DatasetRef {
            name: dataset.name,
            version: dataset.version,
            description: dataset.description,
        });
    }
    Ok(refs)
}

fn entity_params_to_refs(params: Vec<TaskEntityRefParams>) -> Result<Vec<EntityRef>, McpError> {
    let mut refs = Vec::with_capacity(params.len());
    for entity in params {
        validate_identifier("entity_refs[].name", &entity.name)?;
        validate_identifier("entity_refs[].entity_type", &entity.entity_type)?;
        refs.push(EntityRef {
            name: entity.name,
            entity_type: entity.entity_type,
            role: entity.role,
        });
    }
    Ok(refs)
}

fn contributor_params_to_refs(
    params: Vec<TaskContributorParams>,
) -> Result<Vec<ContributorRef>, McpError> {
    let mut refs = Vec::with_capacity(params.len());
    for contributor in params {
        validate_identifier("contributors[].contributor_id", &contributor.contributor_id)?;
        refs.push(ContributorRef {
            contributor_id: contributor.contributor_id,
            display_name: contributor.display_name,
            role: contributor.role,
            contribution: contributor.contribution,
        });
    }
    Ok(refs)
}

fn params_to_task_provenance(params: Option<TaskProvenanceParams>) -> TaskProvenance {
    params
        .map(|p| TaskProvenance {
            uri: p.uri,
            repo: p.repo,
            commit: p.commit,
            path: p.path,
            tool_name: p.tool_name,
            tool_version: p.tool_version,
            tool_call_id: p.tool_call_id,
        })
        .unwrap_or_default()
}

fn parse_task_search_filters(
    filters: Option<&TaskSearchFiltersParams>,
) -> Result<TaskSearchFilters, McpError> {
    let Some(filters) = filters else {
        return Ok(TaskSearchFilters::default());
    };

    let artifact_kind = filters
        .artifact_kind
        .as_deref()
        .map(ArtifactKind::from_str)
        .transpose()
        .map_err(McpError::InvalidParams)?;

    Ok(TaskSearchFilters {
        task_id: filters.task_id.clone(),
        artifact_kind,
        status: filters.status.clone(),
        challenge_id: filters.challenge_id.clone(),
        thread_id: filters.thread_id.clone(),
        reply_to_artifact_id: filters.reply_to_artifact_id.clone(),
        artifact_role: filters.artifact_role.clone(),
        dataset_name: filters.dataset_name.clone(),
        dataset_version: filters.dataset_version.clone(),
        entity_name: filters.entity_name.clone(),
        entity_type: filters.entity_type.clone(),
        tool_name: filters.tool_name.clone(),
        project_id: filters.project_id.clone(),
        agent_id: filters.agent_id.clone(),
        session_id: filters.session_id.clone(),
        requested_action: filters.requested_action.clone(),
        verification_status: filters.verification_status.clone(),
        relation_kind: filters.relation_kind.clone(),
    })
}

fn has_active_task_filters(filters: &TaskSearchFilters) -> bool {
    filters.task_id.is_some()
        || filters.artifact_kind.is_some()
        || filters.status.is_some()
        || filters.challenge_id.is_some()
        || filters.thread_id.is_some()
        || filters.reply_to_artifact_id.is_some()
        || filters.artifact_role.is_some()
        || filters.dataset_name.is_some()
        || filters.dataset_version.is_some()
        || filters.entity_name.is_some()
        || filters.entity_type.is_some()
        || filters.tool_name.is_some()
        || filters.project_id.is_some()
        || filters.agent_id.is_some()
        || filters.session_id.is_some()
        || filters.requested_action.is_some()
        || filters.verification_status.is_some()
        || filters.relation_kind.is_some()
}

/// Convert SourceParams to Source
fn params_to_source(params: Option<SourceParams>) -> Source {
    params
        .map(|p| Source {
            uri: p.uri,
            repo: p.repo,
            commit: p.commit,
            path: p.path,
            tool_name: p.tool_name,
            tool_call_id: p.tool_call_id,
        })
        .unwrap_or_default()
}

/// Format result as MCP content response
fn format_mcp_response<T: Serialize>(result: &T) -> Result<Value, McpError> {
    let json_str = serde_json::to_string(result)
        .map_err(|e| McpError::ToolError(format!("failed to serialize response: {}", e)))?;

    Ok(json!({
        "content": [{
            "type": "text",
            "text": json_str
        }]
    }))
}

async fn resolve_artifacts_for_ranked_chunks<S: Store>(
    store: &S,
    ranked: &[(MemoryChunk, f32)],
) -> Result<HashMap<String, TaskArtifact>, McpError> {
    let mut by_tenant: HashMap<String, (TenantId, Vec<ChunkId>)> = HashMap::new();
    for (chunk, _) in ranked {
        by_tenant
            .entry(chunk.tenant_id.to_string())
            .or_insert_with(|| (chunk.tenant_id.clone(), Vec::new()))
            .1
            .push(chunk.chunk_id.clone());
    }

    let mut artifacts = HashMap::new();
    for (_, (tenant_id, chunk_ids)) in by_tenant {
        artifacts.extend(
            store
                .resolve_artifacts_for_chunks(&tenant_id, &chunk_ids)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
    }
    Ok(artifacts)
}

fn default_status_for_artifact_kind(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::TaskStart | ArtifactKind::TaskProgress => "in_progress",
        ArtifactKind::RunStart => "started",
        ArtifactKind::RunFinish | ArtifactKind::TaskFinish => "completed",
        ArtifactKind::Digest => "generated",
        ArtifactKind::WikiPage => "authored",
        ArtifactKind::Evidence
        | ArtifactKind::Review
        | ArtifactKind::Revision
        | ArtifactKind::Verification
        | ArtifactKind::Decision => "recorded",
    }
}

fn score_text_candidate(query: &str, text: &str, timestamp_created: i64) -> f32 {
    if query.trim().is_empty() {
        return timestamp_created as f32 / 1_000_000_000_000.0;
    }

    let lower_text = text.to_ascii_lowercase();
    let terms = query
        .split_whitespace()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut score = 0.0f32;
    for term in &terms {
        if lower_text.contains(term) {
            score += 1.0;
        }
    }
    if lower_text.contains(&query.to_ascii_lowercase()) {
        score += 2.0;
    }
    score + (timestamp_created as f32 / 1_000_000_000_000.0)
}

fn sort_ranked_items<T, F>(items: &mut [T], query: &str, score_fn: F)
where
    F: Fn(&T) -> (String, i64, bool),
{
    items.sort_by(|left, right| {
        let (left_text, left_ts, left_explicit) = score_fn(left);
        let (right_text, right_ts, right_explicit) = score_fn(right);
        right_explicit
            .cmp(&left_explicit)
            .then_with(|| {
                score_text_candidate(query, &right_text, right_ts)
                    .partial_cmp(&score_text_candidate(query, &left_text, left_ts))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right_ts.cmp(&left_ts))
    });
}

fn sort_highlight_items(items: &mut [HighlightViewItem], query: &str) {
    items.sort_by(|left, right| {
        if query.trim().is_empty() {
            return right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.timestamp_created.cmp(&left.timestamp_created));
        }

        let left_text = format!("{} {}", left.summary, left.rationale);
        let right_text = format!("{} {}", right.summary, right.rationale);
        let left_rank =
            score_text_candidate(query, &left_text, left.timestamp_created) + left.score;
        let right_rank =
            score_text_candidate(query, &right_text, right.timestamp_created) + right.score;
        right_rank
            .partial_cmp(&left_rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
    });
}

async fn scoped_tenants_for_project<S: Store>(
    store: &S,
    primary_tenant: &TenantId,
    project_id: Option<&str>,
) -> Result<Vec<TenantId>, McpError> {
    let Some(project_id) = project_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(vec![primary_tenant.clone()]);
    };

    // Default behavior: tenant isolation. Only widen when the operator
    // has explicitly opted into the cross-tenant fallback via
    // `server.allow_cross_tenant_project_fallback = true`.
    if !cross_tenant_project_fallback_enabled() {
        return Ok(vec![primary_tenant.clone()]);
    }

    let mut scoped = vec![primary_tenant.clone()];
    let mut seen = HashSet::from([primary_tenant.to_string()]);
    for tenant in store
        .list_tenants()
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        if !seen.insert(tenant.to_string()) {
            continue;
        }
        let has_project = !store
            .list_tasks(&tenant, Some(project_id), 1)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
            .is_empty();
        if has_project {
            warn!(
                primary_tenant = %primary_tenant,
                extra_tenant = %tenant,
                project_id,
                "cross-tenant project fallback widened retrieval beyond the caller's tenant"
            );
            scoped.push(tenant);
        }
    }
    Ok(scoped)
}

fn merge_scored_chunk_lists(
    scored_lists: Vec<Vec<(MemoryChunk, f32)>>,
    limit: usize,
) -> Vec<(MemoryChunk, f32)> {
    let mut merged = scored_lists.into_iter().flatten().collect::<Vec<_>>();
    merged.sort_by(|(left_chunk, left_score), (right_chunk, right_score)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right_chunk
                    .timestamp_created
                    .cmp(&left_chunk.timestamp_created)
            })
    });
    let mut seen = HashSet::new();
    merged
        .into_iter()
        .filter(|(chunk, _)| seen.insert(chunk.chunk_id.clone()))
        .take(limit)
        .collect()
}

async fn search_with_scores_for_tenants<S: Store>(
    store: &S,
    tenants: &[TenantId],
    query: &str,
    fetch_k: usize,
) -> Result<Vec<(MemoryChunk, f32)>, McpError> {
    let mut lists = Vec::with_capacity(tenants.len());
    for tenant in tenants {
        lists.push(
            store
                .search_with_scores(tenant, query, fetch_k)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
    }
    Ok(merge_scored_chunk_lists(
        lists,
        fetch_k.saturating_mul(tenants.len().max(1)),
    ))
}

async fn search_with_tier_info_for_tenants<S: Store>(
    store: &S,
    tenants: &[TenantId],
    query: &str,
    fetch_k: usize,
) -> Result<(Vec<(MemoryChunk, f32)>, Option<TieredTiming>), McpError> {
    if tenants.len() == 1 {
        return store
            .search_with_tier_info(&tenants[0], query, fetch_k)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()));
    }

    let mut lists = Vec::with_capacity(tenants.len());
    for tenant in tenants {
        let (results, _) = store
            .search_with_tier_info(tenant, query, fetch_k)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        lists.push(results);
    }
    Ok((
        merge_scored_chunk_lists(lists, fetch_k.saturating_mul(tenants.len().max(1))),
        None,
    ))
}

fn finalize_artifact_for_storage(artifact: &mut TaskArtifact) {
    artifact.promotion_state = derive_artifact_promotion_state(artifact);
}

/// Promote an artifact to `PromotionState::Verified` when, and only when,
/// it countersigns a prior artifact written by a distinct agent.
///
/// The rules:
/// 1. The artifact must be of a review-style kind (`Review`, `Revision`,
///    `Verification`, or `Decision`). Other kinds stay `Canonical`.
/// 2. It must reply to a canonical parent artifact (`reply_to_artifact_id`
///    resolves, and the parent is NOT a digest).
/// 3. The current artifact's `agent_id` must be non-empty AND differ
///    from the parent's `agent_id`. This is the "distinct writer"
///    requirement — it prevents a single agent from stamping its own
///    work as verified.
/// 4. The current artifact must explicitly support the parent's claim
///    (`supports_claim = Some(true)`). `supports_claim = Some(false)`
///    (an explicit rejection) or `None` (no opinion) does NOT promote.
///
/// When all four hold, set `promotion_state = Verified` so
/// `derive_artifact_trust_tier` returns `VerifiedRecord`. Otherwise
/// leave the canonical tier that `finalize_artifact_for_storage`
/// assigned.
pub(crate) async fn promote_if_countersigned<S: Store>(
    store: &S,
    artifact: &mut TaskArtifact,
) -> Result<(), McpError> {
    use crate::types::PromotionState;

    // Rule 1: only review-style kinds are even eligible.
    let eligible = matches!(
        artifact.artifact_kind,
        ArtifactKind::Review
            | ArtifactKind::Revision
            | ArtifactKind::Verification
            | ArtifactKind::Decision
    );
    if !eligible {
        return Ok(());
    }

    // Rule 4: explicit support is required.
    if artifact.supports_claim != Some(true) {
        return Ok(());
    }

    // Rule 3a: current writer must be identified.
    let Some(my_agent) = artifact
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };

    // Rule 2: the reply-to parent must resolve.
    let Some(reply_to) = artifact.reply_to_artifact_id.as_deref() else {
        return Ok(());
    };

    let parent = store
        .get_task_artifact(&artifact.tenant_id, reply_to)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let Some(parent) = parent else {
        return Ok(());
    };

    // Rule 2 (cont): digest parents do not count as canonical trust anchors.
    if parent.artifact_kind == ArtifactKind::Digest {
        return Ok(());
    }

    // Rule 3b: distinct writer.
    let parent_agent = parent
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match parent_agent {
        Some(other) if other != my_agent => {
            artifact.promotion_state = PromotionState::Verified;
            info!(
                artifact_id = %artifact.artifact_id,
                parent_id = %parent.artifact_id,
                my_agent,
                parent_agent = other,
                "promoted artifact to VerifiedRecord via distinct-writer countersignature"
            );
        }
        _ => {
            // Either parent is anonymous, or it's the same writer.
            // Neither case promotes trust.
        }
    }

    Ok(())
}

fn digest_artifacts_equivalent(existing: &TaskArtifact, candidate: &TaskArtifact) -> bool {
    if existing.artifact_kind != ArtifactKind::Digest
        || candidate.artifact_kind != ArtifactKind::Digest
    {
        return false;
    }

    let mut lhs = existing.clone();
    let mut rhs = candidate.clone();
    lhs.timestamp_created = 0;
    rhs.timestamp_created = 0;
    lhs.timestamp_observed = None;
    rhs.timestamp_observed = None;
    lhs == rhs
}

async fn persist_digest_artifact<S: Store>(
    store: &S,
    mut artifact: TaskArtifact,
) -> Result<TaskArtifact, McpError> {
    finalize_artifact_for_storage(&mut artifact);
    if let Some(existing) = store
        .get_task_artifact(&artifact.tenant_id, &artifact.artifact_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        if digest_artifacts_equivalent(&existing, &artifact) {
            return Ok(existing);
        }
    }
    let projections = build_task_projections(&artifact);
    store
        .add_task_artifact(artifact.clone(), projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    Ok(artifact)
}

async fn load_task_views<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TaskResumeView>, McpError> {
    let tenants = scoped_tenants_for_project(store, tenant_id, project_id).await?;
    let mut views = Vec::new();
    for tenant in tenants {
        let tasks = store
            .list_tasks(&tenant, project_id, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        for task in tasks {
            let artifacts = store
                .list_task_artifacts(&tenant, &task.task_id)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            views.push(build_task_resume_view(task, &artifacts));
        }
    }
    Ok(views)
}

async fn load_project_artifacts<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TaskArtifact>, McpError> {
    let mut artifacts = Vec::new();
    let tenants = scoped_tenants_for_project(store, tenant_id, project_id).await?;
    for tenant in tenants {
        let tasks = store
            .list_tasks(&tenant, project_id, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        for task in tasks {
            artifacts.extend(
                store
                    .list_task_artifacts(&tenant, &task.task_id)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?,
            );
        }
    }
    artifacts.sort_by_key(|artifact| std::cmp::Reverse(artifact.timestamp_created));
    Ok(artifacts)
}

async fn ensure_project_brief_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: &str,
    include_related_projects: bool,
) -> Result<(TaskArtifact, ProjectBriefView), McpError> {
    let task_views = load_task_views(store, tenant_id, Some(project_id), 200).await?;
    let same_project_artifacts =
        load_project_artifacts(store, tenant_id, Some(project_id), 200).await?;
    let recent_failures = infer_failure_items(&same_project_artifacts)
        .into_iter()
        .take(10)
        .collect::<Vec<_>>();
    let recent_decisions = infer_decision_items(&same_project_artifacts)
        .into_iter()
        .take(10)
        .collect::<Vec<_>>();
    let evidence_highlights = infer_evidence_items(&same_project_artifacts)
        .into_iter()
        .take(10)
        .collect::<Vec<_>>();

    let related_projects = if include_related_projects {
        store
            .list_tasks(tenant_id, None, 200)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
            .into_iter()
            .filter_map(|task| task.project_id.as_option().map(str::to_string))
            .filter(|candidate| candidate != project_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .take(5)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let brief = build_project_brief_view(
        tenant_id,
        project_id,
        task_views,
        recent_failures.clone(),
        recent_decisions.clone(),
        evidence_highlights.clone(),
        related_projects,
    );
    let artifact =
        persist_digest_artifact(store, build_project_brief_digest_artifact(&brief)).await?;
    Ok((artifact, brief))
}

async fn ensure_task_resume_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    task_id: &str,
) -> Result<(TaskArtifact, TaskResumeView), McpError> {
    let mut task = store
        .list_tasks(tenant_id, None, 500)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
        .into_iter()
        .find(|task| task.task_id == task_id);
    // Fall back to a daemon-wide scan ONLY when the cross-tenant fallback
    // is explicitly enabled. Otherwise a missing task in the caller's
    // tenant means "not found here" — the previous unconditional sweep
    // leaked the existence (and full 500-task listing) of every other
    // tenant on the daemon whenever a task_id was unknown.
    if task.is_none() && cross_tenant_project_fallback_enabled() {
        for other_tenant in store
            .list_tenants()
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
        {
            if &other_tenant == tenant_id {
                continue;
            }
            task = store
                .list_tasks(&other_tenant, None, 500)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?
                .into_iter()
                .find(|task| task.task_id == task_id);
            if task.is_some() {
                warn!(
                    primary_tenant = %tenant_id,
                    extra_tenant = %other_tenant,
                    task_id,
                    "task.resume digest resolved via cross-tenant fallback"
                );
                break;
            }
        }
    }
    let task = task.ok_or_else(|| McpError::ToolError("task not found".to_string()))?;
    let artifacts = store
        .list_task_artifacts(&task.tenant_id, task_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let resume = build_task_resume_view(task, &artifacts);
    let artifact =
        persist_digest_artifact(store, build_task_resume_digest_artifact(&resume)).await?;
    Ok((artifact, resume))
}

fn build_scope_key(project_id: Option<&str>, tenant_id: &TenantId, suffix: &str) -> String {
    project_id
        .map(|project_id| format!("project:{}:{}", project_id, suffix))
        .unwrap_or_else(|| format!("tenant:{}:{}", tenant_id, suffix))
}

async fn ensure_failure_library_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<(TaskArtifact, Vec<FailureViewItem>), McpError> {
    let artifacts = load_project_artifacts(store, tenant_id, project_id, 500).await?;
    let failures = infer_failure_items(&artifacts);
    let source_updated_at_ms = artifacts
        .iter()
        .map(|artifact| artifact.timestamp_created)
        .max()
        .unwrap_or(0);
    let artifact = build_library_digest_artifact(
        tenant_id.clone(),
        project_id.map(|id| ProjectId::from(id)),
        DIGEST_ROLE_FAILURE_LIBRARY,
        &build_scope_key(project_id, tenant_id, DIGEST_ROLE_FAILURE_LIBRARY),
        format!(
            "Failure library for {} contains {} recent failure summaries.",
            project_id.unwrap_or(tenant_id.as_str()),
            failures.len()
        ),
        failures
            .iter()
            .map(|item| item.summary.clone())
            .take(12)
            .collect(),
        Vec::new(),
        Vec::new(),
        failures
            .iter()
            .map(|item| item.artifact_id.clone())
            .collect(),
        source_updated_at_ms,
    );
    let artifact = persist_digest_artifact(store, artifact).await?;
    Ok((artifact, failures))
}

async fn ensure_decision_library_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<(TaskArtifact, Vec<DecisionViewItem>), McpError> {
    let artifacts = load_project_artifacts(store, tenant_id, project_id, 500).await?;
    let decisions = infer_decision_items(&artifacts);
    let source_updated_at_ms = artifacts
        .iter()
        .map(|artifact| artifact.timestamp_created)
        .max()
        .unwrap_or(0);
    let artifact = build_library_digest_artifact(
        tenant_id.clone(),
        project_id.map(|id| ProjectId::from(id)),
        DIGEST_ROLE_DECISION_LIBRARY,
        &build_scope_key(project_id, tenant_id, DIGEST_ROLE_DECISION_LIBRARY),
        format!(
            "Decision library for {} contains {} explicit or inferred decisions.",
            project_id.unwrap_or(tenant_id.as_str()),
            decisions.len()
        ),
        Vec::new(),
        decisions
            .iter()
            .map(|item| item.summary.clone())
            .take(12)
            .collect(),
        Vec::new(),
        decisions
            .iter()
            .map(|item| item.artifact_id.clone())
            .collect(),
        source_updated_at_ms,
    );
    let artifact = persist_digest_artifact(store, artifact).await?;
    Ok((artifact, decisions))
}

async fn ensure_evidence_library_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<(TaskArtifact, Vec<EvidenceViewItem>), McpError> {
    let artifacts = load_project_artifacts(store, tenant_id, project_id, 500).await?;
    let evidence = infer_evidence_items(&artifacts);
    let source_updated_at_ms = artifacts
        .iter()
        .map(|artifact| artifact.timestamp_created)
        .max()
        .unwrap_or(0);
    let artifact = build_library_digest_artifact(
        tenant_id.clone(),
        project_id.map(|id| ProjectId::from(id)),
        DIGEST_ROLE_EVIDENCE_LIBRARY,
        &build_scope_key(project_id, tenant_id, DIGEST_ROLE_EVIDENCE_LIBRARY),
        format!(
            "Evidence library for {} contains {} evidence highlights.",
            project_id.unwrap_or(tenant_id.as_str()),
            evidence.len()
        ),
        Vec::new(),
        evidence
            .iter()
            .map(|item| item.summary.clone())
            .take(12)
            .collect(),
        Vec::new(),
        evidence
            .iter()
            .map(|item| item.artifact_id.clone())
            .collect(),
        source_updated_at_ms,
    );
    let artifact = persist_digest_artifact(store, artifact).await?;
    Ok((artifact, evidence))
}

async fn ensure_highlight_library_digest<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
) -> Result<(TaskArtifact, Vec<HighlightViewItem>), McpError> {
    let artifacts = load_project_artifacts(store, tenant_id, project_id, 500).await?;
    let highlights = infer_highlight_items(&artifacts);
    let source_updated_at_ms = artifacts
        .iter()
        .map(|artifact| artifact.timestamp_created)
        .max()
        .unwrap_or(0);
    let summary = format!(
        "Highlight library for {} contains {} ranked lessons with future-agent uplift.",
        project_id.unwrap_or(tenant_id.as_str()),
        highlights.len()
    );
    let warning_highlights = highlights
        .iter()
        .filter(|item| item.category == "warning")
        .map(|item| item.summary.clone())
        .take(12)
        .collect::<Vec<_>>();
    let validated_highlights = highlights
        .iter()
        .filter(|item| item.category != "warning")
        .map(|item| item.summary.clone())
        .take(12)
        .collect::<Vec<_>>();
    let related_artifact_ids = highlights
        .iter()
        .flat_map(|item| item.supporting_artifact_ids.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let artifact = build_library_digest_artifact(
        tenant_id.clone(),
        project_id.map(|id| ProjectId::from(id)),
        DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        &build_scope_key(project_id, tenant_id, DIGEST_ROLE_HIGHLIGHT_LIBRARY),
        summary,
        warning_highlights,
        validated_highlights,
        Vec::new(),
        related_artifact_ids,
        source_updated_at_ms,
    );
    let artifact = persist_digest_artifact(store, artifact).await?;
    Ok((artifact, highlights))
}

/// Phase 3.4 sweeper: drain the writer-side dirty tracker and
/// regenerate the flagged digests. Returns the number of (scope,
/// role) pairs successfully regenerated. Errors for individual
/// scopes are logged but do not abort the whole sweep — they stay
/// flagged as dirty by virtue of having been drained, so a future
/// sweep will pick them up once the caller re-marks.
///
/// Called from `memory.compact` so operators have an explicit way to
/// force the refresh. A future phase can run this from a background
/// task on a timer.
pub(crate) async fn sweep_dirty_digests<S: Store>(store: &S) -> usize {
    let drained = crate::task_memory::digest_dirty::global().drain_dirty();
    if drained.is_empty() {
        return 0;
    }
    info!(pending = drained.len(), "Phase 3.4: sweeping dirty digests");

    let mut rebuilt = 0usize;
    for key in drained {
        let tenant = match TenantId::new(&key.tenant_id) {
            Ok(t) => t,
            Err(err) => {
                warn!(
                    tenant_id = %key.tenant_id,
                    error = %err,
                    "skipping dirty digest: invalid tenant_id"
                );
                continue;
            }
        };

        let result = match key.role.as_str() {
            crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY => {
                ensure_evidence_library_digest(store, &tenant, key.project_id.as_deref())
                    .await
                    .map(|_| ())
            }
            crate::task_memory::DIGEST_ROLE_DECISION_LIBRARY => {
                ensure_decision_library_digest(store, &tenant, key.project_id.as_deref())
                    .await
                    .map(|_| ())
            }
            crate::task_memory::DIGEST_ROLE_FAILURE_LIBRARY => {
                ensure_failure_library_digest(store, &tenant, key.project_id.as_deref())
                    .await
                    .map(|_| ())
            }
            crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY => {
                ensure_highlight_library_digest(store, &tenant, key.project_id.as_deref())
                    .await
                    .map(|_| ())
            }
            crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF => match key.project_id.as_deref() {
                Some(project_id) => ensure_project_brief_digest(store, &tenant, project_id, true)
                    .await
                    .map(|_| ()),
                None => {
                    warn!(
                        role = %key.role,
                        tenant_id = %tenant,
                        "project_brief digest requires project_id; skipping"
                    );
                    continue;
                }
            },
            _ => {
                warn!(role = %key.role, "unknown digest role in dirty tracker");
                continue;
            }
        };

        match result {
            Ok(_) => rebuilt += 1,
            Err(err) => {
                // Codex follow-up on 3.4 retry semantics: a failed
                // regeneration used to be silently lost when the
                // drain consumed the key. Re-mark the key so the
                // next sweep will retry; otherwise a transient error
                // (temporary lock contention, disk blip) would leave
                // the digest stale forever.
                warn!(
                    role = %key.role,
                    tenant_id = %tenant,
                    project_id = ?key.project_id,
                    error = %err,
                    "digest sweeper failed to regenerate; re-marking for retry"
                );
                crate::task_memory::digest_dirty::global().mark_dirty(
                    crate::task_memory::digest_dirty::DigestDirtyKey {
                        tenant_id: key.tenant_id.clone(),
                        project_id: key.project_id.clone(),
                        role: key.role.clone(),
                    },
                );
            }
        }
    }
    rebuilt
}

async fn rebuild_requested_digests<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    modes: &[QueryMode],
) -> Result<Vec<String>, McpError> {
    let requested = if modes.is_empty() {
        vec![
            QueryMode::BriefProject,
            QueryMode::FindFailures,
            QueryMode::FindDecisions,
            QueryMode::FindEvidence,
            QueryMode::FindHighlights,
        ]
    } else {
        modes.to_vec()
    };

    let mut artifact_ids = Vec::new();
    for mode in requested {
        match mode {
            QueryMode::BriefProject => {
                if let Some(project_id) = project_id {
                    artifact_ids.push(
                        ensure_project_brief_digest(store, tenant_id, project_id, true)
                            .await?
                            .0
                            .artifact_id,
                    );
                }
            }
            QueryMode::FindFailures => artifact_ids.push(
                ensure_failure_library_digest(store, tenant_id, project_id)
                    .await?
                    .0
                    .artifact_id,
            ),
            QueryMode::FindDecisions => artifact_ids.push(
                ensure_decision_library_digest(store, tenant_id, project_id)
                    .await?
                    .0
                    .artifact_id,
            ),
            QueryMode::FindEvidence => artifact_ids.push(
                ensure_evidence_library_digest(store, tenant_id, project_id)
                    .await?
                    .0
                    .artifact_id,
            ),
            QueryMode::FindHighlights => artifact_ids.push(
                ensure_highlight_library_digest(store, tenant_id, project_id)
                    .await?
                    .0
                    .artifact_id,
            ),
            QueryMode::Generic | QueryMode::ResumeTask => {}
        }
    }
    artifact_ids.sort();
    artifact_ids.dedup();
    Ok(artifact_ids)
}

async fn collect_candidate_chunk_ids<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    filters_list: Vec<TaskSearchFilters>,
    limit: usize,
) -> Result<Vec<ChunkId>, McpError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for filters in filters_list {
        let ids = store
            .search_task_projection_chunk_ids(tenant_id, &filters, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        for id in ids {
            if seen.insert(id.clone()) {
                out.push(id);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

async fn candidate_chunk_ids_for_mode<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    mode: QueryMode,
    filters: &TaskSearchFilters,
    limit: usize,
) -> Result<Vec<ChunkId>, McpError> {
    let mut filters_list = Vec::new();
    match mode {
        QueryMode::Generic => {}
        QueryMode::BriefProject => {
            if let Some(project_id) = filters.project_id.as_deref() {
                let _ = ensure_project_brief_digest(store, tenant_id, project_id, true).await?;
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(ArtifactKind::Digest),
                    artifact_role: Some(DIGEST_ROLE_PROJECT_BRIEF.to_string()),
                    project_id: Some(project_id.to_string()),
                    ..Default::default()
                });
                filters_list.push(TaskSearchFilters {
                    project_id: Some(project_id.to_string()),
                    ..Default::default()
                });
            }
        }
        QueryMode::ResumeTask => {
            if let Some(task_id) = filters.task_id.as_deref() {
                let _ = ensure_task_resume_digest(store, tenant_id, task_id).await?;
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(ArtifactKind::Digest),
                    artifact_role: Some(DIGEST_ROLE_TASK_RESUME.to_string()),
                    task_id: Some(task_id.to_string()),
                    ..Default::default()
                });
                filters_list.push(TaskSearchFilters {
                    task_id: Some(task_id.to_string()),
                    ..Default::default()
                });
            }
        }
        QueryMode::FindFailures => {
            let _ = ensure_failure_library_digest(store, tenant_id, filters.project_id.as_deref())
                .await?;
            filters_list.push(TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_FAILURE_LIBRARY.to_string()),
                project_id: filters.project_id.clone(),
                ..Default::default()
            });
            for kind in [
                ArtifactKind::TaskFinish,
                ArtifactKind::TaskProgress,
                ArtifactKind::Digest,
            ] {
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(kind),
                    project_id: filters.project_id.clone(),
                    ..Default::default()
                });
            }
        }
        QueryMode::FindDecisions => {
            let _ = ensure_decision_library_digest(store, tenant_id, filters.project_id.as_deref())
                .await?;
            filters_list.push(TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_DECISION_LIBRARY.to_string()),
                project_id: filters.project_id.clone(),
                ..Default::default()
            });
            for kind in [
                ArtifactKind::Decision,
                ArtifactKind::Verification,
                ArtifactKind::TaskFinish,
            ] {
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(kind),
                    project_id: filters.project_id.clone(),
                    ..Default::default()
                });
            }
        }
        QueryMode::FindEvidence => {
            let _ = ensure_evidence_library_digest(store, tenant_id, filters.project_id.as_deref())
                .await?;
            filters_list.push(TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_EVIDENCE_LIBRARY.to_string()),
                project_id: filters.project_id.clone(),
                ..Default::default()
            });
            for kind in [ArtifactKind::Evidence, ArtifactKind::TaskFinish] {
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(kind),
                    project_id: filters.project_id.clone(),
                    ..Default::default()
                });
            }
        }
        QueryMode::FindHighlights => {
            let _ =
                ensure_highlight_library_digest(store, tenant_id, filters.project_id.as_deref())
                    .await?;
            filters_list.push(TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_HIGHLIGHT_LIBRARY.to_string()),
                project_id: filters.project_id.clone(),
                ..Default::default()
            });
            for kind in [
                ArtifactKind::TaskFinish,
                ArtifactKind::Verification,
                ArtifactKind::Decision,
                ArtifactKind::Review,
            ] {
                filters_list.push(TaskSearchFilters {
                    artifact_kind: Some(kind),
                    project_id: filters.project_id.clone(),
                    ..Default::default()
                });
            }
        }
    }

    collect_candidate_chunk_ids(store, tenant_id, filters_list, limit).await
}

async fn candidate_chunk_ids_for_tenants_and_mode<S: Store>(
    store: &S,
    tenants: &[TenantId],
    mode: QueryMode,
    filters: &TaskSearchFilters,
    limit: usize,
) -> Result<Vec<ChunkId>, McpError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tenant in tenants {
        let ids = candidate_chunk_ids_for_mode(store, tenant, mode, filters, limit).await?;
        for id in ids {
            if seen.insert(id.clone()) {
                out.push(id);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

async fn search_task_projection_chunk_ids_for_tenants<S: Store>(
    store: &S,
    tenants: &[TenantId],
    filters: &TaskSearchFilters,
    limit: usize,
) -> Result<Vec<ChunkId>, McpError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tenant in tenants {
        let ids = store
            .search_task_projection_chunk_ids(tenant, filters, limit)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        for id in ids {
            if seen.insert(id.clone()) {
                out.push(id);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

async fn summary_preferred_results<S: Store>(
    store: &S,
    tenants: &[TenantId],
    query: &str,
    project_id: Option<&str>,
    mode: QueryMode,
    limit: usize,
) -> Result<Vec<(MemoryChunk, f32)>, McpError> {
    let modes = if mode != QueryMode::Generic {
        vec![mode]
    } else if project_id.is_some() {
        vec![
            QueryMode::BriefProject,
            QueryMode::FindFailures,
            QueryMode::FindDecisions,
            QueryMode::FindEvidence,
            QueryMode::FindHighlights,
        ]
    } else {
        Vec::new()
    };

    if modes.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut all_ids = Vec::new();
    let mut seen = HashSet::new();
    for mode in modes {
        let ids = candidate_chunk_ids_for_tenants_and_mode(
            store,
            tenants,
            mode,
            &TaskSearchFilters {
                project_id: project_id.map(|value| value.to_string()),
                ..Default::default()
            },
            limit.saturating_mul(4),
        )
        .await?;
        for id in ids {
            if seen.insert(id.clone()) {
                all_ids.push(id);
            }
        }
    }

    let mut lists = Vec::with_capacity(tenants.len());
    for tenant in tenants {
        lists.push(
            store
                .rerank_chunks_for_query(tenant, query, &all_ids, limit)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
    }
    Ok(merge_scored_chunk_lists(
        lists,
        limit.saturating_mul(tenants.len().max(1)),
    ))
}

fn merge_preferred_and_raw(
    preferred: Vec<(MemoryChunk, f32)>,
    raw: Vec<(MemoryChunk, f32)>,
    limit: usize,
) -> Vec<(MemoryChunk, f32)> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();

    for (chunk, score) in preferred {
        if seen.insert(chunk.chunk_id.clone()) {
            merged.push((chunk, score + 10.0));
            if merged.len() >= limit {
                return merged;
            }
        }
    }
    for (chunk, score) in raw {
        if seen.insert(chunk.chunk_id.clone()) {
            merged.push((chunk, score));
            if merged.len() >= limit {
                break;
            }
        }
    }
    merged
}

fn apply_common_artifact_fields(
    artifact: &mut TaskArtifact,
    project_id: Option<String>,
    parent_task_id: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    status: Option<String>,
    artifact_role: Option<String>,
    challenge_id: Option<String>,
    thread_id: Option<String>,
    reply_to_artifact_id: Option<String>,
    relation_kind: Option<String>,
    dataset_refs: Vec<DatasetRef>,
    entity_refs: Vec<EntityRef>,
    contributors: Vec<ContributorRef>,
    provenance: TaskProvenance,
) {
    artifact.project_id = ProjectId::from(project_id);
    artifact.parent_task_id = parent_task_id;
    artifact.agent_id = agent_id;
    artifact.session_id = session_id;
    artifact.status =
        Some(status.unwrap_or_else(|| {
            default_status_for_artifact_kind(artifact.artifact_kind).to_string()
        }));
    artifact.artifact_role = artifact_role;
    artifact.challenge_id = challenge_id;
    artifact.thread_id = thread_id;
    artifact.reply_to_artifact_id = reply_to_artifact_id;
    artifact.relation_kind = relation_kind;
    artifact.dataset_refs = dataset_refs;
    artifact.entity_refs = entity_refs;
    artifact.contributors = contributors;
    artifact.provenance = provenance;
    artifact.tool_name = artifact
        .provenance
        .tool_name
        .clone()
        .or_else(|| artifact.tool_name.clone());
    artifact.tool_version = artifact
        .provenance
        .tool_version
        .clone()
        .or_else(|| artifact.tool_version.clone());
}

async fn collect_episode_chunks<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    episode_id: &str,
    max_chunks: usize,
) -> Result<Vec<MemoryChunk>, McpError> {
    let page_size = 200usize;
    let mut offset = 0usize;
    let mut episode_chunks = Vec::new();

    loop {
        let page = store
            .list_chunks(tenant_id, page_size, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if page.is_empty() {
            break;
        }

        for chunk in page {
            if extract_episode_id(&chunk.tags).as_deref() == Some(episode_id) {
                episode_chunks.push(chunk);
                if episode_chunks.len() >= max_chunks {
                    return Ok(episode_chunks);
                }
            }
        }

        offset = offset.saturating_add(page_size);
    }

    Ok(episode_chunks)
}

// ---------- Handler Functions ----------

/// Handle memory.search tool call
pub async fn handle_memory_search<S: Store>(
    store: &S,
    params: SearchParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    let parsed_filters = parse_search_filters(params.filters.as_ref())?;
    let debug_tiers = params.debug_tiers.unwrap_or(false);
    let mode = params.mode.unwrap_or_default();
    let project_id_filter = params.project_id.as_deref();
    let has_filters = has_active_search_filters(project_id_filter, &parsed_filters);
    let (visibility_policy, oversample_factor) = resolve_visibility_and_oversample(&params);
    // Pre-visibility trim headroom: `apply_search_filters` takes a cap so
    // we pass `k * oversample_factor` here; `apply_visibility_filter`
    // further trims to `params.k` after hiding non-visible rows.
    let pre_visibility_cap = params.k.saturating_mul(oversample_factor);
    let fetch_k = adaptive_fetch_k(params.k, &params.query, has_filters)
        .max(pre_visibility_cap);
    let digest_tenants = scoped_tenants_for_project(store, &tenant_id, project_id_filter).await?;
    let search_tenants = if project_id_filter.is_some() {
        let all = store
            .list_tenants()
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if all.is_empty() {
            vec![tenant_id.clone()]
        } else {
            all
        }
    } else {
        vec![tenant_id.clone()]
    };

    info!(
        tenant_id = %tenant_id,
        query = %params.query,
        k = params.k,
        fetch_k = fetch_k,
        debug_tiers = debug_tiers,
        "memory.search"
    );

    // Use search_with_tier_info if debug_tiers is requested
    if debug_tiers {
        let (scored_chunks, timing) =
            search_with_tier_info_for_tenants(store, &search_tenants, &params.query, fetch_k)
                .await?;
        let preferred = summary_preferred_results(
            store,
            &digest_tenants,
            &params.query,
            project_id_filter,
            mode,
            params.k.min(8),
        )
        .await?;
        let mut scored_chunks = apply_search_filters(
            merge_preferred_and_raw(preferred, scored_chunks, fetch_k),
            project_id_filter,
            &parsed_filters,
            pre_visibility_cap,
        );
        let mut timing = timing;
        let mut repair_info = None;

        if scored_chunks.is_empty() && !params.query.is_empty() {
            if let Some(repaired_query) = normalize_query_for_repair(&params.query) {
                let (repair_scored, repair_timing) = search_with_tier_info_for_tenants(
                    store,
                    &search_tenants,
                    &repaired_query,
                    fetch_k,
                )
                .await?;
                let repaired_filtered = apply_search_filters(
                    repair_scored,
                    project_id_filter,
                    &parsed_filters,
                    pre_visibility_cap,
                );
                let repaired = !repaired_filtered.is_empty();
                if repaired {
                    scored_chunks = repaired_filtered;
                    timing = repair_timing;
                }
                repair_info = Some(RepairInfo {
                    attempted: true,
                    repaired,
                    original_query: params.query.clone(),
                    repaired_query: Some(repaired_query),
                });
            }
        }

        // Apply lifecycle visibility filter with oversample-and-refill to
        // trim from `pre_visibility_cap` down to `params.k`, hiding
        // Superseded/Expired/History rows unless the caller opted in.
        let scored_chunks =
            apply_visibility_filter(store, scored_chunks, &visibility_policy, params.k).await;

        debug!(
            results_count = scored_chunks.len(),
            "search completed with tier info"
        );

        // Build tier debug info if timing is available
        let tier_info = timing.map(|t| {
            let source_tier = if t.cache_lookup_ms > 0 && t.hot_tier_ms == 0 && t.warm_tier_ms == 0
            {
                "cache".to_string()
            } else if t.hot_tier_ms > 0 && t.warm_tier_ms == 0 {
                "hot".to_string()
            } else if t.warm_tier_ms > 0 {
                "warm".to_string()
            } else {
                "hybrid".to_string()
            };

            let cache_hit = t.cache_lookup_ms > 0 && t.hot_tier_ms == 0 && t.warm_tier_ms == 0;
            let hot_tier_hit = t.hot_tier_ms > 0 && t.warm_tier_ms == 0;

            TierDebugInfo {
                source_tier,
                cache_hit,
                hot_tier_hit,
                cache_lookup_ms: t.cache_lookup_ms,
                hot_tier_ms: t.hot_tier_ms,
                warm_tier_ms: t.warm_tier_ms,
            }
        });

        // Determine source tier per result based on scoring heuristics
        // If we have tier_info, derive per-result tier from overall timing
        let default_tier = tier_info.as_ref().map(|t| t.source_tier.clone());

        let artifacts = resolve_artifacts_for_ranked_chunks(store, &scored_chunks).await?;
        let results: Vec<ChunkResult> = scored_chunks
            .iter()
            .map(|(chunk, score)| {
                chunk_to_result(
                    chunk,
                    *score,
                    default_tier.clone(),
                    artifacts.get(&chunk.chunk_id.to_string()).cloned(),
                )
            })
            .collect();

        return format_mcp_response(&SearchResult {
            results,
            tier_info,
            repair_info,
        });
    }

    // Standard path without tier info
    let scored_chunks =
        search_with_scores_for_tenants(store, &search_tenants, &params.query, fetch_k).await?;
    let preferred = summary_preferred_results(
        store,
        &digest_tenants,
        &params.query,
        project_id_filter,
        mode,
        params.k.min(8),
    )
    .await?;
    let mut scored_chunks = apply_search_filters(
        merge_preferred_and_raw(preferred, scored_chunks, fetch_k),
        project_id_filter,
        &parsed_filters,
        pre_visibility_cap,
    );
    let mut repair_info = None;

    if scored_chunks.is_empty() && !params.query.is_empty() {
        if let Some(repaired_query) = normalize_query_for_repair(&params.query) {
            let repair_scored =
                search_with_scores_for_tenants(store, &search_tenants, &repaired_query, fetch_k)
                    .await?;
            let repaired_filtered = apply_search_filters(
                repair_scored,
                project_id_filter,
                &parsed_filters,
                pre_visibility_cap,
            );
            let repaired = !repaired_filtered.is_empty();
            if repaired {
                scored_chunks = repaired_filtered;
            }
            repair_info = Some(RepairInfo {
                attempted: true,
                repaired,
                original_query: params.query.clone(),
                repaired_query: Some(repaired_query),
            });
        }
    }

    // Apply lifecycle visibility filter with oversample-and-refill
    // (standard path, no tier-debug). This is the matching call site to
    // the debug_tiers branch above and shares the same policy +
    // oversample cap.
    let scored_chunks =
        apply_visibility_filter(store, scored_chunks, &visibility_policy, params.k).await;

    debug!(results_count = scored_chunks.len(), "search completed");

    let artifacts = resolve_artifacts_for_ranked_chunks(store, &scored_chunks).await?;
    let results: Vec<ChunkResult> = scored_chunks
        .iter()
        .map(|(chunk, score)| {
            chunk_to_result(
                chunk,
                *score,
                None,
                artifacts.get(&chunk.chunk_id.to_string()).cloned(),
            )
        })
        .collect();

    format_mcp_response(&SearchResult {
        results,
        tier_info: None,
        repair_info,
    })
}

/// Handle memory.add tool call
pub async fn handle_memory_add<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: AddParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_type = parse_chunk_type(&params.chunk_type)?;

    info!(
        tenant_id = %tenant_id,
        chunk_type = %chunk_type,
        text_len = params.text.len(),
        "memory.add"
    );

    // Ensure tenant directory exists if tenant_manager is available
    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut chunk = MemoryChunk::new(tenant_id, &params.text, chunk_type);

    // Apply optional fields
    if let Some(project_id) = &params.project_id {
        chunk = chunk.with_project(ProjectId::new(Some(project_id.clone())));
    }

    if let Some(episode_id) = &params.episode_id {
        validate_episode_id(episode_id)?;
        let mut tags = chunk.tags.clone();
        tags.push(make_episode_tag(episode_id));
        chunk = chunk.with_tags(tags);
    }

    chunk = chunk.with_source(params_to_source(params.source));

    if !params.tags.is_empty() {
        let mut tags = chunk.tags.clone();
        tags.extend(params.tags);
        chunk = chunk.with_tags(tags);
    }

    // Track E: parse `mode` → IngestionMode (fail-closed) and apply
    // the conversation-mode default review window when the caller
    // didn't pass an explicit review_after_ms.
    let ingestion_mode = parse_ingestion_mode(params.mode.as_deref())?;
    chunk = chunk.with_ingestion_mode(ingestion_mode);
    let effective_review_after_ms =
        apply_conversation_review_default(ingestion_mode, params.review_after_ms);

    // `params.review_after_ms` may have been None on input; substitute
    // the defaulted value so the rest of the handler treats it as
    // explicitly requested.
    let review_after_ms = effective_review_after_ms;
    let has_lifecycle = params.expires_at_ms.is_some() || review_after_ms.is_some();
    let resolved_dedup = match params.supersede_near_duplicates.as_ref() {
        Some(spec) => crate::mcp::dedup::resolve_spec(spec)
            .map_err(|e| McpError::ToolError(e.to_string()))?,
        None => None,
    };

    // Track D path: when dedup is requested, find candidates first
    // (read-only on the store), then atomically supersede each one with
    // the new chunk via PersistentStore::supersede_chunk. The
    // supersede_chunk call already writes the new chunk + the
    // supersession edge in one logical op, so we drive the loop from
    // here rather than calling Store::add separately.
    if let Some(cfg) = resolved_dedup {
        let ps = store.as_persistent().ok_or_else(|| {
            McpError::ToolError(
                "memory.add with supersede_near_duplicates requires a persistent store".into(),
            )
        })?;
        let project_scope = chunk.project_id.as_option().map(|s| s.to_string());
        let candidates = crate::mcp::dedup::compute_dedup_candidates(
            ps,
            &chunk.tenant_id,
            &chunk.text,
            chunk.chunk_type,
            project_scope.as_deref(),
            &cfg,
        )?;

        // Snapshot tenant_id before `chunk` is consumed by either
        // dedup branch.
        let tenant_id_for_extras = chunk.tenant_id.clone();
        let lifecycle_delta = LifecycleDelta {
            expires_at_ms: params.expires_at_ms.map(Some),
            review_after_ms: review_after_ms.map(Some),
            ..Default::default()
        };

        let new_chunk_id = if candidates.is_empty() {
            // No prior matches — fall back to a normal add. Lifecycle
            // overlay still applies if requested.
            if has_lifecycle {
                ps.add_chunk_with_lifecycle(chunk, lifecycle_delta.clone())
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?
            } else {
                store
                    .add(chunk)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?
            }
        } else {
            // Atomically replace the FIRST candidate with the new chunk
            // — `supersede_chunk` writes the payload + supersession
            // edge in one logical op. `compute_dedup_candidates`
            // already filtered to live-head rows (status=Final,
            // superseded_by=None), so the head-only guard inside
            // supersede_chunk will not fail-closed on stale candidates
            // (Codex round-1 D3 HIGH-2).
            let first_old = &candidates[0];
            let new_id = ps
                .supersede_chunk(&tenant_id_for_extras, first_old, chunk)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;

            // Codex round-1 D3 HIGH-1: supersede_chunk does not carry a
            // lifecycle delta through, so the requested temporal overlay
            // (expires_at_ms / review_after_ms) is dropped on the
            // matched-dedup path. Apply it explicitly to the new
            // chunk_id so the dedup-vs-no-dedup behaviour is identical
            // when temporal fields are present.
            if has_lifecycle {
                let mut delta = lifecycle_delta.clone();
                if delta.lifecycle_updated_at_ms.is_none() {
                    delta.lifecycle_updated_at_ms = Some(current_time_ms());
                }
                ps.update_lifecycle(&tenant_id_for_extras, &new_id, &delta)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?;
            }
            new_id
        };

        // We only atomically superseded the FIRST candidate via
        // `supersede_chunk` — the call only handles a 1:1 edge.
        // Additional candidates (rare: only when the prior state
        // already contained multiple live-head duplicates of the same
        // canonical, e.g. a legacy backlog or a concurrent
        // no-dedup writer) are intentionally left untouched. The
        // response reflects exactly what changed so callers don't
        // think they got a stronger guarantee than supersede_chunk
        // actually delivers. A follow-up dedup run will clean up the
        // remaining duplicates one at a time.
        let superseded_ids = if candidates.is_empty() {
            Vec::new()
        } else {
            vec![candidates[0].to_string()]
        };

        info!(
            chunk_id = %new_chunk_id,
            superseded_total = candidates.len(),
            superseded_linked = superseded_ids.len(),
            "chunk added with dedup"
        );
        return format_mcp_response(&serde_json::json!({
            "chunk_id": new_chunk_id.to_string(),
            "superseded_ids": superseded_ids,
        }));
    }

    let chunk_id = if has_lifecycle {
        // Temporal overlay requires the persistent-store write path that
        // updates the lifecycle row in the same logical op. Non-persistent
        // stores (used only by a small handful of tests) have no overlay
        // table, so we refuse rather than silently dropping the fields.
        let ps = store.as_persistent().ok_or_else(|| {
            McpError::ToolError(
                "memory.add with temporal fields requires a persistent store".into(),
            )
        })?;
        let delta = LifecycleDelta {
            expires_at_ms: params.expires_at_ms.map(Some),
            review_after_ms: review_after_ms.map(Some),
            ..Default::default()
        };
        ps.add_chunk_with_lifecycle(chunk, delta)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        store
            .add(chunk)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    info!(chunk_id = %chunk_id, "chunk added");

    format_mcp_response(&AddResult {
        chunk_id: chunk_id.to_string(),
    })
}

/// Parameters for memory.supersede
#[derive(Debug, Deserialize)]
pub struct SupersedeParams {
    #[serde(default)]
    pub tenant_id: String,
    pub old_chunk_id: String,
    pub new_text: String,
    #[serde(rename = "type")]
    pub chunk_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub source: Option<SourceParams>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Handle memory.supersede tool call.
///
/// Atomically supersedes an existing chunk with a new version via
/// `PersistentStore::supersede_chunk`. Returns both the formatted MCP
/// response and a `PostWriteEvent` so the server dispatch arm can run
/// structural indexing for the new chunk (mirroring memory.add).
pub async fn handle_memory_supersede<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: SupersedeParams,
) -> Result<(Value, PostWriteEvent), McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.supersede requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        old_chunk_id = %params.old_chunk_id,
        new_text_len = params.new_text.len(),
        "memory.supersede"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let old_id = ChunkId::parse(&params.old_chunk_id)
        .map_err(|e| McpError::InvalidParams(format!("old_chunk_id: {e}")))?;
    let chunk_type = parse_chunk_type(&params.chunk_type)?;

    // Capture source_path before `params.source` is consumed by
    // params_to_source — `SourceParams` is not Clone, so we lift the
    // path out by reference first and own it for the post-write event.
    let source_path = params.source.as_ref().and_then(|s| s.path.clone());

    let mut new_chunk = MemoryChunk::new(tenant_id.clone(), &params.new_text, chunk_type);
    if let Some(project_id) = params.project_id.clone() {
        new_chunk = new_chunk.with_project(ProjectId::new(Some(project_id)));
    }
    new_chunk = new_chunk.with_source(params_to_source(params.source));
    if !params.tags.is_empty() {
        new_chunk = new_chunk.with_tags(params.tags.clone());
    }

    let new_id = ps
        .supersede_chunk(&tenant_id, &old_id, new_chunk)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    info!(new_chunk_id = %new_id, old_chunk_id = %old_id, "chunk superseded");

    let event = PostWriteEvent {
        tenant_id: tenant_id.to_string(),
        chunk_id: new_id.clone(),
        chunk_type: params.chunk_type.clone(),
        project_id: params.project_id,
        source_path,
        text: params.new_text.clone(),
    };
    let response = format_mcp_response(&json!({
        "new_chunk_id": new_id.to_string(),
        "old_chunk_id": old_id.to_string(),
    }))?;
    Ok((response, event))
}

/// Parameters for memory.set_expiry (Track C6).
///
/// The nested `Option<Option<i64>>` encodes triple-state:
/// - field absent → outer `None` → leave the overlay unchanged.
/// - field present and `null` → `Some(None)` → clear the overlay
///   field.
/// - field present with a value → `Some(Some(v))` → set the field.
#[derive(Debug, Deserialize, Default)]
pub struct SetExpiryParams {
    #[serde(default)]
    pub tenant_id: String,
    pub chunk_id: String,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub expires_at_ms: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub review_after_ms: Option<Option<i64>>,
}

/// Custom deserializer that preserves the "field present but null"
/// signal serde would otherwise collapse to `Option<Option<T>>::None`.
///
/// `#[serde(default)]` alone turns an absent field AND an explicit
/// `null` into the same value (both `None`), which defeats the
/// triple-state contract on `memory.set_expiry`. Wrapping the field
/// with `deserialize_with = "deserialize_some"` makes `null` round-trip
/// as `Some(None)` (clear) while keeping absent fields as `None` (leave).
fn deserialize_some<'de, T, D>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Handle memory.set_expiry tool call (Track C6).
///
/// Updates the `expires_at_ms` and/or `review_after_ms` overlay fields
/// on an existing chunk and bumps the tenant cache version when at
/// least one field changed. Refuses to run on non-persistent stores
/// because the overlay table only exists on `PersistentStore`.
pub async fn handle_memory_set_expiry<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: SetExpiryParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.set_expiry requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_id = ChunkId::parse(&params.chunk_id)
        .map_err(|e| McpError::InvalidParams(format!("chunk_id: {e}")))?;

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    // Reject a no-op payload so callers that forgot to send either
    // field get an explicit error instead of a silently-succeeding
    // cache bump.
    if params.expires_at_ms.is_none() && params.review_after_ms.is_none() {
        return Err(McpError::InvalidParams(
            "memory.set_expiry requires at least one of expires_at_ms / review_after_ms".into(),
        ));
    }

    info!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        set_expires = params.expires_at_ms.is_some(),
        set_review = params.review_after_ms.is_some(),
        "memory.set_expiry"
    );

    let delta = LifecycleDelta {
        expires_at_ms: params.expires_at_ms,
        review_after_ms: params.review_after_ms,
        lifecycle_updated_at_ms: Some(current_time_ms()),
        ..Default::default()
    };

    // Single atomic UPDATE whose rowcount drives the response. Fails
    // closed on both non-existent chunk IDs AND cross-tenant access
    // (the tenant filter is part of the UPDATE's WHERE, so a wrong
    // tenant matches zero rows and returns `Ok(false)` here). No
    // preflight read, so no TOCTOU window.
    let updated = ps
        .update_lifecycle_if_exists(&tenant_id, &chunk_id, &delta)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    if !updated {
        return Err(McpError::ToolError(format!(
            "memory.set_expiry: chunk {chunk_id} not found in tenant {tenant_id}"
        )));
    }

    format_mcp_response(&json!({
        "chunk_id": chunk_id.to_string(),
        "updated": true,
    }))
}

/// Handle memory.add_batch tool call
pub async fn handle_memory_add_batch<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: AddBatchParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        count = params.chunks.len(),
        "memory.add_batch"
    );

    // Ensure tenant directory exists if tenant_manager is available
    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    // Resolve the optional Track D dedup spec once for the whole batch.
    let resolved_dedup = match params.supersede_near_duplicates.as_ref() {
        Some(spec) => crate::mcp::dedup::resolve_spec(spec)
            .map_err(|e| McpError::ToolError(e.to_string()))?,
        None => None,
    };

    // Track D path: when dedup is requested, fall out of the batched
    // fast path entirely and treat each chunk independently — same
    // contract as D3 on memory.add. The response gains a parallel
    // `superseded_ids` array of arrays so callers can correlate.
    if let Some(cfg) = resolved_dedup {
        let ps = store.as_persistent().ok_or_else(|| {
            McpError::ToolError(
                "memory.add_batch with supersede_near_duplicates requires a persistent store"
                    .into(),
            )
        })?;

        // Pre-pass: consume params.chunks once and build the (chunk,
        // delta, has_lifecycle, project_id) tuples up front so
        // validation failures abort cleanly without committing half a
        // batch. SourceParams is not Clone, so we have to move it out
        // of chunk_params here rather than borrow inside the second
        // pass.
        let mut prepared: Vec<(MemoryChunk, LifecycleDelta, bool, Option<String>)> =
            Vec::with_capacity(params.chunks.len());
        for chunk_params in params.chunks {
            let chunk_type = parse_chunk_type(&chunk_params.chunk_type)?;
            let project_id_for_dedup = chunk_params.project_id.clone();
            let mut chunk = MemoryChunk::new(tenant_id.clone(), &chunk_params.text, chunk_type);
            if let Some(project_id) = &chunk_params.project_id {
                chunk = chunk.with_project(ProjectId::new(Some(project_id.clone())));
            }
            if let Some(episode_id) = &chunk_params.episode_id {
                validate_episode_id(episode_id)?;
                let mut tags = chunk.tags.clone();
                tags.push(make_episode_tag(episode_id));
                chunk = chunk.with_tags(tags);
            }
            chunk = chunk.with_source(params_to_source(chunk_params.source));
            if !chunk_params.tags.is_empty() {
                let mut tags = chunk.tags.clone();
                tags.extend(chunk_params.tags);
                chunk = chunk.with_tags(tags);
            }
            // Track E: per-chunk mode + conversation default review window.
            let ingestion_mode = parse_ingestion_mode(chunk_params.mode.as_deref())?;
            chunk = chunk.with_ingestion_mode(ingestion_mode);
            let review_after_ms = apply_conversation_review_default(
                ingestion_mode,
                chunk_params.review_after_ms,
            );
            let has_lifecycle =
                chunk_params.expires_at_ms.is_some() || review_after_ms.is_some();
            let delta = LifecycleDelta {
                expires_at_ms: chunk_params.expires_at_ms.map(Some),
                review_after_ms: review_after_ms.map(Some),
                ..Default::default()
            };
            prepared.push((chunk, delta, has_lifecycle, project_id_for_dedup));
        }

        // Second pass: per-chunk dedup-or-add. Failures still leave
        // earlier rows committed, matching the existing add_batch
        // failure contract.
        let mut chunk_ids: Vec<String> = Vec::with_capacity(prepared.len());
        let mut superseded_ids: Vec<Vec<String>> = Vec::with_capacity(prepared.len());
        for (chunk, delta, has_lifecycle, project_id) in prepared {
            let candidates = crate::mcp::dedup::compute_dedup_candidates(
                ps,
                &tenant_id,
                &chunk.text,
                chunk.chunk_type,
                project_id.as_deref(),
                &cfg,
            )?;
            let new_id = if candidates.is_empty() {
                if has_lifecycle {
                    ps.add_chunk_with_lifecycle(chunk, delta.clone())
                        .await
                        .map_err(|e| McpError::ToolError(e.to_string()))?
                } else {
                    store
                        .add(chunk)
                        .await
                        .map_err(|e| McpError::ToolError(e.to_string()))?
                }
            } else {
                let first_old = &candidates[0];
                let new_id = ps
                    .supersede_chunk(&tenant_id, first_old, chunk)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?;
                if has_lifecycle {
                    let mut d = delta.clone();
                    if d.lifecycle_updated_at_ms.is_none() {
                        d.lifecycle_updated_at_ms = Some(current_time_ms());
                    }
                    ps.update_lifecycle(&tenant_id, &new_id, &d)
                        .await
                        .map_err(|e| McpError::ToolError(e.to_string()))?;
                }
                new_id
            };
            // Mirror D3: only the first candidate is actually linked
            // by supersede_chunk; report only what we changed.
            let linked = if candidates.is_empty() {
                Vec::new()
            } else {
                vec![candidates[0].to_string()]
            };
            chunk_ids.push(new_id.to_string());
            superseded_ids.push(linked);
        }

        info!(
            count = chunk_ids.len(),
            "batch add (with dedup) completed"
        );
        return format_mcp_response(&serde_json::json!({
            "chunk_ids": chunk_ids,
            "superseded_ids": superseded_ids,
        }));
    }

    // If any chunk carries a temporal overlay field, fall out of the
    // Track E: pre-pass over every chunk to apply ingestion_mode +
    // conversation-mode review default. This decides per-chunk whether
    // a lifecycle delta is required and produces the (chunk, delta)
    // tuples both branches consume. Batches without any per-chunk
    // lifecycle (no expires_at_ms / review_after_ms / conversation
    // mode) keep the bulk `store.add_batch` fast path unchanged.
    let mut prepared: Vec<(MemoryChunk, LifecycleDelta, bool)> =
        Vec::with_capacity(params.chunks.len());
    for chunk_params in params.chunks {
        let chunk_type = parse_chunk_type(&chunk_params.chunk_type)?;
        let mut chunk = MemoryChunk::new(tenant_id.clone(), &chunk_params.text, chunk_type);
        if let Some(project_id) = &chunk_params.project_id {
            chunk = chunk.with_project(ProjectId::new(Some(project_id.clone())));
        }
        if let Some(episode_id) = &chunk_params.episode_id {
            validate_episode_id(episode_id)?;
            let mut tags = chunk.tags.clone();
            tags.push(make_episode_tag(episode_id));
            chunk = chunk.with_tags(tags);
        }
        chunk = chunk.with_source(params_to_source(chunk_params.source));
        if !chunk_params.tags.is_empty() {
            let mut tags = chunk.tags.clone();
            tags.extend(chunk_params.tags);
            chunk = chunk.with_tags(tags);
        }
        let ingestion_mode = parse_ingestion_mode(chunk_params.mode.as_deref())?;
        chunk = chunk.with_ingestion_mode(ingestion_mode);
        let review_after_ms = apply_conversation_review_default(
            ingestion_mode,
            chunk_params.review_after_ms,
        );
        let delta = LifecycleDelta {
            expires_at_ms: chunk_params.expires_at_ms.map(Some),
            review_after_ms: review_after_ms.map(Some),
            ..Default::default()
        };
        let has_lifecycle = !delta.is_empty();
        prepared.push((chunk, delta, has_lifecycle));
    }
    let any_lifecycle = prepared.iter().any(|(_, _, hl)| *hl);

    let chunk_ids = if any_lifecycle {
        let ps = store.as_persistent().ok_or_else(|| {
            McpError::ToolError(
                "memory.add_batch with temporal fields requires a persistent store".into(),
            )
        })?;
        // Per-chunk through add_chunk_with_lifecycle so the per-row
        // overlay is applied. Failures still leave earlier rows
        // committed — same contract as the bulk add_batch fast path.
        let mut ids = Vec::with_capacity(prepared.len());
        for (chunk, delta, _) in prepared {
            let id = ps
                .add_chunk_with_lifecycle(chunk, delta)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            ids.push(id);
        }
        ids
    } else {
        // No lifecycle overlay anywhere → bulk path. The chunks already
        // carry the per-row ingestion_mode label (set in the pre-pass);
        // store.add_batch threads that through to ChunkMetadata.
        let chunks: Vec<MemoryChunk> = prepared.into_iter().map(|(c, _, _)| c).collect();
        store
            .add_batch(chunks)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    info!(count = chunk_ids.len(), "batch add completed");

    format_mcp_response(&AddBatchResult {
        chunk_ids: chunk_ids.iter().map(|id| id.to_string()).collect(),
    })
}

/// Parameters for memory.export_markdown (Track G2).
#[derive(Debug, Deserialize)]
pub struct ExportMarkdownParams {
    #[serde(default)]
    pub tenant_id: String,
    /// Optional project filter — when set, only chunks under this
    /// project are exported.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Maximum chunks to read from metadata. Defaults to 10_000;
    /// callers can raise it for whole-tenant exports.
    #[serde(default = "default_export_limit")]
    pub limit: usize,
}

fn default_export_limit() -> usize {
    10_000
}

/// Handle memory.export_markdown (Track G2). Read-only — never writes
/// to disk; the CLI (G3) consumes the returned `{path, content}`
/// tuples and writes them on the user's machine.
pub async fn handle_memory_export_markdown<S: Store>(
    store: &S,
    params: ExportMarkdownParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.export_markdown requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    // SQL-level project filter when scoped, so a noisy tenant doesn't
    // starve the scoped export by burning the row budget on rows from
    // other projects (Codex round-1 G2 MEDIUM finding).
    let project_filter = params.project_id.as_deref();
    let metas = if let Some(pid) = project_filter {
        ps.metadata()
            .list_recent_for_project(&tenant_id, Some(pid), params.limit)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        ps.metadata()
            .list(&tenant_id, params.limit, 0)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    let mut chunks: Vec<MemoryChunk> = Vec::with_capacity(metas.len());
    for meta in metas {
        if meta.status != ChunkStatus::Final || meta.lifecycle.superseded_by.is_some() {
            continue;
        }
        if let Some(pid) = project_filter {
            if meta.project_id.as_deref() != Some(pid) {
                continue;
            }
        }
        match store
            .get(&tenant_id, &meta.chunk_id)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
        {
            Some(chunk) => chunks.push(chunk),
            None => continue,
        }
    }

    let files = crate::mcp::markdown_export::render_markdown_tree(&chunks);
    let payload: Vec<serde_json::Value> = files
        .into_iter()
        .map(|f| serde_json::json!({ "path": f.path, "content": f.content }))
        .collect();

    format_mcp_response(&serde_json::json!({ "files": payload }))
}

/// Parameters for memory.find_near_duplicates (Track D5).
///
/// Read-only preview that mirrors the candidates
/// `memory.add(supersede_near_duplicates=...)` would actually link.
/// Pool sizes and scope semantics match the write path exactly so the
/// preview never reports a candidate the write path would miss (or
/// vice versa) — Codex round-1 D5 MEDIUM finding.
#[derive(Debug, Deserialize)]
pub struct FindNearDuplicatesParams {
    #[serde(default)]
    pub tenant_id: String,
    pub text: String,
    #[serde(rename = "type", default = "default_doc_type")]
    pub chunk_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// When set, also returns trigram-Jaccard candidates with score ≥
    /// this threshold over the same FUZZY_RECENT_POOL_SIZE pool the
    /// write path uses. Absent = exact-only.
    #[serde(default)]
    pub fuzzy_threshold: Option<f32>,
    /// `"project"` (default) restricts the candidate pool to rows with
    /// the same project_id (incl. project_id IS NULL when the probe
    /// has no project). `"tenant"` widens to the whole tenant.
    #[serde(default)]
    pub scope: Option<String>,
}

fn default_doc_type() -> String {
    "doc".into()
}

/// Handle memory.find_near_duplicates (Track D5). Read-only.
pub async fn handle_memory_find_near_duplicates<S: Store>(
    store: &S,
    params: FindNearDuplicatesParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError(
            "memory.find_near_duplicates requires a persistent store".into(),
        )
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_type = parse_chunk_type(&params.chunk_type)?;

    let scope_project = match params.scope.as_deref().unwrap_or("project") {
        "project" => true,
        "tenant" => false,
        other => {
            return Err(McpError::InvalidParams(format!(
                "scope: expected 'project' or 'tenant', got '{other}'"
            )));
        }
    };

    let canonical = crate::store::supersession::canonicalize_for_type(&params.text, chunk_type);
    let project_filter = if scope_project {
        params.project_id.as_deref()
    } else {
        None
    };

    // Exact: SQL pre-filters by canonical, so a Rust post-filter to
    // honour project_id IS NULL is cheap and safe.
    let exact_metas = ps
        .metadata()
        .list_by_canonical_text(&tenant_id, project_filter, &canonical)
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let exact: Vec<String> = exact_metas
        .into_iter()
        .filter(|m| {
            // Live-head only: don't surface previously-superseded rows
            // (mirrors compute_dedup_candidates semantics).
            m.status == ChunkStatus::Final && m.lifecycle.superseded_by.is_none()
        })
        .filter(|m| {
            !scope_project
                || params.project_id.is_none() && m.project_id.is_none()
                || m.project_id.as_deref() == params.project_id.as_deref()
        })
        .map(|m| m.chunk_id.to_string())
        .collect();

    // Fuzzy: optional. Pool size is fixed at FUZZY_RECENT_POOL_SIZE
    // so the preview's candidate set is exactly the one
    // `compute_dedup_candidates` would consider on the write path
    // (Codex round-1 D5 MEDIUM finding). Emits (chunk_id, similarity)
    // pairs ordered by score desc.
    let mut fuzzy_pairs: Vec<(String, f32)> = Vec::new();
    if let Some(threshold) = params.fuzzy_threshold {
        let limit = crate::mcp::dedup::FUZZY_RECENT_POOL_SIZE;
        let metas = if scope_project && params.project_id.is_none() {
            ps.metadata()
                .list_recent_with_null_project(&tenant_id, limit)
                .map_err(|e| McpError::ToolError(e.to_string()))?
        } else {
            ps.metadata()
                .list_recent_for_project(&tenant_id, project_filter, limit)
                .map_err(|e| McpError::ToolError(e.to_string()))?
        };
        for m in metas {
            if !(m.status == ChunkStatus::Final && m.lifecycle.superseded_by.is_none()) {
                continue;
            }
            if scope_project
                && !(params.project_id.is_none() && m.project_id.is_none()
                    || m.project_id.as_deref() == params.project_id.as_deref())
            {
                continue;
            }
            let other = m.canonical_text.as_deref().unwrap_or("");
            let score = crate::store::supersession::jaccard_trigram_score(&canonical, other);
            if score >= threshold {
                fuzzy_pairs.push((m.chunk_id.to_string(), score));
            }
        }
        fuzzy_pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }

    let fuzzy: Vec<serde_json::Value> = fuzzy_pairs
        .into_iter()
        .map(|(id, sim)| serde_json::json!({ "chunk_id": id, "similarity": sim }))
        .collect();

    format_mcp_response(&serde_json::json!({
        "exact_matches": exact,
        "fuzzy_matches": fuzzy,
    }))
}

// ----------------------------------------------------------------
// Track F5 — OMF MCP handlers.
// ----------------------------------------------------------------

/// Parameters for memory.export_omf (Track F5).
#[derive(Debug, Deserialize, Default)]
pub struct ExportOmfParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// Include history-tier rows in the export (false = live-only).
    #[serde(default)]
    pub include_history: bool,
    /// When absent, defaults to true (matches `ExportOptions`).
    #[serde(default)]
    pub include_superseded: Option<bool>,
    /// When absent, defaults to true (matches `ExportOptions`).
    #[serde(default)]
    pub include_expired: Option<bool>,
}

/// Handle memory.export_omf (Track F5). Read-only.
pub async fn handle_memory_export_omf<S: Store>(
    store: &S,
    params: ExportOmfParams,
) -> Result<Value, McpError> {
    let ps = store
        .as_persistent()
        .ok_or_else(|| McpError::ToolError("memory.export_omf requires a persistent store".into()))?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    let opts = crate::omf::export::ExportOptions {
        project_id: params.project_id,
        include_history: params.include_history,
        include_superseded: params.include_superseded.unwrap_or(true),
        include_expired: params.include_expired.unwrap_or(true),
    };
    let doc = crate::omf::export::export_omf(ps, &tenant_id, opts)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&json!({ "document": doc }))
}

/// Parameters for memory.preview_omf_import (Track F5).
#[derive(Debug, Deserialize)]
pub struct PreviewOmfImportParams {
    #[serde(default)]
    pub tenant_id: String,
    /// The OMF document to preview. Required.
    pub document: crate::omf::OmfDocument,
    /// Include items whose top-level status is "archived"/"expired".
    /// Defaults to true (matches `ImportOptions`).
    #[serde(default)]
    pub include_archived: Option<bool>,
    /// Optional fuzzy threshold. Absent = exact-only.
    #[serde(default)]
    pub fuzzy_threshold: Option<f32>,
}

/// Handle memory.preview_omf_import (Track F5). Read-only dry-run.
pub async fn handle_memory_preview_omf_import<S: Store>(
    store: &S,
    params: PreviewOmfImportParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.preview_omf_import requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    let opts = crate::omf::import::ImportOptions {
        include_archived: params.include_archived.unwrap_or(true),
        fuzzy_threshold: params.fuzzy_threshold,
    };
    let preview = crate::omf::import::preview_omf_import(ps, &tenant_id, &params.document, opts)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&json!({
        "total": preview.total,
        "to_import": preview.to_import,
        "duplicates": preview.duplicates,
        "filtered": preview.filtered,
        "unscoped": preview.unscoped,
        "by_project": preview.by_project,
    }))
}

/// Parameters for memory.import_omf (Track F5).
#[derive(Debug, Deserialize)]
pub struct ImportOmfParams {
    #[serde(default)]
    pub tenant_id: String,
    /// The OMF document to import. Required.
    pub document: crate::omf::OmfDocument,
    /// Include items whose top-level status is "archived"/"expired".
    /// Defaults to true.
    #[serde(default)]
    pub include_archived: Option<bool>,
    /// Optional fuzzy threshold. Absent = exact-only.
    #[serde(default)]
    pub fuzzy_threshold: Option<f32>,
}

/// Handle memory.import_omf (Track F5).
///
/// Returns both the formatted MCP response and a list of
/// `PostWriteEvent`s — one per newly imported chunk — so the server
/// dispatch arm can run structural indexing identically to how
/// memory.add_batch + memory.supersede already do.
pub async fn handle_memory_import_omf<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: ImportOmfParams,
) -> Result<(Value, Vec<PostWriteEvent>), McpError> {
    let ps = store
        .as_persistent()
        .ok_or_else(|| McpError::ToolError("memory.import_omf requires a persistent store".into()))?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let opts = crate::omf::import::ImportOptions {
        include_archived: params.include_archived.unwrap_or(true),
        fuzzy_threshold: params.fuzzy_threshold,
    };
    let (result, imported) =
        crate::omf::import::import_omf_with_events(ps, &tenant_id, &params.document, opts)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;

    let tenant_id_str = tenant_id.to_string();
    let events: Vec<PostWriteEvent> = imported
        .into_iter()
        .map(|ic| PostWriteEvent::from_imported_chunk(ic, &tenant_id_str))
        .collect();

    let response = format_mcp_response(&json!({
        "total": result.total,
        "imported": result.imported,
        "duplicates": result.duplicates,
        "skipped": result.skipped,
    }))?;
    Ok((response, events))
}

/// Handle task.start tool call.
pub async fn handle_task_start<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskStartParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    // `goal` remains the only hard-required field on task.start in
    // v0.3.1+ (see Phase 2.2). motivation/hypothesis/scientific_question
    // became optional — they can be empty strings when the caller has
    // nothing to say; richer task records still fill them in.
    validate_identifier("goal", &params.goal)?;
    if let Some(parent_task_id) = params.parent_task_id.as_deref() {
        validate_identifier("parent_task_id", parent_task_id)?;
    }

    info!(
        tenant_id = %tenant_id,
        goal = %params.goal,
        "task.start"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_start(tenant_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.parent_task_id = params.parent_task_id;
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.goal = Some(params.goal);
    artifact.motivation = Some(params.motivation);
    artifact.hypothesis = Some(params.hypothesis);
    artifact.scientific_question = Some(params.scientific_question);
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.expected_outputs = params.expected_outputs;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    finalize_artifact_for_storage(&mut artifact);
    let projections = build_task_projections(&artifact);
    let result = store
        .add_task_artifact(artifact, projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.finish tool call.
pub async fn handle_task_finish<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskFinishParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    // Confidence is optional in v0.3.1+; only validate when supplied.
    if let Some(confidence) = params.confidence {
        validate_confidence(confidence)?;
    }

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        "task.finish"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_finish(tenant_id, params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.status = Some(params.status.unwrap_or_else(|| "completed".to_string()));
    artifact.goal = params.goal;
    artifact.scientific_question = params.scientific_question;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.what_worked = params.what_worked;
    artifact.what_failed = params.what_failed;
    artifact.validation = params.validation;
    artifact.uncertainty = params.uncertainty;
    artifact.followups = params.followups;
    // `confidence` is optional in v0.3.1+; only attach when the caller
    // actually asserted a value.
    artifact.confidence = params.confidence;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    finalize_artifact_for_storage(&mut artifact);
    let projections = build_task_projections(&artifact);
    // Capture scope for the dirty-digest hook before `artifact` moves
    // into the store.
    let tenant_for_dirty = artifact.tenant_id.clone();
    let project_for_dirty = artifact.project_id.as_option().map(str::to_string);
    let result = store
        .add_task_artifact(artifact, projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Phase 3.4: task.finish rolls up what_worked / what_failed /
    // validation, which are exactly the inputs to the failure,
    // highlight, and project_brief digests.
    mark_task_finish_digests_dirty(&tenant_for_dirty, project_for_dirty.as_deref());

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.progress tool call.
pub async fn handle_task_progress<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskProgressParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("summary", &params.summary)?;
    validate_identifier("next_step", &params.next_step)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        "task.progress"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_progress(tenant_id, params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.summary = Some(params.summary);
    artifact.blockers = params.blockers;
    artifact.what_failed = params.failed_attempts;
    artifact.followups = vec![params.next_step];
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    finalize_artifact_for_storage(&mut artifact);
    let result = store
        .add_task_artifact(
            artifact.clone(),
            // Phase 2.5: high-frequency task.* handlers emit one
            // projection per call (the base summary) instead of the
            // legacy 4-7 fanout. See
            // `build_task_projections_minimal` for rationale.
            build_task_projections_minimal(&artifact),
        )
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.run_start tool call.
pub async fn handle_task_run_start<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskRunStartParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("tool_name", &params.tool_name)?;
    validate_identifier("command", &params.command)?;
    validate_identifier("why_chosen", &params.why_chosen)?;
    if params.inputs.is_empty() {
        return Err(McpError::InvalidParams(
            "inputs must not be empty".to_string(),
        ));
    }

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        tool_name = %params.tool_name,
        "task.run_start"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_run_start(tenant_id, params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.summary = params.summary;
    artifact.tool_name = Some(params.tool_name);
    artifact.tool_version = params.tool_version;
    artifact.command = Some(params.command);
    artifact.why_chosen = Some(params.why_chosen);
    artifact.parameters = Some(params.parameters);
    artifact.inputs = params.inputs;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    if artifact.provenance.tool_name.is_none() {
        artifact.provenance.tool_name = artifact.tool_name.clone();
    }
    if artifact.provenance.tool_version.is_none() {
        artifact.provenance.tool_version = artifact.tool_version.clone();
    }

    finalize_artifact_for_storage(&mut artifact);
    // run_start keeps full projections because the separate Run
    // projection carries tool/command/parameters content that
    // retrieval filters rely on (see task_search_filters_exactly_by_tool_and_dataset).
    let result = store
        .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.run_finish tool call.
pub async fn handle_task_run_finish<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskRunFinishParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("status", &params.status)?;
    validate_identifier("notes", &params.notes)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        status = %params.status,
        "task.run_finish"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_run_finish(tenant_id, params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.status = Some(params.status);
    artifact.tool_name = params.tool_name;
    artifact.tool_version = params.tool_version;
    artifact.command = params.command;
    artifact.outputs = params.outputs;
    artifact.metrics = params.metrics;
    artifact.summary = Some(params.notes);
    artifact.validation = params.validation;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    if artifact.provenance.tool_name.is_none() {
        artifact.provenance.tool_name = artifact.tool_name.clone();
    }
    if artifact.provenance.tool_version.is_none() {
        artifact.provenance.tool_version = artifact.tool_version.clone();
    }

    finalize_artifact_for_storage(&mut artifact);
    // run_finish keeps full projections so tool/outputs/metrics are
    // still indexed as retrievable text for tool-name filters.
    let result = store
        .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.add_evidence tool call.
pub async fn handle_task_add_evidence<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskAddEvidenceParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("summary", &params.summary)?;
    validate_identifier("evidence_kind", &params.evidence_kind)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        evidence_kind = %params.evidence_kind,
        "task.add_evidence"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    // Keep `tenant_id` available for the post-write dirty-digest hook.
    let mut artifact = TaskArtifact::new_evidence(tenant_id.clone(), params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    // Summary is optional in v0.3.1+; only set when non-empty so
    // downstream `score_text_candidate` does not index a bogus empty
    // string.
    artifact.summary = (!params.summary.is_empty()).then_some(params.summary);
    artifact.evidence_kind = Some(params.evidence_kind);
    artifact.supports_claim = params.supports_claim;
    artifact.metrics = match (params.metric_name, params.metric_value, params.metrics) {
        (_, _, Some(metrics)) => Some(metrics),
        (Some(metric_name), Some(metric_value), None) => Some(json!({
            "metric_name": metric_name,
            "metric_value": metric_value,
        })),
        _ => None,
    };
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    finalize_artifact_for_storage(&mut artifact);
    let result = store
        .add_task_artifact(
            artifact.clone(),
            // Phase 2.5: high-frequency task.* handlers emit one
            // projection per call (the base summary) instead of the
            // legacy 4-7 fanout. See
            // `build_task_projections_minimal` for rationale.
            build_task_projections_minimal(&artifact),
        )
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Phase 3.4: evidence writes invalidate the evidence library,
    // highlight library (which ranks evidence-backed lessons), and
    // project brief (which summarizes evidence density).
    mark_evidence_related_digests_dirty(&tenant_id, artifact.project_id.as_option());

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Phase 3.4: mark every digest whose view depends on evidence
/// content as dirty. Called from `task.add_evidence` and from the
/// artifact.create path when the kind influences evidence aggregation.
fn mark_evidence_related_digests_dirty(tenant_id: &TenantId, project_id: Option<&str>) {
    let project = project_id.map(str::to_string);
    for role in [
        crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY,
        crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF,
    ] {
        crate::task_memory::digest_dirty::mark_dirty(tenant_id.to_string(), project.clone(), role);
    }
}

/// Phase 3.4: mark digests affected by decision/review/revision
/// artifact writes.
fn mark_decision_related_digests_dirty(tenant_id: &TenantId, project_id: Option<&str>) {
    let project = project_id.map(str::to_string);
    for role in [
        crate::task_memory::DIGEST_ROLE_DECISION_LIBRARY,
        crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF,
    ] {
        crate::task_memory::digest_dirty::mark_dirty(tenant_id.to_string(), project.clone(), role);
    }
}

/// Phase 3.4: `task.finish` captures `what_failed` / `validation` /
/// `what_worked` / `followups`, which feed ALL four canonical-data
/// digest families (`infer_failure_items`, `infer_decision_items`,
/// `infer_evidence_items`, `infer_highlight_items` in `task_memory::digests`
/// all consume `TaskFinish`). Mark every one dirty so the sweeper
/// refreshes the full set; dropping decision/evidence here was a
/// Codex-flagged coverage hole.
fn mark_task_finish_digests_dirty(tenant_id: &TenantId, project_id: Option<&str>) {
    let project = project_id.map(str::to_string);
    for role in [
        crate::task_memory::DIGEST_ROLE_FAILURE_LIBRARY,
        crate::task_memory::DIGEST_ROLE_DECISION_LIBRARY,
        crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY,
        crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF,
    ] {
        crate::task_memory::digest_dirty::mark_dirty(tenant_id.to_string(), project.clone(), role);
    }
}

/// Handle artifact.create tool call.
pub async fn handle_artifact_create<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: ArtifactCreateParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let artifact_kind =
        ArtifactKind::from_str(&params.artifact_kind).map_err(McpError::InvalidParams)?;

    // Digest artifacts are server-generated by the compaction runner /
    // memory.compact path (via `persist_digest_artifact`). Because their
    // IDs are deterministic on (role, scope), accepting client-authored
    // digests lets any caller overwrite the project's canonical digest
    // artifacts (`project_brief`, `failure_library`, …). Reject them at
    // the boundary — the only legitimate way to refresh a digest is via
    // `memory.compact`.
    if artifact_kind == ArtifactKind::Digest {
        return Err(McpError::InvalidParams(
            "artifact.create: digests are server-generated; \
             use memory.compact to refresh digest artifacts"
                .to_string(),
        ));
    }

    if let Some(confidence) = params.confidence {
        validate_confidence(confidence)?;
    }
    if let Some(reply_to_artifact_id) = params.reply_to_artifact_id.as_deref() {
        validate_identifier("reply_to_artifact_id", reply_to_artifact_id)?;
    }

    info!(
        tenant_id = %tenant_id,
        artifact_kind = %artifact_kind.as_str(),
        "artifact.create"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let parent_artifact = if let Some(reply_to_artifact_id) = params.reply_to_artifact_id.as_deref()
    {
        store
            .get_task_artifact(&tenant_id, reply_to_artifact_id)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        None
    };

    let explicit_task_id = match params.task_id.as_deref() {
        Some(task_id) => {
            validate_identifier("task_id", task_id)?;
            Some(task_id.to_string())
        }
        None => None,
    };
    let task_id = explicit_task_id.clone().or_else(|| {
        parent_artifact
            .as_ref()
            .map(|artifact| artifact.task_id.clone())
    });

    let mut artifact = match artifact_kind {
        ArtifactKind::TaskStart => {
            let mut artifact = TaskArtifact::new_task_start(tenant_id.clone());
            if let Some(task_id) = explicit_task_id.clone() {
                artifact.task_id = task_id;
            }
            artifact
        }
        ArtifactKind::TaskProgress => TaskArtifact::new_task_progress(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::RunStart => TaskArtifact::new_run_start(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::RunFinish => TaskArtifact::new_run_finish(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::Evidence => TaskArtifact::new_evidence(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::Review => TaskArtifact::new_review(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams("task_id is required for review artifacts".to_string())
            })?,
        ),
        ArtifactKind::Revision => TaskArtifact::new_revision(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams("task_id is required for revision artifacts".to_string())
            })?,
        ),
        ArtifactKind::Verification => TaskArtifact::new_verification(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for verification artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::Decision => TaskArtifact::new_decision(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams("task_id is required for decision artifacts".to_string())
            })?,
        ),
        ArtifactKind::Digest => {
            let role = params.artifact_role.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "artifact_role is required for digest artifacts".to_string(),
                )
            })?;
            let digest_scope = params
                .project_id
                .clone()
                .or_else(|| task_id.clone())
                .unwrap_or_else(|| "tenant".to_string());
            let (artifact_id, synthetic_task_id, digest_key) =
                crate::task_memory::stable_digest_identity(&role, &digest_scope);
            let mut artifact =
                TaskArtifact::new_digest(tenant_id.clone(), synthetic_task_id, digest_key, role);
            artifact.artifact_id = artifact_id;
            artifact
        }
        ArtifactKind::TaskFinish => TaskArtifact::new_task_finish(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::WikiPage => TaskArtifact::new_wiki_page(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for wiki_page artifacts".to_string(),
                )
            })?,
        ),
    };

    // Phase 0 trust boundary: `content` is only allowed on `wiki_page`
    // kinds. Reject non-empty `content` on every other kind at the
    // MCP boundary so stored rows carry a consistent invariant
    // (validator elsewhere, e.g. digests.rs, can treat `content ==
    // Some(_)` as `kind == WikiPage` without needing a fallback).
    if let Some(content) = params.content.as_ref() {
        if !content.is_empty() && artifact_kind != ArtifactKind::WikiPage {
            return Err(McpError::InvalidParams(format!(
                "artifact.create: `content` is only accepted on `wiki_page` artifacts; \
                 got artifact_kind={}",
                artifact_kind.as_str()
            )));
        }
    }

    let inherited_project_id = parent_artifact
        .as_ref()
        .and_then(|artifact| artifact.project_id.as_option().map(str::to_string));
    let inherited_thread_id = parent_artifact
        .as_ref()
        .map(|artifact| artifact.thread_key().to_string());
    let inherited_challenge_id = parent_artifact
        .as_ref()
        .and_then(|artifact| artifact.challenge_id.clone());

    let dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    let entity_refs = entity_params_to_refs(params.entity_refs)?;
    let contributors = contributor_params_to_refs(params.contributors)?;
    let provenance = params_to_task_provenance(params.provenance);

    artifact.tool_name = params.tool_name;
    artifact.tool_version = params.tool_version;
    artifact.command = params.command;
    artifact.parameters = params.parameters;
    artifact.inputs = params.inputs;
    artifact.outputs = params.outputs;
    artifact.metrics = params.metrics;
    artifact.why_chosen = params.why_chosen;
    artifact.goal = params.goal;
    artifact.motivation = params.motivation;
    artifact.hypothesis = params.hypothesis;
    artifact.scientific_question = params.scientific_question;
    artifact.method_summary = params.method_summary;
    artifact.summary = params.summary;
    artifact.content = params.content;
    artifact.evidence_kind = params.evidence_kind;
    artifact.supports_claim = params.supports_claim;
    artifact.blockers = params.blockers;
    artifact.what_worked = params.what_worked;
    artifact.what_failed = params.what_failed;
    artifact.validation = params.validation;
    artifact.uncertainty = params.uncertainty;
    artifact.followups = params.followups;
    artifact.expected_outputs = params.expected_outputs;
    artifact.related_artifact_ids = params.related_artifact_ids;
    artifact.confidence = params.confidence;
    artifact.requested_action = params.requested_action;
    artifact.verification_status = params.verification_status;
    artifact.compute_budget = params.compute_budget;
    artifact.cost_actual = params.cost_actual;
    artifact.data_access_level = params.data_access_level;
    artifact.policy_tags = params.policy_tags;
    artifact.allowed_tools = params.allowed_tools;
    artifact.approval_state = params.approval_state;

    let relation_kind = params.relation_kind.or_else(|| {
        if params.reply_to_artifact_id.is_some() {
            Some(match artifact_kind {
                ArtifactKind::Review => "reviews".to_string(),
                ArtifactKind::Revision => "revises".to_string(),
                ArtifactKind::Verification => "verifies".to_string(),
                _ => "reply_to".to_string(),
            })
        } else {
            None
        }
    });
    let thread_id = params
        .thread_id
        .or(inherited_thread_id)
        .or_else(|| Some(artifact.task_id.clone()));

    apply_common_artifact_fields(
        &mut artifact,
        params.project_id.or(inherited_project_id),
        params.parent_task_id,
        resolved_agent_id(params.agent_id.as_deref()),
        params.session_id,
        params.status,
        params.artifact_role,
        params.challenge_id.or(inherited_challenge_id),
        thread_id,
        params.reply_to_artifact_id,
        relation_kind,
        dataset_refs,
        entity_refs,
        contributors,
        provenance,
    );

    finalize_artifact_for_storage(&mut artifact);
    // If this artifact countersigns a prior canonical artifact written
    // by a different agent, upgrade the promotion state to Verified.
    // This is the ONLY path that produces `VerifiedRecord` trust today.
    promote_if_countersigned(store, &mut artifact).await?;
    let projections = build_task_projections(&artifact);
    // Capture scope + kind for the Phase 3.4 dirty-digest hook before
    // the artifact moves into the store.
    let tenant_for_dirty = artifact.tenant_id.clone();
    let project_for_dirty = artifact.project_id.as_option().map(str::to_string);
    let kind_for_dirty = artifact.artifact_kind;
    let validation_for_dirty: Vec<String> = artifact.validation.clone();
    let result = store
        .add_task_artifact(artifact, projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Phase 3.4: decisions, reviews, verifications invalidate the
    // decision/highlight/project_brief libraries. Evidence artifacts
    // invalidate the evidence family. Additionally (Codex follow-up):
    // any artifact with non-empty `validation` also feeds the evidence
    // library via `infer_evidence_items`, so we dirty that family too
    // even for review/decision/verification kinds when validation is
    // present. `revision` is intentionally narrower — revisions are
    // meta-edits and don't flow into the decision/evidence aggregates.
    match kind_for_dirty {
        ArtifactKind::Decision | ArtifactKind::Review | ArtifactKind::Verification => {
            mark_decision_related_digests_dirty(&tenant_for_dirty, project_for_dirty.as_deref());
        }
        ArtifactKind::Evidence => {
            mark_evidence_related_digests_dirty(&tenant_for_dirty, project_for_dirty.as_deref());
        }
        ArtifactKind::Revision => {
            // Revisions only touch the thread structure + highlight
            // ranking, not the library content directly.
            crate::task_memory::digest_dirty::mark_dirty(
                tenant_for_dirty.to_string(),
                project_for_dirty.clone(),
                crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY,
            );
            crate::task_memory::digest_dirty::mark_dirty(
                tenant_for_dirty.to_string(),
                project_for_dirty.clone(),
                crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF,
            );
        }
        _ => {}
    }
    // Any artifact that carries validation flows into the evidence
    // library regardless of kind.
    if !validation_for_dirty.is_empty() && !matches!(kind_for_dirty, ArtifactKind::Evidence) {
        crate::task_memory::digest_dirty::mark_dirty(
            tenant_for_dirty.to_string(),
            project_for_dirty.clone(),
            crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY,
        );
    }

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.get tool call.
pub async fn handle_task_get<S: Store>(
    store: &S,
    params: TaskGetParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;

    let artifacts = store
        .list_task_artifacts(&tenant_id, &params.task_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskGetResult {
        task_id: params.task_id,
        artifacts,
    })
}

/// Handle artifact.get tool call.
pub async fn handle_artifact_get<S: Store>(
    store: &S,
    params: ArtifactGetParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("artifact_id", &params.artifact_id)?;

    let artifact = store
        .get_task_artifact(&tenant_id, &params.artifact_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&ArtifactGetResult { artifact })
}

/// Handle task.search tool call.
pub async fn handle_task_search<S: Store>(
    store: &S,
    params: TaskSearchParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    let filters = parse_task_search_filters(params.filters.as_ref())?;
    let mode = params.mode.unwrap_or_default();
    let has_filters = has_active_task_filters(&filters);
    let candidate_limit = if has_filters {
        params.k.saturating_mul(20).clamp(50, 1000)
    } else {
        params.k.saturating_mul(25).clamp(100, 1000)
    };
    let scoped_tenants =
        scoped_tenants_for_project(store, &tenant_id, filters.project_id.as_deref()).await?;

    let mut chunk_ids = if mode != QueryMode::Generic {
        candidate_chunk_ids_for_tenants_and_mode(
            store,
            &scoped_tenants,
            mode,
            &filters,
            candidate_limit,
        )
        .await?
    } else {
        Vec::new()
    };
    let base_chunk_ids = search_task_projection_chunk_ids_for_tenants(
        store,
        &scoped_tenants,
        &filters,
        candidate_limit,
    )
    .await?;
    let mut seen = chunk_ids.iter().cloned().collect::<HashSet<_>>();
    for chunk_id in base_chunk_ids {
        if seen.insert(chunk_id.clone()) {
            chunk_ids.push(chunk_id);
        }
    }
    let mut ranked_lists = Vec::with_capacity(scoped_tenants.len());
    for tenant in &scoped_tenants {
        ranked_lists.push(
            store
                .rerank_chunks_for_query(tenant, &params.query, &chunk_ids, params.k)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
    }
    let ranked = merge_scored_chunk_lists(ranked_lists, params.k);
    let artifacts = resolve_artifacts_for_ranked_chunks(store, &ranked).await?;
    let results = ranked
        .iter()
        .map(|(chunk, score)| {
            chunk_to_result(
                chunk,
                *score,
                None,
                artifacts.get(&chunk.chunk_id.to_string()).cloned(),
            )
        })
        .collect::<Vec<_>>();

    format_mcp_response(&SearchResult {
        results,
        tier_info: None,
        repair_info: None,
    })
}

async fn search_artifacts_internal<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    query: &str,
    k: usize,
    filters: &TaskSearchFilters,
    mode: QueryMode,
) -> Result<Vec<ArtifactSearchHit>, McpError> {
    let has_filters = has_active_task_filters(filters);
    let candidate_limit = if has_filters {
        k.saturating_mul(20).clamp(50, 1000)
    } else {
        k.saturating_mul(25).clamp(100, 1000)
    };
    let scoped_tenants =
        scoped_tenants_for_project(store, tenant_id, filters.project_id.as_deref()).await?;

    let mut chunk_ids = if mode != QueryMode::Generic {
        candidate_chunk_ids_for_tenants_and_mode(
            store,
            &scoped_tenants,
            mode,
            filters,
            candidate_limit,
        )
        .await?
    } else {
        Vec::new()
    };
    let base_chunk_ids = search_task_projection_chunk_ids_for_tenants(
        store,
        &scoped_tenants,
        filters,
        candidate_limit,
    )
    .await?;
    let mut seen = chunk_ids.iter().cloned().collect::<HashSet<_>>();
    for chunk_id in base_chunk_ids {
        if seen.insert(chunk_id.clone()) {
            chunk_ids.push(chunk_id);
        }
    }
    let mut ranked_lists = Vec::with_capacity(scoped_tenants.len());
    for tenant in &scoped_tenants {
        ranked_lists.push(
            store
                .rerank_chunks_for_query(tenant, query, &chunk_ids, candidate_limit)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
    }
    let ranked = merge_scored_chunk_lists(ranked_lists, candidate_limit);
    let artifacts = resolve_artifacts_for_ranked_chunks(store, &ranked).await?;

    let mut seen = HashSet::new();
    let mut results = Vec::new();
    for (chunk, score) in ranked {
        let Some(artifact) = artifacts.get(&chunk.chunk_id.to_string()).cloned() else {
            continue;
        };
        if !seen.insert(artifact.artifact_id.clone()) {
            continue;
        }
        results.push(build_artifact_search_hit(artifact, score, Some(&chunk)));
        if results.len() >= k {
            break;
        }
    }

    Ok(results)
}

/// Handle artifact.search tool call.
pub async fn handle_artifact_search<S: Store>(
    store: &S,
    params: TaskSearchParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    let filters = parse_task_search_filters(params.filters.as_ref())?;
    let mode = params.mode.unwrap_or_default();
    let results =
        search_artifacts_internal(store, &tenant_id, &params.query, params.k, &filters, mode)
            .await?;

    format_mcp_response(&ArtifactSearchResult { results })
}

/// Handle artifact.list_thread tool call.
pub async fn handle_artifact_list_thread<S: Store>(
    store: &S,
    params: ArtifactListThreadParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    let thread_id = match (params.thread_id, params.artifact_id) {
        (Some(thread_id), _) => {
            validate_identifier("thread_id", &thread_id)?;
            thread_id
        }
        (None, Some(artifact_id)) => {
            validate_identifier("artifact_id", &artifact_id)?;
            let artifact = store
                .get_task_artifact(&tenant_id, &artifact_id)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?
                .ok_or_else(|| McpError::ToolError("artifact not found".to_string()))?;
            artifact.thread_key().to_string()
        }
        (None, None) => {
            return Err(McpError::InvalidParams(
                "artifact.list_thread requires thread_id or artifact_id".to_string(),
            ));
        }
    };

    let artifacts = store
        .list_thread_artifacts(&tenant_id, &thread_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&ArtifactThreadResult {
        thread_id,
        artifacts,
    })
}

fn dedupe_grounding_refs(refs: impl IntoIterator<Item = GroundingRef>) -> Vec<GroundingRef> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for reference in refs {
        if seen.insert(reference.artifact_id.clone()) {
            out.push(reference);
        }
    }
    out
}

fn dedupe_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn grounding_status_label(status: GroundingStatus) -> &'static str {
    match status {
        GroundingStatus::VerifiedRecord => "verified_record",
        GroundingStatus::CanonicallyGrounded => "canonically_grounded",
        GroundingStatus::DigestOnly => "digest_only",
        GroundingStatus::InsufficientGrounding => "insufficient_grounding",
        GroundingStatus::Conflicted => "conflicted",
    }
}

fn grounding_confidence(
    status: GroundingStatus,
    support_count: usize,
    conflict_count: usize,
) -> f32 {
    let support_boost = (support_count.min(3) as f32) * 0.03;
    match status {
        GroundingStatus::VerifiedRecord => (0.92 + support_boost).min(0.99),
        GroundingStatus::CanonicallyGrounded => (0.82 + support_boost).min(0.94),
        GroundingStatus::DigestOnly => 0.45,
        GroundingStatus::InsufficientGrounding => 0.12,
        GroundingStatus::Conflicted => {
            let penalty = (conflict_count.min(3) as f32) * 0.04;
            (0.38 - penalty).max(0.18)
        }
    }
}

async fn digest_wrapper_metadata<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    artifact: &TaskArtifact,
) -> Result<(TrustTier, Vec<GroundingRef>, VerificationHint), McpError> {
    let trust_tier = derive_artifact_trust_tier(artifact);
    let mut grounding_refs = resolve_grounding_refs_by_artifact_ids(
        store,
        tenant_id,
        artifact.project_id.as_option(),
        &artifact.related_artifact_ids,
        12,
    )
    .await?;
    if grounding_refs.is_empty() {
        grounding_refs.push(build_grounding_ref(artifact, None));
    }
    let verification_hint = verification_hint_for_trust_tier(trust_tier);
    Ok((trust_tier, grounding_refs, verification_hint))
}

fn artifact_matches_conflict_scope(
    artifact: &TaskArtifact,
    project_id: Option<&str>,
    task_id: Option<&str>,
    thread_id: Option<&str>,
    support_task_ids: &HashSet<String>,
    support_thread_ids: &HashSet<String>,
) -> bool {
    if let Some(project_id) = project_id {
        if artifact.project_id.as_option() != Some(project_id) {
            return false;
        }
    }
    if let Some(task_id) = task_id {
        return artifact.task_id == task_id;
    }
    if let Some(thread_id) = thread_id {
        return artifact.thread_key() == thread_id;
    }

    support_task_ids.contains(&artifact.task_id)
        || support_thread_ids.contains(artifact.thread_key())
}

async fn persist_verification_artifact<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    params: &ArtifactVerifyParams,
    grounding_status: GroundingStatus,
    confidence: f32,
    supporting_artifacts: &[GroundingRef],
    conflicting_artifacts: &[GroundingRef],
    consulted_digests: &[GroundingRef],
    notes: &[String],
) -> Result<TaskArtifact, McpError> {
    let task_id = params
        .record_task_id
        .clone()
        .or_else(|| params.task_id.clone())
        .or_else(|| supporting_artifacts.first().map(|reference| reference.task_id.clone()))
        .or_else(|| conflicting_artifacts.first().map(|reference| reference.task_id.clone()))
        .ok_or_else(|| {
            McpError::InvalidParams(
                "create_artifact=true requires record_task_id, task_id, or canonically grounded artifacts".to_string(),
            )
        })?;

    let mut artifact = TaskArtifact::new_verification(tenant_id.clone(), task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id.clone());
    // Attribute the verification record to the caller's agent_id when
    // supplied. Without this the artifact is anonymous, and the
    // countersignature promotion in `promote_if_countersigned` cannot
    // elevate it to `VerifiedRecord` — a deliberate safeguard that
    // keeps self-attributed "verifications" from laundering trust.
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.artifact_role = Some("claim_grounding".to_string());
    artifact.summary = Some(format!(
        "Claim grounding status: {}. Claim: {}",
        grounding_status_label(grounding_status),
        params.claim
    ));
    artifact.validation = dedupe_strings(
        supporting_artifacts
            .iter()
            .map(|reference| format!("Supporting artifact: {}", reference.artifact_id))
            .chain(notes.iter().cloned()),
    );
    artifact.what_failed = dedupe_strings(
        conflicting_artifacts
            .iter()
            .map(|reference| format!("Conflicting artifact: {}", reference.artifact_id))
            .chain(match grounding_status {
                GroundingStatus::DigestOnly => Some(
                    "Only digest artifacts were found; no canonical artifact directly grounded the claim.".to_string(),
                ),
                GroundingStatus::InsufficientGrounding => Some(
                    "No canonical artifact directly grounded the claim.".to_string(),
                ),
                _ => None,
            }),
    );
    artifact.related_artifact_ids = dedupe_strings(
        supporting_artifacts
            .iter()
            .map(|reference| reference.artifact_id.clone())
            .chain(
                conflicting_artifacts
                    .iter()
                    .map(|reference| reference.artifact_id.clone()),
            )
            .chain(
                consulted_digests
                    .iter()
                    .map(|reference| reference.artifact_id.clone()),
            ),
    );
    artifact.confidence = Some(confidence);
    artifact.verification_status = Some(grounding_status_label(grounding_status).to_string());
    artifact.thread_id = params
        .thread_id
        .clone()
        .or_else(|| {
            supporting_artifacts
                .first()
                .map(|reference| reference.thread_id.clone())
        })
        .or_else(|| {
            conflicting_artifacts
                .first()
                .map(|reference| reference.thread_id.clone())
        })
        .or_else(|| Some(task_id.clone()));

    finalize_artifact_for_storage(&mut artifact);
    // Match handle_artifact_create: the verification record can only be
    // treated as a VerifiedRecord after a distinct-writer countersignature
    // check; otherwise it stays at `Canonical`.
    promote_if_countersigned(store, &mut artifact).await?;
    let projections = build_task_projections(&artifact);
    store
        .add_task_artifact(artifact.clone(), projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    Ok(artifact)
}

/// Handle artifact.verify tool call.
pub async fn handle_artifact_verify<S: Store>(
    store: &S,
    params: ArtifactVerifyParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if params.claim.trim().is_empty() {
        return Err(McpError::InvalidParams(
            "claim must not be empty".to_string(),
        ));
    }
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }
    if let Some(task_id) = params.task_id.as_deref() {
        validate_identifier("task_id", task_id)?;
    }
    if let Some(thread_id) = params.thread_id.as_deref() {
        validate_identifier("thread_id", thread_id)?;
    }
    if let Some(record_task_id) = params.record_task_id.as_deref() {
        validate_identifier("record_task_id", record_task_id)?;
    }
    for artifact_id in &params.candidate_artifact_ids {
        validate_identifier("candidate_artifact_ids", artifact_id)?;
    }

    let lookup_tenants =
        artifact_lookup_tenants(store, &tenant_id, params.project_id.as_deref()).await?;
    let mut seen_artifacts = HashSet::new();
    let mut explicit_digest_candidate = false;
    let candidate_hits = if params.candidate_artifact_ids.is_empty() {
        let filters = TaskSearchFilters {
            project_id: params.project_id.clone(),
            task_id: params.task_id.clone(),
            thread_id: params.thread_id.clone(),
            ..Default::default()
        };
        search_artifacts_internal(
            store,
            &tenant_id,
            &params.claim,
            params.k,
            &filters,
            QueryMode::Generic,
        )
        .await?
    } else {
        let mut hits = Vec::new();
        for artifact_id in &params.candidate_artifact_ids {
            let Some(artifact) =
                get_artifact_by_id_in_scope(store, &lookup_tenants, artifact_id).await?
            else {
                continue;
            };
            if !seen_artifacts.insert(artifact.artifact_id.clone()) {
                continue;
            }
            if derive_artifact_trust_tier(&artifact) == TrustTier::CompiledDigestHint {
                explicit_digest_candidate = true;
            }
            hits.push(build_artifact_search_hit(
                artifact.clone(),
                artifact_claim_score(&artifact, &params.claim),
                None,
            ));
        }
        hits
    };

    let mut canonical_hits = Vec::new();
    let mut digest_hits = Vec::new();
    for hit in candidate_hits {
        if derive_artifact_trust_tier(&hit.artifact) == TrustTier::CompiledDigestHint {
            digest_hits.push(hit);
        } else {
            canonical_hits.push(hit);
        }
    }

    let mut notes = Vec::new();
    if canonical_hits.is_empty() && !digest_hits.is_empty() {
        let expanded_ids = digest_hits
            .iter()
            .flat_map(|hit| hit.artifact.related_artifact_ids.iter().cloned())
            .collect::<Vec<_>>();
        let expanded_refs = resolve_grounding_refs_by_artifact_ids(
            store,
            &tenant_id,
            params.project_id.as_deref(),
            &expanded_ids,
            params.k.saturating_mul(2),
        )
        .await?;
        if !expanded_refs.is_empty() {
            notes.push(format!(
                "Expanded {} canonical artifact references from digest candidates.",
                expanded_refs.len()
            ));
        }
        for reference in expanded_refs {
            let Some(artifact) =
                get_artifact_by_id_in_scope(store, &lookup_tenants, &reference.artifact_id).await?
            else {
                continue;
            };
            if seen_artifacts.insert(artifact.artifact_id.clone()) {
                canonical_hits.push(build_artifact_search_hit(
                    artifact.clone(),
                    artifact_claim_score(&artifact, &params.claim),
                    None,
                ));
            }
        }
    }

    let supporting_hits = canonical_hits
        .iter()
        .filter(|hit| artifact_supports_claim(&hit.artifact, &params.claim, hit.score))
        .collect::<Vec<_>>();
    let support_task_ids = supporting_hits
        .iter()
        .map(|hit| hit.artifact.task_id.clone())
        .collect::<HashSet<_>>();
    let support_thread_ids = supporting_hits
        .iter()
        .map(|hit| hit.artifact.thread_key().to_string())
        .collect::<HashSet<_>>();

    let conflicting_hits = if supporting_hits.is_empty() {
        Vec::new()
    } else {
        canonical_hits
            .iter()
            .filter(|hit| artifact_has_negative_marker(&hit.artifact))
            .filter(|hit| {
                artifact_matches_conflict_scope(
                    &hit.artifact,
                    params.project_id.as_deref(),
                    params.task_id.as_deref(),
                    params.thread_id.as_deref(),
                    &support_task_ids,
                    &support_thread_ids,
                )
            })
            .collect::<Vec<_>>()
    };

    let supporting_artifacts = dedupe_grounding_refs(
        supporting_hits
            .iter()
            .flat_map(|hit| hit.grounding_refs.clone()),
    );
    let conflicting_artifacts = dedupe_grounding_refs(
        conflicting_hits
            .iter()
            .flat_map(|hit| hit.grounding_refs.clone()),
    );
    let consulted_digests =
        if params.include_digests || supporting_artifacts.is_empty() || explicit_digest_candidate {
            dedupe_grounding_refs(
                digest_hits
                    .iter()
                    .flat_map(|hit| hit.grounding_refs.clone()),
            )
        } else {
            Vec::new()
        };

    if !digest_hits.is_empty() {
        notes.push(
            "Digest artifacts were consulted as compiled hints and not counted as primary evidence."
                .to_string(),
        );
    }
    if !conflicting_artifacts.is_empty() {
        notes.push(
            "Conflict detection is intentionally narrow in v1 and only uses explicit same-scope negative markers."
                .to_string(),
        );
    }

    let grounding_status = if !supporting_artifacts.is_empty() && !conflicting_artifacts.is_empty()
    {
        GroundingStatus::Conflicted
    } else if !supporting_artifacts.is_empty() {
        if supporting_hits
            .iter()
            .any(|hit| derive_artifact_trust_tier(&hit.artifact) == TrustTier::VerifiedRecord)
        {
            GroundingStatus::VerifiedRecord
        } else {
            GroundingStatus::CanonicallyGrounded
        }
    } else if !consulted_digests.is_empty() {
        GroundingStatus::DigestOnly
    } else {
        GroundingStatus::InsufficientGrounding
    };
    let confidence = grounding_confidence(
        grounding_status,
        supporting_artifacts.len(),
        conflicting_artifacts.len(),
    );
    let verification_artifact = if params.create_artifact {
        Some(
            persist_verification_artifact(
                store,
                &tenant_id,
                &params,
                grounding_status,
                confidence,
                &supporting_artifacts,
                &conflicting_artifacts,
                &consulted_digests,
                &notes,
            )
            .await?,
        )
    } else {
        None
    };

    format_mcp_response(&ArtifactVerifyResult {
        claim: params.claim,
        grounding_status,
        confidence,
        supporting_artifacts,
        conflicting_artifacts,
        consulted_digests,
        notes,
        verification_artifact,
    })
}

pub async fn handle_context_brief_project<S: Store>(
    store: &S,
    params: ProjectBriefParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("project_id", &params.project_id)?;
    validate_search_k(params.k)?;

    let (artifact, mut brief) = ensure_project_brief_digest(
        store,
        &tenant_id,
        &params.project_id,
        params.include_related_projects,
    )
    .await?;

    if !params.query.trim().is_empty() {
        sort_ranked_items(&mut brief.recent_failures, &params.query, |item| {
            (item.summary.clone(), item.timestamp_created, false)
        });
        sort_ranked_items(&mut brief.recent_decisions, &params.query, |item| {
            (item.summary.clone(), item.timestamp_created, item.explicit)
        });
        sort_ranked_items(&mut brief.evidence_highlights, &params.query, |item| {
            (item.summary.clone(), item.timestamp_created, false)
        });
        brief.recent_failures.truncate(params.k.min(10));
        brief.recent_decisions.truncate(params.k.min(10));
        brief.evidence_highlights.truncate(params.k.min(10));
    }
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&ProjectBriefResult {
        artifact,
        brief,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_task_resume<S: Store>(
    store: &S,
    params: TaskResumeParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_search_k(params.k)?;

    let (artifact, mut resume) =
        ensure_task_resume_digest(store, &tenant_id, &params.task_id).await?;

    if !params.query.trim().is_empty() {
        sort_ranked_items(&mut resume.recent_runs, &params.query, |item| {
            (
                format!(
                    "{} {} {}",
                    item.tool_name.clone().unwrap_or_default(),
                    item.command.clone().unwrap_or_default(),
                    item.status.clone().unwrap_or_default()
                ),
                item.timestamp_created,
                false,
            )
        });
        resume.recent_runs.truncate(params.k.min(5));
    }
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&TaskResumeResult {
        artifact,
        resume,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_artifact_find_failures<S: Store>(
    store: &S,
    params: ArtifactLibraryParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }

    let (artifact, mut results) =
        ensure_failure_library_digest(store, &tenant_id, params.project_id.as_deref()).await?;
    sort_ranked_items(&mut results, &params.query, |item| {
        (item.summary.clone(), item.timestamp_created, false)
    });
    results.truncate(params.k);
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&FailureSearchResult {
        artifact,
        results,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_artifact_find_decisions<S: Store>(
    store: &S,
    params: ArtifactLibraryParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }

    let (artifact, mut results) =
        ensure_decision_library_digest(store, &tenant_id, params.project_id.as_deref()).await?;
    sort_ranked_items(&mut results, &params.query, |item| {
        (item.summary.clone(), item.timestamp_created, item.explicit)
    });
    results.truncate(params.k);
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&DecisionSearchViewResult {
        artifact,
        results,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_artifact_find_evidence<S: Store>(
    store: &S,
    params: ArtifactLibraryParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }

    let (artifact, mut results) =
        ensure_evidence_library_digest(store, &tenant_id, params.project_id.as_deref()).await?;
    sort_ranked_items(&mut results, &params.query, |item| {
        (item.summary.clone(), item.timestamp_created, false)
    });
    results.truncate(params.k);
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&EvidenceSearchViewResult {
        artifact,
        results,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_artifact_find_highlights<S: Store>(
    store: &S,
    params: ArtifactLibraryParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }

    let (artifact, mut results) =
        ensure_highlight_library_digest(store, &tenant_id, params.project_id.as_deref()).await?;
    sort_highlight_items(&mut results, &params.query);
    results.truncate(params.k);
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&HighlightSearchViewResult {
        artifact,
        results,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

/// Handle memory.get tool call.
///
/// Routes through `Store::get_with_lifecycle` so the caller sees the
/// authoritative lifecycle overlay (status + tier + supersedes edges),
/// then applies `VisibilityPolicy` — Superseded, Expired, and
/// History-tier chunks are hidden by default. When hidden, the response
/// omits the chunk payload and advertises `hidden: true` plus the
/// status/tier so callers can decide whether to retry with an
/// `include_*` flag.
pub async fn handle_memory_get<S: Store>(store: &S, params: GetParams) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;

    debug!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        "memory.get"
    );

    let resolved = match store
        .get_with_lifecycle(&tenant_id, &chunk_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        Some(r) => r,
        None => {
            debug!(chunk_id = %chunk_id, "chunk not found");
            return format_mcp_response(&json!({ "found": false }));
        }
    };

    let policy = VisibilityPolicy {
        include_superseded: params.include_superseded.unwrap_or(false),
        include_expired: params.include_expired.unwrap_or(false),
        include_history: params.include_history.unwrap_or(false),
    };

    // Single consolidation point for the lifecycle visibility rule —
    // `is_visible_at` covers status, tier, and the wall-clock
    // `expires_at_ms` window. B1 (search filter) and C3/C4 (tiering)
    // share this method so the rule never drifts between call sites.
    let now_ms = current_time_ms();
    if !policy.is_visible_at(resolved.status, &resolved.lifecycle, now_ms) {
        // `hidden_reason` tells the caller which `include_*` flag would
        // flip this row visible, so an agent that got `{hidden:true}`
        // can retry with the right knob without having to triangulate
        // from status + tier + expires_at_ms.
        //
        // Precedence MUST mirror `VisibilityPolicy::is_visible_at`
        // exactly, otherwise this discriminator reports a flag that
        // wouldn't actually unhide the row. The policy hides in the
        // order: status → tier → wall-clock expiry. `Deleted` rows
        // never reach this branch because `get_with_lifecycle` filters
        // them upstream; `Error` rows do reach here because the store
        // layer returns them (they are hidden by `is_visible`'s
        // status arm), and we report them as `"error"` — there is no
        // `include_error` knob, but the discriminator still describes
        // the state accurately instead of falling through to a wrong
        // bucket like `"history"`.
        use crate::types::{ChunkStatus, MemoryTier};
        let reason = match resolved.status {
            ChunkStatus::Superseded => "superseded",
            ChunkStatus::Expired => "expired",
            ChunkStatus::Error => "error",
            // At this point the status arm of `is_visible_at` accepted
            // the row, so the hide must be tier-based or clock-based.
            // Check tier first to match the policy's own order.
            _ if resolved.lifecycle.tier == MemoryTier::History => "history",
            _ if resolved
                .lifecycle
                .expires_at_ms
                .is_some_and(|t| t <= now_ms) =>
            {
                "expired"
            }
            // Unreachable: if none of the above, the row would have
            // been visible. Keep a defensive fallback rather than
            // panicking so a future policy change can't take the
            // handler down.
            _ => "unknown",
        };
        info!(
            chunk_id = %chunk_id,
            status = %resolved.status,
            tier = %resolved.lifecycle.tier,
            reason = reason,
            "memory.get hidden by visibility policy"
        );
        return format_mcp_response(&json!({
            "found": true,
            "hidden": true,
            "status": resolved.status.to_string(),
            "tier": resolved.lifecycle.tier.to_string(),
            "hidden_reason": reason,
        }));
    }

    info!(chunk_id = %chunk_id, "chunk found");
    format_mcp_response(&json!({
        "found": true,
        "chunk": resolved.chunk,
        "lifecycle": resolved.lifecycle,
        "status": resolved.status.to_string(),
    }))
}

/// Current wall-clock time in milliseconds since UNIX_EPOCH.
///
/// Local helper for handlers that need a timestamp but do not want to
/// reach into the persistent store layer (which carries its own
/// `current_time_ms`).
fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Handle memory.delete tool call
pub async fn handle_memory_delete<S: Store>(
    store: &S,
    params: DeleteParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;

    info!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        "memory.delete"
    );

    let deleted = store
        .delete(&tenant_id, &chunk_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    if deleted {
        info!(chunk_id = %chunk_id, "chunk deleted");
    } else {
        warn!(chunk_id = %chunk_id, "chunk not found for deletion");
    }

    format_mcp_response(&DeleteResult { deleted })
}

/// Handle memory.feedback tool call
pub async fn handle_memory_feedback<S: Store>(
    store: &S,
    params: FeedbackParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;
    let query = params.query.trim();
    if query.is_empty() {
        return Err(McpError::InvalidParams(
            "query must not be empty".to_string(),
        ));
    }
    let relevance = parse_relevance_label(&params.relevance)?;

    let chunk = store
        .get(&tenant_id, &chunk_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    if chunk.is_none() {
        return Err(McpError::InvalidParams(
            "chunk_id not found for tenant".to_string(),
        ));
    }

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let feedback = FeedbackEntry::new(
        tenant_id,
        query.to_string(),
        chunk_id,
        relevance,
        timestamp_ms,
    );
    store
        .add_feedback(feedback)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&FeedbackResult { stored: true })
}

/// Handle memory.stats tool call
pub async fn handle_memory_stats<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: StatsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(tenant_id = %tenant_id, "memory.stats");

    let store_stats: StoreStats = store
        .stats(&tenant_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Get disk stats if tenant_manager is available
    let disk_stats = tenant_manager
        .map(|tm| {
            tm.tenant_disk_stats(&tenant_id)
                .ok()
                .map(|ds| DiskStatsResult {
                    total_bytes: ds.total_bytes,
                    segment_count: ds.segment_count,
                })
        })
        .flatten();

    // Get compaction metrics if available
    let compaction = store
        .get_compaction_metrics(&tenant_id)
        .ok()
        .map(|m| CompactionStatsResult {
            tombstone_ratio: m.tombstone_ratio,
            active_chunks: m.active_chunks,
            deleted_chunks: m.deleted_chunks,
            segment_count: m.segment_count,
            hnsw_staleness: m.hnsw_staleness,
            hnsw_cache_size: m.hnsw_cache_size,
            hnsw_index_size: m.hnsw_index_size,
            needs_compaction: m.tombstone_ratio > 0.20
                || m.segment_count > 10
                || m.hnsw_staleness > 0.15,
        });

    format_mcp_response(&StatsResult {
        total_chunks: store_stats.total_chunks,
        deleted_chunks: store_stats.deleted_chunks,
        chunk_types: store_stats.chunk_types,
        disk_stats,
        compaction,
    })
}

/// Handle memory.metrics tool call
pub fn handle_memory_metrics(
    metrics: &MetricsCollector,
    index_stats: HashMap<String, IndexStats>,
    params: MetricsParams,
) -> Result<Value, McpError> {
    info!(
        tenant_id = ?params.tenant_id,
        include_recent = params.include_recent,
        include_tiered = params.include_tiered,
        "memory.metrics"
    );

    // Filter index stats by tenant if specified. `memory.metrics`
    // intentionally keeps strict semantics here: if the caller passed a
    // tenant_id, it must parse — we do NOT fall back to the default so
    // an empty string doesn't silently show all tenants.
    let filtered_stats = if let Some(ref tenant_id_str) = params.tenant_id {
        let tenant_id = validate_tenant_id(tenant_id_str)?;
        index_stats
            .into_iter()
            .filter(|(k, _)| k == tenant_id.as_str())
            .collect()
    } else {
        index_stats
    };

    let mut snapshot = metrics.snapshot(filtered_stats);

    if !params.include_recent {
        snapshot.recent_queries.clear();
    }

    // Clear tiered stats if not requested
    if !params.include_tiered {
        snapshot.tiered = Default::default();
    }

    format_mcp_response(&snapshot)
}

/// Handle memory.compact tool call
pub async fn handle_memory_compact<S: Store>(
    store: &S,
    params: CompactParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        force = params.force,
        project_id = ?params.project_id,
        force_digest_rebuild = params.force_digest_rebuild,
        "memory.compact"
    );

    let digest_modes = params.digest_modes.clone().unwrap_or_default();
    let should_rebuild_digests = params.force_digest_rebuild || !digest_modes.is_empty();

    // Phase 3.4: before checking thresholds, drain the writer-side
    // dirty tracker and regenerate any digests that were flagged by
    // `task.add_evidence` / `task.finish` / `artifact.create`. This
    // gives operators a knob — `memory.compact` — to actually action
    // the writer-driven invalidations without also paying the cost of
    // a full storage compaction. Any explicit `digest_modes` or
    // `force_digest_rebuild` below still runs as before.
    let dirty_digests_swept = sweep_dirty_digests(store).await;
    if dirty_digests_swept > 0 {
        debug!(
            swept = dirty_digests_swept,
            "Phase 3.4: regenerated dirty digests flagged by writer paths"
        );
    }

    if params.force {
        // Force compaction regardless of thresholds
        let result = store
            .run_compaction(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;

        info!(
            tenant_id = %tenant_id,
            tombstones = result.tombstones_processed,
            hnsw_rebuilt = result.hnsw_rebuild.is_some(),
            segments_merged = result.segment_merge.is_some(),
            cache_invalidated = result.cache_entries_invalidated,
            duration_ms = result.duration.as_millis(),
            "compaction completed (forced)"
        );

        let digest_artifacts = if should_rebuild_digests {
            rebuild_requested_digests(
                store,
                &tenant_id,
                params.project_id.as_deref(),
                &digest_modes,
            )
            .await?
        } else {
            Vec::new()
        };

        return format_mcp_response(&json!({
            "status": "completed",
            "tombstones_processed": result.tombstones_processed,
            "hnsw_rebuild": result.hnsw_rebuild.map(|r| json!({
                "embeddings_processed": r.embeddings_processed,
                "embeddings_included": r.embeddings_included,
                "embeddings_excluded": r.embeddings_excluded,
                "duration_ms": r.duration.as_millis()
            })),
            "segment_merge": result.segment_merge.map(|r| json!({
                "segments_before": r.segments_before,
                "segments_after": r.segments_after,
                "segments_merged": r.segments_merged,
                "duration_ms": r.duration.as_millis()
                })),
                "cache_entries_invalidated": result.cache_entries_invalidated,
                "duration_ms": result.duration.as_millis(),
                "digest_artifacts": digest_artifacts
        }));
    }

    // Check thresholds first
    match store.run_compaction_if_needed(&tenant_id) {
        Ok(Some(result)) => {
            info!(
                tenant_id = %tenant_id,
                tombstones = result.tombstones_processed,
                hnsw_rebuilt = result.hnsw_rebuild.is_some(),
                segments_merged = result.segment_merge.is_some(),
                cache_invalidated = result.cache_entries_invalidated,
                duration_ms = result.duration.as_millis(),
                "compaction completed"
            );

            let digest_artifacts = if should_rebuild_digests {
                rebuild_requested_digests(
                    store,
                    &tenant_id,
                    params.project_id.as_deref(),
                    &digest_modes,
                )
                .await?
            } else {
                Vec::new()
            };

            format_mcp_response(&json!({
                "status": "completed",
                "tombstones_processed": result.tombstones_processed,
                "hnsw_rebuild": result.hnsw_rebuild.map(|r| json!({
                    "embeddings_processed": r.embeddings_processed,
                    "embeddings_included": r.embeddings_included,
                    "embeddings_excluded": r.embeddings_excluded,
                    "duration_ms": r.duration.as_millis()
                })),
                "segment_merge": result.segment_merge.map(|r| json!({
                    "segments_before": r.segments_before,
                    "segments_after": r.segments_after,
                    "segments_merged": r.segments_merged,
                    "duration_ms": r.duration.as_millis()
                })),
                "cache_entries_invalidated": result.cache_entries_invalidated,
                "duration_ms": result.duration.as_millis(),
                "digest_artifacts": digest_artifacts
            }))
        }
        Ok(None) => {
            debug!(tenant_id = %tenant_id, "compaction skipped - thresholds not exceeded");

            let digest_artifacts = if should_rebuild_digests {
                rebuild_requested_digests(
                    store,
                    &tenant_id,
                    params.project_id.as_deref(),
                    &digest_modes,
                )
                .await?
            } else {
                Vec::new()
            };

            format_mcp_response(&json!({
                "status": if digest_artifacts.is_empty() { "skipped" } else { "completed" },
                "reason": if digest_artifacts.is_empty() { "No compaction needed - all thresholds below limits" } else { "Storage compaction skipped; digests refreshed" },
                "digest_artifacts": digest_artifacts
            }))
        }
        Err(e) => Err(McpError::ToolError(e.to_string())),
    }
}

/// Handle memory.consolidate_episode tool call
pub async fn handle_memory_consolidate_episode<S: Store>(
    store: &S,
    params: ConsolidateEpisodeParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_episode_id(&params.episode_id)?;

    if params.max_chunks == 0 {
        return Err(McpError::InvalidParams(
            "max_chunks must be greater than 0".to_string(),
        ));
    }

    let mut episode_chunks =
        collect_episode_chunks(store, &tenant_id, &params.episode_id, params.max_chunks).await?;
    if episode_chunks.is_empty() {
        return Err(McpError::ToolError(format!(
            "no chunks found for episode '{}'",
            params.episode_id
        )));
    }

    episode_chunks.sort_by_key(|chunk| chunk.timestamp_created);
    let summary_text = build_episode_summary_text(&params.episode_id, &episode_chunks);
    let tags = vec![
        make_episode_tag(&params.episode_id),
        "episode_summary:true".to_string(),
        format!("episode_source_chunks:{}", episode_chunks.len()),
    ];

    let summary_chunk = MemoryChunk::new(tenant_id.clone(), summary_text, ChunkType::Summary)
        .with_tags(tags)
        .with_source(Source::from_tool(
            "memory.consolidate_episode",
            Option::<String>::None,
        ));
    let summary_chunk_id = store
        .add(summary_chunk)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    if !params.retain_source_chunks {
        for chunk in &episode_chunks {
            let _ = store
                .delete(&tenant_id, &chunk.chunk_id)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
        }
    }

    format_mcp_response(&ConsolidateEpisodeResult {
        summary_chunk_id: summary_chunk_id.to_string(),
        source_chunk_count: episode_chunks.len(),
        retained_source_chunks: params.retain_source_chunks,
    })
}

/// Handle context.list_subsystems tool call
pub async fn handle_context_list_subsystems<S: Store>(
    store: &S,
    params: ContextListSubsystemsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(500);
    let prefix = params
        .prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    info!(tenant_id = %tenant_id, prefix = ?prefix, limit = limit, "context.list_subsystems");

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut summaries: HashMap<String, (usize, HashSet<String>)> = HashMap::new();

    for chunk in chunks {
        let subsystems = tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX);
        for subsystem in subsystems {
            if let Some(prefix) = prefix {
                if !subsystem.starts_with(prefix) {
                    continue;
                }
            }

            let entry = summaries.entry(subsystem).or_insert((0, HashSet::new()));
            entry.0 += 1;

            if let Some(path) = chunk.source.path.as_deref() {
                entry.1.insert(path.to_string());
            }
            for file_tag in tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX) {
                entry.1.insert(file_tag);
            }
        }
    }

    let mut subsystem_summaries: Vec<SubsystemSummary> = summaries
        .into_iter()
        .map(|(key, (chunk_count, files))| SubsystemSummary {
            key,
            chunk_count,
            file_count: files.len(),
        })
        .collect();
    subsystem_summaries.sort_by(|a, b| a.key.cmp(&b.key));
    subsystem_summaries.truncate(limit);

    format_mcp_response(&ContextListSubsystemsResult {
        subsystems: subsystem_summaries,
    })
}

/// Handle context.get_files_for_subsystem tool call
pub async fn handle_context_get_files_for_subsystem<S: Store>(
    store: &S,
    params: ContextGetFilesForSubsystemParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let subsystem_key = params.subsystem_key.trim();
    if subsystem_key.is_empty() {
        return Err(McpError::InvalidParams(
            "subsystem_key must not be empty".to_string(),
        ));
    }
    let limit = params.limit.min(2_000);

    info!(tenant_id = %tenant_id, subsystem_key = subsystem_key, limit = limit, "context.get_files_for_subsystem");

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut files = HashSet::new();

    for chunk in chunks {
        if !chunk_matches_subsystem(&chunk, subsystem_key) {
            continue;
        }

        if let Some(path) = chunk.source.path.as_deref() {
            files.insert(path.to_string());
        }
        for file_tag in tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX) {
            files.insert(file_tag);
        }
    }

    let mut files: Vec<String> = files.into_iter().collect();
    files.sort();
    files.truncate(limit);

    format_mcp_response(&ContextGetFilesForSubsystemResult {
        subsystem_key: subsystem_key.to_string(),
        files,
    })
}

/// Handle context.search_context_documents tool call.
///
/// Phase 2.4 consolidation: operators should prefer
/// `memory.search` with `mode = "generic"` plus tag filters for new
/// integrations. `context.search_context_documents` still offers a
/// context-doc-specific return shape and remains supported for
/// existing callers, but we emit a deprecation-style log each call so
/// the usage is visible in telemetry.
pub async fn handle_context_search_documents<S: Store>(
    store: &S,
    params: ContextSearchDocumentsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    warn!(
        tool = "context.search_context_documents",
        "deprecated: prefer memory.search with tag filters / mode"
    );

    let tier = params
        .tier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(tier) = tier {
        if tier != "hot" && tier != "cold" {
            return Err(McpError::InvalidParams(
                "tier must be one of: hot, cold".to_string(),
            ));
        }
    }

    let subsystem_key = params
        .subsystem_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_filters = subsystem_key.is_some() || tier.is_some();
    let fetch_k = adaptive_fetch_k(params.k, &params.query, has_filters);

    info!(
        tenant_id = %tenant_id,
        query = %params.query,
        k = params.k,
        fetch_k = fetch_k,
        subsystem_key = ?subsystem_key,
        tier = ?tier,
        "context.search_context_documents"
    );

    // TODO(B1): apply VisibilityPolicy via apply_visibility_filter to hide
    // superseded/expired/history chunks. memory.get (A8) enforces this at
    // the point-lookup; the search path still leaks non-active content
    // until Track B1 wires the overlay into search_with_scores.
    let scored_chunks = store
        .search_with_scores(&tenant_id, &params.query, fetch_k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    let mut filtered = Vec::new();
    for (chunk, score) in scored_chunks {
        if !is_context_chunk(&chunk) {
            continue;
        }
        if let Some(subsystem_key) = subsystem_key {
            if !chunk_matches_subsystem(&chunk, subsystem_key) {
                continue;
            }
        }
        if !chunk_matches_tier(&chunk, tier) {
            continue;
        }

        let source_tier = if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            Some("hot".to_string())
        } else if has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD) {
            Some("cold".to_string())
        } else {
            None
        };
        filtered.push(chunk_to_result(&chunk, score, source_tier, None));
        if filtered.len() >= params.k {
            break;
        }
    }

    format_mcp_response(&ContextSearchDocumentsResult { results: filtered })
}

/// Handle context.find_relevant_context tool call
pub async fn handle_context_find_relevant_context<S: Store>(
    store: &S,
    params: ContextFindRelevantContextParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    let subsystem_keys: Vec<String> = params
        .subsystem_keys
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let has_filters = !subsystem_keys.is_empty();
    let fetch_k = adaptive_fetch_k(params.k, &params.task, has_filters);

    info!(
        tenant_id = %tenant_id,
        task = %params.task,
        k = params.k,
        include_hot = params.include_hot,
        subsystem_keys = subsystem_keys.len(),
        fetch_k = fetch_k,
        "context.find_relevant_context"
    );

    let mut dedupe = HashSet::new();
    let mut results = Vec::new();
    let mut hot_included = false;

    if params.include_hot {
        let mut hot_chunks = collect_all_chunks(store, &tenant_id, 20_000).await?;
        hot_chunks.retain(|chunk| {
            has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT)
                && chunk_matches_any_subsystem(chunk, &subsystem_keys)
        });
        hot_chunks.sort_by_key(|chunk| std::cmp::Reverse(chunk.timestamp_created));

        for chunk in hot_chunks.into_iter().take(params.k.min(5)) {
            let id = chunk.chunk_id.to_string();
            if dedupe.insert(id) {
                hot_included = true;
                results.push(chunk_to_result(&chunk, 1.0, Some("hot".to_string()), None));
            }
        }
    }

    // TODO(B1): apply VisibilityPolicy via apply_visibility_filter to hide
    // superseded/expired/history chunks. memory.get (A8) enforces this at
    // the point-lookup; the search path still leaks non-active content
    // until Track B1 wires the overlay into search_with_scores.
    let scored_chunks = store
        .search_with_scores(&tenant_id, &params.task, fetch_k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    for (chunk, score) in scored_chunks {
        if !is_context_chunk(&chunk) {
            continue;
        }
        if !params.include_hot && has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            continue;
        }
        if !chunk_matches_any_subsystem(&chunk, &subsystem_keys) {
            continue;
        }

        let id = chunk.chunk_id.to_string();
        if !dedupe.insert(id) {
            continue;
        }

        let source_tier = if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            Some("hot".to_string())
        } else if has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD) {
            Some("cold".to_string())
        } else {
            None
        };
        results.push(chunk_to_result(&chunk, score, source_tier, None));
        if results.len() >= params.k {
            break;
        }
    }

    format_mcp_response(&ContextFindRelevantContextResult {
        results,
        hot_included,
    })
}

/// Handle context.suggest_agent tool call
pub async fn handle_context_suggest_agent<S: Store>(
    store: &S,
    params: ContextSuggestAgentParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    let changed_files: Vec<String> = params
        .changed_files
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();

    info!(
        tenant_id = %tenant_id,
        task = %params.task,
        changed_files = changed_files.len(),
        k = params.k,
        "context.suggest_agent"
    );

    #[derive(Default)]
    struct AgentScore {
        score: f32,
        reasons: HashSet<String>,
        matched_triggers: HashSet<String>,
    }

    let task_lower = params.task.to_ascii_lowercase();
    let task_tokens: Vec<String> = params
        .task
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() >= 3)
        .map(|token| token.to_ascii_lowercase())
        .collect();

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut scores: HashMap<String, AgentScore> = HashMap::new();

    for chunk in chunks {
        let agent_names = tag_values(&chunk.tags, TAG_CTX_AGENT_PREFIX);
        if agent_names.is_empty() {
            continue;
        }

        let chunk_text = chunk.text.to_ascii_lowercase();
        let triggers = tag_values(&chunk.tags, TAG_CTX_TRIGGER_PREFIX);
        let subsystem_tags = tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX);
        let file_tags = tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX);

        for agent_name in agent_names {
            let mut score = 0.1f32;
            let mut reasons = HashSet::new();
            let mut matched_triggers = HashSet::new();

            let lexical_hits = task_tokens
                .iter()
                .filter(|token| chunk_text.contains(token.as_str()))
                .count();
            if lexical_hits > 0 {
                score += lexical_hits as f32 * 0.03;
                reasons.insert(format!("keyword_overlap:{}", lexical_hits));
            }

            if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
                score += 0.05;
                reasons.insert("hot_tier_profile".to_string());
            }

            for subsystem in &subsystem_tags {
                if task_lower.contains(&subsystem.to_ascii_lowercase()) {
                    score += 0.15;
                    reasons.insert(format!("subsystem_match:{}", subsystem));
                }
            }

            for trigger in &triggers {
                for changed_file in &changed_files {
                    if wildcard_match(trigger, changed_file) {
                        score += 0.6;
                        matched_triggers.insert(format!("{} -> {}", trigger, changed_file));
                    }
                }
            }

            for file_tag in &file_tags {
                for changed_file in &changed_files {
                    if wildcard_match(file_tag, changed_file)
                        || changed_file.contains(file_tag)
                        || file_tag.contains(changed_file)
                    {
                        score += 0.2;
                        reasons.insert(format!("file_match:{}", file_tag));
                    }
                }
            }

            let entry = scores.entry(agent_name).or_default();
            if score > entry.score {
                entry.score = score;
            }
            entry.reasons.extend(reasons);
            entry.matched_triggers.extend(matched_triggers);
        }
    }

    let considered_agents = scores.len();
    let mut recommendations: Vec<AgentSuggestion> = scores
        .into_iter()
        .map(|(agent_name, score)| {
            let mut reasons: Vec<String> = score.reasons.into_iter().collect();
            reasons.sort();
            let mut matched_triggers: Vec<String> = score.matched_triggers.into_iter().collect();
            matched_triggers.sort();

            AgentSuggestion {
                agent_name,
                score: score.score,
                reasons,
                matched_triggers,
            }
        })
        .collect();

    recommendations.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.agent_name.cmp(&b.agent_name))
    });
    recommendations.truncate(params.k);

    format_mcp_response(&ContextSuggestAgentResult {
        recommendations,
        considered_agents,
    })
}

/// Handle context.get_hot_context tool call
pub async fn handle_context_get_hot_context<S: Store>(
    store: &S,
    params: ContextGetHotContextParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    info!(tenant_id = %tenant_id, k = params.k, "context.get_hot_context");

    let mut chunks = collect_all_chunks(store, &tenant_id, 20_000).await?;
    chunks.retain(|chunk| has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT));
    chunks.sort_by_key(|chunk| std::cmp::Reverse(chunk.timestamp_created));

    let results: Vec<ChunkResult> = chunks
        .iter()
        .take(params.k)
        .map(|chunk| chunk_to_result(chunk, 1.0, Some("hot".to_string()), None))
        .collect();

    format_mcp_response(&ContextGetHotContextResult { results })
}

// ---------- Structural Query Handlers ----------

use crate::structural::{CallerInfo, ImportInfo, SymbolLocation, SymbolQueryService};

/// Handle code.find_definition tool call
pub fn handle_find_definition(
    query_service: &SymbolQueryService,
    params: FindDefinitionParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        "code.find_definition"
    );

    let locations = query_service
        .find_symbol_definition(&tenant_id, &params.name, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = locations.len(), "find_definition completed");

    let definitions: Vec<SymbolLocationResult> = locations
        .into_iter()
        .map(symbol_location_to_result)
        .collect();

    format_mcp_response(&FindDefinitionResult { definitions })
}

/// Handle code.find_references tool call
pub fn handle_find_references(
    query_service: &SymbolQueryService,
    params: FindReferencesParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        "code.find_references"
    );

    let locations = query_service
        .find_references(&tenant_id, &params.name, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = locations.len(), "find_references completed");

    let references: Vec<SymbolLocationResult> = locations
        .into_iter()
        .map(symbol_location_to_result)
        .collect();

    format_mcp_response(&FindReferencesResult { references })
}

/// Handle code.find_callers tool call
pub fn handle_find_callers(
    query_service: &SymbolQueryService,
    params: FindCallersParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    // Clamp depth to 1-3
    let depth = params.depth.clamp(1, 3);

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        depth = depth,
        "code.find_callers"
    );

    let caller_infos = query_service
        .find_callers(
            &tenant_id,
            &params.name,
            depth,
            params.project_id.as_deref(),
        )
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = caller_infos.len(), "find_callers completed");

    let callers: Vec<CallerInfoResult> = caller_infos
        .into_iter()
        .map(caller_info_to_result)
        .collect();

    format_mcp_response(&FindCallersResult { callers })
}

/// Handle code.find_imports tool call
pub fn handle_find_imports(
    query_service: &SymbolQueryService,
    params: FindImportsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        module = %params.module,
        "code.find_imports"
    );

    let import_infos = query_service
        .find_imports(&tenant_id, &params.module, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = import_infos.len(), "find_imports completed");

    let imports: Vec<ImportInfoResult> = import_infos
        .into_iter()
        .map(import_info_to_result)
        .collect();

    format_mcp_response(&FindImportsResult { imports })
}

/// Convert SymbolLocation to result type
fn symbol_location_to_result(loc: SymbolLocation) -> SymbolLocationResult {
    SymbolLocationResult {
        file_path: loc.file_path,
        name: loc.name,
        kind: loc.kind.as_str().to_string(),
        line_start: loc.line_start,
        line_end: loc.line_end,
        col_start: loc.col_start,
        col_end: loc.col_end,
        signature: loc.signature,
        docstring: loc.docstring,
        visibility: loc.visibility,
        language: loc.language,
    }
}

/// Convert CallerInfo to result type
fn caller_info_to_result(info: CallerInfo) -> CallerInfoResult {
    CallerInfoResult {
        caller_name: info.caller_name,
        caller_file: info.caller_file,
        call_line: info.call_line,
        call_col: info.call_col,
        caller_kind: info.caller_kind.as_str().to_string(),
        depth: info.depth,
    }
}

/// Convert ImportInfo to result type
fn import_info_to_result(info: ImportInfo) -> ImportInfoResult {
    ImportInfoResult {
        importing_file: info.importing_file,
        import_line: info.import_line,
        alias: info.alias,
    }
}

// ---------- Trace Query Handlers ----------

use crate::structural::{
    parse_iso_datetime, ErrorResult, FrameInfo, TimeRange as StructuralTimeRange, ToolCallResult,
    TraceQueryService,
};

/// Result type for debug.find_tool_calls
#[derive(Debug, Serialize, Deserialize)]
pub struct FindToolCallsResult {
    pub tool_calls: Vec<ToolCallResult>,
    pub total_count: usize,
}

/// Result type for debug.find_errors
#[derive(Debug, Serialize, Deserialize)]
pub struct FindErrorsResult {
    pub errors: Vec<ErrorResultResponse>,
    pub total_count: usize,
}

/// Error result with optional frames
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResultResponse {
    pub trace_id: i64,
    pub error_signature: String,
    pub error_message: String,
    pub timestamp_ms: i64,
    pub timestamp_formatted: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<FrameInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Convert ErrorResult to response, optionally including frames
fn error_to_response(error: ErrorResult, include_frames: bool) -> ErrorResultResponse {
    ErrorResultResponse {
        trace_id: error.trace_id,
        error_signature: error.error_signature,
        error_message: error.error_message,
        timestamp_ms: error.timestamp_ms,
        timestamp_formatted: error.timestamp_formatted,
        frames: if include_frames {
            Some(error.frames)
        } else {
            None
        },
        session_id: error.session_id,
    }
}

/// Parse time range from optional ISO 8601 strings
fn parse_trace_time_range(
    time_from: Option<&str>,
    time_to: Option<&str>,
) -> Result<Option<StructuralTimeRange>, McpError> {
    let from_ms = match time_from {
        Some(s) => Some(parse_iso_datetime(s).map_err(|e| McpError::InvalidParams(e.to_string()))?),
        None => None,
    };
    let to_ms = match time_to {
        Some(s) => Some(parse_iso_datetime(s).map_err(|e| McpError::InvalidParams(e.to_string()))?),
        None => None,
    };

    if from_ms.is_none() && to_ms.is_none() {
        Ok(None)
    } else {
        Ok(Some(StructuralTimeRange { from_ms, to_ms }))
    }
}

/// Handle debug.find_tool_calls tool call
pub fn handle_find_tool_calls(
    trace_service: &TraceQueryService,
    params: FindToolCallsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(100);

    // Parse time range
    let time_range =
        parse_trace_time_range(params.time_from.as_deref(), params.time_to.as_deref())?;

    info!(
        tenant_id = %tenant_id,
        tool_name = ?params.tool_name,
        session_id = ?params.session_id,
        errors_only = params.errors_only,
        limit = limit,
        "debug.find_tool_calls"
    );

    let tool_calls = if params.errors_only {
        trace_service
            .find_tool_calls_with_errors(&tenant_id, time_range)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        trace_service
            .find_tool_calls(
                &tenant_id,
                params.tool_name.as_deref(),
                time_range,
                params.session_id.as_deref(),
                limit,
            )
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    debug!(
        results_count = tool_calls.len(),
        "find_tool_calls completed"
    );

    let total_count = tool_calls.len();
    format_mcp_response(&FindToolCallsResult {
        tool_calls,
        total_count,
    })
}

/// Handle debug.find_errors tool call
pub fn handle_find_errors(
    trace_service: &TraceQueryService,
    params: FindErrorsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(100);

    // Parse time range
    let time_range =
        parse_trace_time_range(params.time_from.as_deref(), params.time_to.as_deref())?;

    info!(
        tenant_id = %tenant_id,
        error_signature = ?params.error_signature,
        function_name = ?params.function_name,
        file_path = ?params.file_path,
        limit = limit,
        "debug.find_errors"
    );

    let error_results = trace_service
        .find_errors(
            &tenant_id,
            params.error_signature.as_deref(),
            params.function_name.as_deref(),
            params.file_path.as_deref(),
            time_range,
            limit,
        )
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = error_results.len(), "find_errors completed");

    let total_count = error_results.len();
    let errors: Vec<ErrorResultResponse> = error_results
        .into_iter()
        .map(|e| error_to_response(e, params.include_frames))
        .collect();

    format_mcp_response(&FindErrorsResult {
        errors,
        total_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemoryStore, Store};
    use proptest::prelude::*;
    use serde::de::DeserializeOwned;
    use std::sync::{Mutex, MutexGuard};

    /// Serialize tests that flip the process-global
    /// `ALLOW_CROSS_TENANT_PROJECT_FALLBACK` atomic. Without this, parallel
    /// tests would interleave writes to the flag and observe each other's
    /// state.
    static FALLBACK_FLAG_MUTEX: Mutex<()> = Mutex::new(());

    fn with_fallback_flag<'a>() -> MutexGuard<'a, ()> {
        FALLBACK_FLAG_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn make_store() -> MemoryStore {
        MemoryStore::new()
    }

    fn parse_tool_payload<T: DeserializeOwned>(result: &Value) -> T {
        let text = result["content"][0]["text"]
            .as_str()
            .expect("tool response should include JSON text");
        serde_json::from_str(text).expect("tool response text should parse as JSON payload")
    }

    #[tokio::test]
    async fn search_empty_store() {
        let store = make_store();
        let params = SearchParams {
            tenant_id: "test".to_string(),
            query: "hello".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
        };

        let result = handle_memory_search(&store, params).await.unwrap();
        assert!(result["content"].is_array());

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_result: SearchResult = serde_json::from_str(text).unwrap();
        assert!(search_result.results.is_empty());
    }

    #[tokio::test]
    async fn search_rejects_k_zero() {
        let store = make_store();
        let params = SearchParams {
            tenant_id: "test".to_string(),
            query: "hello".to_string(),
            project_id: None,
            k: 0,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
        };

        let result = handle_memory_search(&store, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn search_rejects_k_above_max() {
        let store = make_store();
        let params = SearchParams {
            tenant_id: "test".to_string(),
            query: "hello".to_string(),
            project_id: None,
            k: 101,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
        };

        let result = handle_memory_search(&store, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    proptest! {
        #[test]
        fn validate_search_k_property(k in 0usize..=200usize) {
            let result = validate_search_k(k);
            if (1..=100).contains(&k) {
                prop_assert!(result.is_ok());
            } else {
                prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
            }
        }
    }

    #[test]
    fn adaptive_fetch_k_expands_for_complex_queries() {
        let query = "this is a very long and complex search query with many tokens";
        assert_eq!(adaptive_fetch_k(10, query, false), 20);
        assert_eq!(adaptive_fetch_k(10, query, true), 100);
        assert_eq!(adaptive_fetch_k(10, "short query", false), 10);
    }

    #[test]
    fn normalize_query_for_repair_rewrites_noise() {
        let repaired = normalize_query_for_repair("Alpha!unique?marker").unwrap();
        assert_eq!(repaired, "alpha unique marker");
        assert!(normalize_query_for_repair("clean query").is_none());
    }

    proptest! {
        #[test]
        fn validate_search_time_range_order_property(day_a in 1u8..=28, day_b in 1u8..=28) {
            let filters = SearchFilters {
                types: None,
                episode_id: None,
                time_range: Some(TimeRange {
                    from: Some(format!("2026-01-{day_a:02}T00:00:00Z")),
                    to: Some(format!("2026-01-{day_b:02}T23:59:59Z")),
                }),
            };

            let result = validate_search_time_range(Some(&filters));
            if day_a <= day_b {
                prop_assert!(result.is_ok());
            } else {
                prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
            }
        }
    }

    proptest! {
        #[test]
        fn validate_search_time_range_rejects_invalid_iso(invalid in "[A-Za-z]{1,16}") {
            let filters = SearchFilters {
                types: None,
                episode_id: None,
                time_range: Some(TimeRange {
                    from: Some(invalid),
                    to: Some("2026-01-01T00:00:00Z".to_string()),
                }),
            };

            let result = validate_search_time_range(Some(&filters));
            prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
        }
    }

    #[tokio::test]
    async fn add_and_search() {
        let store = make_store();

        // Add a chunk
        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "hello world".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,
        
        mode: None,
        supersede_near_duplicates: None,
        };

        let add_result = handle_memory_add(&store, None, add_params).await.unwrap();
        let text = add_result["content"][0]["text"].as_str().unwrap();
        let add_response: AddResult = serde_json::from_str(text).unwrap();
        assert!(!add_response.chunk_id.is_empty());

        // Search for it
        let search_params = SearchParams {
            tenant_id: "test".to_string(),
            query: "hello".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
        };

        let search_result = handle_memory_search(&store, search_params).await.unwrap();
        let text = search_result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(search_response.results[0].text, "hello world");
    }

    #[tokio::test]
    async fn search_filters_by_project_id() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "project a chunk".to_string(),
                chunk_type: "doc".to_string(),
                project_id: Some("project_a".to_string()),
                episode_id: None,
                source: None,
                tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "project b chunk".to_string(),
                chunk_type: "doc".to_string(),
                project_id: Some("project_b".to_string()),
                episode_id: None,
                source: None,
                tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "chunk".to_string(),
                project_id: Some("project_a".to_string()),
                k: 10,
                filters: None,
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(search_response.results[0].text, "project a chunk");
    }

    #[tokio::test]
    async fn search_filters_by_types() {
        let store = make_store();

        for (text, chunk_type) in [
            ("doc chunk", "doc"),
            ("code chunk", "code"),
            ("trace chunk", "trace"),
        ] {
            handle_memory_add(
                &store,
                None,
                AddParams {
                    tenant_id: "test".to_string(),
                    text: text.to_string(),
                    chunk_type: chunk_type.to_string(),
                    project_id: None,
                    episode_id: None,
                    source: None,
                    tags: vec![],
                expires_at_ms: None,
                review_after_ms: None,
                
                mode: None,
                supersede_near_duplicates: None,
                },
            )
            .await
            .unwrap();
        }

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "chunk".to_string(),
                project_id: None,
                k: 10,
                filters: Some(SearchFilters {
                    types: Some(vec!["code".to_string(), "doc".to_string()]),
                    episode_id: None,
                    time_range: None,
                }),
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 2);
        assert!(search_response
            .results
            .iter()
            .all(|r| matches!(r.chunk_type.as_str(), "doc" | "code")));
    }

    #[tokio::test]
    async fn search_filters_by_time_range() {
        let store = make_store();
        let tenant_id = TenantId::new("test").unwrap();

        let mut old_chunk = MemoryChunk::new(tenant_id.clone(), "old chunk", ChunkType::Doc);
        old_chunk.timestamp_created =
            crate::structural::parse_iso_datetime("2026-01-01T00:00:00Z").unwrap();
        store.add(old_chunk).await.unwrap();

        let mut middle_chunk = MemoryChunk::new(tenant_id.clone(), "middle chunk", ChunkType::Doc);
        middle_chunk.timestamp_created =
            crate::structural::parse_iso_datetime("2026-01-15T12:00:00Z").unwrap();
        store.add(middle_chunk).await.unwrap();

        let mut new_chunk = MemoryChunk::new(tenant_id, "new chunk", ChunkType::Doc);
        new_chunk.timestamp_created =
            crate::structural::parse_iso_datetime("2026-02-01T00:00:00Z").unwrap();
        store.add(new_chunk).await.unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "chunk".to_string(),
                project_id: None,
                k: 10,
                filters: Some(SearchFilters {
                    types: None,
                    episode_id: None,
                    time_range: Some(TimeRange {
                        from: Some("2026-01-10T00:00:00Z".to_string()),
                        to: Some("2026-01-20T23:59:59Z".to_string()),
                    }),
                }),
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(search_response.results[0].text, "middle chunk");
    }

    #[tokio::test]
    async fn search_filters_by_episode_id() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "episode alpha".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: Some("ep1".to_string()),
                source: None,
                tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "episode beta".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: Some("ep2".to_string()),
                source: None,
                tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "episode".to_string(),
                project_id: None,
                k: 10,
                filters: Some(SearchFilters {
                    types: None,
                    episode_id: Some("ep1".to_string()),
                    time_range: None,
                }),
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(
            search_response.results[0].episode_id.as_deref(),
            Some("ep1")
        );
    }

    #[tokio::test]
    async fn search_returns_citation_with_provenance_and_offsets() {
        let store = make_store();

        let long_text = format!(
            "alpha_unique_marker {}",
            "lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(80)
        );

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: long_text,
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    uri: Some("file:///tmp/test_doc.md".to_string()),
                    repo: Some("acme/repo".to_string()),
                    commit: Some("abc123".to_string()),
                    path: Some("docs/test_doc.md".to_string()),
                    tool_name: Some("ingest".to_string()),
                    tool_call_id: Some("call-1".to_string()),
                }),
                tags: vec![],
                expires_at_ms: None,
                review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "alpha_unique_marker".to_string(),
                project_id: None,
                k: 10,
                filters: None,
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert!(!search_response.results.is_empty());

        let citation = search_response.results[0]
            .citation
            .as_ref()
            .expect("citation should be present");

        assert!(!citation.citation_id.is_empty());
        assert!(!citation.content_hash.is_empty());
        assert_eq!(citation.source_path.as_deref(), Some("docs/test_doc.md"));
        assert_eq!(citation.source_tool_name.as_deref(), Some("ingest"));
        assert!(citation.chunk_index.is_some());
        assert!(citation.total_chunks.is_some());
        assert!(citation.char_start.is_some());
        assert!(citation.char_end.is_some());
    }

    #[tokio::test]
    async fn search_repair_loop_recovers_result() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "alpha unique marker".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "alpha!unique?marker".to_string(),
                project_id: None,
                k: 5,
                filters: None,
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(search_response.results[0].text, "alpha unique marker");

        let repair_info = search_response
            .repair_info
            .as_ref()
            .expect("repair_info should be present");
        assert!(repair_info.attempted);
        assert!(repair_info.repaired);
        assert_eq!(
            repair_info.repaired_query.as_deref(),
            Some("alpha unique marker")
        );
    }

    #[tokio::test]
    async fn add_with_all_fields() {
        let store = make_store();

        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "function hello() {}".to_string(),
            chunk_type: "code".to_string(),
            project_id: Some("my_project".to_string()),
            episode_id: None,
            source: Some(SourceParams {
                path: Some("src/main.rs".to_string()),
                repo: Some("my-repo".to_string()),
                ..Default::default()
            }),
            tags: vec!["rust".to_string(), "function".to_string()],
            expires_at_ms: None,
            review_after_ms: None,
        
        mode: None,
        supersede_near_duplicates: None,
        };

        let result = handle_memory_add(&store, None, add_params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let response: AddResult = serde_json::from_str(text).unwrap();

        // Verify the chunk was stored correctly
        let get_params = GetParams {
            tenant_id: "test".to_string(),
            chunk_id: response.chunk_id.clone(),
            include_superseded: None,
            include_expired: None,
            include_history: None,
        };

        let get_result = handle_memory_get(&store, get_params).await.unwrap();
        let text = get_result["content"][0]["text"].as_str().unwrap();
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["found"].as_bool(), Some(true));
        let chunk: MemoryChunk = serde_json::from_value(body["chunk"].clone()).unwrap();

        assert_eq!(chunk.text, "function hello() {}");
        assert_eq!(chunk.chunk_type, ChunkType::Code);
        assert_eq!(chunk.source.path, Some("src/main.rs".to_string()));
        assert_eq!(chunk.tags, vec!["rust", "function"]);
    }

    #[tokio::test]
    async fn add_batch() {
        let store = make_store();

        let params = AddBatchParams {
            tenant_id: "test".to_string(),
            supersede_near_duplicates: None,
            chunks: vec![
                BatchChunkParams {
                    text: "chunk 1".to_string(),
                    chunk_type: "doc".to_string(),
                    project_id: None,
                    episode_id: None,
                    source: None,
                    tags: vec![],
                expires_at_ms: None,
                review_after_ms: None,
                
                mode: None,
                },
                BatchChunkParams {
                    text: "chunk 2".to_string(),
                    chunk_type: "code".to_string(),
                    project_id: None,
                    episode_id: None,
                    source: None,
                    tags: vec![],
                expires_at_ms: None,
                review_after_ms: None,

                mode: None,
                },
            ],
        };

        let result = handle_memory_add_batch(&store, None, params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let response: AddBatchResult = serde_json::from_str(text).unwrap();
        assert_eq!(response.chunk_ids.len(), 2);
    }

    #[tokio::test]
    async fn delete_chunk() {
        let store = make_store();

        // Add a chunk
        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "to be deleted".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,
        
        mode: None,
        supersede_near_duplicates: None,
        };

        let add_result = handle_memory_add(&store, None, add_params).await.unwrap();
        let text = add_result["content"][0]["text"].as_str().unwrap();
        let add_response: AddResult = serde_json::from_str(text).unwrap();

        // Delete it
        let delete_params = DeleteParams {
            tenant_id: "test".to_string(),
            chunk_id: add_response.chunk_id.clone(),
        };

        let delete_result = handle_memory_delete(&store, delete_params).await.unwrap();
        let text = delete_result["content"][0]["text"].as_str().unwrap();
        let delete_response: DeleteResult = serde_json::from_str(text).unwrap();
        assert!(delete_response.deleted);

        // Verify it's no longer retrievable
        let get_params = GetParams {
            tenant_id: "test".to_string(),
            chunk_id: add_response.chunk_id,
            include_superseded: None,
            include_expired: None,
            include_history: None,
        };

        let get_result = handle_memory_get(&store, get_params).await.unwrap();
        let text = get_result["content"][0]["text"].as_str().unwrap();
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            body["found"].as_bool(),
            Some(false),
            "deleted chunk must surface as found=false via memory.get"
        );
    }

    #[tokio::test]
    async fn feedback_records_relevance_event() {
        let store = make_store();

        let add_result = handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "feedback target chunk".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();
        let add_text = add_result["content"][0]["text"].as_str().unwrap();
        let add_payload: AddResult = serde_json::from_str(add_text).unwrap();

        let feedback_result = handle_memory_feedback(
            &store,
            FeedbackParams {
                tenant_id: "test".to_string(),
                query: "feedback target".to_string(),
                chunk_id: add_payload.chunk_id,
                relevance: "relevant".to_string(),
            },
        )
        .await
        .unwrap();

        let text = feedback_result["content"][0]["text"].as_str().unwrap();
        let payload: FeedbackResult = serde_json::from_str(text).unwrap();
        assert!(payload.stored);
    }

    #[tokio::test]
    async fn stats() {
        let store = make_store();

        // Add some chunks
        for i in 0..3 {
            let add_params = AddParams {
                tenant_id: "test".to_string(),
                text: format!("doc {}", i),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![],
                expires_at_ms: None,
                review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            };
            handle_memory_add(&store, None, add_params).await.unwrap();
        }

        let params = StatsParams {
            tenant_id: "test".to_string(),
        };

        let result = handle_memory_stats(&store, None, params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let stats: StatsResult = serde_json::from_str(text).unwrap();

        assert_eq!(stats.total_chunks, 3);
        assert_eq!(stats.deleted_chunks, 0);
        assert_eq!(stats.chunk_types.get("doc"), Some(&3));
    }

    #[tokio::test]
    async fn invalid_tenant_id() {
        let store = make_store();

        let params = SearchParams {
            tenant_id: "invalid-tenant".to_string(), // hyphens not allowed
            query: "test".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
        };

        let result = handle_memory_search(&store, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn invalid_chunk_type() {
        let store = make_store();

        let params = AddParams {
            tenant_id: "test".to_string(),
            text: "hello".to_string(),
            chunk_type: "invalid_type".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,
        
        mode: None,
        supersede_near_duplicates: None,
        };

        let result = handle_memory_add(&store, None, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn invalid_chunk_id() {
        let store = make_store();

        let params = GetParams {
            tenant_id: "test".to_string(),
            chunk_id: "not-a-uuid".to_string(),
            include_superseded: None,
            include_expired: None,
            include_history: None,
        };

        let result = handle_memory_get(&store, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn tenant_isolation() {
        let store = make_store();

        // Add chunk as tenant A
        let add_params = AddParams {
            tenant_id: "tenant_a".to_string(),
            text: "secret data".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,
        
        mode: None,
        supersede_near_duplicates: None,
        };

        handle_memory_add(&store, None, add_params).await.unwrap();

        // Search as tenant B - should return empty
        let search_params = SearchParams {
            tenant_id: "tenant_b".to_string(),
            query: "secret".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
        };

        let result = handle_memory_search(&store, search_params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert!(search_response.results.is_empty());
    }

    #[tokio::test]
    async fn search_with_debug_tiers() {
        let store = make_store();

        // Add a chunk
        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "debug tier test".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,
        
        mode: None,
        supersede_near_duplicates: None,
        };

        handle_memory_add(&store, None, add_params).await.unwrap();

        // Search with debug_tiers enabled
        let search_params = SearchParams {
            tenant_id: "test".to_string(),
            query: "debug".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: Some(true),
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
        };

        let result = handle_memory_search(&store, search_params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();

        // MemoryStore doesn't have tiered support, so tier_info should be None
        // and source_tier on results should be None (since timing is None)
        assert_eq!(search_response.results.len(), 1);
        assert!(search_response.tier_info.is_none());
    }

    #[tokio::test]
    async fn context_list_subsystems_groups_by_subsystem_tag() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "retrieval planning doc".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    path: Some("src/retrieval/mod.rs".to_string()),
                    ..Default::default()
                }),
                tags: vec![
                    "ctx:doc".to_string(),
                    "ctx:subsystem:retrieval".to_string(),
                    "ctx:file:src/retrieval/mod.rs".to_string(),
                ],
            
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "retrieval indexing notes".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    path: Some("src/retrieval/index.rs".to_string()),
                    ..Default::default()
                }),
                tags: vec![
                    "ctx:doc".to_string(),
                    "ctx:subsystem:retrieval".to_string(),
                    "ctx:file:src/retrieval/index.rs".to_string(),
                ],
            
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "planner decision".to_string(),
                chunk_type: "decision".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    path: Some("src/planner/mod.rs".to_string()),
                    ..Default::default()
                }),
                tags: vec![
                    "ctx:doc".to_string(),
                    "ctx:subsystem:planner".to_string(),
                    "ctx:file:src/planner/mod.rs".to_string(),
                ],
            
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        let result = handle_context_list_subsystems(
            &store,
            ContextListSubsystemsParams {
                tenant_id: "test".to_string(),
                prefix: None,
                limit: 50,
            },
        )
        .await
        .unwrap();

        let payload: ContextListSubsystemsResult = parse_tool_payload(&result);
        assert_eq!(payload.subsystems.len(), 2);

        let retrieval = payload
            .subsystems
            .iter()
            .find(|entry| entry.key == "retrieval")
            .expect("retrieval subsystem should exist");
        assert_eq!(retrieval.chunk_count, 2);
        assert_eq!(retrieval.file_count, 2);
    }

    #[tokio::test]
    async fn context_get_files_for_subsystem_returns_tag_and_source_paths() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "storage architecture".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    path: Some("crates/memd/src/store/mod.rs".to_string()),
                    ..Default::default()
                }),
                tags: vec![
                    "ctx:subsystem:storage".to_string(),
                    "ctx:file:crates/memd/src/store/hybrid.rs".to_string(),
                ],
            
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        let result = handle_context_get_files_for_subsystem(
            &store,
            ContextGetFilesForSubsystemParams {
                tenant_id: "test".to_string(),
                subsystem_key: "storage".to_string(),
                limit: 10,
            },
        )
        .await
        .unwrap();

        let payload: ContextGetFilesForSubsystemResult = parse_tool_payload(&result);
        assert_eq!(payload.subsystem_key, "storage");
        assert_eq!(payload.files.len(), 2);
        assert!(payload
            .files
            .contains(&"crates/memd/src/store/mod.rs".to_string()));
        assert!(payload
            .files
            .contains(&"crates/memd/src/store/hybrid.rs".to_string()));
    }

    #[tokio::test]
    async fn context_search_documents_filters_by_tier_and_subsystem() {
        let store = make_store();

        for (text, tier_tag) in [
            ("hot retrieval context", "ctx:tier:hot"),
            ("cold retrieval context", "ctx:tier:cold"),
        ] {
            handle_memory_add(
                &store,
                None,
                AddParams {
                    tenant_id: "test".to_string(),
                    text: text.to_string(),
                    chunk_type: "doc".to_string(),
                    project_id: None,
                    episode_id: None,
                    source: None,
                    tags: vec![
                        "ctx:doc".to_string(),
                        "ctx:subsystem:retrieval".to_string(),
                        tier_tag.to_string(),
                    ],
                
                expires_at_ms: None,
                review_after_ms: None,
                
                mode: None,
                supersede_near_duplicates: None,
                },
            )
            .await
            .unwrap();
        }

        let result = handle_context_search_documents(
            &store,
            ContextSearchDocumentsParams {
                tenant_id: "test".to_string(),
                query: "retrieval".to_string(),
                k: 10,
                subsystem_key: Some("retrieval".to_string()),
                tier: Some("hot".to_string()),
            },
        )
        .await
        .unwrap();

        let payload: ContextSearchDocumentsResult = parse_tool_payload(&result);
        assert_eq!(payload.results.len(), 1);
        assert_eq!(payload.results[0].text, "hot retrieval context");
        assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
    }

    #[tokio::test]
    async fn context_find_relevant_context_can_prepend_hot_chunks() {
        let store = make_store();
        let tenant = TenantId::new("test").unwrap();

        let mut hot = MemoryChunk::new(tenant.clone(), "incident runbook", ChunkType::Doc);
        hot.tags = vec!["ctx:tier:hot".to_string(), "ctx:subsystem:ops".to_string()];
        hot.timestamp_created = 10;
        store.add(hot).await.unwrap();

        let mut relevant = MemoryChunk::new(
            tenant,
            "database migration checklist for ops",
            ChunkType::Doc,
        );
        relevant.tags = vec![
            "ctx:doc".to_string(),
            "ctx:subsystem:ops".to_string(),
            "ctx:tier:cold".to_string(),
        ];
        relevant.timestamp_created = 5;
        store.add(relevant).await.unwrap();

        let result = handle_context_find_relevant_context(
            &store,
            ContextFindRelevantContextParams {
                tenant_id: "test".to_string(),
                task: "database migration".to_string(),
                k: 5,
                subsystem_keys: Some(vec!["ops".to_string()]),
                include_hot: true,
            },
        )
        .await
        .unwrap();

        let payload: ContextFindRelevantContextResult = parse_tool_payload(&result);
        assert!(payload.hot_included);
        assert!(!payload.results.is_empty());
        assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
    }

    #[tokio::test]
    async fn context_suggest_agent_uses_trigger_and_file_matches() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "storage compaction and WAL tuning playbook".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![
                    "ctx:agent:storage-specialist".to_string(),
                    "ctx:trigger:crates/memd/src/store/*".to_string(),
                    "ctx:subsystem:storage".to_string(),
                    "ctx:file:crates/memd/src/store/hybrid.rs".to_string(),
                    "ctx:tier:hot".to_string(),
                ],
            
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        let result = handle_context_suggest_agent(
            &store,
            ContextSuggestAgentParams {
                tenant_id: "test".to_string(),
                task: "Improve storage compaction behavior".to_string(),
                changed_files: Some(vec!["crates/memd/src/store/hybrid.rs".to_string()]),
                k: 3,
            },
        )
        .await
        .unwrap();

        let payload: ContextSuggestAgentResult = parse_tool_payload(&result);
        assert!(!payload.recommendations.is_empty());
        assert_eq!(
            payload.recommendations[0].agent_name,
            "storage-specialist".to_string()
        );
        assert!(!payload.recommendations[0].matched_triggers.is_empty());
    }

    #[tokio::test]
    async fn context_get_hot_context_returns_most_recent_chunks() {
        let store = make_store();
        let tenant = TenantId::new("test").unwrap();

        let mut older = MemoryChunk::new(tenant.clone(), "older hot context", ChunkType::Doc);
        older.tags = vec!["ctx:tier:hot".to_string()];
        older.timestamp_created = 1;
        store.add(older).await.unwrap();

        let mut newest = MemoryChunk::new(tenant, "newest hot context", ChunkType::Doc);
        newest.tags = vec!["ctx:tier:hot".to_string()];
        newest.timestamp_created = 2;
        store.add(newest).await.unwrap();

        let result = handle_context_get_hot_context(
            &store,
            ContextGetHotContextParams {
                tenant_id: "test".to_string(),
                k: 1,
            },
        )
        .await
        .unwrap();

        let payload: ContextGetHotContextResult = parse_tool_payload(&result);
        assert_eq!(payload.results.len(), 1);
        assert_eq!(payload.results[0].text, "newest hot context");
        assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
    }

    #[tokio::test]
    async fn task_get_returns_full_artifact_history() {
        let store = make_store();

        let start: TaskArtifactResult = parse_tool_payload(
            &handle_task_start(
                &store,
                None,
                TaskStartParams {
                    tenant_id: "test".to_string(),
                    project_id: Some("proj_alpha".to_string()),
                    parent_task_id: None,
                    agent_id: Some("agent-1".to_string()),
                    session_id: Some("session-7".to_string()),
                    goal: "Quantify the stress-response regulon".to_string(),
                    motivation: "The regulator mechanism is unresolved".to_string(),
                    hypothesis: "Sigma factor S drives the induced genes".to_string(),
                    scientific_question: "Which genes increase after the perturbation?".to_string(),
                    dataset_refs: vec![TaskDatasetRefParams {
                        name: "rna_seq".to_string(),
                        version: Some("v1".to_string()),
                        description: None,
                    }],
                    expected_outputs: vec!["differential expression table".to_string()],
                    entity_refs: vec![],
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        handle_task_progress(
            &store,
            None,
            TaskProgressParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                summary: "Initial QC exposed one low-depth replicate".to_string(),
                blockers: vec!["One replicate is borderline".to_string()],
                failed_attempts: vec!["Default trimming removed too much signal".to_string()],
                next_step: "Re-run with stricter QC but lighter trimming".to_string(),
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        handle_task_run_start(
            &store,
            None,
            TaskRunStartParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                tool_name: "mmseqs".to_string(),
                tool_version: Some("15".to_string()),
                command: "mmseqs search db query out tmp".to_string(),
                why_chosen: "Fast enough for iterative parameter sweeps".to_string(),
                parameters: json!({"sensitivity": 7.5}),
                inputs: vec!["query.faa".to_string()],
                summary: Some("Homology search for candidate regulators".to_string()),
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        handle_task_run_finish(
            &store,
            None,
            TaskRunFinishParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                status: "completed".to_string(),
                tool_name: Some("mmseqs".to_string()),
                tool_version: Some("15".to_string()),
                command: Some("mmseqs search db query out tmp".to_string()),
                outputs: vec!["hits.tsv".to_string()],
                metrics: Some(json!({"top_hit_bitscore": 310.5})),
                notes: "Recovered a strong candidate regulator".to_string(),
                validation: vec!["Top hit was stable across reruns".to_string()],
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        handle_task_add_evidence(
            &store,
            None,
            TaskAddEvidenceParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                summary: "Top hit exceeded the curated threshold".to_string(),
                evidence_kind: "metric".to_string(),
                supports_claim: Some(true),
                metric_name: Some("top_hit_bitscore".to_string()),
                metric_value: Some(json!(310.5)),
                metrics: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_task_get(
            &store,
            TaskGetParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id,
            },
        )
        .await
        .unwrap();

        let payload: TaskGetResult = parse_tool_payload(&result);
        assert_eq!(payload.artifacts.len(), 5);
        assert!(payload
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_kind == ArtifactKind::TaskStart));
        assert!(payload
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_kind == ArtifactKind::Evidence));
    }

    #[tokio::test]
    async fn task_search_filters_exactly_by_tool_and_dataset() {
        let store = make_store();

        let task_a: TaskArtifactResult = parse_tool_payload(
            &handle_task_start(
                &store,
                None,
                TaskStartParams {
                    tenant_id: "test".to_string(),
                    project_id: Some("proj_alpha".to_string()),
                    parent_task_id: None,
                    agent_id: None,
                    session_id: None,
                    goal: "Task A goal".to_string(),
                    motivation: "Task A motivation".to_string(),
                    hypothesis: "Task A hypothesis".to_string(),
                    scientific_question: "Task A question".to_string(),
                    dataset_refs: vec![TaskDatasetRefParams {
                        name: "rna_seq".to_string(),
                        version: Some("v1".to_string()),
                        description: None,
                    }],
                    expected_outputs: vec!["table".to_string()],
                    entity_refs: vec![],
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        handle_task_run_start(
            &store,
            None,
            TaskRunStartParams {
                tenant_id: "test".to_string(),
                task_id: task_a.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                tool_name: "mmseqs".to_string(),
                tool_version: None,
                command: "mmseqs search db query out tmp".to_string(),
                why_chosen: "Fast iterative search".to_string(),
                parameters: json!({"sensitivity": 7.5}),
                inputs: vec!["query.faa".to_string()],
                summary: Some("Candidate search".to_string()),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "rna_seq".to_string(),
                    version: Some("v1".to_string()),
                    description: None,
                }],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        let task_b: TaskArtifactResult = parse_tool_payload(
            &handle_task_start(
                &store,
                None,
                TaskStartParams {
                    tenant_id: "test".to_string(),
                    project_id: Some("proj_beta".to_string()),
                    parent_task_id: None,
                    agent_id: None,
                    session_id: None,
                    goal: "Task B goal".to_string(),
                    motivation: "Task B motivation".to_string(),
                    hypothesis: "Task B hypothesis".to_string(),
                    scientific_question: "Task B question".to_string(),
                    dataset_refs: vec![TaskDatasetRefParams {
                        name: "proteomics".to_string(),
                        version: Some("v2".to_string()),
                        description: None,
                    }],
                    expected_outputs: vec!["summary".to_string()],
                    entity_refs: vec![],
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        handle_task_run_start(
            &store,
            None,
            TaskRunStartParams {
                tenant_id: "test".to_string(),
                task_id: task_b.task_id,
                project_id: Some("proj_beta".to_string()),
                agent_id: None,
                session_id: None,
                tool_name: "blast".to_string(),
                tool_version: None,
                command: "blastp -query q -db db".to_string(),
                why_chosen: "Reference comparison".to_string(),
                parameters: json!({"evalue": 1e-5}),
                inputs: vec!["query.faa".to_string()],
                summary: Some("Candidate search".to_string()),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "proteomics".to_string(),
                    version: Some("v2".to_string()),
                    description: None,
                }],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_task_search(
            &store,
            TaskSearchParams {
                tenant_id: "test".to_string(),
                query: "parameter sweeps".to_string(),
                k: 10,
                filters: Some(TaskSearchFiltersParams {
                    task_id: Some(task_a.task_id),
                    artifact_kind: Some("run_start".to_string()),
                    status: Some("started".to_string()),
                    challenge_id: None,
                    thread_id: None,
                    reply_to_artifact_id: None,
                    artifact_role: None,
                    dataset_name: Some("rna_seq".to_string()),
                    dataset_version: Some("v1".to_string()),
                    entity_name: None,
                    entity_type: None,
                    tool_name: Some("mmseqs".to_string()),
                    project_id: Some("proj_alpha".to_string()),
                    agent_id: None,
                    session_id: None,
                    requested_action: None,
                    verification_status: None,
                    relation_kind: None,
                }),
                mode: None,
            },
        )
        .await
        .unwrap();

        let payload: SearchResult = parse_tool_payload(&result);
        assert_eq!(payload.results.len(), 1);
        assert!(payload.results[0]
            .tags
            .iter()
            .any(|tag| tag.starts_with("task:kind:run_start")));
    }

    #[tokio::test]
    async fn task_search_project_scope_spans_tenants() {
        let _flag_guard = with_fallback_flag();
        let store = make_store();

        // This test exercises the LEGACY cross-tenant project fallback,
        // which became opt-in in v0.3.1 (see the tenant-isolation
        // regression test above). Flip the flag on for this scenario only
        // and restore the default at the end so sibling tests stay
        // isolated.
        set_cross_tenant_project_fallback(true);

        let start: TaskArtifactResult = parse_tool_payload(
            &handle_task_start(
                &store,
                None,
                TaskStartParams {
                    tenant_id: "default".to_string(),
                    project_id: Some("advanced_benchmark".to_string()),
                    parent_task_id: None,
                    agent_id: None,
                    session_id: None,
                    goal: "Record benchmark continuity".to_string(),
                    motivation: "Later agents should recover this task across tenant aliases"
                        .to_string(),
                    hypothesis: "Project-scoped retrieval should bridge tenant mismatch"
                        .to_string(),
                    scientific_question: "Can task search recover cross-tenant project history?"
                        .to_string(),
                    dataset_refs: vec![],
                    expected_outputs: vec!["handoff".to_string()],
                    entity_refs: vec![],
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        handle_task_progress(
            &store,
            None,
            TaskProgressParams {
                tenant_id: "default".to_string(),
                task_id: start.task_id,
                project_id: Some("advanced_benchmark".to_string()),
                agent_id: None,
                session_id: None,
                summary: "Recovered prior benchmark context".to_string(),
                blockers: vec![],
                failed_attempts: vec![],
                next_step: "Continue strict reproduction".to_string(),
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_task_search(
            &store,
            TaskSearchParams {
                tenant_id: "benchmark".to_string(),
                query: "benchmark continuity".to_string(),
                k: 5,
                filters: Some(TaskSearchFiltersParams {
                    project_id: Some("advanced_benchmark".to_string()),
                    ..Default::default()
                }),
                mode: None,
            },
        )
        .await
        .unwrap();

        let payload: SearchResult = parse_tool_payload(&result);
        assert!(!payload.results.is_empty());
        let artifact = payload.results[0]
            .artifact
            .clone()
            .expect("artifact should be attached");
        assert_eq!(artifact.tenant_id.as_str(), "default");
        assert_eq!(artifact.project_id.as_option(), Some("advanced_benchmark"));

        set_cross_tenant_project_fallback(false);
    }

    #[tokio::test]
    async fn memory_search_project_scope_spans_tenants_for_raw_chunks() {
        let _flag_guard = with_fallback_flag();
        // Same legacy-fallback scenario as task_search_project_scope_spans_tenants:
        // the widening is opt-in in v0.3.1+ and must be enabled here.
        set_cross_tenant_project_fallback(true);

        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "default".to_string(),
                text: "strict reproduction blocker for advanced benchmark".to_string(),
                chunk_type: "summary".to_string(),
                project_id: Some("advanced_benchmark".to_string()),
                episode_id: None,
                source: None,
                tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,
            
            mode: None,
            supersede_near_duplicates: None,
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "benchmark".to_string(),
                query: "strict reproduction blocker".to_string(),
                project_id: Some("advanced_benchmark".to_string()),
                k: 5,
                filters: None,
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();

        let payload: SearchResult = parse_tool_payload(&result);
        assert!(!payload.results.is_empty());
        assert!(payload.results[0]
            .text
            .contains("strict reproduction blocker"));

        set_cross_tenant_project_fallback(false);
    }

    #[tokio::test]
    async fn artifact_create_get_search_and_thread_flow() {
        let store = make_store();

        let start: TaskArtifactResult = parse_tool_payload(
            &handle_task_start(
                &store,
                None,
                TaskStartParams {
                    tenant_id: "test".to_string(),
                    project_id: Some("shared_proto".to_string()),
                    parent_task_id: None,
                    agent_id: Some("planner-1".to_string()),
                    session_id: Some("session-a".to_string()),
                    goal: "Coordinate a shared artifact thread".to_string(),
                    motivation: "Multiple agents should reuse and critique the same record"
                        .to_string(),
                    hypothesis: "Artifact-native collaboration reduces duplicated work".to_string(),
                    scientific_question: "How should critique flow through the shared thread?"
                        .to_string(),
                    dataset_refs: vec![TaskDatasetRefParams {
                        name: "repo_snapshot".to_string(),
                        version: Some("head".to_string()),
                        description: None,
                    }],
                    expected_outputs: vec!["thread seed".to_string()],
                    entity_refs: vec![],
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        let review: TaskArtifactResult = parse_tool_payload(
            &handle_artifact_create(
                &store,
                None,
                ArtifactCreateParams {
                    tenant_id: "test".to_string(),
                    artifact_kind: "review".to_string(),
                    task_id: Some(start.task_id.clone()),
                    project_id: Some("shared_proto".to_string()),
                    parent_task_id: None,
                    agent_id: Some("reviewer-1".to_string()),
                    session_id: Some("session-b".to_string()),
                    status: None,
                    artifact_role: Some("critique".to_string()),
                    challenge_id: Some("artifact_protocol".to_string()),
                    thread_id: None,
                    reply_to_artifact_id: Some(start.artifact_id.clone()),
                    relation_kind: None,
                    goal: None,
                    motivation: None,
                    hypothesis: None,
                    scientific_question: None,
                    method_summary: None,
                    summary: Some(
                        "Need a clearer review and verification path for artifacts".to_string(),
                    ),
                    content: None,
                    evidence_kind: None,
                    supports_claim: None,
                    blockers: vec![],
                    what_worked: vec![],
                    what_failed: vec!["Search still centers projection chunks".to_string()],
                    validation: vec![],
                    uncertainty: vec![
                        "Exact artifact exchange semantics are still thin".to_string()
                    ],
                    followups: vec!["Add artifact.search and thread inspection".to_string()],
                    expected_outputs: vec![],
                    related_artifact_ids: vec![],
                    contributors: vec![TaskContributorParams {
                        contributor_id: "pi".to_string(),
                        display_name: Some("Principal Investigator".to_string()),
                        role: Some("human_scientist".to_string()),
                        contribution: Some("Requested critique of the seed artifact".to_string()),
                    }],
                    dataset_refs: vec![],
                    entity_refs: vec![],
                    tool_name: Some("artifact.create".to_string()),
                    tool_version: None,
                    command: None,
                    parameters: None,
                    inputs: vec![],
                    outputs: vec![],
                    metrics: None,
                    why_chosen: None,
                    confidence: Some(0.74),
                    requested_action: Some("review".to_string()),
                    verification_status: Some("pending".to_string()),
                    compute_budget: None,
                    cost_actual: None,
                    data_access_level: Some("local_private".to_string()),
                    policy_tags: vec!["prototype".to_string()],
                    allowed_tools: vec!["task.search".to_string(), "artifact.search".to_string()],
                    approval_state: Some("not_required".to_string()),
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        let get_payload: ArtifactGetResult = parse_tool_payload(
            &handle_artifact_get(
                &store,
                ArtifactGetParams {
                    tenant_id: "test".to_string(),
                    artifact_id: review.artifact_id.clone(),
                },
            )
            .await
            .unwrap(),
        );
        let review_artifact = get_payload.artifact.expect("artifact should exist");
        assert_eq!(
            review_artifact.challenge_id.as_deref(),
            Some("artifact_protocol")
        );
        assert_eq!(
            review_artifact.reply_to_artifact_id.as_deref(),
            Some(start.artifact_id.as_str())
        );
        assert_eq!(review_artifact.requested_action.as_deref(), Some("review"));
        assert_eq!(
            review_artifact.verification_status.as_deref(),
            Some("pending")
        );
        assert_eq!(review_artifact.thread_key(), start.task_id.as_str());
        assert_eq!(review_artifact.contributors.len(), 1);

        let thread_payload: ArtifactThreadResult = parse_tool_payload(
            &handle_artifact_list_thread(
                &store,
                ArtifactListThreadParams {
                    tenant_id: "test".to_string(),
                    thread_id: None,
                    artifact_id: Some(review.artifact_id.clone()),
                },
            )
            .await
            .unwrap(),
        );
        assert_eq!(thread_payload.thread_id, start.task_id);
        assert_eq!(thread_payload.artifacts.len(), 2);

        let search_payload: ArtifactSearchResult = parse_tool_payload(
            &handle_artifact_search(
                &store,
                TaskSearchParams {
                    tenant_id: "test".to_string(),
                    query: "clearer review path".to_string(),
                    k: 5,
                    filters: Some(TaskSearchFiltersParams {
                        task_id: None,
                        artifact_kind: Some("review".to_string()),
                        status: None,
                        challenge_id: Some("artifact_protocol".to_string()),
                        thread_id: None,
                        reply_to_artifact_id: Some(start.artifact_id.clone()),
                        artifact_role: Some("critique".to_string()),
                        dataset_name: None,
                        dataset_version: None,
                        entity_name: None,
                        entity_type: None,
                        tool_name: None,
                        project_id: Some("shared_proto".to_string()),
                        agent_id: None,
                        session_id: None,
                        requested_action: Some("review".to_string()),
                        verification_status: Some("pending".to_string()),
                        relation_kind: Some("reviews".to_string()),
                    }),
                    mode: None,
                },
            )
            .await
            .unwrap(),
        );
        assert_eq!(search_payload.results.len(), 1);
        assert_eq!(
            search_payload.results[0].artifact.artifact_id,
            review.artifact_id
        );
        assert_eq!(
            search_payload.results[0].trust_tier,
            TrustTier::CanonicalRecord
        );
        assert!(!search_payload.results[0].grounding_refs.is_empty());
        assert!(
            !search_payload.results[0]
                .verification_hint
                .requires_verification
        );

        let task_search_payload: SearchResult = parse_tool_payload(
            &handle_task_search(
                &store,
                TaskSearchParams {
                    tenant_id: "test".to_string(),
                    query: "clearer review path".to_string(),
                    k: 5,
                    filters: Some(TaskSearchFiltersParams {
                        task_id: Some(thread_payload.thread_id),
                        artifact_kind: Some("review".to_string()),
                        status: None,
                        challenge_id: Some("artifact_protocol".to_string()),
                        thread_id: None,
                        reply_to_artifact_id: Some(start.artifact_id),
                        artifact_role: Some("critique".to_string()),
                        dataset_name: None,
                        dataset_version: None,
                        entity_name: None,
                        entity_type: None,
                        tool_name: None,
                        project_id: Some("shared_proto".to_string()),
                        agent_id: None,
                        session_id: None,
                        requested_action: Some("review".to_string()),
                        verification_status: Some("pending".to_string()),
                        relation_kind: Some("reviews".to_string()),
                    }),
                    mode: None,
                },
            )
            .await
            .unwrap(),
        );
        assert!(!task_search_payload.results.is_empty());
        assert!(task_search_payload
            .results
            .iter()
            .all(|result| result.artifact.is_some()));
        assert_eq!(
            task_search_payload.results.iter().find_map(|result| {
                result
                    .artifact
                    .as_ref()
                    .and_then(|artifact| artifact.artifact_role.as_deref())
            }),
            Some("critique")
        );
    }

    #[tokio::test]
    async fn task_start_stores_canonical_artifact_and_projection_chunks() {
        let store = make_store();

        let result = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "test".to_string(),
                project_id: Some("proj_alpha".to_string()),
                parent_task_id: None,
                agent_id: Some("agent-1".to_string()),
                session_id: Some("session-7".to_string()),
                goal: "Quantify the stress-response regulon".to_string(),
                motivation: "The regulator mechanism is unresolved".to_string(),
                hypothesis: "Sigma factor S drives the induced genes".to_string(),
                scientific_question: "Which genes increase after the perturbation?".to_string(),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "rna_seq".to_string(),
                    version: Some("v1".to_string()),
                    description: None,
                }],
                expected_outputs: vec!["differential expression table".to_string()],
                entity_refs: vec![TaskEntityRefParams {
                    name: "RpoS".to_string(),
                    entity_type: "protein".to_string(),
                    role: Some("candidate regulator".to_string()),
                }],
                provenance: Some(TaskProvenanceParams {
                    tool_name: Some("codex".to_string()),
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();

        let payload: TaskArtifactResult = parse_tool_payload(&result);
        assert!(!payload.artifact_id.is_empty());
        assert!(!payload.task_id.is_empty());
        assert!(!payload.projection_chunk_ids.is_empty());

        let tenant = TenantId::new("test").unwrap();
        let stored = store
            .get_task_artifact(&tenant, &payload.artifact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.goal.as_deref(),
            Some("Quantify the stress-response regulon")
        );
        assert_eq!(stored.dataset_refs.len(), 1);

        let search = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "stress-response regulon".to_string(),
                project_id: Some("proj_alpha".to_string()),
                k: 10,
                filters: None,
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();
        let search_payload: SearchResult = parse_tool_payload(&search);
        assert!(!search_payload.results.is_empty());
        assert!(search_payload.results.iter().any(|result| {
            result
                .tags
                .iter()
                .any(|tag| tag.starts_with("task:kind:task_start"))
        }));
        assert!(search_payload
            .results
            .iter()
            .any(|result| result.trust_tier == TrustTier::CanonicalRecord));
        assert!(search_payload
            .results
            .iter()
            .any(|result| !result.grounding_refs.is_empty()));
    }

    #[tokio::test]
    async fn task_finish_stores_failed_and_validation_projections() {
        let store = make_store();

        let result =
            handle_task_finish(
                &store,
                None,
                TaskFinishParams {
                    tenant_id: "test".to_string(),
                    task_id: "task-123".to_string(),
                    project_id: Some("proj_alpha".to_string()),
                    agent_id: Some("agent-1".to_string()),
                    session_id: Some("session-7".to_string()),
                    status: Some("completed".to_string()),
                    goal: Some("Quantify the stress-response regulon".to_string()),
                    scientific_question: None,
                    dataset_refs: vec![],
                    entity_refs: vec![],
                    what_worked: vec![
                        "Re-running with stricter QC stabilized the hit list".to_string()
                    ],
                    what_failed: vec!["The first alignment preset over-trimmed reads".to_string()],
                    validation: vec!["Independent replicate confirmed the top genes".to_string()],
                    uncertainty: vec!["One replicate remains borderline".to_string()],
                    followups: vec!["Collect an additional replicate".to_string()],
                    confidence: Some(0.78),
                    provenance: None,
                },
            )
            .await
            .unwrap();

        let payload: TaskArtifactResult = parse_tool_payload(&result);
        let tenant = TenantId::new("test").unwrap();
        let stored = store
            .get_task_artifact(&tenant, &payload.artifact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.task_id, "task-123");
        assert_eq!(stored.confidence, Some(0.78));

        let search = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "over-trimmed reads".to_string(),
                project_id: Some("proj_alpha".to_string()),
                k: 10,
                filters: None,
                debug_tiers: None,
                mode: None,
                include_superseded: None,
                include_expired: None,
                include_history: None,
                oversample_factor: None,
            },
        )
        .await
        .unwrap();
        let search_payload: SearchResult = parse_tool_payload(&search);
        assert!(search_payload.results.iter().any(|result| {
            result
                .tags
                .iter()
                .any(|tag| tag.starts_with("task:projection:failed"))
        }));
    }

    #[tokio::test]
    async fn task_finish_rejects_out_of_range_confidence() {
        let store = make_store();

        let result = handle_task_finish(
            &store,
            None,
            TaskFinishParams {
                tenant_id: "test".to_string(),
                task_id: "task-123".to_string(),
                project_id: None,
                agent_id: None,
                session_id: None,
                status: None,
                goal: None,
                scientific_question: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                what_worked: vec![],
                what_failed: vec![],
                validation: vec![],
                uncertainty: vec![],
                followups: vec![],
                confidence: Some(1.1),
                provenance: None,
            },
        )
        .await;

        assert!(matches!(result, Err(McpError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn context_brief_project_generates_digest_artifact() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_a".to_string(),
                project_id: Some("proj_alpha".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Ship the project brief".to_string(),
                motivation: "New agents need a concise resume surface".to_string(),
                hypothesis: "A persisted project brief will reduce context-search noise"
                    .to_string(),
                scientific_question: "Can a digest summarize current task state?".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["brief artifact".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        handle_task_finish(
            &store,
            None,
            TaskFinishParams {
                tenant_id: "tenant_a".to_string(),
                task_id: start_payload.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                status: None,
                goal: None,
                scientific_question: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                what_worked: vec!["Digest summarization reduced retrieval fan-out".to_string()],
                what_failed: vec!["Raw chunk search alone was noisy".to_string()],
                validation: vec!["Project brief response returned one active task".to_string()],
                uncertainty: vec![],
                followups: vec!["Bias memory.search toward project digests".to_string()],
                confidence: Some(0.9),
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_context_brief_project(
            &store,
            ProjectBriefParams {
                tenant_id: "tenant_a".to_string(),
                project_id: "proj_alpha".to_string(),
                query: "".to_string(),
                k: 10,
                include_related_projects: true,
            },
        )
        .await
        .unwrap();

        let payload: ProjectBriefResult = parse_tool_payload(&result);
        assert_eq!(payload.artifact.artifact_kind, ArtifactKind::Digest);
        assert_eq!(
            payload.artifact.artifact_role.as_deref(),
            Some(DIGEST_ROLE_PROJECT_BRIEF)
        );
        assert_eq!(payload.brief.project_id, "proj_alpha");
        assert_eq!(payload.trust_tier, TrustTier::CompiledDigestHint);
        assert!(payload.verification_hint.requires_verification);
        assert!(!payload.grounding_refs.is_empty());
        assert!(
            !payload.brief.recent_completed_tasks.is_empty()
                || !payload.brief.active_tasks.is_empty()
        );
    }

    #[tokio::test]
    async fn artifact_verify_reports_canonical_support_and_can_persist_record() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_verify".to_string(),
                project_id: Some("proj_verify".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Verify grounding boundary".to_string(),
                motivation: "Need an explicit trust boundary".to_string(),
                hypothesis: "Canonical artifacts should ground the claim".to_string(),
                scientific_question: "Can artifact.verify recover direct canonical support?"
                    .to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["verification result".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        handle_task_finish(
            &store,
            None,
            TaskFinishParams {
                tenant_id: "tenant_verify".to_string(),
                task_id: start_payload.task_id.clone(),
                project_id: Some("proj_verify".to_string()),
                agent_id: None,
                session_id: None,
                status: None,
                goal: None,
                scientific_question: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                what_worked: vec![
                    "Canonical artifacts are the trust anchor for grounded claims".to_string(),
                ],
                what_failed: vec![],
                validation: vec![
                    "Grounding should prefer canonical artifacts over digests".to_string()
                ],
                uncertainty: vec![],
                followups: vec![],
                confidence: Some(0.9),
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_artifact_verify(
            &store,
            ArtifactVerifyParams {
                tenant_id: "tenant_verify".to_string(),
                claim: "canonical artifacts are the trust anchor".to_string(),
                project_id: Some("proj_verify".to_string()),
                task_id: Some(start_payload.task_id.clone()),
                thread_id: None,
                candidate_artifact_ids: vec![],
                k: 8,
                include_digests: false,
                create_artifact: true,
                record_task_id: Some(start_payload.task_id.clone()),
                agent_id: None,
            },
        )
        .await
        .unwrap();

        let payload: ArtifactVerifyResult = parse_tool_payload(&result);
        assert_eq!(
            payload.grounding_status,
            GroundingStatus::CanonicallyGrounded
        );
        assert!(!payload.supporting_artifacts.is_empty());
        assert!(payload.conflicting_artifacts.is_empty());
        let verification_artifact = payload
            .verification_artifact
            .expect("verification artifact should be persisted");
        assert_eq!(
            verification_artifact.artifact_kind,
            ArtifactKind::Verification
        );
        assert_eq!(
            verification_artifact.verification_status.as_deref(),
            Some("canonically_grounded")
        );
    }

    #[tokio::test]
    async fn artifact_verify_returns_digest_only_when_only_unbacked_digest_matches() {
        let store = make_store();

        // `artifact.create` rejects `artifact_kind = digest` (digests are
        // server-generated via memory.compact to prevent ID-based overwrite
        // of canonical digests). Use the server-side `persist_digest_artifact`
        // path directly to set up the test fixture.
        let tenant = TenantId::new("tenant_digest").unwrap();
        let mut digest = TaskArtifact::new_digest(
            tenant.clone(),
            "digest_task_project_brief::proj_digest",
            "project_brief::proj_digest",
            "project_brief",
        );
        digest.project_id = ProjectId::from("proj_digest");
        digest.summary = Some("Digest-only hint about an isolated semantic summary".to_string());
        let digest = persist_digest_artifact(&store, digest)
            .await
            .expect("server-side digest persist must succeed");

        let result = handle_artifact_verify(
            &store,
            ArtifactVerifyParams {
                tenant_id: "tenant_digest".to_string(),
                claim: "isolated semantic summary".to_string(),
                project_id: Some("proj_digest".to_string()),
                task_id: None,
                thread_id: None,
                candidate_artifact_ids: vec![digest.artifact_id],
                k: 8,
                include_digests: false,
                create_artifact: false,
                record_task_id: None,
                agent_id: None,
            },
        )
        .await
        .unwrap();

        let payload: ArtifactVerifyResult = parse_tool_payload(&result);
        assert_eq!(payload.grounding_status, GroundingStatus::DigestOnly);
        assert!(payload.supporting_artifacts.is_empty());
        assert_eq!(payload.consulted_digests.len(), 1);
    }

    /// Regression test for the tenant-isolation default:
    /// `scoped_tenants_for_project` must NOT widen across tenants when
    /// `allow_cross_tenant_project_fallback` is false (the v0.3.1 default).
    /// The legacy sweep leaked tenant B's project-scoped artifacts to any
    /// caller in tenant A that guessed the same `project_id`.
    #[tokio::test]
    async fn scoped_tenants_respects_isolation_default() {
        let _flag_guard = with_fallback_flag();
        let store = make_store();

        // Seed two tenants with the same project_id.
        handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_a".to_string(),
                project_id: Some("shared".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "A's work".to_string(),
                motivation: "m".to_string(),
                hypothesis: "h".to_string(),
                scientific_question: "q".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["o".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_b".to_string(),
                project_id: Some("shared".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "B's work".to_string(),
                motivation: "m".to_string(),
                hypothesis: "h".to_string(),
                scientific_question: "q".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["o".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        // Default (flag off): tenant A should see ONLY its own tenant.
        set_cross_tenant_project_fallback(false);
        let scoped =
            scoped_tenants_for_project(&store, &TenantId::new("tenant_a").unwrap(), Some("shared"))
                .await
                .unwrap();
        assert_eq!(
            scoped,
            vec![TenantId::new("tenant_a").unwrap()],
            "default isolation must not widen across tenants"
        );

        // Opt-in (flag on): should widen to include tenant_b.
        set_cross_tenant_project_fallback(true);
        let scoped =
            scoped_tenants_for_project(&store, &TenantId::new("tenant_a").unwrap(), Some("shared"))
                .await
                .unwrap();
        assert!(
            scoped.contains(&TenantId::new("tenant_b").unwrap()),
            "flag-on must widen retrieval to other tenants sharing the project_id"
        );

        // Reset global state so sibling tests see the default.
        set_cross_tenant_project_fallback(false);
    }

    /// Phase 2.5: `task.progress` and `task.add_evidence` emit ONE
    /// projection per call (the base summary) instead of the legacy
    /// fanout of 2-3 kind-specific chunks. task.start / task.finish /
    /// task.run_start / task.run_finish keep the full fanout because
    /// their kind-specific projections carry tool/command text that
    /// downstream filters rely on.
    #[tokio::test]
    async fn task_progress_emits_single_projection_chunk() {
        let store = make_store();

        // Seed a task so progress has something to reply to.
        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "amplification".to_string(),
                project_id: Some("proj".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "measure write amplification".to_string(),
                motivation: String::new(),
                hypothesis: String::new(),
                scientific_question: String::new(),
                dataset_refs: vec![],
                expected_outputs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        let progress = handle_task_progress(
            &store,
            None,
            TaskProgressParams {
                tenant_id: "amplification".to_string(),
                task_id: start_payload.task_id.clone(),
                project_id: Some("proj".to_string()),
                agent_id: None,
                session_id: None,
                summary: "investigated legacy fanout".to_string(),
                blockers: vec!["waiting on review".to_string()],
                failed_attempts: vec![],
                next_step: "cut projection count".to_string(),
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let progress_payload: TaskArtifactResult = parse_tool_payload(&progress);

        // Before Phase 2.5 this value was 2 (TaskSummary base +
        // blocker/followup fanout). After the cut it must be exactly 1.
        assert_eq!(
            progress_payload.projection_chunk_ids.len(),
            1,
            "task.progress must emit exactly one projection chunk; \
             write amplification regression if this grows"
        );
    }

    /// Phase 2.1 (Codex coverage gap): the file-arm of
    /// `resolve_tenant_id`. With `$MEMD_DEFAULT_TENANT` cleared and a
    /// pinned `~/.memd/default_tenant` file, the file's contents must
    /// win over the literal `"default"` fallback. Also verifies that
    /// env still overrides the file when both are present.
    #[test]
    fn resolve_tenant_id_reads_pinned_default_tenant_file() {
        let _flag_guard = with_fallback_flag();
        let previous_env = std::env::var("MEMD_DEFAULT_TENANT").ok();
        let previous_home = std::env::var("HOME").ok();

        let tmp = tempfile::tempdir().unwrap();
        let memd_dir = tmp.path().join(".memd");
        std::fs::create_dir_all(&memd_dir).unwrap();
        std::fs::write(memd_dir.join("default_tenant"), "  file_pinned_tenant\n").unwrap();

        // SAFETY: tests serialized via `with_fallback_flag()`.
        unsafe {
            std::env::remove_var("MEMD_DEFAULT_TENANT");
            std::env::set_var("HOME", tmp.path());
        }

        // Explicit empty → env empty → file wins.
        let resolved = resolve_tenant_id("").unwrap();
        assert_eq!(
            resolved.as_str(),
            "file_pinned_tenant",
            "file arm must take precedence over the literal `default` fallback"
        );

        // When both env and file are present, env must win.
        unsafe { std::env::set_var("MEMD_DEFAULT_TENANT", "env_wins") };
        let resolved_env = resolve_tenant_id("").unwrap();
        assert_eq!(resolved_env.as_str(), "env_wins");

        // Restore environment.
        unsafe {
            if let Some(prev) = previous_home {
                std::env::set_var("HOME", prev);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(prev) = previous_env {
                std::env::set_var("MEMD_DEFAULT_TENANT", prev);
            } else {
                std::env::remove_var("MEMD_DEFAULT_TENANT");
            }
        }
    }

    /// Phase 3.4 regression: a `task.add_evidence` write must mark
    /// the evidence / highlight / project_brief digests dirty on the
    /// writer side. The dirty tracker is a process-global singleton,
    /// so this test holds the policy-flag mutex (which already
    /// serializes other tests that manipulate globals) to get
    /// exclusive access, then drains the tracker before and after.
    #[tokio::test]
    async fn task_add_evidence_marks_evidence_digests_dirty() {
        use crate::task_memory::digest_dirty::{global as dirty_tracker, DigestDirtyKey};
        use crate::task_memory::{
            DIGEST_ROLE_EVIDENCE_LIBRARY, DIGEST_ROLE_HIGHLIGHT_LIBRARY, DIGEST_ROLE_PROJECT_BRIEF,
        };

        // Serialize with sibling tests that manipulate other global
        // state (e.g., the cross-tenant fallback flag). This also
        // prevents concurrent writer paths from other tests from
        // polluting our dirty-tracker snapshot.
        let _flag_guard = with_fallback_flag();
        let _ = dirty_tracker().drain_dirty();

        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "dirty_ev".to_string(),
                project_id: Some("proj_dirty".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "phase 3.4 writer-dirty test".to_string(),
                motivation: String::new(),
                hypothesis: String::new(),
                scientific_question: String::new(),
                dataset_refs: vec![],
                expected_outputs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        // task.add_evidence should flag the evidence + highlight +
        // project_brief digests as dirty.
        handle_task_add_evidence(
            &store,
            None,
            TaskAddEvidenceParams {
                tenant_id: "dirty_ev".to_string(),
                task_id: start_payload.task_id,
                project_id: Some("proj_dirty".to_string()),
                agent_id: None,
                session_id: None,
                summary: "sentinel evidence".to_string(),
                evidence_kind: "unit_test".to_string(),
                supports_claim: Some(true),
                metric_name: None,
                metric_value: None,
                metrics: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        // Tracker is a process-global, so other concurrent tests may
        // also contribute entries. Check our specific (tenant,
        // project, role) triples are present rather than asserting
        // the total count.
        for role in [
            DIGEST_ROLE_EVIDENCE_LIBRARY,
            DIGEST_ROLE_HIGHLIGHT_LIBRARY,
            DIGEST_ROLE_PROJECT_BRIEF,
        ] {
            let key = DigestDirtyKey {
                tenant_id: "dirty_ev".to_string(),
                project_id: Some("proj_dirty".to_string()),
                role: role.to_string(),
            };
            assert!(
                dirty_tracker().contains(&key),
                "{} digest must be marked dirty after task.add_evidence; \
                 current dirty entries: {:?}",
                role,
                dirty_tracker().drain_dirty(),
            );
        }
    }

    /// Phase 2.2: `task.start` accepts only `{goal}` as the
    /// hard-required surface — motivation, hypothesis, and the rest
    /// default to empty. An agent that just wants to log "I started
    /// working on X" should not be forced to invent fields.
    #[tokio::test]
    async fn task_start_accepts_minimal_goal_only_payload() {
        let _flag_guard = with_fallback_flag();
        // Point HOME at an empty temp dir so no pinned
        // `~/.memd/default_tenant` file from the developer machine
        // redirects the implicit default.
        let tmp = tempfile::tempdir().unwrap();
        let previous_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let previous_env = std::env::var("MEMD_DEFAULT_TENANT").ok();
        unsafe { std::env::remove_var("MEMD_DEFAULT_TENANT") };

        let store = make_store();

        // Exactly the minimum: no tenant_id, no motivation, no
        // hypothesis, etc.
        let params: TaskStartParams = serde_json::from_value(json!({
            "goal": "Minimal start scenario"
        }))
        .expect("task.start must deserialize from just `{goal}`");

        let result = handle_task_start(&store, None, params).await.unwrap();
        let payload: TaskArtifactResult = parse_tool_payload(&result);

        // With env cleared and no pinned file, the resolver falls
        // back to the literal "default" tenant.
        let artifact = store
            .get_task_artifact(&TenantId::new("default").unwrap(), &payload.artifact_id)
            .await
            .unwrap()
            .expect("artifact must land in the `default` tenant");
        assert_eq!(artifact.goal.as_deref(), Some("Minimal start scenario"));

        // Restore env for sibling tests.
        if let Some(prev) = previous_home {
            unsafe { std::env::set_var("HOME", prev) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }
        if let Some(prev) = previous_env {
            unsafe { std::env::set_var("MEMD_DEFAULT_TENANT", prev) };
        }
    }

    /// Phase 2.1: `tenant_id` resolution falls through an ordered chain
    /// of sources. Explicit value wins; otherwise `$MEMD_DEFAULT_TENANT`
    /// is consulted; otherwise `~/.memd/default_tenant` (not covered
    /// here to avoid touching `$HOME`); finally the literal `"default"`
    /// is used.
    ///
    /// Test is serialized via `with_fallback_flag()` because it
    /// manipulates process env vars.
    #[test]
    fn resolve_tenant_id_falls_back_through_env_and_literal_default() {
        let _flag_guard = with_fallback_flag();
        let previous = std::env::var("MEMD_DEFAULT_TENANT").ok();

        // Explicit non-empty wins even when env is set.
        // SAFETY: tests are serialized via the fallback-flag mutex.
        unsafe { std::env::set_var("MEMD_DEFAULT_TENANT", "env_default") };
        let explicit = resolve_tenant_id("explicit_tenant").unwrap();
        assert_eq!(explicit.as_str(), "explicit_tenant");

        // Empty explicit + env set → env wins.
        let env_resolved = resolve_tenant_id("").unwrap();
        assert_eq!(env_resolved.as_str(), "env_default");

        // Empty explicit + unset env (and presumably no pinned file in
        // the test environment) → literal "default".
        unsafe { std::env::remove_var("MEMD_DEFAULT_TENANT") };
        // Point HOME at an empty temp dir so the file-lookup arm cannot
        // find a pinned value left over from the developer machine.
        let tmp = tempfile::tempdir().unwrap();
        let previous_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let fallback = resolve_tenant_id("   ").unwrap();
        assert_eq!(fallback.as_str(), "default");

        // Restore environment for sibling tests.
        if let Some(prev) = previous_home {
            unsafe { std::env::set_var("HOME", prev) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }
        if let Some(prev) = previous {
            unsafe { std::env::set_var("MEMD_DEFAULT_TENANT", prev) };
        }
    }

    /// Writer-identity resolution in v0.3.1 is explicit-only:
    /// non-empty explicit → that value, else → anonymous (`None`). The
    /// previous prototype maintained a process-global default derived
    /// from `initialize.clientInfo` but that was unsound across shared
    /// HTTP sessions (identity bleed + re-initialize forgery). See the
    /// comment on `resolved_agent_id`.
    #[test]
    fn resolved_agent_id_uses_explicit_value_or_anonymous() {
        assert_eq!(
            resolved_agent_id(Some("codex@0.12")),
            Some("codex@0.12".to_string()),
            "non-empty explicit identifier is returned as-is"
        );
        assert!(
            resolved_agent_id(Some("   ")).is_none(),
            "whitespace-only explicit value must NOT masquerade as an identity"
        );
        assert!(
            resolved_agent_id(Some("")).is_none(),
            "empty string must be treated as anonymous"
        );
        assert!(
            resolved_agent_id(None).is_none(),
            "absent agent_id is anonymous; the countersignature path will refuse to promote"
        );
    }

    /// End-to-end: a `task.start` without an explicit `agent_id`
    /// persists an anonymous artifact in v0.3.1. Identity auto-fill
    /// from session state is deferred to Phase 2.
    #[tokio::test]
    async fn task_start_without_explicit_agent_id_stays_anonymous() {
        let store = make_store();

        let start_value = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "writer_anon".to_string(),
                project_id: Some("proj".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "test anonymous write".to_string(),
                motivation: "no identity supplied".to_string(),
                hypothesis: "anonymous writes stay anonymous".to_string(),
                scientific_question: "does it stay None?".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["ok".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let payload: TaskArtifactResult = parse_tool_payload(&start_value);

        let canonical = store
            .get_task_artifact(&TenantId::new("writer_anon").unwrap(), &payload.artifact_id)
            .await
            .unwrap()
            .expect("artifact must be persisted");
        assert!(
            canonical.agent_id.is_none(),
            "artifact must remain anonymous when no agent_id is supplied; \
             got {:?}",
            canonical.agent_id
        );

        // Explicit agent_id still persists as-is.
        let start_explicit = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "writer_anon".to_string(),
                project_id: Some("proj".to_string()),
                parent_task_id: None,
                agent_id: Some("planner-override".to_string()),
                session_id: None,
                goal: "explicit".to_string(),
                motivation: "m".to_string(),
                hypothesis: "h".to_string(),
                scientific_question: "q".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["ok".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let explicit_payload: TaskArtifactResult = parse_tool_payload(&start_explicit);
        let canonical_explicit = store
            .get_task_artifact(
                &TenantId::new("writer_anon").unwrap(),
                &explicit_payload.artifact_id,
            )
            .await
            .unwrap()
            .expect("artifact must be persisted");
        assert_eq!(
            canonical_explicit.agent_id.as_deref(),
            Some("planner-override")
        );
    }

    /// End-to-end trust-tier test: a single-agent `artifact.create` with
    /// `artifact_kind = "verification"` and agent-labelled fields must
    /// NOT produce a `VerifiedRecord`. Only a countersignature from a
    /// distinct `agent_id` can promote trust.
    #[tokio::test]
    async fn single_writer_verification_is_not_verified_record() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "trust_solo".to_string(),
                project_id: Some("proj".to_string()),
                parent_task_id: None,
                agent_id: Some("solo".to_string()),
                session_id: None,
                goal: "test solo".to_string(),
                motivation: "m".to_string(),
                hypothesis: "h".to_string(),
                scientific_question: "q".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["ok".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        // Same writer "solo" tries to self-verify via artifact.create.
        let verify_value = handle_artifact_create(
            &store,
            None,
            artifact_params_minimal(
                "trust_solo",
                "verification",
                &start_payload.task_id,
                Some("solo"),
                Some(&start_payload.artifact_id),
                "looks good to me",
                Some(true),
                Some("verified"),
                Some("approved"),
            ),
        )
        .await
        .unwrap();
        let verify_payload: TaskArtifactResult = parse_tool_payload(&verify_value);

        let persisted = store
            .get_task_artifact(
                &TenantId::new("trust_solo").unwrap(),
                &verify_payload.artifact_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            derive_artifact_trust_tier(&persisted),
            TrustTier::CanonicalRecord,
            "single-writer verification cannot be VerifiedRecord"
        );
    }

    /// Positive test: a verification artifact written by a DIFFERENT
    /// agent, replying to the original and explicitly supporting the
    /// claim, is promoted to `VerifiedRecord`.
    #[tokio::test]
    async fn distinct_writer_countersignature_produces_verified_record() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "trust_pair".to_string(),
                project_id: Some("proj".to_string()),
                parent_task_id: None,
                agent_id: Some("author".to_string()),
                session_id: None,
                goal: "test pair".to_string(),
                motivation: "m".to_string(),
                hypothesis: "h".to_string(),
                scientific_question: "q".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["ok".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        // A DIFFERENT agent verifies.
        let verify_value = handle_artifact_create(
            &store,
            None,
            artifact_params_minimal(
                "trust_pair",
                "verification",
                &start_payload.task_id,
                Some("reviewer"),
                Some(&start_payload.artifact_id),
                "independently reproduced",
                Some(true),
                None,
                None,
            ),
        )
        .await
        .unwrap();
        let verify_payload: TaskArtifactResult = parse_tool_payload(&verify_value);

        let persisted = store
            .get_task_artifact(
                &TenantId::new("trust_pair").unwrap(),
                &verify_payload.artifact_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            derive_artifact_trust_tier(&persisted),
            TrustTier::VerifiedRecord,
            "countersignature from a distinct agent_id must promote trust"
        );
    }

    /// Codex-review regression (v0.3.1): the old process-global
    /// `SESSION_DEFAULT_AGENT_ID` let a single client reinitialize as a
    /// different persona and forge a countersignature by writing an
    /// anonymous reply that the server backfilled with the new default.
    /// The fix removes the default entirely — anonymous writes stay
    /// anonymous, and the countersignature check refuses to promote.
    #[tokio::test]
    async fn anonymous_verification_never_promotes_to_verified() {
        let store = make_store();

        // Author writes with agent_id = "alice".
        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "trust_forge".to_string(),
                project_id: Some("proj".to_string()),
                parent_task_id: None,
                agent_id: Some("alice".to_string()),
                session_id: None,
                goal: "anti-forgery scenario".to_string(),
                motivation: "m".to_string(),
                hypothesis: "h".to_string(),
                scientific_question: "q".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["ok".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        // "Verification" submitted WITHOUT an explicit agent_id — this is
        // what the old default-backfill path would have silently
        // attributed to whichever client most recently called initialize.
        let verify = handle_artifact_create(
            &store,
            None,
            artifact_params_minimal(
                "trust_forge",
                "verification",
                &start_payload.task_id,
                None, // <-- anonymous
                Some(&start_payload.artifact_id),
                "I say it's fine",
                Some(true),
                None,
                None,
            ),
        )
        .await
        .unwrap();
        let verify_payload: TaskArtifactResult = parse_tool_payload(&verify);

        let persisted = store
            .get_task_artifact(
                &TenantId::new("trust_forge").unwrap(),
                &verify_payload.artifact_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            persisted.agent_id.is_none(),
            "anonymous write must stay anonymous; got {:?}",
            persisted.agent_id
        );
        assert_eq!(
            derive_artifact_trust_tier(&persisted),
            TrustTier::CanonicalRecord,
            "anonymous verification must never produce VerifiedRecord"
        );
    }

    /// Negative test: a reviewer who explicitly REJECTS the claim
    /// (`supports_claim = false`) must NOT promote trust, even with a
    /// distinct agent_id.
    #[tokio::test]
    async fn distinct_writer_explicit_rejection_does_not_promote() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "trust_reject".to_string(),
                project_id: Some("proj".to_string()),
                parent_task_id: None,
                agent_id: Some("author".to_string()),
                session_id: None,
                goal: "test reject".to_string(),
                motivation: "m".to_string(),
                hypothesis: "h".to_string(),
                scientific_question: "q".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["ok".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        let verify_value = handle_artifact_create(
            &store,
            None,
            artifact_params_minimal(
                "trust_reject",
                "review",
                &start_payload.task_id,
                Some("reviewer"),
                Some(&start_payload.artifact_id),
                "could not reproduce",
                Some(false),
                None,
                None,
            ),
        )
        .await
        .unwrap();
        let verify_payload: TaskArtifactResult = parse_tool_payload(&verify_value);

        let persisted = store
            .get_task_artifact(
                &TenantId::new("trust_reject").unwrap(),
                &verify_payload.artifact_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            derive_artifact_trust_tier(&persisted),
            TrustTier::CanonicalRecord,
            "explicit rejection must leave the reviewer's artifact at canonical"
        );
    }

    fn artifact_params_minimal(
        tenant_id: &str,
        artifact_kind: &str,
        task_id: &str,
        agent_id: Option<&str>,
        reply_to_artifact_id: Option<&str>,
        summary: &str,
        supports_claim: Option<bool>,
        verification_status: Option<&str>,
        approval_state: Option<&str>,
    ) -> ArtifactCreateParams {
        ArtifactCreateParams {
            tenant_id: tenant_id.to_string(),
            artifact_kind: artifact_kind.to_string(),
            task_id: Some(task_id.to_string()),
            project_id: None,
            parent_task_id: None,
            agent_id: agent_id.map(|s| s.to_string()),
            session_id: None,
            status: None,
            artifact_role: None,
            challenge_id: None,
            thread_id: None,
            reply_to_artifact_id: reply_to_artifact_id.map(|s| s.to_string()),
            relation_kind: None,
            goal: None,
            motivation: None,
            hypothesis: None,
            scientific_question: None,
            method_summary: None,
            summary: Some(summary.to_string()),
            content: None,
            evidence_kind: None,
            supports_claim,
            blockers: vec![],
            what_worked: vec![],
            what_failed: vec![],
            validation: vec![],
            uncertainty: vec![],
            followups: vec![],
            expected_outputs: vec![],
            related_artifact_ids: vec![],
            contributors: vec![],
            dataset_refs: vec![],
            entity_refs: vec![],
            tool_name: None,
            tool_version: None,
            command: None,
            parameters: None,
            inputs: vec![],
            outputs: vec![],
            metrics: None,
            why_chosen: None,
            confidence: None,
            requested_action: None,
            verification_status: verification_status.map(|s| s.to_string()),
            compute_budget: None,
            cost_actual: None,
            data_access_level: None,
            policy_tags: vec![],
            allowed_tools: vec![],
            approval_state: approval_state.map(|s| s.to_string()),
            provenance: None,
        }
    }

    /// Regression test for the digest-forgery mitigation: `artifact.create`
    /// must reject any attempt to write `artifact_kind = "digest"`. Digests
    /// are server-generated and have deterministic IDs; accepting
    /// agent-authored digests lets any caller overwrite the canonical
    /// `project_brief` / `failure_library` / etc. artifacts.
    #[tokio::test]
    async fn artifact_create_rejects_agent_authored_digest() {
        let store = make_store();

        let err = handle_artifact_create(
            &store,
            None,
            ArtifactCreateParams {
                tenant_id: "tenant_forge".to_string(),
                artifact_kind: "digest".to_string(),
                task_id: None,
                project_id: Some("proj_forge".to_string()),
                parent_task_id: None,
                agent_id: Some("attacker".to_string()),
                session_id: None,
                status: None,
                artifact_role: Some("project_brief".to_string()),
                challenge_id: None,
                thread_id: None,
                reply_to_artifact_id: None,
                relation_kind: None,
                goal: None,
                motivation: None,
                hypothesis: None,
                scientific_question: None,
                method_summary: None,
                summary: Some("forged brief that overwrites the real digest".to_string()),
                content: None,
                evidence_kind: None,
                supports_claim: None,
                blockers: vec![],
                what_worked: vec![],
                what_failed: vec![],
                validation: vec![],
                uncertainty: vec![],
                followups: vec![],
                expected_outputs: vec![],
                related_artifact_ids: vec![],
                contributors: vec![],
                dataset_refs: vec![],
                entity_refs: vec![],
                tool_name: None,
                tool_version: None,
                command: None,
                parameters: None,
                inputs: vec![],
                outputs: vec![],
                metrics: None,
                why_chosen: None,
                confidence: None,
                requested_action: None,
                verification_status: None,
                compute_budget: None,
                cost_actual: None,
                data_access_level: None,
                policy_tags: vec![],
                allowed_tools: vec![],
                approval_state: None,
                provenance: None,
            },
        )
        .await
        .expect_err("agent-authored digests must be rejected");

        match err {
            McpError::InvalidParams(msg) => {
                assert!(
                    msg.contains("digests are server-generated"),
                    "error message should explain digest policy, got: {}",
                    msg
                );
            }
            other => panic!("expected InvalidParams, got: {:?}", other),
        }
    }

    /// Phase 0 of the memd-wiki v2 plan: the `content` field is
    /// exclusively for `wiki_page` artifacts. A non-empty `content`
    /// submitted with any other `artifact_kind` must be rejected at
    /// the MCP boundary with a clear `InvalidParams` message — this
    /// keeps the storage-row invariant "content is Some iff kind is
    /// WikiPage" honest so downstream consumers (rendering, lint,
    /// digest builders) can rely on it.
    #[tokio::test]
    async fn artifact_create_rejects_content_on_non_wiki_page_kind() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_wiki_content".to_string(),
                project_id: Some("memd".to_string()),
                parent_task_id: None,
                agent_id: Some("author-1".to_string()),
                session_id: None,
                goal: "Exercise wiki_page content invariant".to_string(),
                motivation: "Phase 0 trust boundary".to_string(),
                hypothesis: "Non-WikiPage kinds cannot carry content".to_string(),
                scientific_question: "Does the MCP validator reject misplaced content?".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["rejection".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        let mut params = artifact_params_minimal(
            "tenant_wiki_content",
            "task_progress",
            &start_payload.task_id,
            Some("author-1"),
            None,
            "progress update",
            None,
            None,
            None,
        );
        params.content = Some("# stray markdown body".to_string());

        let err = handle_artifact_create(&store, None, params)
            .await
            .expect_err("non-wiki_page kinds must not accept content");
        match err {
            McpError::InvalidParams(msg) => {
                assert!(
                    msg.contains("wiki_page") && msg.contains("content"),
                    "error should explain content is wiki_page-only; got: {msg}"
                );
                assert!(
                    msg.contains("task_progress"),
                    "error should name the rejected kind; got: {msg}"
                );
            }
            other => panic!("expected InvalidParams, got: {other:?}"),
        }
    }

    /// Phase 0 (codex-folded §4.2 of the plan): a distinct-writer
    /// `Verification` artifact that replies to a `WikiPage` is itself
    /// promoted to `VerifiedRecord` via the existing countersignature
    /// path, but the WikiPage's own `promotion_state` / trust tier
    /// never change. This test nails down BOTH halves: the child
    /// promotes, the parent stays at `CanonicalRecord`.
    #[tokio::test]
    async fn wiki_page_verification_child_promotes_child_not_parent() {
        let store = make_store();
        let tenant = TenantId::new("wiki_child_promote").unwrap();

        // Author a WikiPage.
        let mut page_params = artifact_params_minimal(
            tenant.as_str(),
            "wiki_page",
            "task-wiki-promote",
            Some("author-alpha"),
            None,
            "Verification boundary concept page.",
            None,
            None,
            None,
        );
        page_params.artifact_role = Some("concept".to_string());
        page_params.content = Some(
            "# Verification boundary\n\nLLM-authored concept page body.".to_string(),
        );
        page_params.related_artifact_ids = vec!["0199".to_string()];

        let page_value = handle_artifact_create(&store, None, page_params).await.unwrap();
        let page_payload: TaskArtifactResult = parse_tool_payload(&page_value);

        let page = store
            .get_task_artifact(&tenant, &page_payload.artifact_id)
            .await
            .unwrap()
            .expect("wiki_page was persisted");
        assert_eq!(page.artifact_kind, ArtifactKind::WikiPage);
        assert_eq!(
            derive_artifact_trust_tier(&page),
            TrustTier::CanonicalRecord,
            "fresh wiki_page must start at CanonicalRecord"
        );

        // A distinct writer files a Verification countersigning the page.
        let verify_value = handle_artifact_create(
            &store,
            None,
            artifact_params_minimal(
                tenant.as_str(),
                "verification",
                "task-wiki-promote",
                Some("reviewer-beta"),
                Some(&page_payload.artifact_id),
                "Independently confirmed the claim.",
                Some(true),
                Some("verified"),
                Some("approved"),
            ),
        )
        .await
        .unwrap();
        let verify_payload: TaskArtifactResult = parse_tool_payload(&verify_value);

        // The child verification is promoted to VerifiedRecord.
        let verify = store
            .get_task_artifact(&tenant, &verify_payload.artifact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            derive_artifact_trust_tier(&verify),
            TrustTier::VerifiedRecord,
            "distinct-writer verification replying to wiki_page must promote to VerifiedRecord"
        );

        // The parent wiki_page stays at CanonicalRecord forever — the
        // promotion path targets the child, not the parent.
        let parent_after = store
            .get_task_artifact(&tenant, &page_payload.artifact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            derive_artifact_trust_tier(&parent_after),
            TrustTier::CanonicalRecord,
            "wiki_page trust tier must remain CanonicalRecord after a verifying child"
        );
        assert_ne!(
            parent_after.promotion_state,
            crate::types::PromotionState::Verified,
            "wiki_page promotion_state must not upgrade via a child's countersignature"
        );
    }

    #[tokio::test]
    async fn artifact_verify_marks_same_task_negative_marker_as_conflict() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_conflict".to_string(),
                project_id: Some("proj_conflict".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Exercise conflict detection".to_string(),
                motivation: "Need narrow same-scope conflict checks".to_string(),
                hypothesis: "Explicit negative markers should create a conflict".to_string(),
                scientific_question: "Can artifact.verify detect obvious same-task disagreement?"
                    .to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["conflict result".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        handle_artifact_create(
            &store,
            None,
            ArtifactCreateParams {
                tenant_id: "tenant_conflict".to_string(),
                artifact_kind: "evidence".to_string(),
                task_id: Some(start_payload.task_id.clone()),
                project_id: Some("proj_conflict".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                status: None,
                artifact_role: None,
                challenge_id: None,
                thread_id: Some(start_payload.task_id.clone()),
                reply_to_artifact_id: None,
                relation_kind: None,
                goal: None,
                motivation: None,
                hypothesis: None,
                scientific_question: None,
                method_summary: None,
                summary: Some("The digest planner is reliable for scoped retrieval".to_string()),
                content: None,
                evidence_kind: Some("integration_test".to_string()),
                supports_claim: Some(true),
                blockers: vec![],
                what_worked: vec![],
                what_failed: vec![],
                validation: vec!["Scoped retrieval stayed stable".to_string()],
                uncertainty: vec![],
                followups: vec![],
                expected_outputs: vec![],
                related_artifact_ids: vec![],
                contributors: vec![],
                dataset_refs: vec![],
                entity_refs: vec![],
                tool_name: None,
                tool_version: None,
                command: None,
                parameters: None,
                inputs: vec![],
                outputs: vec![],
                metrics: None,
                why_chosen: None,
                confidence: None,
                requested_action: None,
                verification_status: None,
                compute_budget: None,
                cost_actual: None,
                data_access_level: None,
                policy_tags: vec![],
                allowed_tools: vec![],
                approval_state: None,
                provenance: None,
            },
        )
        .await
        .unwrap();

        handle_artifact_create(
            &store,
            None,
            ArtifactCreateParams {
                tenant_id: "tenant_conflict".to_string(),
                artifact_kind: "verification".to_string(),
                task_id: Some(start_payload.task_id.clone()),
                project_id: Some("proj_conflict".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                status: None,
                artifact_role: Some("claim_grounding".to_string()),
                challenge_id: None,
                thread_id: Some(start_payload.task_id.clone()),
                reply_to_artifact_id: None,
                relation_kind: None,
                goal: None,
                motivation: None,
                hypothesis: None,
                scientific_question: None,
                method_summary: None,
                summary: Some(
                    "The digest planner is not reliable when validation is absent".to_string(),
                ),
                content: None,
                evidence_kind: None,
                supports_claim: Some(false),
                blockers: vec![],
                what_worked: vec![],
                what_failed: vec!["Missing validation breaks reliability".to_string()],
                validation: vec![],
                uncertainty: vec![],
                followups: vec![],
                expected_outputs: vec![],
                related_artifact_ids: vec![],
                contributors: vec![],
                dataset_refs: vec![],
                entity_refs: vec![],
                tool_name: None,
                tool_version: None,
                command: None,
                parameters: None,
                inputs: vec![],
                outputs: vec![],
                metrics: None,
                why_chosen: None,
                confidence: None,
                requested_action: None,
                verification_status: Some("conflicted".to_string()),
                compute_budget: None,
                cost_actual: None,
                data_access_level: None,
                policy_tags: vec![],
                allowed_tools: vec![],
                approval_state: None,
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_artifact_verify(
            &store,
            ArtifactVerifyParams {
                tenant_id: "tenant_conflict".to_string(),
                claim: "digest planner reliable".to_string(),
                project_id: Some("proj_conflict".to_string()),
                task_id: Some(start_payload.task_id),
                thread_id: None,
                candidate_artifact_ids: vec![],
                k: 8,
                include_digests: false,
                create_artifact: false,
                record_task_id: None,
                agent_id: None,
            },
        )
        .await
        .unwrap();

        let payload: ArtifactVerifyResult = parse_tool_payload(&result);
        assert_eq!(payload.grounding_status, GroundingStatus::Conflicted);
        assert!(!payload.supporting_artifacts.is_empty());
        assert!(!payload.conflicting_artifacts.is_empty());
    }

    #[tokio::test]
    async fn context_brief_project_does_not_rewrite_unchanged_digest() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_a".to_string(),
                project_id: Some("proj_alpha".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Ship the project brief".to_string(),
                motivation: "New agents need a concise resume surface".to_string(),
                hypothesis: "A persisted project brief will reduce context-search noise"
                    .to_string(),
                scientific_question: "Can a digest summarize current task state?".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["brief artifact".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        handle_task_finish(
            &store,
            None,
            TaskFinishParams {
                tenant_id: "tenant_a".to_string(),
                task_id: start_payload.task_id,
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                status: None,
                goal: None,
                scientific_question: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                what_worked: vec!["Digest summarization reduced retrieval fan-out".to_string()],
                what_failed: vec!["Raw chunk search alone was noisy".to_string()],
                validation: vec!["Project brief response returned one active task".to_string()],
                uncertainty: vec![],
                followups: vec!["Bias memory.search toward project digests".to_string()],
                confidence: Some(0.9),
                provenance: None,
            },
        )
        .await
        .unwrap();

        let first = handle_context_brief_project(
            &store,
            ProjectBriefParams {
                tenant_id: "tenant_a".to_string(),
                project_id: "proj_alpha".to_string(),
                query: "".to_string(),
                k: 10,
                include_related_projects: true,
            },
        )
        .await
        .unwrap();
        let first_payload: ProjectBriefResult = parse_tool_payload(&first);
        let chunks_after_first = store
            .stats(&TenantId::new("tenant_a").unwrap())
            .await
            .unwrap()
            .total_chunks;

        let second = handle_context_brief_project(
            &store,
            ProjectBriefParams {
                tenant_id: "tenant_a".to_string(),
                project_id: "proj_alpha".to_string(),
                query: "".to_string(),
                k: 10,
                include_related_projects: true,
            },
        )
        .await
        .unwrap();
        let second_payload: ProjectBriefResult = parse_tool_payload(&second);
        let chunks_after_second = store
            .stats(&TenantId::new("tenant_a").unwrap())
            .await
            .unwrap()
            .total_chunks;

        assert_eq!(
            first_payload.artifact.artifact_id,
            second_payload.artifact.artifact_id
        );
        assert_eq!(
            first_payload.artifact.timestamp_created,
            second_payload.artifact.timestamp_created
        );
        assert_eq!(chunks_after_first, chunks_after_second);
    }

    #[tokio::test]
    async fn artifact_find_failures_returns_library_and_failure_hits() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_b".to_string(),
                project_id: Some("proj_beta".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Exercise failure library".to_string(),
                motivation: "Need failure-first recall".to_string(),
                hypothesis: "what_failed fields should be surfaced as reusable failures"
                    .to_string(),
                scientific_question: "Can failure digests summarize recent problems?".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["failure library".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        handle_task_progress(
            &store,
            None,
            TaskProgressParams {
                tenant_id: "tenant_b".to_string(),
                task_id: start_payload.task_id.clone(),
                project_id: Some("proj_beta".to_string()),
                agent_id: None,
                session_id: None,
                summary: "Compilation failed in the digest path".to_string(),
                blockers: vec!["Digest query planner missing project brief candidates".to_string()],
                failed_attempts: vec!["Raw search mode returned only generic chunks".to_string()],
                next_step: "Add digest-aware candidate collection".to_string(),
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_artifact_find_failures(
            &store,
            ArtifactLibraryParams {
                tenant_id: "tenant_b".to_string(),
                project_id: Some("proj_beta".to_string()),
                query: "digest planner".to_string(),
                k: 10,
            },
        )
        .await
        .unwrap();

        let payload: FailureSearchResult = parse_tool_payload(&result);
        assert_eq!(payload.artifact.artifact_kind, ArtifactKind::Digest);
        assert_eq!(
            payload.artifact.artifact_role.as_deref(),
            Some(DIGEST_ROLE_FAILURE_LIBRARY)
        );
        assert!(!payload.results.is_empty());
        assert!(payload.results[0].summary.contains("Digest"));
    }

    #[tokio::test]
    async fn artifact_find_highlights_returns_ranked_lessons_without_rewriting_unchanged_digest() {
        let store = make_store();

        let first = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_h".to_string(),
                project_id: Some("proj_highlight".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Capture reusable agent lessons".to_string(),
                motivation: "Need a high-signal highlight library".to_string(),
                hypothesis: "Validated repeated tactics should surface as highlights".to_string(),
                scientific_question: "Can highlight digests rank future-agent lessons?".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["highlight library".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let first_payload: TaskArtifactResult = parse_tool_payload(&first);

        handle_task_finish(
            &store,
            None,
            TaskFinishParams {
                tenant_id: "tenant_h".to_string(),
                task_id: first_payload.task_id,
                project_id: Some("proj_highlight".to_string()),
                agent_id: None,
                session_id: None,
                status: None,
                goal: None,
                scientific_question: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                what_worked: vec!["Use digest persistence idempotence".to_string()],
                what_failed: vec![
                    "Rewriting unchanged digests creates retrieval noise".to_string(),
                ],
                validation: vec!["Repeated refreshes do not add chunks".to_string()],
                uncertainty: vec![],
                followups: vec![],
                confidence: Some(0.85),
                provenance: None,
            },
        )
        .await
        .unwrap();

        let second = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_h".to_string(),
                project_id: Some("proj_highlight".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Reconfirm reusable agent lessons".to_string(),
                motivation: "Need repetition for stronger promotion".to_string(),
                hypothesis: "Repeated tactics should rank above one-off notes".to_string(),
                scientific_question: "Do repeated successful lessons outrank one-offs?".to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["highlight library".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let second_payload: TaskArtifactResult = parse_tool_payload(&second);

        handle_task_finish(
            &store,
            None,
            TaskFinishParams {
                tenant_id: "tenant_h".to_string(),
                task_id: second_payload.task_id,
                project_id: Some("proj_highlight".to_string()),
                agent_id: None,
                session_id: None,
                status: None,
                goal: None,
                scientific_question: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                what_worked: vec!["Use digest persistence idempotence".to_string()],
                what_failed: vec![
                    "Rewriting unchanged digests creates retrieval noise".to_string(),
                ],
                validation: vec!["Repeated refreshes do not add chunks".to_string()],
                uncertainty: vec![],
                followups: vec![],
                confidence: Some(0.9),
                provenance: None,
            },
        )
        .await
        .unwrap();

        let first = handle_artifact_find_highlights(
            &store,
            ArtifactLibraryParams {
                tenant_id: "tenant_h".to_string(),
                project_id: Some("proj_highlight".to_string()),
                query: "".to_string(),
                k: 10,
            },
        )
        .await
        .unwrap();
        let first_payload: HighlightSearchViewResult = parse_tool_payload(&first);
        let chunks_after_first = store
            .stats(&TenantId::new("tenant_h").unwrap())
            .await
            .unwrap()
            .total_chunks;

        let second = handle_artifact_find_highlights(
            &store,
            ArtifactLibraryParams {
                tenant_id: "tenant_h".to_string(),
                project_id: Some("proj_highlight".to_string()),
                query: "".to_string(),
                k: 10,
            },
        )
        .await
        .unwrap();
        let second_payload: HighlightSearchViewResult = parse_tool_payload(&second);
        let chunks_after_second = store
            .stats(&TenantId::new("tenant_h").unwrap())
            .await
            .unwrap()
            .total_chunks;

        assert_eq!(first_payload.artifact.artifact_kind, ArtifactKind::Digest);
        assert_eq!(
            first_payload.artifact.artifact_role.as_deref(),
            Some(DIGEST_ROLE_HIGHLIGHT_LIBRARY)
        );
        assert!(!first_payload.results.is_empty());
        assert_eq!(first_payload.results[0].category, "tactic");
        assert!(first_payload.results[0]
            .summary
            .contains("digest persistence idempotence"));
        assert_eq!(first_payload.results[0].support_count, 2);
        assert_eq!(
            first_payload.artifact.timestamp_created,
            second_payload.artifact.timestamp_created
        );
        assert_eq!(chunks_after_first, chunks_after_second);
    }

    #[tokio::test]
    async fn memory_compact_can_refresh_digests_without_storage_compaction() {
        let store = make_store();

        let start = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "tenant_c".to_string(),
                project_id: Some("proj_gamma".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Prepare digest-only compaction".to_string(),
                motivation: "Need on-demand digest refreshes".to_string(),
                hypothesis:
                    "memory.compact should rebuild digests even when storage compaction is skipped"
                        .to_string(),
                scientific_question: "Can digest rebuild run without tombstone thresholds?"
                    .to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["digest rebuild".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();
        let start_payload: TaskArtifactResult = parse_tool_payload(&start);

        handle_task_finish(
            &store,
            None,
            TaskFinishParams {
                tenant_id: "tenant_c".to_string(),
                task_id: start_payload.task_id,
                project_id: Some("proj_gamma".to_string()),
                agent_id: None,
                session_id: None,
                status: None,
                goal: None,
                scientific_question: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                what_worked: vec!["Digest rebuild can be triggered explicitly".to_string()],
                what_failed: vec!["No storage compaction threshold was exceeded".to_string()],
                validation: vec!["Compaction response returned digest artifact ids".to_string()],
                uncertainty: vec![],
                followups: vec![],
                confidence: Some(0.8),
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_memory_compact(
            &store,
            CompactParams {
                tenant_id: "tenant_c".to_string(),
                force: false,
                project_id: Some("proj_gamma".to_string()),
                digest_modes: Some(vec![QueryMode::BriefProject, QueryMode::FindFailures]),
                force_digest_rebuild: true,
            },
        )
        .await
        .unwrap();

        let payload: Value = parse_tool_payload(&result);
        assert_eq!(payload["status"].as_str(), Some("completed"));
        assert!(payload["digest_artifacts"]
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false));
    }
}
