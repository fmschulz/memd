use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    lesson_from_event, problem_id, required_event, resolve_case, validation, ConditionalLesson,
    LessonVisibility, ROLE_CORRECTION, ROLE_LESSON,
};
use crate::error::Result;
use crate::store::Store;
use crate::task_memory::{TaskArtifact, TaskSearchFilters};
use crate::types::{ProjectId, TenantId};

/// Exact-scope and lexical query for current lessons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonQuery {
    pub tenant_id: TenantId,
    #[serde(default)]
    pub project_id: ProjectId,
    pub symptom: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub machine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
    #[serde(default)]
    pub include_shared: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// One eligible current lesson and its lexical overlap score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplicableLesson {
    pub artifact: TaskArtifact,
    pub lexical_score: usize,
}

/// Structured reason why recall deliberately returned no lesson.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RecallAbstention {
    NoMatch,
    UnknownRequiredCondition { fields: Vec<String> },
    Conflict { lesson_ids: Vec<String> },
}

/// Result of lesson recall. A conflict never degrades into an arbitrary pick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LessonRecall {
    Matches { lessons: Vec<ApplicableLesson> },
    Abstain { reason: RecallAbstention },
}

/// Retrieve current lessons that match scope, exact conditions, and symptom words.
pub async fn retrieve_lessons<S: Store + ?Sized>(
    store: &S,
    query: LessonQuery,
) -> Result<LessonRecall> {
    if query.symptom.trim().is_empty() {
        return Err(validation("symptom must not be empty"));
    }
    if query.limit == 0 {
        return Err(validation("limit must be greater than zero"));
    }

    let candidates = candidate_lessons(store, &query).await?;
    let mut problem_ids = BTreeSet::new();
    for artifact in candidates.values() {
        if let Some(id) = problem_id(required_event(artifact)?) {
            problem_ids.insert(id.to_string());
        }
    }

    let query_tokens = lexical_tokens(&query.symptom);
    let mut current = HashMap::new();
    let mut branch_conflicts = Vec::new();
    for problem_id in problem_ids {
        let case = resolve_case(store, &query.tenant_id, &problem_id).await?;
        for artifact in case.current_lessons {
            current.insert(artifact.artifact_id.clone(), artifact);
        }
        for ids in case.lesson_conflicts {
            for id in &ids {
                if let Some(artifact) = case
                    .events
                    .iter()
                    .find(|artifact| artifact.artifact_id == *id)
                {
                    current.insert(id.clone(), artifact.clone());
                }
            }
            branch_conflicts.push(ids);
        }
    }

    let mut unknown_fields = BTreeSet::new();
    let mut matches = Vec::new();
    for artifact in current.values() {
        let lesson = lesson_from_event(required_event(artifact)?)
            .expect("current lesson head must contain lesson data");
        if !scope_matches(artifact, lesson, &query) {
            continue;
        }
        let score = lexical_overlap(&query_tokens, &lesson.symptom_terms);
        if score == 0 {
            continue;
        }
        match conditions_match(lesson, &query) {
            ConditionMatch::Eligible => matches.push(ApplicableLesson {
                artifact: artifact.clone(),
                lexical_score: score,
            }),
            ConditionMatch::Unknown(fields) => unknown_fields.extend(fields),
            ConditionMatch::Mismatch => {}
        }
    }

    if matches.is_empty() {
        if !unknown_fields.is_empty() {
            return Ok(LessonRecall::Abstain {
                reason: RecallAbstention::UnknownRequiredCondition {
                    fields: unknown_fields.into_iter().collect(),
                },
            });
        }
        return Ok(LessonRecall::Abstain {
            reason: RecallAbstention::NoMatch,
        });
    }

    let matched_ids = matches
        .iter()
        .map(|item| item.artifact.artifact_id.as_str())
        .collect::<HashSet<_>>();
    for mut ids in branch_conflicts {
        if !ids.iter().any(|id| matched_ids.contains(id.as_str())) {
            continue;
        }
        ids.sort();
        ids.dedup();
        return Ok(LessonRecall::Abstain {
            reason: RecallAbstention::Conflict { lesson_ids: ids },
        });
    }

    let mut explicit_conflicts = BTreeSet::new();
    for item in &matches {
        let lesson = lesson_from_event(required_event(&item.artifact)?)
            .expect("matched artifact must be a lesson");
        for conflict_id in &lesson.conflicts_with {
            if matched_ids.contains(conflict_id.as_str()) {
                explicit_conflicts.insert(item.artifact.artifact_id.clone());
                explicit_conflicts.insert(conflict_id.clone());
            }
        }
    }
    if !explicit_conflicts.is_empty() {
        return Ok(LessonRecall::Abstain {
            reason: RecallAbstention::Conflict {
                lesson_ids: explicit_conflicts.into_iter().collect(),
            },
        });
    }

    matches.sort_by(|left, right| {
        right
            .lexical_score
            .cmp(&left.lexical_score)
            .then_with(|| {
                super::event_time(&right.artifact).cmp(&super::event_time(&left.artifact))
            })
            .then_with(|| left.artifact.artifact_id.cmp(&right.artifact.artifact_id))
    });
    matches.truncate(query.limit);
    Ok(LessonRecall::Matches { lessons: matches })
}

async fn candidate_lessons<S: Store + ?Sized>(
    store: &S,
    query: &LessonQuery,
) -> Result<HashMap<String, TaskArtifact>> {
    let mut artifacts = HashMap::new();
    for role in [ROLE_LESSON, ROLE_CORRECTION] {
        let filters = TaskSearchFilters {
            artifact_role: Some(role.to_string()),
            project_id: if query.include_shared {
                None
            } else {
                query.project_id.as_option().map(str::to_string)
            },
            ..Default::default()
        };
        let chunk_ids = store
            .search_task_projection_chunk_ids(&query.tenant_id, &filters, EXHAUSTIVE_LIMIT)
            .await?;
        for artifact in store
            .resolve_artifacts_for_chunks(&query.tenant_id, &chunk_ids)
            .await?
            .into_values()
        {
            let shared = lesson_from_event(required_event(&artifact)?)
                .is_some_and(|lesson| lesson.visibility == LessonVisibility::Shared);
            if artifact.project_id == query.project_id || (query.include_shared && shared) {
                artifacts.insert(artifact.artifact_id.clone(), artifact);
            }
        }
    }
    Ok(artifacts)
}

fn scope_matches(artifact: &TaskArtifact, lesson: &ConditionalLesson, query: &LessonQuery) -> bool {
    artifact.project_id == query.project_id
        || (query.include_shared && lesson.visibility == LessonVisibility::Shared)
}

enum ConditionMatch {
    Eligible,
    Unknown(Vec<String>),
    Mismatch,
}

fn conditions_match(lesson: &ConditionalLesson, query: &LessonQuery) -> ConditionMatch {
    let mut unknown = Vec::new();
    for (field, required, actual) in [
        (
            "machine",
            lesson.applicability.machine.as_deref(),
            query.machine.as_deref(),
        ),
        (
            "tool",
            lesson.applicability.tool.as_deref(),
            query.tool.as_deref(),
        ),
        (
            "version",
            lesson.applicability.version.as_deref(),
            query.version.as_deref(),
        ),
    ] {
        match (required, actual) {
            (Some(_), None) => unknown.push(field.to_string()),
            (Some(required), Some(actual)) if required != actual => {
                return ConditionMatch::Mismatch;
            }
            _ => {}
        }
    }
    if unknown.is_empty() {
        ConditionMatch::Eligible
    } else {
        ConditionMatch::Unknown(unknown)
    }
}

fn lexical_overlap(query: &HashSet<String>, terms: &[String]) -> usize {
    let lesson_tokens = terms
        .iter()
        .flat_map(|term| lexical_tokens(term))
        .collect::<HashSet<_>>();
    query.intersection(&lesson_tokens).count()
}

fn lexical_tokens(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .filter(|token| !is_stop_word(token))
        .collect()
}

fn is_stop_word(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "by"
            | "for"
            | "from"
            | "in"
            | "is"
            | "it"
            | "not"
            | "of"
            | "on"
            | "or"
            | "that"
            | "the"
            | "this"
            | "to"
            | "was"
            | "with"
    )
}

const fn default_limit() -> usize {
    5
}

// Fits in SQLite's signed LIMIT parameter on 64-bit targets while remaining
// exhaustive for every realistic local store. The API never silently caps to
// a small candidate window.
const EXHAUSTIVE_LIMIT: usize = usize::MAX / 2;
