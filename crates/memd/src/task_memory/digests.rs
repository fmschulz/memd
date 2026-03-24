use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{sanitize_tag_value, ArtifactKind, TaskArtifact, TaskRecord};
use crate::types::{ProjectId, PromotionState, TenantId};

pub const DIGEST_ROLE_PROJECT_BRIEF: &str = "project_brief";
pub const DIGEST_ROLE_TASK_RESUME: &str = "task_resume";
pub const DIGEST_ROLE_FAILURE_LIBRARY: &str = "failure_library";
pub const DIGEST_ROLE_DECISION_LIBRARY: &str = "decision_library";
pub const DIGEST_ROLE_EVIDENCE_LIBRARY: &str = "evidence_library";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDigestItem {
    pub artifact_id: String,
    pub tool_name: Option<String>,
    pub status: Option<String>,
    pub command: Option<String>,
    pub timestamp_created: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResumeView {
    pub task: TaskRecord,
    pub latest_summary: Option<String>,
    pub blockers: Vec<String>,
    pub what_worked: Vec<String>,
    pub what_failed: Vec<String>,
    pub validation: Vec<String>,
    pub followups: Vec<String>,
    pub recent_runs: Vec<RunDigestItem>,
    pub source_artifact_ids: Vec<String>,
    pub promotion_state: PromotionState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureViewItem {
    pub artifact_id: String,
    pub task_id: String,
    pub project_id: Option<String>,
    pub summary: String,
    pub promotion_state: PromotionState,
    pub timestamp_created: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionViewItem {
    pub artifact_id: String,
    pub task_id: String,
    pub project_id: Option<String>,
    pub summary: String,
    pub explicit: bool,
    pub promotion_state: PromotionState,
    pub timestamp_created: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceViewItem {
    pub artifact_id: String,
    pub task_id: String,
    pub project_id: Option<String>,
    pub summary: String,
    pub supports_claim: Option<bool>,
    pub promotion_state: PromotionState,
    pub timestamp_created: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBriefView {
    pub tenant_id: String,
    pub project_id: String,
    pub overview: String,
    pub active_tasks: Vec<TaskResumeView>,
    pub recent_completed_tasks: Vec<TaskResumeView>,
    pub recent_failures: Vec<FailureViewItem>,
    pub recent_decisions: Vec<DecisionViewItem>,
    pub evidence_highlights: Vec<EvidenceViewItem>,
    pub related_projects: Vec<String>,
    pub source_task_ids: Vec<String>,
    pub source_artifact_ids: Vec<String>,
    pub promotion_state: PromotionState,
    pub updated_at_ms: i64,
}

fn dedupe_keep_order(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
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

fn max_promotion(promotions: impl IntoIterator<Item = PromotionState>) -> PromotionState {
    let mut best = PromotionState::Raw;
    for promotion in promotions {
        let score = match promotion {
            PromotionState::Raw => 0,
            PromotionState::Summarized => 1,
            PromotionState::Canonical => 2,
            PromotionState::Verified => 3,
        };
        let best_score = match best {
            PromotionState::Raw => 0,
            PromotionState::Summarized => 1,
            PromotionState::Canonical => 2,
            PromotionState::Verified => 3,
        };
        if score > best_score {
            best = promotion;
        }
    }
    best
}

pub fn stable_digest_identity(role: &str, scope_key: &str) -> (String, String, String) {
    let digest_key = format!(
        "{}::{}",
        sanitize_tag_value(role),
        sanitize_tag_value(scope_key)
    );
    (
        format!("digest_artifact_{}", digest_key),
        format!("digest_task_{}", digest_key),
        digest_key,
    )
}

pub fn build_task_resume_view(task: TaskRecord, artifacts: &[TaskArtifact]) -> TaskResumeView {
    let mut sorted = artifacts.to_vec();
    sorted.sort_by_key(|artifact| std::cmp::Reverse(artifact.timestamp_created));

    let latest_summary = sorted.iter().find_map(|artifact| artifact.event_summary());
    let blockers = dedupe_keep_order(sorted.iter().flat_map(|artifact| artifact.blockers.clone()));
    let what_worked = dedupe_keep_order(
        sorted
            .iter()
            .flat_map(|artifact| artifact.what_worked.clone()),
    );
    let what_failed = dedupe_keep_order(
        sorted
            .iter()
            .flat_map(|artifact| artifact.what_failed.clone()),
    );
    let validation = dedupe_keep_order(
        sorted
            .iter()
            .flat_map(|artifact| artifact.validation.clone()),
    );
    let followups = dedupe_keep_order(
        sorted
            .iter()
            .flat_map(|artifact| artifact.followups.clone()),
    );
    let recent_runs = sorted
        .iter()
        .filter(|artifact| {
            matches!(
                artifact.artifact_kind,
                ArtifactKind::RunStart | ArtifactKind::RunFinish
            )
        })
        .take(5)
        .map(|artifact| RunDigestItem {
            artifact_id: artifact.artifact_id.clone(),
            tool_name: artifact.tool_name.clone(),
            status: artifact.status.clone(),
            command: artifact.command.clone(),
            timestamp_created: artifact.timestamp_created,
        })
        .collect::<Vec<_>>();
    let source_artifact_ids = sorted
        .iter()
        .map(|artifact| artifact.artifact_id.clone())
        .collect::<Vec<_>>();
    let promotion_state = max_promotion(sorted.iter().map(|artifact| artifact.promotion_state));

    TaskResumeView {
        task,
        latest_summary,
        blockers,
        what_worked,
        what_failed,
        validation,
        followups,
        recent_runs,
        source_artifact_ids,
        promotion_state,
    }
}

pub fn infer_failure_items(artifacts: &[TaskArtifact]) -> Vec<FailureViewItem> {
    let mut items = Vec::new();
    for artifact in artifacts {
        let mut parts = Vec::new();
        if !artifact.what_failed.is_empty() {
            parts.extend(artifact.what_failed.clone());
        }
        if !artifact.blockers.is_empty() {
            parts.extend(artifact.blockers.clone());
        }
        let summary = dedupe_keep_order(parts).join("; ");
        if summary.is_empty() {
            continue;
        }
        items.push(FailureViewItem {
            artifact_id: artifact.artifact_id.clone(),
            task_id: artifact.task_id.clone(),
            project_id: artifact.project_id.as_option().map(str::to_string),
            summary,
            promotion_state: artifact.promotion_state,
            timestamp_created: artifact.timestamp_created,
        });
    }
    items.sort_by_key(|item| std::cmp::Reverse(item.timestamp_created));
    items
}

pub fn infer_decision_items(artifacts: &[TaskArtifact]) -> Vec<DecisionViewItem> {
    let mut items = Vec::new();
    for artifact in artifacts {
        if artifact.artifact_kind == ArtifactKind::Decision {
            let summary = artifact
                .event_summary()
                .unwrap_or_else(|| "Decision recorded".to_string());
            items.push(DecisionViewItem {
                artifact_id: artifact.artifact_id.clone(),
                task_id: artifact.task_id.clone(),
                project_id: artifact.project_id.as_option().map(str::to_string),
                summary,
                explicit: true,
                promotion_state: artifact.promotion_state,
                timestamp_created: artifact.timestamp_created,
            });
            continue;
        }

        if !matches!(
            artifact.artifact_kind,
            ArtifactKind::TaskFinish | ArtifactKind::Verification | ArtifactKind::Review
        ) {
            continue;
        }

        let mut pieces = Vec::new();
        if let Some(summary) = artifact.summary.as_ref() {
            pieces.push(summary.clone());
        }
        if !artifact.what_worked.is_empty() {
            pieces.push(format!("Worked: {}", artifact.what_worked.join("; ")));
        }
        if !artifact.validation.is_empty() {
            pieces.push(format!("Validation: {}", artifact.validation.join("; ")));
        }
        if pieces.is_empty() {
            continue;
        }
        items.push(DecisionViewItem {
            artifact_id: artifact.artifact_id.clone(),
            task_id: artifact.task_id.clone(),
            project_id: artifact.project_id.as_option().map(str::to_string),
            summary: pieces.join(" "),
            explicit: false,
            promotion_state: artifact.promotion_state,
            timestamp_created: artifact.timestamp_created,
        });
    }
    items.sort_by(|left, right| {
        std::cmp::Reverse(left.explicit)
            .cmp(&std::cmp::Reverse(right.explicit))
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
    });
    items
}

pub fn infer_evidence_items(artifacts: &[TaskArtifact]) -> Vec<EvidenceViewItem> {
    let mut items = Vec::new();
    for artifact in artifacts {
        if artifact.artifact_kind == ArtifactKind::Evidence {
            let summary = artifact
                .summary
                .clone()
                .or_else(|| artifact.event_summary())
                .unwrap_or_else(|| "Evidence recorded".to_string());
            items.push(EvidenceViewItem {
                artifact_id: artifact.artifact_id.clone(),
                task_id: artifact.task_id.clone(),
                project_id: artifact.project_id.as_option().map(str::to_string),
                summary,
                supports_claim: artifact.supports_claim,
                promotion_state: artifact.promotion_state,
                timestamp_created: artifact.timestamp_created,
            });
            continue;
        }

        if !artifact.validation.is_empty() {
            items.push(EvidenceViewItem {
                artifact_id: artifact.artifact_id.clone(),
                task_id: artifact.task_id.clone(),
                project_id: artifact.project_id.as_option().map(str::to_string),
                summary: artifact.validation.join("; "),
                supports_claim: None,
                promotion_state: artifact.promotion_state,
                timestamp_created: artifact.timestamp_created,
            });
        }
    }
    items.sort_by_key(|item| std::cmp::Reverse(item.timestamp_created));
    items
}

pub fn build_task_resume_digest_artifact(view: &TaskResumeView) -> TaskArtifact {
    let scope_key = view.task.task_id.clone();
    let (artifact_id, _task_id, digest_key) =
        stable_digest_identity(DIGEST_ROLE_TASK_RESUME, &scope_key);
    let mut artifact = TaskArtifact::new_digest(
        view.task.tenant_id.clone(),
        view.task.task_id.clone(),
        digest_key,
        DIGEST_ROLE_TASK_RESUME,
    );
    artifact.artifact_id = artifact_id;
    artifact.project_id = view.task.project_id.clone();
    artifact.goal = view.task.goal.clone();
    artifact.scientific_question = view.task.scientific_question.clone();
    artifact.hypothesis = view.task.hypothesis.clone();
    artifact.summary = view
        .latest_summary
        .clone()
        .or_else(|| view.task.goal.clone());
    artifact.blockers = view.blockers.clone();
    artifact.what_worked = view.what_worked.clone();
    artifact.what_failed = view.what_failed.clone();
    artifact.validation = view.validation.clone();
    artifact.followups = view.followups.clone();
    artifact.related_artifact_ids = view.source_artifact_ids.clone();
    artifact.source_updated_at_ms = Some(view.task.updated_at_ms);
    artifact.promotion_state = PromotionState::Summarized;
    artifact
}

pub fn build_library_digest_artifact(
    tenant_id: TenantId,
    project_id: Option<ProjectId>,
    role: &str,
    scope_key: &str,
    summary: String,
    what_failed: Vec<String>,
    validation: Vec<String>,
    followups: Vec<String>,
    related_artifact_ids: Vec<String>,
    source_updated_at_ms: i64,
) -> TaskArtifact {
    let (artifact_id, task_id, digest_key) = stable_digest_identity(role, scope_key);
    let mut artifact = TaskArtifact::new_digest(tenant_id, task_id, digest_key, role);
    artifact.artifact_id = artifact_id;
    artifact.project_id = project_id.unwrap_or_default();
    artifact.summary = Some(summary);
    artifact.what_failed = what_failed;
    artifact.validation = validation;
    artifact.followups = followups;
    artifact.related_artifact_ids = related_artifact_ids;
    artifact.source_updated_at_ms = Some(source_updated_at_ms);
    artifact.promotion_state = PromotionState::Summarized;
    artifact
}

pub fn build_project_brief_view(
    tenant_id: &TenantId,
    project_id: &str,
    task_views: Vec<TaskResumeView>,
    recent_failures: Vec<FailureViewItem>,
    recent_decisions: Vec<DecisionViewItem>,
    evidence_highlights: Vec<EvidenceViewItem>,
    related_projects: Vec<String>,
) -> ProjectBriefView {
    let mut active_tasks = Vec::new();
    let mut recent_completed_tasks = Vec::new();
    let mut source_task_ids = Vec::new();
    let mut source_artifact_ids = Vec::new();
    let mut updated_at_ms = 0i64;

    for view in task_views {
        source_task_ids.push(view.task.task_id.clone());
        source_artifact_ids.extend(view.source_artifact_ids.clone());
        updated_at_ms = updated_at_ms.max(view.task.updated_at_ms);
        if matches!(view.task.status.as_deref(), Some("completed" | "success")) {
            recent_completed_tasks.push(view);
        } else {
            active_tasks.push(view);
        }
    }

    active_tasks.sort_by_key(|view| std::cmp::Reverse(view.task.updated_at_ms));
    recent_completed_tasks.sort_by_key(|view| std::cmp::Reverse(view.task.updated_at_ms));
    active_tasks.truncate(5);
    recent_completed_tasks.truncate(5);

    let overview = format!(
        "Project {} has {} active tasks, {} recent completed tasks, {} recent failures, {} decisions, and {} evidence highlights.",
        project_id,
        active_tasks.len(),
        recent_completed_tasks.len(),
        recent_failures.len(),
        recent_decisions.len(),
        evidence_highlights.len()
    );

    ProjectBriefView {
        tenant_id: tenant_id.to_string(),
        project_id: project_id.to_string(),
        overview,
        active_tasks,
        recent_completed_tasks,
        recent_failures,
        recent_decisions,
        evidence_highlights,
        related_projects,
        source_task_ids: dedupe_keep_order(source_task_ids),
        source_artifact_ids: dedupe_keep_order(source_artifact_ids),
        promotion_state: PromotionState::Summarized,
        updated_at_ms,
    }
}

pub fn build_project_brief_digest_artifact(view: &ProjectBriefView) -> TaskArtifact {
    let summary = view.overview.clone();
    let what_failed = view
        .recent_failures
        .iter()
        .map(|item| item.summary.clone())
        .take(8)
        .collect::<Vec<_>>();
    let validation = view
        .evidence_highlights
        .iter()
        .map(|item| item.summary.clone())
        .take(8)
        .collect::<Vec<_>>();
    let followups = view
        .active_tasks
        .iter()
        .flat_map(|task| task.followups.clone())
        .take(10)
        .collect::<Vec<_>>();
    build_library_digest_artifact(
        TenantId::new(view.tenant_id.clone()).expect("tenant_id already validated"),
        Some(ProjectId::from(Some(view.project_id.clone()))),
        DIGEST_ROLE_PROJECT_BRIEF,
        &view.project_id,
        summary,
        what_failed,
        validation,
        followups,
        view.source_artifact_ids.clone(),
        view.updated_at_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_resume_digest_reuses_real_task_id() {
        let task = TaskRecord {
            task_id: "task-123".to_string(),
            tenant_id: TenantId::new("team").unwrap(),
            project_id: ProjectId::from(Some("proj".to_string())),
            status: Some("in_progress".to_string()),
            goal: Some("Finish the brief builder".to_string()),
            scientific_question: None,
            hypothesis: None,
            last_artifact_id: "artifact-1".to_string(),
            started_at_ms: Some(1),
            finished_at_ms: None,
            updated_at_ms: 10,
        };
        let view = TaskResumeView {
            task,
            latest_summary: Some("Most recent task summary".to_string()),
            blockers: vec!["Need better ranking".to_string()],
            what_worked: vec!["Task records are queryable".to_string()],
            what_failed: vec!["Raw chunk search was too noisy".to_string()],
            validation: vec!["Unit tests pass".to_string()],
            followups: vec!["Add digest-aware retrieval".to_string()],
            recent_runs: Vec::new(),
            source_artifact_ids: vec!["artifact-1".to_string()],
            promotion_state: PromotionState::Canonical,
        };

        let artifact = build_task_resume_digest_artifact(&view);
        assert_eq!(artifact.task_id, "task-123");
        assert_eq!(artifact.artifact_kind, ArtifactKind::Digest);
        assert_eq!(
            artifact.artifact_role.as_deref(),
            Some(DIGEST_ROLE_TASK_RESUME)
        );
        assert_eq!(artifact.promotion_state, PromotionState::Summarized);
    }
}
