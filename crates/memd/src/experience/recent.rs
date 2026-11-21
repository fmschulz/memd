//! Read the current lessons of a project, newest first, for the `memory.md`
//! digest. The digest shows the lesson itself; recall by symptom stays with
//! `memd experience find`.

use std::collections::HashMap;

use serde::Serialize;

use super::{
    event_time, lesson_from_event, resolve_case, ConditionalLesson, ExperienceCase,
    ROLE_CORRECTION, ROLE_LESSON,
};
use crate::error::Result;
use crate::store::Store;
use crate::task_memory::{TaskArtifact, TaskSearchFilters};
use crate::types::TenantId;

/// Lesson artifacts one call looks at, however many places it must fill.
pub const LESSON_WINDOW: usize = 30;

/// Largest case, in stored artifacts of its task, that the call resolves.
/// `resolve_case` loads and validates the whole history, so a longer case is
/// skipped.
pub const MAX_CASE_ARTIFACTS: usize = 200;

/// Why a candidate lesson is not shown.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LessonSkip {
    /// A newer correction replaced it.
    Superseded,
    /// Its case holds conflicting current lessons.
    Conflict,
    /// The places are full.
    OverLimit,
    /// The case holds more than [`MAX_CASE_ARTIFACTS`] artifacts.
    TooLarge,
    /// The case did not resolve.
    Unresolvable,
}

/// One lesson artifact the call looked at.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RecentLesson {
    pub problem_id: String,
    pub lesson_id: String,
    /// When the lesson was observed, as its writer states.
    pub event_time_ms: i64,
    /// When the store created the lesson. This orders the candidates.
    pub created_at_ms: i64,
    /// `None` when the lesson is shown.
    pub skipped: Option<LessonSkip>,
    #[serde(skip)]
    pub lesson: ConditionalLesson,
}

/// The newest lesson artifacts of the projects in `project_ids`, at most
/// [`LESSON_WINDOW`] of them, newest first by creation time. The metadata
/// query cuts the window by creation time, so the final order uses the same
/// clock: the creation time is set by the store, while the event time is
/// stated by the writer and can be backdated. Each is marked shown or
/// skipped; at most `limit` are shown. A case resolves once, and no case
/// resolves after the places are full.
///
/// One metadata query per project and role finds the artifacts. Lessons of
/// other projects are never read, shared or not.
pub async fn recent_lessons<S: Store + ?Sized>(
    store: &S,
    tenant_id: &TenantId,
    project_ids: &[String],
    limit: usize,
) -> Result<Vec<RecentLesson>> {
    let mut found: HashMap<String, TaskArtifact> = HashMap::new();
    for project_id in project_ids {
        for role in [ROLE_LESSON, ROLE_CORRECTION] {
            let filters = TaskSearchFilters {
                artifact_role: Some(role.to_string()),
                project_id: Some(project_id.clone()),
                ..Default::default()
            };
            let chunk_ids = store
                .search_task_projection_chunk_ids(tenant_id, &filters, LESSON_WINDOW)
                .await?;
            for artifact in store
                .resolve_artifacts_for_chunks(tenant_id, &chunk_ids)
                .await?
                .into_values()
            {
                if artifact.project_id.as_option() == Some(project_id.as_str()) {
                    found.insert(artifact.artifact_id.clone(), artifact);
                }
            }
        }
    }
    let mut candidates = found.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .timestamp_created
            .cmp(&left.timestamp_created)
            .then_with(|| right.artifact_id.cmp(&left.artifact_id))
    });
    candidates.truncate(LESSON_WINDOW);

    let mut cases: HashMap<String, CaseState> = HashMap::new();
    let mut shown = 0usize;
    let mut out = Vec::with_capacity(candidates.len());
    for artifact in candidates {
        let Some(lesson) = artifact.experience.as_ref().and_then(lesson_from_event) else {
            continue;
        };
        let skipped = if shown >= limit {
            Some(LessonSkip::OverLimit)
        } else {
            if !cases.contains_key(&lesson.problem_id) {
                let size = store
                    .count_task_artifacts(tenant_id, &artifact.task_id)
                    .await?;
                let state = if size > MAX_CASE_ARTIFACTS {
                    CaseState::TooLarge
                } else {
                    match resolve_case(store, tenant_id, &lesson.problem_id).await {
                        Ok(case) => CaseState::Resolved(Box::new(case)),
                        Err(_) => CaseState::Unresolvable,
                    }
                };
                cases.insert(lesson.problem_id.clone(), state);
            }
            match &cases[&lesson.problem_id] {
                CaseState::TooLarge => Some(LessonSkip::TooLarge),
                CaseState::Unresolvable => Some(LessonSkip::Unresolvable),
                CaseState::Resolved(case) if !case.lesson_conflicts.is_empty() => {
                    Some(LessonSkip::Conflict)
                }
                CaseState::Resolved(case)
                    if !case
                        .current_lessons
                        .iter()
                        .any(|current| current.artifact_id == artifact.artifact_id) =>
                {
                    Some(LessonSkip::Superseded)
                }
                CaseState::Resolved(_) => None,
            }
        };
        if skipped.is_none() {
            shown += 1;
        }
        out.push(RecentLesson {
            problem_id: lesson.problem_id.clone(),
            lesson_id: artifact.artifact_id.clone(),
            event_time_ms: event_time(&artifact),
            created_at_ms: artifact.timestamp_created,
            skipped,
            lesson: lesson.clone(),
        });
    }
    Ok(out)
}

enum CaseState {
    Resolved(Box<ExperienceCase>),
    TooLarge,
    Unresolvable,
}
