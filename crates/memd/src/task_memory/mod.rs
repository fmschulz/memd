//! Scientific/task-oriented memory schema and retrieval projections.
//!
//! Keeps the canonical task artifact envelope separate from the retrieval
//! projection chunks stored in the main search engine.

pub mod digest_dirty;
mod digests;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;
use uuid::Uuid;

use crate::types::{ChunkType, MemoryChunk, ProjectId, PromotionState, Source, TenantId};

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_v7_id() -> String {
    Uuid::now_v7().to_string()
}

pub(crate) fn sanitize_tag_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

pub use digests::{
    build_library_digest_artifact, build_project_brief_digest_artifact, build_project_brief_view,
    build_task_resume_digest_artifact, build_task_resume_view, infer_decision_items,
    infer_evidence_items, infer_failure_items, infer_highlight_items, stable_digest_identity,
    DecisionViewItem, EvidenceViewItem, FailureViewItem, HighlightViewItem, ProjectBriefView,
    RunDigestItem, TaskResumeView, DIGEST_ROLE_DECISION_LIBRARY, DIGEST_ROLE_EVIDENCE_LIBRARY,
    DIGEST_ROLE_FAILURE_LIBRARY, DIGEST_ROLE_HIGHLIGHT_LIBRARY, DIGEST_ROLE_PROJECT_BRIEF,
    DIGEST_ROLE_TASK_RESUME,
};

fn join_lines(items: &[String]) -> String {
    items.join("; ")
}

/// Canonical task artifact kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    TaskStart,
    TaskProgress,
    RunStart,
    RunFinish,
    Evidence,
    Review,
    Revision,
    Verification,
    Decision,
    Digest,
    TaskFinish,
    /// LLM-authored concept / entity page. Carries a `content` markdown
    /// body and non-empty `grounding_refs` (enforced at the MCP
    /// boundary). Trust semantics match other canonical records: a
    /// fresh `WikiPage` sits at `TrustTier::CanonicalRecord` forever;
    /// presence of distinct-writer `Verification` children whose
    /// `reply_to_artifact_id` targets the page signals "verified"
    /// state in the rendered wiki, but never promotes the page's own
    /// `promotion_state`.
    WikiPage,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskStart => "task_start",
            Self::TaskProgress => "task_progress",
            Self::RunStart => "run_start",
            Self::RunFinish => "run_finish",
            Self::Evidence => "evidence",
            Self::Review => "review",
            Self::Revision => "revision",
            Self::Verification => "verification",
            Self::Decision => "decision",
            Self::Digest => "digest",
            Self::TaskFinish => "task_finish",
            Self::WikiPage => "wiki_page",
        }
    }
}

impl FromStr for ArtifactKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "task_start" => Ok(Self::TaskStart),
            "task_progress" => Ok(Self::TaskProgress),
            "run_start" => Ok(Self::RunStart),
            "run_finish" => Ok(Self::RunFinish),
            "evidence" => Ok(Self::Evidence),
            "review" => Ok(Self::Review),
            "revision" => Ok(Self::Revision),
            "verification" => Ok(Self::Verification),
            "decision" => Ok(Self::Decision),
            "digest" => Ok(Self::Digest),
            "task_finish" => Ok(Self::TaskFinish),
            "wiki_page" => Ok(Self::WikiPage),
            _ => Err(format!(
                "invalid artifact_kind '{}', must be one of: task_start, task_progress, run_start, run_finish, evidence, review, revision, verification, decision, digest, task_finish, wiki_page",
                value
            )),
        }
    }
}

/// Projection chunk kind derived from a canonical artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionKind {
    TaskGoal,
    TaskSummary,
    Run,
    Evidence,
    Decision,
    Digest,
    Worked,
    Failed,
    Validation,
}

impl ProjectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskGoal => "task_goal",
            Self::TaskSummary => "task_summary",
            Self::Run => "run",
            Self::Evidence => "evidence",
            Self::Decision => "decision",
            Self::Digest => "digest",
            Self::Worked => "worked",
            Self::Failed => "failed",
            Self::Validation => "validation",
        }
    }
}

/// Trust tier exposed at the MCP boundary.
///
/// Semantic retrieval can suggest candidates, but canonical artifacts remain
/// the trust anchor and explicit verification artifacts sit above both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    #[default]
    SemanticCandidate,
    CanonicalRecord,
    CompiledDigestHint,
    VerifiedRecord,
}

/// Dataset reference attached to a task artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetRef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
}

impl DatasetRef {
    pub fn display_name(&self) -> String {
        match self.version.as_deref() {
            Some(version) if !version.is_empty() => format!("{} ({})", self.name, version),
            _ => self.name.clone(),
        }
    }

    pub fn key(&self) -> String {
        format!(
            "{}::{}",
            sanitize_tag_value(&self.name),
            sanitize_tag_value(self.version.as_deref().unwrap_or(""))
        )
    }
}

/// Entity reference attached to a task artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRef {
    pub name: String,
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub role: Option<String>,
}

impl EntityRef {
    pub fn display_name(&self) -> String {
        match self.role.as_deref() {
            Some(role) if !role.is_empty() => {
                format!("{} [{}; role={}]", self.name, self.entity_type, role)
            }
            _ => format!("{} [{}]", self.name, self.entity_type),
        }
    }

    pub fn key(&self) -> String {
        format!(
            "{}::{}",
            sanitize_tag_value(&self.name),
            sanitize_tag_value(&self.entity_type)
        )
    }
}

/// Contributor metadata attached to a knowledge artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributorRef {
    pub contributor_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub contribution: Option<String>,
}

/// Optional provenance for a canonical task artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProvenance {
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
    pub tool_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

impl From<&TaskProvenance> for Source {
    fn from(value: &TaskProvenance) -> Self {
        Self {
            uri: value.uri.clone(),
            repo: value.repo.clone(),
            commit: value.commit.clone(),
            path: value.path.clone(),
            tool_name: value.tool_name.clone(),
            tool_call_id: value.tool_call_id.clone(),
        }
    }
}

/// Canonical scientific/task artifact envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskArtifact {
    pub artifact_id: String,
    pub artifact_kind: ArtifactKind,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_task_id: Option<String>,
    pub tenant_id: TenantId,
    #[serde(default)]
    pub project_id: ProjectId,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub challenge_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reply_to_artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub relation_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub motivation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hypothesis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scientific_question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub method_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub summary: Option<String>,
    /// Optional full-markdown body. Only populated for
    /// `ArtifactKind::WikiPage` — the MCP validator rejects non-empty
    /// `content` on every other kind. Nullable so existing artifact
    /// rows round-trip unchanged.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub evidence_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub supports_claim: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub what_worked: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub what_failed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertainty: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributors: Vec<ContributorRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dataset_refs: Vec<DatasetRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_refs: Vec<EntityRef>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parameters: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metrics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub why_chosen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub requested_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verification_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub compute_budget: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cost_actual: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data_access_level: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub approval_state: Option<String>,
    #[serde(default)]
    pub promotion_state: PromotionState,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub digest_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_updated_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "TaskProvenance::is_empty")]
    pub provenance: TaskProvenance,
    pub timestamp_created: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub timestamp_observed: Option<i64>,
}

impl TaskProvenance {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

impl TaskArtifact {
    pub fn new(
        artifact_kind: ArtifactKind,
        tenant_id: TenantId,
        task_id: impl Into<String>,
    ) -> Self {
        let now_ms = current_time_ms();
        Self {
            artifact_id: new_v7_id(),
            artifact_kind,
            task_id: task_id.into(),
            parent_task_id: None,
            tenant_id,
            project_id: ProjectId::none(),
            agent_id: None,
            session_id: None,
            status: None,
            artifact_role: None,
            challenge_id: None,
            thread_id: None,
            reply_to_artifact_id: None,
            relation_kind: None,
            goal: None,
            motivation: None,
            hypothesis: None,
            scientific_question: None,
            method_summary: None,
            summary: None,
            content: None,
            evidence_kind: None,
            supports_claim: None,
            blockers: Vec::new(),
            what_worked: Vec::new(),
            what_failed: Vec::new(),
            validation: Vec::new(),
            uncertainty: Vec::new(),
            followups: Vec::new(),
            expected_outputs: Vec::new(),
            related_artifact_ids: Vec::new(),
            contributors: Vec::new(),
            dataset_refs: Vec::new(),
            entity_refs: Vec::new(),
            tool_name: None,
            tool_version: None,
            command: None,
            parameters: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            metrics: None,
            why_chosen: None,
            confidence: None,
            requested_action: None,
            verification_status: None,
            compute_budget: None,
            cost_actual: None,
            data_access_level: None,
            policy_tags: Vec::new(),
            allowed_tools: Vec::new(),
            approval_state: None,
            promotion_state: PromotionState::Raw,
            digest_key: None,
            source_updated_at_ms: None,
            provenance: TaskProvenance::default(),
            timestamp_created: now_ms,
            timestamp_observed: None,
        }
    }

    pub fn new_task_start(tenant_id: TenantId) -> Self {
        let mut artifact = Self::new(ArtifactKind::TaskStart, tenant_id, new_v7_id());
        artifact.status = Some("in_progress".to_string());
        artifact
    }

    pub fn new_task_finish(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::TaskFinish, tenant_id, task_id);
        artifact.status = Some("completed".to_string());
        artifact
    }

    pub fn new_task_progress(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::TaskProgress, tenant_id, task_id);
        artifact.status = Some("in_progress".to_string());
        artifact
    }

    pub fn new_run_start(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::RunStart, tenant_id, task_id);
        artifact.status = Some("started".to_string());
        artifact
    }

    pub fn new_run_finish(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::RunFinish, tenant_id, task_id);
        artifact.status = Some("completed".to_string());
        artifact
    }

    pub fn new_evidence(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        Self::new(ArtifactKind::Evidence, tenant_id, task_id)
    }

    pub fn new_review(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::Review, tenant_id, task_id);
        artifact.status = Some("recorded".to_string());
        artifact
    }

    pub fn new_revision(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::Revision, tenant_id, task_id);
        artifact.status = Some("recorded".to_string());
        artifact
    }

    pub fn new_verification(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::Verification, tenant_id, task_id);
        artifact.status = Some("recorded".to_string());
        artifact
    }

    pub fn new_decision(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::Decision, tenant_id, task_id);
        artifact.status = Some("recorded".to_string());
        artifact
    }

    /// Canonical constructor for an LLM-authored concept / entity page.
    ///
    /// The caller is still responsible for populating `content`,
    /// `summary`, `artifact_role`, `related_artifact_ids` (used as
    /// grounding refs at the MCP boundary) before the page is handed to
    /// `artifact.create`. The handler validates those fields before
    /// storing the row — see `crates/memd/src/mcp/handlers.rs`.
    pub fn new_wiki_page(tenant_id: TenantId, task_id: impl Into<String>) -> Self {
        let mut artifact = Self::new(ArtifactKind::WikiPage, tenant_id, task_id);
        artifact.status = Some("authored".to_string());
        artifact
    }

    pub fn new_digest(
        tenant_id: TenantId,
        task_id: impl Into<String>,
        digest_key: impl Into<String>,
        artifact_role: impl Into<String>,
    ) -> Self {
        let mut artifact = Self::new(ArtifactKind::Digest, tenant_id, task_id);
        artifact.status = Some("generated".to_string());
        artifact.artifact_role = Some(artifact_role.into());
        artifact.digest_key = Some(digest_key.into());
        artifact
    }

    pub fn thread_key(&self) -> &str {
        self.thread_id.as_deref().unwrap_or(&self.task_id)
    }

    pub fn event_summary(&self) -> Option<String> {
        if let Some(summary) = self.summary.as_ref().filter(|s| !s.trim().is_empty()) {
            return Some(summary.clone());
        }
        if let Some(goal) = self.goal.as_ref().filter(|s| !s.trim().is_empty()) {
            return Some(goal.clone());
        }
        if !self.what_worked.is_empty() {
            return Some(join_lines(&self.what_worked));
        }
        if !self.what_failed.is_empty() {
            return Some(join_lines(&self.what_failed));
        }
        if let Some(command) = self.command.as_ref().filter(|s| !s.trim().is_empty()) {
            return Some(command.clone());
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub tenant_id: TenantId,
    #[serde(default)]
    pub project_id: ProjectId,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scientific_question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hypothesis: Option<String>,
    pub last_artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub started_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub finished_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

/// A retrieval chunk derived from the canonical artifact.
#[derive(Debug, Clone)]
pub struct TaskProjection {
    pub kind: ProjectionKind,
    pub chunk: MemoryChunk,
}

/// Write result for one stored task artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskArtifactWriteResult {
    pub task_id: String,
    pub artifact_id: String,
    pub projection_chunk_ids: Vec<String>,
}

/// Exact filters supported by task-aware search.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSearchFilters {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact_kind: Option<ArtifactKind>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub challenge_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reply_to_artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dataset_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dataset_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub entity_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub entity_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub requested_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verification_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub relation_kind: Option<String>,
}

fn base_projection_tags(artifact: &TaskArtifact) -> Vec<String> {
    let artifact_promotion_state = derive_artifact_promotion_state(artifact);
    let mut tags = vec![
        format!("task:id:{}", sanitize_tag_value(&artifact.task_id)),
        format!(
            "task:artifact:{}",
            sanitize_tag_value(&artifact.artifact_id)
        ),
        format!("task:kind:{}", artifact.artifact_kind.as_str()),
        format!(
            "task:promotion:{}",
            sanitize_tag_value(&artifact_promotion_state.to_string())
        ),
    ];
    if let Some(status) = artifact.status.as_ref().filter(|s| !s.trim().is_empty()) {
        tags.push(format!("task:status:{}", sanitize_tag_value(status)));
    }
    if let Some(challenge_id) = artifact
        .challenge_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        tags.push(format!(
            "task:challenge:{}",
            sanitize_tag_value(challenge_id)
        ));
    }
    if let Some(thread_id) = artifact
        .thread_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        tags.push(format!("task:thread:{}", sanitize_tag_value(thread_id)));
    }
    if let Some(artifact_role) = artifact
        .artifact_role
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        tags.push(format!("task:role:{}", sanitize_tag_value(artifact_role)));
    }
    if let Some(digest_key) = artifact
        .digest_key
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        tags.push(format!("task:digest:{}", sanitize_tag_value(digest_key)));
    }
    if let Some(verification_status) = artifact
        .verification_status
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        tags.push(format!(
            "task:verification:{}",
            sanitize_tag_value(verification_status)
        ));
    }
    for dataset in &artifact.dataset_refs {
        tags.push(format!("task:dataset:{}", dataset.key()));
    }
    for entity in &artifact.entity_refs {
        tags.push(format!("task:entity:{}", entity.key()));
    }
    tags
}

pub fn derive_artifact_trust_tier(artifact: &TaskArtifact) -> TrustTier {
    // `VerifiedRecord` is reserved for artifacts the server has
    // explicitly promoted via `promote_if_countersigned` (see
    // `mcp::handlers`). Agent-supplied `verification_status` /
    // `approval_state` / `ArtifactKind::Verification` previously unlocked
    // the tier directly — that was a self-labelling laundering channel
    // because any caller could write an artifact that stamped itself
    // verified. The fix: the only signal that promotes trust is the
    // server-computed `PromotionState::Verified`.
    if artifact.promotion_state == PromotionState::Verified {
        return TrustTier::VerifiedRecord;
    }

    if artifact.artifact_kind == ArtifactKind::Digest {
        return TrustTier::CompiledDigestHint;
    }

    TrustTier::CanonicalRecord
}

pub fn derive_chunk_trust_tier(artifact: Option<&TaskArtifact>) -> TrustTier {
    artifact
        .map(derive_artifact_trust_tier)
        .unwrap_or(TrustTier::SemanticCandidate)
}

/// Pure derivation of promotion state from artifact content.
///
/// This function intentionally NEVER returns `PromotionState::Verified`.
/// Verification is a server-side decision made by
/// `mcp::handlers::promote_if_countersigned` after looking up the
/// reply-to parent and checking distinct-writer countersignature; it is
/// the only path that should produce a `Verified` promotion in v0.3.1+.
///
/// Historically this function honoured agent-supplied fields
/// (`verification_status`, `approval_state`, `ArtifactKind::Verification`,
/// non-empty `validation`). That allowed any single agent to self-label
/// as verified, which turned the trust boundary into marketing. Those
/// branches are removed.
pub fn derive_artifact_promotion_state(artifact: &TaskArtifact) -> PromotionState {
    match artifact.artifact_kind {
        ArtifactKind::Digest => PromotionState::Summarized,
        ArtifactKind::TaskStart
        | ArtifactKind::TaskProgress
        | ArtifactKind::RunStart
        | ArtifactKind::RunFinish
        | ArtifactKind::Evidence
        | ArtifactKind::Review
        | ArtifactKind::Revision
        | ArtifactKind::Verification
        | ArtifactKind::Decision
        | ArtifactKind::TaskFinish
        | ArtifactKind::WikiPage => PromotionState::Canonical,
    }
}

pub fn derive_chunk_promotion_state(
    artifact: &TaskArtifact,
    projection_kind: ProjectionKind,
) -> PromotionState {
    match derive_artifact_promotion_state(artifact) {
        PromotionState::Verified => match projection_kind {
            ProjectionKind::Validation
            | ProjectionKind::Evidence
            | ProjectionKind::Decision
            | ProjectionKind::Digest => PromotionState::Verified,
            _ => PromotionState::Summarized,
        },
        PromotionState::Canonical => match projection_kind {
            ProjectionKind::TaskGoal
            | ProjectionKind::Run
            | ProjectionKind::Evidence
            | ProjectionKind::Decision => PromotionState::Canonical,
            _ => PromotionState::Summarized,
        },
        PromotionState::Summarized => PromotionState::Summarized,
        PromotionState::Raw => PromotionState::Raw,
    }
}

fn build_projection_chunk(
    artifact: &TaskArtifact,
    kind: ProjectionKind,
    chunk_type: ChunkType,
    text: String,
) -> TaskProjection {
    let tool_name = artifact.provenance.tool_name.clone().or_else(|| {
        Some(match artifact.artifact_kind {
            ArtifactKind::TaskStart => "task.start".to_string(),
            ArtifactKind::TaskFinish => "task.finish".to_string(),
            ArtifactKind::TaskProgress => "task.progress".to_string(),
            ArtifactKind::RunStart => "task.run_start".to_string(),
            ArtifactKind::RunFinish => "task.run_finish".to_string(),
            ArtifactKind::Evidence => "task.add_evidence".to_string(),
            ArtifactKind::Review
            | ArtifactKind::Revision
            | ArtifactKind::Verification
            | ArtifactKind::Decision
            | ArtifactKind::WikiPage => "artifact.create".to_string(),
            ArtifactKind::Digest => "memory.compact".to_string(),
        })
    });
    let mut source = Source::from(&artifact.provenance);
    source.tool_name = tool_name;

    let mut chunk = MemoryChunk::new(artifact.tenant_id.clone(), text, chunk_type)
        .with_project(artifact.project_id.clone())
        .with_source(source)
        .with_promotion_state(derive_chunk_promotion_state(artifact, kind));
    if let Some(agent_id) = artifact.agent_id.as_deref() {
        chunk = chunk.with_agent(agent_id.to_string());
    }
    let mut tags = base_projection_tags(artifact);
    tags.push(format!("task:projection:{}", kind.as_str()));
    chunk = chunk.with_tags(tags);
    chunk.timestamp_created = artifact.timestamp_created;
    chunk.timestamp_observed = artifact.timestamp_observed;

    TaskProjection { kind, chunk }
}

/// Produce just the base summary projection chunk for a canonical
/// artifact — no fanout.
///
/// Phase 2.5 write-amplification cut: high-frequency handlers
/// (`task.progress`, `task.run_start`, `task.run_finish`,
/// `task.add_evidence`) now emit one projection per call instead of
/// 4-7. The base summary already carries every field the retrieval
/// layer needs (task_id, kind, status, role, summary, goal,
/// verification_status, …); the library-digest pipeline reads from
/// canonical artifacts directly, not projection chunks, so library
/// quality is unaffected. `task.start` and `task.finish` keep the
/// full fanout via `build_task_projections` because their richer
/// decomposition is genuinely useful for onboarding-style retrieval.
pub fn build_task_projections_minimal(artifact: &TaskArtifact) -> Vec<TaskProjection> {
    let mut all = build_task_projections(artifact);
    all.truncate(1);
    all
}

/// Build retrieval-friendly projection chunks from a canonical task artifact.
pub fn build_task_projections(artifact: &TaskArtifact) -> Vec<TaskProjection> {
    let dataset_names: Vec<String> = artifact
        .dataset_refs
        .iter()
        .map(DatasetRef::display_name)
        .collect();
    let entity_names: Vec<String> = artifact
        .entity_refs
        .iter()
        .map(EntityRef::display_name)
        .collect();
    let mut projections = Vec::new();

    let mut summary_lines = vec![format!(
        "Task {} {} for task {}.",
        artifact.artifact_kind.as_str(),
        artifact
            .status
            .as_deref()
            .map(|s| format!("status {}", s))
            .unwrap_or_else(|| "recorded".to_string()),
        artifact.task_id
    )];
    if let Some(role) = artifact.artifact_role.as_ref() {
        summary_lines.push(format!("Artifact role: {}", role));
    }
    if let Some(challenge_id) = artifact.challenge_id.as_ref() {
        summary_lines.push(format!("Challenge: {}", challenge_id));
    }
    if let Some(thread_id) = artifact.thread_id.as_ref() {
        summary_lines.push(format!("Thread: {}", thread_id));
    }
    if let Some(reply_to) = artifact.reply_to_artifact_id.as_ref() {
        summary_lines.push(format!("Reply to: {}", reply_to));
    }
    if let Some(goal) = artifact.goal.as_ref() {
        summary_lines.push(format!("Goal: {}", goal));
    }
    if let Some(question) = artifact.scientific_question.as_ref() {
        summary_lines.push(format!("Scientific question: {}", question));
    }
    if !dataset_names.is_empty() {
        summary_lines.push(format!("Datasets: {}", dataset_names.join(", ")));
    }
    if !entity_names.is_empty() {
        summary_lines.push(format!("Entities: {}", entity_names.join(", ")));
    }
    if let Some(requested_action) = artifact.requested_action.as_ref() {
        summary_lines.push(format!("Requested action: {}", requested_action));
    }
    if let Some(verification_status) = artifact.verification_status.as_ref() {
        summary_lines.push(format!("Verification status: {}", verification_status));
    }
    if let Some(summary) = artifact.event_summary() {
        summary_lines.push(format!("Summary: {}", summary));
    }
    projections.push(build_projection_chunk(
        artifact,
        match artifact.artifact_kind {
            ArtifactKind::TaskStart => ProjectionKind::TaskGoal,
            ArtifactKind::RunStart | ArtifactKind::RunFinish => ProjectionKind::Run,
            ArtifactKind::Evidence => ProjectionKind::Evidence,
            ArtifactKind::Decision => ProjectionKind::Decision,
            ArtifactKind::Digest => ProjectionKind::Digest,
            ArtifactKind::Verification => ProjectionKind::Validation,
            _ => ProjectionKind::TaskSummary,
        },
        match artifact.artifact_kind {
            ArtifactKind::TaskStart => ChunkType::Plan,
            ArtifactKind::RunStart | ArtifactKind::RunFinish => ChunkType::Trace,
            ArtifactKind::Evidence => ChunkType::Research,
            ArtifactKind::Decision => ChunkType::Decision,
            ArtifactKind::Digest => ChunkType::Summary,
            ArtifactKind::Verification => ChunkType::Summary,
            _ => ChunkType::Summary,
        },
        summary_lines.join("\n"),
    ));

    if artifact.artifact_kind == ArtifactKind::TaskStart {
        let mut goal_lines = Vec::new();
        if let Some(goal) = artifact.goal.as_ref() {
            goal_lines.push(format!("Goal: {}", goal));
        }
        if let Some(motivation) = artifact.motivation.as_ref() {
            goal_lines.push(format!("Motivation: {}", motivation));
        }
        if let Some(hypothesis) = artifact.hypothesis.as_ref() {
            goal_lines.push(format!("Hypothesis: {}", hypothesis));
        }
        if !artifact.expected_outputs.is_empty() {
            goal_lines.push(format!(
                "Expected outputs: {}",
                artifact.expected_outputs.join(", ")
            ));
        }
        if !goal_lines.is_empty() {
            projections.push(build_projection_chunk(
                artifact,
                ProjectionKind::TaskGoal,
                ChunkType::Plan,
                goal_lines.join("\n"),
            ));
        }
    }

    if matches!(
        artifact.artifact_kind,
        ArtifactKind::TaskProgress | ArtifactKind::TaskFinish
    ) && (!artifact.blockers.is_empty() || !artifact.followups.is_empty())
    {
        let mut progress_lines = Vec::new();
        if !artifact.blockers.is_empty() {
            progress_lines.push(format!("Blockers: {}", join_lines(&artifact.blockers)));
        }
        if !artifact.followups.is_empty() {
            progress_lines.push(format!("Next steps: {}", join_lines(&artifact.followups)));
        }
        projections.push(build_projection_chunk(
            artifact,
            ProjectionKind::TaskSummary,
            ChunkType::Summary,
            progress_lines.join("\n"),
        ));
    }

    if matches!(
        artifact.artifact_kind,
        ArtifactKind::RunStart | ArtifactKind::RunFinish
    ) {
        let mut run_lines = Vec::new();
        if let Some(tool_name) = artifact.tool_name.as_ref() {
            run_lines.push(format!("Tool: {}", tool_name));
        }
        if let Some(tool_version) = artifact.tool_version.as_ref() {
            run_lines.push(format!("Tool version: {}", tool_version));
        }
        if let Some(command) = artifact.command.as_ref() {
            run_lines.push(format!("Command: {}", command));
        }
        if let Some(why_chosen) = artifact.why_chosen.as_ref() {
            run_lines.push(format!("Why chosen: {}", why_chosen));
        }
        if !artifact.inputs.is_empty() {
            run_lines.push(format!("Inputs: {}", artifact.inputs.join(", ")));
        }
        if !artifact.outputs.is_empty() {
            run_lines.push(format!("Outputs: {}", artifact.outputs.join(", ")));
        }
        if let Some(parameters) = artifact.parameters.as_ref() {
            run_lines.push(format!("Parameters: {}", parameters));
        }
        if let Some(metrics) = artifact.metrics.as_ref() {
            run_lines.push(format!("Metrics: {}", metrics));
        }
        if !run_lines.is_empty() {
            projections.push(build_projection_chunk(
                artifact,
                ProjectionKind::Run,
                ChunkType::Trace,
                run_lines.join("\n"),
            ));
        }
    }

    if artifact.artifact_kind == ArtifactKind::Evidence {
        let mut evidence_lines = Vec::new();
        if let Some(evidence_kind) = artifact.evidence_kind.as_ref() {
            evidence_lines.push(format!("Evidence kind: {}", evidence_kind));
        }
        if let Some(supports_claim) = artifact.supports_claim {
            evidence_lines.push(format!("Supports claim: {}", supports_claim));
        }
        if let Some(metrics) = artifact.metrics.as_ref() {
            evidence_lines.push(format!("Metrics: {}", metrics));
        }
        if !evidence_lines.is_empty() {
            projections.push(build_projection_chunk(
                artifact,
                ProjectionKind::Evidence,
                ChunkType::Research,
                evidence_lines.join("\n"),
            ));
        }
    }

    if !artifact.what_worked.is_empty() {
        projections.push(build_projection_chunk(
            artifact,
            ProjectionKind::Worked,
            ChunkType::Summary,
            format!(
                "What worked for task {}: {}",
                artifact.task_id,
                join_lines(&artifact.what_worked)
            ),
        ));
    }

    if !artifact.what_failed.is_empty() || !artifact.uncertainty.is_empty() {
        let mut failed_lines = Vec::new();
        if !artifact.what_failed.is_empty() {
            failed_lines.push(format!(
                "What failed for task {}: {}",
                artifact.task_id,
                join_lines(&artifact.what_failed)
            ));
        }
        if !artifact.uncertainty.is_empty() {
            failed_lines.push(format!(
                "Uncertainty: {}",
                join_lines(&artifact.uncertainty)
            ));
        }
        projections.push(build_projection_chunk(
            artifact,
            ProjectionKind::Failed,
            ChunkType::Research,
            failed_lines.join("\n"),
        ));
    }

    if !artifact.validation.is_empty() || !artifact.followups.is_empty() {
        let mut validation_lines = Vec::new();
        if !artifact.validation.is_empty() {
            validation_lines.push(format!("Validation: {}", join_lines(&artifact.validation)));
        }
        if !artifact.followups.is_empty() {
            validation_lines.push(format!("Followups: {}", join_lines(&artifact.followups)));
        }
        if let Some(confidence) = artifact.confidence {
            validation_lines.push(format!("Confidence: {:.3}", confidence));
        }
        if let Some(verification_status) = artifact.verification_status.as_ref() {
            validation_lines.push(format!("Verification status: {}", verification_status));
        }
        if let Some(approval_state) = artifact.approval_state.as_ref() {
            validation_lines.push(format!("Approval state: {}", approval_state));
        }
        projections.push(build_projection_chunk(
            artifact,
            ProjectionKind::Validation,
            ChunkType::Summary,
            validation_lines.join("\n"),
        ));
    }

    projections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProjectId;

    #[test]
    fn task_artifact_round_trips_through_json() {
        let tenant_id = TenantId::new("science_team").unwrap();
        let mut artifact = TaskArtifact::new_task_start(tenant_id);
        artifact.project_id = ProjectId::new(Some("proj_alpha".to_string()));
        artifact.goal = Some("Characterize the regulator under stress".to_string());
        artifact.motivation = Some("The pathway response is unresolved".to_string());
        artifact.hypothesis = Some("Gene X drives the phenotype".to_string());
        artifact.scientific_question = Some("Does Gene X increase expression?".to_string());
        artifact.dataset_refs = vec![DatasetRef {
            name: "rna_seq".to_string(),
            version: Some("v1".to_string()),
            description: None,
        }];
        artifact.expected_outputs = vec!["differential expression table".to_string()];

        let json = serde_json::to_string(&artifact).unwrap();
        let parsed: TaskArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_id, artifact.task_id);
        assert_eq!(parsed.goal, artifact.goal);
        assert_eq!(parsed.dataset_refs, artifact.dataset_refs);
    }

    #[test]
    fn task_finish_builds_worked_and_failed_projections() {
        let tenant_id = TenantId::new("science_team").unwrap();
        let mut artifact = TaskArtifact::new_task_finish(tenant_id, "task-123");
        artifact.goal = Some("Validate the mutant phenotype".to_string());
        artifact.what_worked = vec!["QC thresholds removed low-depth samples".to_string()];
        artifact.what_failed =
            vec!["The first aligner configuration over-trimmed reads".to_string()];
        artifact.validation = vec!["Re-run reproduced the differential hits".to_string()];
        artifact.uncertainty = vec!["One replicate remains underpowered".to_string()];
        artifact.followups = vec!["Collect one more replicate".to_string()];
        artifact.confidence = Some(0.82);

        let projections = build_task_projections(&artifact);
        assert!(projections.iter().any(|p| p.kind == ProjectionKind::Worked));
        assert!(projections.iter().any(|p| p.kind == ProjectionKind::Failed));
        assert!(projections
            .iter()
            .any(|p| p.kind == ProjectionKind::Validation));
    }

    #[test]
    fn projection_tags_include_task_identity() {
        let tenant_id = TenantId::new("science_team").unwrap();
        let artifact = TaskArtifact::new_task_start(tenant_id);

        let projections = build_task_projections(&artifact);
        assert!(!projections.is_empty());
        let tags = &projections[0].chunk.tags;
        assert!(tags.iter().any(|tag| tag.starts_with("task:id:")));
        assert!(tags.iter().any(|tag| tag.starts_with("task:artifact:")));
        assert!(tags.iter().any(|tag| tag.starts_with("task:kind:")));
    }

    #[test]
    fn parse_artifact_kind_accepts_snake_case_values() {
        assert_eq!(
            ArtifactKind::from_str("task_progress").unwrap(),
            ArtifactKind::TaskProgress
        );
        assert_eq!(
            ArtifactKind::from_str("run_finish").unwrap(),
            ArtifactKind::RunFinish
        );
        assert_eq!(
            ArtifactKind::from_str("decision").unwrap(),
            ArtifactKind::Decision
        );
        assert_eq!(
            ArtifactKind::from_str("digest").unwrap(),
            ArtifactKind::Digest
        );
        assert_eq!(
            ArtifactKind::from_str("wiki_page").unwrap(),
            ArtifactKind::WikiPage
        );
        assert!(ArtifactKind::from_str("unknown").is_err());
    }

    /// v2 Phase 0: `wiki_page` artifact kind round-trips through its
    /// snake_case name and through JSON with a populated `content`
    /// field. Both directions matter: the MCP wire format is the JSON
    /// name; the storage layer round-trips through JSON.
    #[test]
    fn wiki_page_artifact_kind_roundtrips_and_carries_content() {
        assert_eq!(ArtifactKind::WikiPage.as_str(), "wiki_page");
        let err = ArtifactKind::from_str("not_a_kind").unwrap_err();
        assert!(
            err.contains("wiki_page"),
            "FromStr error must list the new wiki_page kind; got: {err}"
        );

        let tenant = TenantId::new("wiki_author").unwrap();
        let mut page = TaskArtifact::new_wiki_page(tenant, "task-wiki-1");
        page.project_id = ProjectId::new(Some("memd".to_string()));
        page.artifact_role = Some("concept".to_string());
        page.summary = Some("Explains the verification boundary.".to_string());
        page.content = Some(
            "# Verification boundary\n\nClaims require distinct-writer countersignatures."
                .to_string(),
        );
        page.related_artifact_ids = vec!["0199...".to_string()];

        let json = serde_json::to_string(&page).expect("wiki_page should serialize");
        assert!(json.contains("\"artifact_kind\":\"wiki_page\""));
        assert!(json.contains("\"content\":"));

        let parsed: TaskArtifact =
            serde_json::from_str(&json).expect("wiki_page should round-trip");
        assert_eq!(parsed.artifact_kind, ArtifactKind::WikiPage);
        assert_eq!(parsed.content, page.content);
        assert_eq!(parsed.artifact_role, Some("concept".to_string()));
    }

    /// v2 Phase 0 (trust model, §4.2 of the plan): a freshly-authored
    /// `wiki_page` sits at `TrustTier::CanonicalRecord`, and stays
    /// there even if a distinct-writer `Verification` child replies to
    /// it — the promotion path targets the verification artifact
    /// being written, not the parent it cites. The WikiPage's trust
    /// tier is a property of the page itself, not of its children.
    #[test]
    fn wiki_page_stays_canonical_record_even_with_verification_children() {
        let tenant = TenantId::new("wiki_trust").unwrap();

        let page = TaskArtifact::new_wiki_page(tenant.clone(), "task-wiki-2");
        assert_eq!(
            derive_artifact_promotion_state(&page),
            PromotionState::Canonical,
            "fresh wiki_page derives to Canonical, not Verified or Summarized"
        );
        assert_eq!(
            derive_artifact_trust_tier(&page),
            TrustTier::CanonicalRecord,
            "fresh wiki_page starts at CanonicalRecord trust tier"
        );

        // Simulate the state after a distinct-writer Verification
        // child has been recorded against this page. The child would
        // be promoted via promote_if_countersigned (covered separately
        // in mcp::handlers tests); the PAGE itself never changes
        // promotion_state. Assert directly that an externally-
        // unmodified WikiPage remains at CanonicalRecord.
        assert_eq!(
            page.promotion_state,
            PromotionState::Raw,
            "wiki_page promotion_state starts at Raw pre-storage"
        );
        let mut stored = page.clone();
        stored.promotion_state = derive_artifact_promotion_state(&stored);
        assert_eq!(
            derive_artifact_trust_tier(&stored),
            TrustTier::CanonicalRecord,
            "post-storage wiki_page stays at CanonicalRecord trust tier"
        );

        // Even if some caller maliciously attempts to self-label the
        // page with verification-adjacent fields, the trust tier must
        // not upgrade — only server-computed PromotionState::Verified
        // does that, and the server only sets Verified on the child,
        // never on the wiki_page via this path.
        let mut self_labelled = page.clone();
        self_labelled.verification_status = Some("verified".to_string());
        self_labelled.approval_state = Some("approved".to_string());
        self_labelled.validation = vec!["self-asserted".to_string()];
        self_labelled.promotion_state = derive_artifact_promotion_state(&self_labelled);
        assert_eq!(
            derive_artifact_trust_tier(&self_labelled),
            TrustTier::CanonicalRecord,
            "self-labelled wiki_page cannot reach VerifiedRecord — §4.2 laundering block"
        );
    }

    #[test]
    fn evidence_and_run_artifacts_emit_specialized_projections() {
        let tenant_id = TenantId::new("science_team").unwrap();
        let mut run = TaskArtifact::new_run_start(tenant_id.clone(), "task-1");
        run.tool_name = Some("mmseqs".to_string());
        run.command = Some("mmseqs search db query out tmp".to_string());
        let run_projections = build_task_projections(&run);
        assert!(run_projections
            .iter()
            .any(|p| p.kind == ProjectionKind::Run));

        let mut evidence = TaskArtifact::new_evidence(tenant_id, "task-1");
        evidence.summary = Some("Top hit exceeded the threshold".to_string());
        evidence.evidence_kind = Some("metric".to_string());
        evidence.supports_claim = Some(true);
        evidence.metrics = Some(serde_json::json!({"score": 0.93}));
        let evidence_projections = build_task_projections(&evidence);
        assert!(evidence_projections
            .iter()
            .any(|p| p.kind == ProjectionKind::Evidence));
    }

    #[test]
    fn decision_and_digest_artifacts_emit_specialized_projections() {
        let tenant_id = TenantId::new("science_team").unwrap();

        let mut decision = TaskArtifact::new_decision(tenant_id.clone(), "task-1");
        decision.summary = Some("Adopt task-artifact digests for new-agent onboarding".to_string());
        decision.validation =
            vec!["Prototype query flow returned the expected task state".to_string()];
        decision.promotion_state = derive_artifact_promotion_state(&decision);
        let decision_projections = build_task_projections(&decision);
        assert!(decision_projections
            .iter()
            .any(|p| p.kind == ProjectionKind::Decision));

        let mut digest = TaskArtifact::new_digest(
            tenant_id,
            "task-1",
            "digest::project".to_string(),
            "project_brief".to_string(),
        );
        digest.summary = Some("Project brief for the current tenant".to_string());
        digest.promotion_state = derive_artifact_promotion_state(&digest);
        let digest_projections = build_task_projections(&digest);
        assert!(digest_projections
            .iter()
            .any(|p| p.kind == ProjectionKind::Digest));
        assert_eq!(digest.promotion_state, PromotionState::Summarized);
    }

    /// Regression test: `derive_artifact_promotion_state` is a PURE
    /// function and must never return `Verified`. Verification is a
    /// store-side decision made by
    /// `mcp::handlers::promote_if_countersigned` that cannot be reached
    /// by only reading an artifact's fields. Previously this function
    /// honoured agent-supplied `verification_status`, `approval_state`,
    /// `ArtifactKind::Verification`, and non-empty `validation` — each
    /// was a laundering channel that let a single agent self-label.
    #[test]
    fn derive_artifact_promotion_state_never_returns_verified() {
        let tenant = TenantId::new("trust_pure").unwrap();

        let mut verification = TaskArtifact::new(ArtifactKind::Verification, tenant.clone(), "t1");
        verification.verification_status = Some("verified".to_string());
        verification.approval_state = Some("approved".to_string());
        verification.validation = vec!["lgtm".to_string()];
        assert_eq!(
            derive_artifact_promotion_state(&verification),
            PromotionState::Canonical,
            "Verification kind with self-labelled fields must NOT promote"
        );

        let mut decision = TaskArtifact::new_decision(tenant.clone(), "t1");
        decision.validation = vec!["manual review passed".to_string()];
        decision.supports_claim = Some(true);
        assert_eq!(
            derive_artifact_promotion_state(&decision),
            PromotionState::Canonical,
            "Decision with non-empty validation must NOT promote"
        );

        let mut review = TaskArtifact::new(ArtifactKind::Review, tenant.clone(), "t1");
        review.verification_status = Some("approved".to_string());
        assert_eq!(
            derive_artifact_promotion_state(&review),
            PromotionState::Canonical,
            "Review with self-labelled approval must NOT promote"
        );

        let digest = TaskArtifact::new_digest(
            tenant,
            "digest_task",
            "digest::scope".to_string(),
            "project_brief".to_string(),
        );
        assert_eq!(
            derive_artifact_promotion_state(&digest),
            PromotionState::Summarized,
            "Digest kind maps to Summarized regardless of agent-supplied fields"
        );
    }

    /// Regression test: `derive_artifact_trust_tier` only returns
    /// `VerifiedRecord` when `promotion_state == Verified`. Previously
    /// it also honoured `ArtifactKind::Verification` and agent-supplied
    /// `verification_status` / `approval_state`.
    #[test]
    fn derive_artifact_trust_tier_requires_server_verified_promotion() {
        let tenant = TenantId::new("trust_tier").unwrap();

        let mut agent_labelled =
            TaskArtifact::new(ArtifactKind::Verification, tenant.clone(), "t1");
        agent_labelled.verification_status = Some("verified".to_string());
        agent_labelled.approval_state = Some("approved".to_string());
        agent_labelled.promotion_state = PromotionState::Canonical;
        assert_eq!(
            derive_artifact_trust_tier(&agent_labelled),
            TrustTier::CanonicalRecord,
            "agent-supplied labels must not produce VerifiedRecord"
        );

        let mut server_promoted = TaskArtifact::new(ArtifactKind::Review, tenant, "t1");
        server_promoted.promotion_state = PromotionState::Verified;
        assert_eq!(
            derive_artifact_trust_tier(&server_promoted),
            TrustTier::VerifiedRecord,
            "server-side PromotionState::Verified is the only path to VerifiedRecord"
        );
    }
}
