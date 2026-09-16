//! Typed, causal experience records stored in canonical task artifacts.

mod bundle;
mod recall;
mod validation;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;
use crate::store::Store;
use crate::task_memory::{
    build_task_projections_minimal, ArtifactKind, ExecutionContext, TaskArtifact, TaskProvenance,
};
use crate::types::{ProjectId, PromotionState, TenantId};

pub use bundle::{export_bundle, import_bundle, ExperienceBundleV1, ImportReport};
pub use recall::{retrieve_lessons, ApplicableLesson, LessonQuery, LessonRecall, RecallAbstention};
pub(crate) use validation::{
    attempt_ordinal, current_attempt, current_lesson_heads, event_time, problem_id, required_event,
    sort_artifacts, validate_artifact_shape, validate_case, validation,
};
use validation::{require_text, validate_receipt, validate_terms};

static EXPERIENCE_MUTATIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ROLE_PROBLEM: &str = "experience_problem";
const ROLE_ATTEMPT: &str = "experience_attempt";
const ROLE_CHECK: &str = "experience_check";
const ROLE_LESSON: &str = "experience_lesson";
const ROLE_CORRECTION: &str = "experience_correction";

/// Context captured by the caller that records an experience event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperienceContext {
    pub tenant_id: TenantId,
    #[serde(default)]
    pub project_id: ProjectId,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub observed_at_ms: Option<i64>,
    #[serde(default)]
    pub provenance: TaskProvenance,
}

/// Events accepted by the general record endpoint.
///
/// Checks are intentionally absent. Callers assert a historical
/// [`CheckReceipt`] through [`record_check`]; the public CLI constructs one
/// from its local executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordEvent {
    Problem {
        description: String,
        symptom_terms: Vec<String>,
    },
    Attempt {
        problem_id: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        previous_attempt_id: Option<String>,
        action: String,
    },
    Lesson(ConditionalLesson),
    Correction {
        problem_id: String,
        supersedes_id: String,
        replacement: ConditionalLesson,
    },
}

/// Typed payload stored in `TaskArtifact.experience`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExperienceEvent {
    Problem {
        description: String,
        symptom_terms: Vec<String>,
    },
    Attempt {
        problem_id: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        previous_attempt_id: Option<String>,
        ordinal: u32,
        action: String,
    },
    Check(Box<CheckReceipt>),
    Lesson(ConditionalLesson),
    Correction {
        problem_id: String,
        supersedes_id: String,
        replacement: ConditionalLesson,
    },
}

/// Scope in which a lesson may be recalled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonVisibility {
    #[default]
    Project,
    Shared,
}

/// Exact environment conditions required for a lesson to apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonApplicability {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub machine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
}

/// Guidance supported by one successful recorded check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionalLesson {
    pub problem_id: String,
    pub supporting_check_id: String,
    pub guidance: String,
    pub symptom_terms: Vec<String>,
    #[serde(default)]
    pub applicability: LessonApplicability,
    #[serde(default)]
    pub visibility: LessonVisibility,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts_with: Vec<String>,
}

/// What bytes a source fingerprint covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceFingerprintCoverage {
    TrackedRepository,
    ExplicitPaths { paths: Vec<String> },
}

/// Hash of the declared source coverage at one point in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub sha256: String,
    pub coverage: SourceFingerprintCoverage,
}

/// Historical receipt produced by the caller-side check executor.
///
/// Output bodies are never stored. An absent output hash means the stream was
/// not completely drained and makes the receipt ineligible to support a
/// lesson.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReceipt {
    pub attempt_id: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stdout_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stderr_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_before: Option<SourceFingerprint>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_after: Option<SourceFingerprint>,
    pub execution: ExecutionContext,
}

impl CheckReceipt {
    /// True when the command completed successfully and both streams were drained.
    pub fn command_succeeded(&self) -> bool {
        !self.timed_out
            && self.exit_code == Some(0)
            && self.stdout_sha256.is_some()
            && self.stderr_sha256.is_some()
    }

    /// True only for a complete, successful historical receipt.
    pub fn supports_lesson(&self) -> bool {
        self.command_succeeded()
            && match (&self.source_before, &self.source_after) {
                (Some(before), Some(after)) => before == after,
                _ => false,
            }
    }

    /// Stable persisted status derived from command and source evidence.
    pub fn status(&self) -> &'static str {
        if self.supports_lesson() {
            "passed"
        } else if self.command_succeeded() {
            "source_unverified"
        } else if self.timed_out {
            "timed_out"
        } else {
            "failed"
        }
    }
}

/// Validated target returned before a caller executes a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckTarget {
    pub task_id: String,
    pub problem_id: String,
    pub attempt_id: String,
    pub ordinal: u32,
}

/// Current causal view of one problem and all of its recorded history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperienceCase {
    pub problem: TaskArtifact,
    pub events: Vec<TaskArtifact>,
    pub current_lessons: Vec<TaskArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lesson_conflicts: Vec<Vec<String>>,
    pub resolution_state: ExperienceResolutionState,
}

/// Derived state of a problem's current lesson heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceResolutionState {
    Open,
    Resolved,
    Conflicted,
}

/// Record a non-check experience event.
pub async fn record<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    event: RecordEvent,
) -> Result<TaskArtifact> {
    let _guard = EXPERIENCE_MUTATIONS.lock().await;
    record_unlocked(store, context, event).await
}

async fn record_unlocked<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    event: RecordEvent,
) -> Result<TaskArtifact> {
    match event {
        RecordEvent::Problem {
            description,
            symptom_terms,
        } => record_problem(store, context, description, symptom_terms).await,
        RecordEvent::Attempt {
            problem_id,
            previous_attempt_id,
            action,
        } => record_attempt(store, context, problem_id, previous_attempt_id, action).await,
        RecordEvent::Lesson(lesson) => record_lesson(store, context, lesson).await,
        RecordEvent::Correction {
            problem_id,
            supersedes_id,
            replacement,
        } => record_correction(store, context, problem_id, supersedes_id, replacement).await,
    }
}

/// Validate that an attempt is the current checkable head for this scope.
pub async fn validate_check_target<S: Store + ?Sized>(
    store: &S,
    tenant_id: &TenantId,
    project_id: &ProjectId,
    attempt_id: &str,
) -> Result<CheckTarget> {
    let (target, attempt) = load_check_target(store, tenant_id, project_id, attempt_id).await?;
    let artifacts = store
        .list_task_artifacts(tenant_id, &attempt.task_id)
        .await?;
    let current = current_attempt(&artifacts)?.ok_or_else(|| {
        validation(format!(
            "problem '{}' has no current attempt",
            target.problem_id
        ))
    })?;
    if current.artifact_id != attempt_id {
        return Err(validation(format!(
            "attempt '{attempt_id}' is not the current attempt; current attempt is '{}'",
            current.artifact_id
        )));
    }

    Ok(target)
}

async fn load_check_target<S: Store + ?Sized>(
    store: &S,
    tenant_id: &TenantId,
    project_id: &ProjectId,
    attempt_id: &str,
) -> Result<(CheckTarget, TaskArtifact)> {
    let attempt = get_experience(store, tenant_id, attempt_id)
        .await?
        .ok_or_else(|| validation(format!("attempt '{attempt_id}' does not exist")))?;
    let (problem_id, ordinal) = match required_event(&attempt)? {
        ExperienceEvent::Attempt {
            problem_id,
            ordinal,
            ..
        } => (problem_id.clone(), *ordinal),
        _ => {
            return Err(validation(format!(
                "artifact '{attempt_id}' is not an experience attempt"
            )));
        }
    };
    ensure_project(project_id, &attempt.project_id)?;

    Ok((
        CheckTarget {
            task_id: attempt.task_id.clone(),
            problem_id,
            attempt_id: attempt_id.to_string(),
            ordinal,
        },
        attempt,
    ))
}

/// Store a receipt created by the caller-side check executor.
pub async fn record_check<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    receipt: CheckReceipt,
) -> Result<TaskArtifact> {
    let _guard = EXPERIENCE_MUTATIONS.lock().await;
    record_check_unlocked(store, context, receipt).await
}

async fn record_check_unlocked<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    receipt: CheckReceipt,
) -> Result<TaskArtifact> {
    validate_receipt(&receipt)?;
    let (target, attempt) = load_check_target(
        store,
        &context.tenant_id,
        &context.project_id,
        &receipt.attempt_id,
    )
    .await?;
    ensure_causal_time(context.observed_at_ms, &attempt, "check")?;

    let status = receipt.status();
    let mut artifact = TaskArtifact::new(
        ArtifactKind::Verification,
        context.tenant_id.clone(),
        target.task_id,
    );
    apply_context(&mut artifact, &context);
    artifact.status = Some(status.to_string());
    artifact.artifact_role = Some(ROLE_CHECK.to_string());
    artifact.reply_to_artifact_id = Some(receipt.attempt_id.clone());
    artifact.relation_kind = Some("checks_attempt".to_string());
    artifact.summary = Some(format!(
        "Recorded check for attempt {}: {}",
        receipt.attempt_id, status
    ));
    artifact.verification_status = Some("recorded_check".to_string());
    artifact.related_artifact_ids = vec![receipt.attempt_id.clone()];
    artifact.experience = Some(ExperienceEvent::Check(Box::new(receipt)));
    write_artifact(store, artifact).await
}

/// Fetch one typed experience artifact by ID.
pub async fn get<S: Store + ?Sized>(
    store: &S,
    tenant_id: &TenantId,
    artifact_id: &str,
) -> Result<Option<TaskArtifact>> {
    get_experience(store, tenant_id, artifact_id).await
}

/// Resolve one problem to its current attempt/check/lesson view.
pub async fn resolve_case<S: Store + ?Sized>(
    store: &S,
    tenant_id: &TenantId,
    problem_id: &str,
) -> Result<ExperienceCase> {
    let problem = get_experience(store, tenant_id, problem_id)
        .await?
        .ok_or_else(|| validation(format!("problem '{problem_id}' does not exist")))?;
    if !matches!(required_event(&problem)?, ExperienceEvent::Problem { .. }) {
        return Err(validation(format!(
            "artifact '{problem_id}' is not an experience problem"
        )));
    }

    let mut events = store
        .list_task_artifacts(tenant_id, &problem.task_id)
        .await?
        .into_iter()
        .filter(|artifact| artifact.experience.is_some())
        .collect::<Vec<_>>();
    sort_artifacts(&mut events);
    validate_case(&events)?;
    let (current_lessons, mut lesson_conflicts) = current_lesson_heads(&events)?;
    let current_ids = current_lessons
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect::<HashSet<_>>();
    for artifact in &current_lessons {
        let lesson = lesson_from_event(required_event(artifact)?)
            .expect("current lesson head must contain lesson data");
        let mut conflict = vec![artifact.artifact_id.clone()];
        conflict.extend(
            lesson
                .conflicts_with
                .iter()
                .filter(|id| current_ids.contains(id.as_str()))
                .cloned(),
        );
        conflict.sort();
        conflict.dedup();
        if conflict.len() > 1 && !lesson_conflicts.contains(&conflict) {
            lesson_conflicts.push(conflict);
        }
    }
    lesson_conflicts.sort();
    let resolution_state = if !lesson_conflicts.is_empty() {
        ExperienceResolutionState::Conflicted
    } else if current_lessons.is_empty() {
        ExperienceResolutionState::Open
    } else {
        ExperienceResolutionState::Resolved
    };

    Ok(ExperienceCase {
        problem,
        events,
        current_lessons,
        lesson_conflicts,
        resolution_state,
    })
}

async fn record_problem<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    description: String,
    symptom_terms: Vec<String>,
) -> Result<TaskArtifact> {
    require_text("description", &description)?;
    validate_terms(&symptom_terms)?;
    if context.project_id.as_option().is_none() {
        return Err(validation(
            "experience problems require an explicit project_id",
        ));
    }

    let task_id = format!("experience-{}", Uuid::now_v7());
    let mut artifact = TaskArtifact::new(
        ArtifactKind::TaskProgress,
        context.tenant_id.clone(),
        task_id,
    );
    apply_context(&mut artifact, &context);
    artifact.status = Some("open".to_string());
    artifact.artifact_role = Some(ROLE_PROBLEM.to_string());
    artifact.summary = Some(description.clone());
    artifact.what_failed = symptom_terms.clone();
    artifact.experience = Some(ExperienceEvent::Problem {
        description,
        symptom_terms,
    });
    write_artifact(store, artifact).await
}

async fn record_attempt<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    problem_id: String,
    previous_attempt_id: Option<String>,
    action: String,
) -> Result<TaskArtifact> {
    require_text("problem_id", &problem_id)?;
    require_text("action", &action)?;
    let problem = require_problem(store, &context, &problem_id).await?;
    let artifacts = store
        .list_task_artifacts(&context.tenant_id, &problem.task_id)
        .await?;
    let head = current_attempt(&artifacts)?;
    let ordinal = match head {
        None => {
            if previous_attempt_id.is_some() {
                return Err(validation(
                    "the first attempt must not set previous_attempt_id",
                ));
            }
            1
        }
        Some(head) => {
            if previous_attempt_id.as_deref() != Some(head.artifact_id.as_str()) {
                return Err(validation(format!(
                    "previous_attempt_id must target current attempt '{}'",
                    head.artifact_id
                )));
            }
            ensure_causal_time(context.observed_at_ms, head, "attempt")?;
            attempt_ordinal(head)?
                .checked_add(1)
                .ok_or_else(|| validation("attempt ordinal overflow"))?
        }
    };

    let mut artifact = TaskArtifact::new(
        ArtifactKind::RunFinish,
        context.tenant_id.clone(),
        problem.task_id,
    );
    apply_context(&mut artifact, &context);
    artifact.status = Some("recorded".to_string());
    artifact.artifact_role = Some(ROLE_ATTEMPT.to_string());
    artifact.reply_to_artifact_id = Some(
        previous_attempt_id
            .clone()
            .unwrap_or_else(|| problem_id.clone()),
    );
    artifact.relation_kind = Some("attempts_problem".to_string());
    artifact.summary = Some(action.clone());
    artifact.related_artifact_ids = match &previous_attempt_id {
        Some(previous) => vec![problem_id.clone(), previous.clone()],
        None => vec![problem_id.clone()],
    };
    artifact.experience = Some(ExperienceEvent::Attempt {
        problem_id,
        previous_attempt_id,
        ordinal,
        action,
    });
    write_artifact(store, artifact).await
}

async fn record_lesson<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    lesson: ConditionalLesson,
) -> Result<TaskArtifact> {
    let problem = validate_lesson(store, &context, &lesson).await?;
    validate_conflict_refs(store, &context, &problem, &lesson).await?;

    let mut artifact = TaskArtifact::new(
        ArtifactKind::Decision,
        context.tenant_id.clone(),
        problem.task_id,
    );
    apply_context(&mut artifact, &context);
    artifact.status = Some("recorded".to_string());
    artifact.artifact_role = Some(ROLE_LESSON.to_string());
    artifact.reply_to_artifact_id = Some(lesson.supporting_check_id.clone());
    artifact.relation_kind = Some("learned_from_check".to_string());
    artifact.summary = Some(lesson.guidance.clone());
    artifact.what_worked = vec![lesson.guidance.clone()];
    artifact.related_artifact_ids = validation::lesson_related_ids(&lesson);
    artifact.experience = Some(ExperienceEvent::Lesson(lesson));
    write_artifact(store, artifact).await
}

async fn record_correction<S: Store + ?Sized>(
    store: &S,
    context: ExperienceContext,
    problem_id: String,
    supersedes_id: String,
    replacement: ConditionalLesson,
) -> Result<TaskArtifact> {
    require_text("supersedes_id", &supersedes_id)?;
    if replacement.problem_id != problem_id {
        return Err(validation(
            "correction problem_id must match replacement.problem_id",
        ));
    }
    let problem = validate_lesson(store, &context, &replacement).await?;
    validate_conflict_refs(store, &context, &problem, &replacement).await?;
    let case = resolve_case(store, &context.tenant_id, &problem_id).await?;
    let is_conflicted_head = case
        .lesson_conflicts
        .iter()
        .flatten()
        .any(|artifact_id| artifact_id == &supersedes_id);
    let current = case
        .current_lessons
        .iter()
        .find(|artifact| artifact.artifact_id == supersedes_id)
        .or_else(|| {
            if is_conflicted_head {
                case.events
                    .iter()
                    .find(|artifact| artifact.artifact_id == supersedes_id)
            } else {
                None
            }
        })
        .ok_or_else(|| {
            validation(format!(
                "supersedes_id '{supersedes_id}' is not a current lesson head"
            ))
        })?;
    ensure_causal_time(context.observed_at_ms, current, "correction")?;

    let mut artifact = TaskArtifact::new(
        ArtifactKind::Revision,
        context.tenant_id.clone(),
        problem.task_id,
    );
    apply_context(&mut artifact, &context);
    artifact.status = Some("recorded".to_string());
    artifact.artifact_role = Some(ROLE_CORRECTION.to_string());
    artifact.reply_to_artifact_id = Some(supersedes_id.clone());
    artifact.relation_kind = Some("supersedes_lesson".to_string());
    artifact.summary = Some(replacement.guidance.clone());
    artifact.what_worked = vec![replacement.guidance.clone()];
    artifact.related_artifact_ids = {
        let mut refs = validation::lesson_related_ids(&replacement);
        refs.push(supersedes_id.clone());
        refs.sort();
        refs.dedup();
        refs
    };
    artifact.experience = Some(ExperienceEvent::Correction {
        problem_id,
        supersedes_id,
        replacement,
    });
    write_artifact(store, artifact).await
}

async fn write_artifact<S: Store + ?Sized>(
    store: &S,
    artifact: TaskArtifact,
) -> Result<TaskArtifact> {
    let projections = build_task_projections_minimal(&artifact);
    store
        .add_task_artifact(artifact.clone(), projections)
        .await?;
    Ok(artifact)
}

async fn require_problem<S: Store + ?Sized>(
    store: &S,
    context: &ExperienceContext,
    problem_id: &str,
) -> Result<TaskArtifact> {
    let problem = get_experience(store, &context.tenant_id, problem_id)
        .await?
        .ok_or_else(|| validation(format!("problem '{problem_id}' does not exist")))?;
    if !matches!(required_event(&problem)?, ExperienceEvent::Problem { .. }) {
        return Err(validation(format!(
            "artifact '{problem_id}' is not an experience problem"
        )));
    }
    ensure_project(&context.project_id, &problem.project_id)?;
    Ok(problem)
}

async fn get_experience<S: Store + ?Sized>(
    store: &S,
    tenant_id: &TenantId,
    artifact_id: &str,
) -> Result<Option<TaskArtifact>> {
    Ok(store
        .get_task_artifact(tenant_id, artifact_id)
        .await?
        .filter(|artifact| artifact.experience.is_some()))
}

async fn validate_lesson<S: Store + ?Sized>(
    store: &S,
    context: &ExperienceContext,
    lesson: &ConditionalLesson,
) -> Result<TaskArtifact> {
    validation::validate_embedded_lesson(lesson)?;
    let problem = require_problem(store, context, &lesson.problem_id).await?;
    if lesson.visibility == LessonVisibility::Project && problem.project_id.as_option().is_none() {
        return Err(validation(
            "project-visible lessons require an explicit project_id",
        ));
    }

    let check = get_experience(store, &context.tenant_id, &lesson.supporting_check_id)
        .await?
        .ok_or_else(|| {
            validation(format!(
                "supporting check '{}' does not exist",
                lesson.supporting_check_id
            ))
        })?;
    let ExperienceEvent::Check(receipt) = required_event(&check)? else {
        return Err(validation(format!(
            "artifact '{}' is not an experience check",
            lesson.supporting_check_id
        )));
    };
    if check.task_id != problem.task_id || check.project_id != problem.project_id {
        return Err(validation("supporting check is outside the problem scope"));
    }
    if !receipt.supports_lesson() {
        return Err(validation(
            "supporting check must have exit_code 0, no timeout, complete output hashes, and unchanged source evidence",
        ));
    }
    ensure_causal_time(context.observed_at_ms, &check, "lesson")?;
    Ok(problem)
}

async fn validate_conflict_refs<S: Store + ?Sized>(
    store: &S,
    context: &ExperienceContext,
    problem: &TaskArtifact,
    lesson: &ConditionalLesson,
) -> Result<()> {
    let mut seen = HashSet::new();
    for conflict_id in &lesson.conflicts_with {
        require_text("conflicts_with", conflict_id)?;
        if !seen.insert(conflict_id) {
            return Err(validation(format!(
                "duplicate conflict reference '{conflict_id}'"
            )));
        }
        let target = get_experience(store, &context.tenant_id, conflict_id)
            .await?
            .ok_or_else(|| {
                validation(format!("conflicting lesson '{conflict_id}' does not exist"))
            })?;
        if lesson_from_event(required_event(&target)?).is_none() {
            return Err(validation(format!(
                "conflict reference '{conflict_id}' is not a lesson or correction"
            )));
        }
        if target.task_id != problem.task_id || target.project_id != problem.project_id {
            return Err(validation(format!(
                "conflict reference '{conflict_id}' is outside the problem scope"
            )));
        }
    }
    Ok(())
}

fn apply_context(artifact: &mut TaskArtifact, context: &ExperienceContext) {
    artifact.project_id = context.project_id.clone();
    artifact.agent_id = context.agent_id.clone();
    artifact.session_id = context.session_id.clone();
    artifact.provenance = context.provenance.clone();
    artifact.timestamp_observed = context.observed_at_ms;
    artifact.promotion_state = PromotionState::Canonical;
}

fn ensure_project(requested: &ProjectId, actual: &ProjectId) -> Result<()> {
    if requested != actual {
        return Err(validation(format!(
            "project scope mismatch: requested '{}', artifact belongs to '{}'",
            requested, actual
        )));
    }
    Ok(())
}

fn ensure_causal_time(
    observed_at_ms: Option<i64>,
    parent: &TaskArtifact,
    event_name: &str,
) -> Result<()> {
    if let Some(observed_at_ms) = observed_at_ms {
        if observed_at_ms < event_time(parent) {
            return Err(validation(format!(
                "{event_name} observed_at_ms predates causal target '{}'",
                parent.artifact_id
            )));
        }
    }
    Ok(())
}

pub(crate) fn lesson_from_event(event: &ExperienceEvent) -> Option<&ConditionalLesson> {
    match event {
        ExperienceEvent::Lesson(lesson) => Some(lesson),
        ExperienceEvent::Correction { replacement, .. } => Some(replacement),
        _ => None,
    }
}
