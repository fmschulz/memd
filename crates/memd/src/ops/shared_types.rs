use super::*;

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
    /// When true, attach same-event sibling chunks to each ranked hit under
    /// `expanded_siblings`. The ranked hit list itself is unchanged.
    #[serde(default)]
    pub expand_event_siblings: bool,
    /// When true, prefix each result's `text` with its observed (event) date
    /// (`[YYYY-MM-DD]`) at recall, for chunks stored with an `event_time_ms`.
    /// Off by default; opt-in for temporal-QA consumers. Chunks without an
    /// observed time are returned unchanged.
    #[serde(default)]
    pub render_event_time: bool,
    /// Optional fixed wall-clock reference for recency, feedback, and outcome decay.
    /// This makes ranking reproducible over a frozen corpus without changing
    /// stored timestamps. It is not a historical snapshot: lifecycle visibility
    /// is evaluated at request time, and chunks written after this timestamp
    /// remain eligible. Searches with this field do not record usage or retrieval
    /// episodes. Normal interactive searches should omit it.
    #[serde(default)]
    pub ranking_time_ms: Option<i64>,
    /// Collapse ranked results that share a `source.uri`, keeping only the
    /// best-ranked chunk per source before the final trim to `k`. Large
    /// documents are stored as several chunks that all carry the parent
    /// URI; without collapsing, fragments of one document can crowd the
    /// top-k while other relevant sources never surface. Chunks without a
    /// source URI are never collapsed.
    #[serde(default)]
    pub dedupe_by_source: bool,
    /// Opt into a smaller response shape. Full mode remains the default.
    #[serde(default)]
    pub compact: bool,
    /// Approximate token budget for result payloads. Setting this also
    /// enables compact response packing.
    #[serde(default)]
    pub token_budget: Option<usize>,
    /// Override whether chunk text is included.
    #[serde(default)]
    pub include_text: Option<bool>,
    /// Override whether linked canonical artifacts are included.
    #[serde(default)]
    pub include_artifact: Option<bool>,
    /// Stable, non-sensitive task identifier used only for explicit outcome
    /// attribution. Stored as plaintext in the retrieval episode.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Stable, non-sensitive thread identifier used only for explicit outcome
    /// attribution. Stored as plaintext in the retrieval episode.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Outcome-policy mode. Defaults to shadow so outcome scores are measured
    /// without changing the served order.
    #[serde(default)]
    pub ranking_policy: Option<RankingPolicyMode>,
    /// Expanded candidate-pool multiplier for outcome shadow scoring.
    /// Defaults to 4 and the resulting pool is capped at 200 rows.
    #[serde(default)]
    pub candidate_multiplier: Option<usize>,
    #[serde(skip)]
    pub suppress_usage_event: bool,
    #[serde(skip)]
    pub suppress_retrieval_episode: bool,
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
            expand_event_siblings: false,
            render_event_time: false,
            ranking_time_ms: None,
            dedupe_by_source: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            task_id: None,
            thread_id: None,
            ranking_policy: None,
            candidate_multiplier: None,
            suppress_usage_event: false,
            suppress_retrieval_episode: false,
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
    /// Optional event time (ms since epoch): when the underlying event
    /// occurred, as distinct from ingestion time. Persisted as the chunk's
    /// `timestamp_observed` for bi-temporal retrieval and render-at-recall.
    #[serde(default)]
    pub event_time_ms: Option<i64>,
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
    /// Optional event time (ms since epoch) for this chunk. Same semantics
    /// as `AddParams::event_time_ms` — persisted as `timestamp_observed`.
    #[serde(default)]
    pub event_time_ms: Option<i64>,
    /// Optional ingestion mode label for this chunk. Same semantics as
    /// `AddParams::mode` — accepted now, consumed by Track E.
    #[serde(default)]
    pub mode: Option<String>,
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
    #[serde(default)]
    pub compact: bool,
    #[serde(default)]
    pub token_budget: Option<usize>,
    #[serde(default)]
    pub include_artifact: Option<bool>,
    #[serde(default)]
    pub include_matched_text: Option<bool>,
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

fn default_duplicate_limit() -> usize {
    10
}

/// Parameters for memory.health
#[derive(Debug, Deserialize)]
pub struct HealthParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub include_examples: bool,
    #[serde(default = "default_duplicate_limit")]
    pub duplicate_limit: usize,
    #[serde(default = "default_true")]
    pub include_recent: bool,
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

/// Parameters for memory.record_outcome.
#[derive(Debug, Deserialize)]
pub struct RecordOutcomeParams {
    #[serde(default)]
    pub tenant_id: String,
    pub episode_id: String,
    pub outcome: String,
    pub verifier_type: String,
    #[serde(default)]
    pub used_chunk_ids: Vec<String>,
    #[serde(default)]
    pub harmful_chunk_ids: Vec<String>,
    #[serde(default)]
    pub evidence_reference: Option<String>,
    #[serde(default)]
    pub event_time_ms: Option<i64>,
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
    /// Opaque identifier callers use to attribute a later verified outcome.
    /// Always serialized: `null` means no episode was recorded. Fixed-clock
    /// callers use that explicit null to acknowledge read-only replay.
    #[serde(default)]
    pub retrieval_episode_id: Option<String>,
    /// Inspectable policy diagnostics for the recorded episode.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ranking_policy: Option<RankingPolicyInfo>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub budget_info: Option<BudgetInfo>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scope_expansion: Option<ScopeExpansion>,
    /// Tier debug info (only present when debug_tiers=true)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tier_info: Option<TierDebugInfo>,
    /// Repair-loop diagnostics when a fallback query rewrite was attempted
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repair_info: Option<RepairInfo>,
    /// In-band scope/degradation report so agents can tell "no memory
    /// exists" from "memory exists one flag away" and detect degraded
    /// retrieval.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scope_status: Option<ScopeStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingPolicyInfo {
    pub version: String,
    pub mode: RankingPolicyMode,
    pub candidate_count: usize,
    pub shadow_order_changed: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetInfo {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub requested_budget: Option<usize>,
    pub estimated_output_tokens: usize,
    pub truncated: bool,
    #[serde(default)]
    pub omitted_fields: Vec<String>,
    pub dropped_result_count: usize,
    pub duplicate_drop_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginScope {
    pub requested_tenant_id: String,
    pub origin_tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub origin_project_id: Option<String>,
    #[serde(default)]
    pub alias_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeExpansion {
    pub requested_tenant_id: String,
    pub requested_project_id: String,
    #[serde(default)]
    pub aliases: Vec<OriginScope>,
}

/// Retrieval scope and degradation report attached to every
/// memory.search / agent-context payload. Wrong-scope and degraded
/// retrieval previously collapsed into the same empty-but-successful
/// response; this makes them distinguishable in-band.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeStatus {
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    /// "hybrid" for ranked semantic retrieval, "text_fallback" when
    /// queries degrade to substring matching at constant score.
    pub retrieval_mode: String,
    /// Tenant-wide candidate hits outside the requested project.
    /// Counted lazily, only when a project-scoped search returns fewer
    /// than k results.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub wider_scope_hits: Option<usize>,
    /// Exact widening guidance when wider_scope_hits is set.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub widen_hint: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

/// Single chunk in search results.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkResult {
    pub chunk_id: String,
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub origin: Option<OriginScope>,
    /// Opt-in same-event context returned by `memory.search` when
    /// `expand_event_siblings=true`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub expanded_siblings: Vec<ChunkResult>,
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
    /// Backward-compatible id for the first stored chunk.
    pub chunk_id: String,
    /// Every physical chunk stored for this logical add, including split
    /// children. The primary `chunk_id` is always first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_chunk_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deduped_existing_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_after_ms: Option<i64>,
    /// Set when this write created a brand-new tenant, so a mistyped
    /// --tenant-id forking a fresh silo is visible in the payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_tenant: Option<bool>,
}

/// Result of a batch add operation
#[derive(Debug, Serialize, Deserialize)]
pub struct AddBatchResult {
    pub chunk_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_decisions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deduped_existing_ids: Option<Vec<Option<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_decisions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_reasons: Option<Vec<String>>,
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
    pub artifact_id: String,
    pub task_id: String,
    pub artifact_kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact: Option<TaskArtifact>,
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub origin: Option<OriginScope>,
}

/// Result of artifact.search.
#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactSearchResult {
    pub results: Vec<ArtifactSearchHit>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub budget_info: Option<BudgetInfo>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scope_expansion: Option<ScopeExpansion>,
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

/// Result of an explicit retrieval-outcome operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordOutcomeResult {
    pub event_id: String,
    pub episode_id: String,
    pub stored: bool,
    pub ranking_eligible: bool,
}

/// Result of memory.consolidate_episode
#[derive(Debug, Serialize, Deserialize)]
pub struct ConsolidateEpisodeResult {
    pub summary_chunk_id: String,
    pub source_chunk_count: usize,
    pub retained_source_chunks: bool,
    pub run_id: String,
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
    #[serde(default)]
    pub active_chunks: usize,
    #[serde(default)]
    pub candidate_chunks: usize,
    pub deleted_chunks: usize,
    /// Backward-compatible active chunk-type counts.
    pub chunk_types: HashMap<String, usize>,
    #[serde(default)]
    pub chunk_types_active: HashMap<String, usize>,
    #[serde(default)]
    pub chunk_types_deleted: HashMap<String, usize>,
    #[serde(default)]
    pub chunk_types_all: HashMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub disk_stats: Option<DiskStatsResult>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub compaction: Option<CompactionStatsResult>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthScopeResult {
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub aliases: Vec<OriginScope>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkTypeHealthResult {
    pub active: HashMap<String, usize>,
    pub all: HashMap<String, usize>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct LatencyHealthResult {
    pub recent_search_count: usize,
    pub p50_total_ms: u64,
    pub p95_total_ms: u64,
    pub p99_total_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryHealthResult {
    pub scope: HealthScopeResult,
    pub counts: HealthCounts,
    pub chunk_types: ChunkTypeHealthResult,
    pub duplicates: DuplicateHealth,
    pub index_coverage: IndexCoverageHealth,
    pub payload: PayloadHealth,
    pub latency: LatencyHealthResult,
    #[serde(default)]
    pub warnings: Vec<String>,
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
