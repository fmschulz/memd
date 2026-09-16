use std::collections::{HashMap, HashSet};

use super::{
    lesson_from_event, CheckReceipt, ConditionalLesson, ExperienceEvent, LessonApplicability,
    SourceFingerprintCoverage, ROLE_ATTEMPT, ROLE_CHECK, ROLE_CORRECTION, ROLE_LESSON,
    ROLE_PROBLEM,
};
use crate::error::{MemdError, Result};
use crate::task_memory::{ArtifactKind, TaskArtifact};
use crate::types::PromotionState;

pub(crate) fn validation(message: impl Into<String>) -> MemdError {
    MemdError::ValidationError(message.into())
}

pub(crate) fn require_text(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(validation(format!("{field} must not be empty")));
    }
    Ok(())
}

pub(crate) fn validate_terms(terms: &[String]) -> Result<()> {
    if terms.is_empty() || terms.iter().any(|term| term.trim().is_empty()) {
        return Err(validation(
            "symptom_terms must contain at least one non-empty term",
        ));
    }
    Ok(())
}

pub(crate) fn validate_applicability(applicability: &LessonApplicability) -> Result<()> {
    for (field, value) in [
        ("machine", applicability.machine.as_deref()),
        ("tool", applicability.tool.as_deref()),
        ("version", applicability.version.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(validation(format!("{field} must not be empty")));
        }
    }
    if applicability.version.is_some() && applicability.tool.is_none() {
        return Err(validation("version applicability requires tool"));
    }
    Ok(())
}

pub(crate) fn validate_receipt(receipt: &CheckReceipt) -> Result<()> {
    require_text("attempt_id", &receipt.attempt_id)?;
    require_text("cwd", &receipt.cwd)?;
    if receipt.argv.is_empty() || receipt.argv.iter().any(|argument| argument.is_empty()) {
        return Err(validation(
            "argv must contain a non-empty executable and arguments",
        ));
    }
    if receipt.finished_at_ms < receipt.started_at_ms {
        return Err(validation(
            "finished_at_ms must be greater than or equal to started_at_ms",
        ));
    }
    validate_optional_hash("stdout_sha256", receipt.stdout_sha256.as_deref())?;
    validate_optional_hash("stderr_sha256", receipt.stderr_sha256.as_deref())?;
    for fingerprint in [
        receipt.source_before.as_ref(),
        receipt.source_after.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_hash("source fingerprint", &fingerprint.sha256)?;
        if let SourceFingerprintCoverage::ExplicitPaths { paths } = &fingerprint.coverage {
            if paths.is_empty() || paths.iter().any(|path| path.trim().is_empty()) {
                return Err(validation(
                    "explicit fingerprint coverage requires non-empty paths",
                ));
            }
            let unique = paths.iter().collect::<HashSet<_>>();
            if unique.len() != paths.len() {
                return Err(validation(
                    "explicit fingerprint coverage contains duplicate paths",
                ));
            }
        }
    }
    Ok(())
}

fn validate_optional_hash(field: &str, hash: Option<&str>) -> Result<()> {
    if let Some(hash) = hash {
        validate_hash(field, hash)?;
    }
    Ok(())
}

fn validate_hash(field: &str, hash: &str) -> Result<()> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(validation(format!(
            "{field} must be a lowercase 64-character SHA-256 hex digest"
        )));
    }
    Ok(())
}

pub(crate) fn required_event(artifact: &TaskArtifact) -> Result<&ExperienceEvent> {
    artifact.experience.as_ref().ok_or_else(|| {
        validation(format!(
            "artifact '{}' is not an experience record",
            artifact.artifact_id
        ))
    })
}

pub(crate) fn event_time(artifact: &TaskArtifact) -> i64 {
    artifact
        .timestamp_observed
        .unwrap_or(artifact.timestamp_created)
}

pub(crate) fn problem_id(event: &ExperienceEvent) -> Option<&str> {
    match event {
        ExperienceEvent::Problem { .. } => None,
        ExperienceEvent::Attempt { problem_id, .. }
        | ExperienceEvent::Correction { problem_id, .. } => Some(problem_id),
        ExperienceEvent::Check(_) => None,
        ExperienceEvent::Lesson(lesson) => Some(&lesson.problem_id),
    }
}

pub(crate) fn attempt_ordinal(artifact: &TaskArtifact) -> Result<u32> {
    match required_event(artifact)? {
        ExperienceEvent::Attempt { ordinal, .. } => Ok(*ordinal),
        _ => Err(validation(format!(
            "artifact '{}' is not an attempt",
            artifact.artifact_id
        ))),
    }
}

pub(crate) fn current_attempt(artifacts: &[TaskArtifact]) -> Result<Option<&TaskArtifact>> {
    let attempts = artifacts
        .iter()
        .filter(|artifact| matches!(artifact.experience, Some(ExperienceEvent::Attempt { .. })))
        .collect::<Vec<_>>();
    if attempts.is_empty() {
        return Ok(None);
    }

    let by_id = attempts
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), *artifact))
        .collect::<HashMap<_, _>>();
    let mut ordinals = HashSet::new();
    for attempt in &attempts {
        let ExperienceEvent::Attempt {
            previous_attempt_id,
            ordinal,
            ..
        } = required_event(attempt)?
        else {
            unreachable!();
        };
        if !ordinals.insert(*ordinal) {
            return Err(validation(format!(
                "duplicate attempt ordinal {ordinal} in task '{}'",
                attempt.task_id
            )));
        }
        match (*ordinal, previous_attempt_id.as_deref()) {
            (1, None) => {}
            (1, Some(_)) => {
                return Err(validation("attempt ordinal 1 cannot have a predecessor"));
            }
            (_, None) => {
                return Err(validation(format!(
                    "attempt ordinal {ordinal} requires previous_attempt_id"
                )));
            }
            (_, Some(previous_id)) => {
                let previous = by_id.get(previous_id).ok_or_else(|| {
                    validation(format!("previous attempt '{previous_id}' does not exist"))
                })?;
                let previous_ordinal = attempt_ordinal(previous)?;
                let expected = previous_ordinal
                    .checked_add(1)
                    .ok_or_else(|| validation("attempt ordinal overflow"))?;
                if expected != *ordinal {
                    return Err(validation(format!(
                        "attempt '{}' does not extend ordinal {}",
                        attempt.artifact_id, previous_ordinal
                    )));
                }
            }
        }
    }
    Ok(attempts
        .into_iter()
        .max_by_key(|artifact| attempt_ordinal(artifact).unwrap_or(0)))
}

pub(crate) fn validate_case(artifacts: &[TaskArtifact]) -> Result<()> {
    let problems = artifacts
        .iter()
        .filter(|artifact| matches!(artifact.experience, Some(ExperienceEvent::Problem { .. })))
        .collect::<Vec<_>>();
    if problems.len() != 1 {
        return Err(validation(format!(
            "experience task must contain exactly one problem, found {}",
            problems.len()
        )));
    }
    let problem = problems[0];
    for artifact in artifacts {
        if artifact.task_id != problem.task_id
            || artifact.tenant_id != problem.tenant_id
            || artifact.project_id != problem.project_id
        {
            return Err(validation(format!(
                "artifact '{}' crosses its problem scope",
                artifact.artifact_id
            )));
        }
        validate_artifact_shape(artifact)?;
        if let Some(referenced_problem_id) = problem_id(required_event(artifact)?) {
            if referenced_problem_id != problem.artifact_id {
                return Err(validation(format!(
                    "artifact '{}' references a different problem",
                    artifact.artifact_id
                )));
            }
        }
    }
    current_attempt(artifacts)?;

    let by_id = artifacts
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact))
        .collect::<HashMap<_, _>>();
    for artifact in artifacts {
        match required_event(artifact)? {
            ExperienceEvent::Problem { .. } => {}
            ExperienceEvent::Attempt {
                problem_id,
                previous_attempt_id,
                ..
            } => {
                ensure_ref(&by_id, problem_id, "problem", artifact)?;
                if let Some(previous) = previous_attempt_id {
                    ensure_ref(&by_id, previous, "previous attempt", artifact)?;
                }
            }
            ExperienceEvent::Check(receipt) => {
                validate_receipt(receipt)?;
                let target = ensure_ref(&by_id, &receipt.attempt_id, "attempt", artifact)?;
                if !matches!(required_event(target)?, ExperienceEvent::Attempt { .. }) {
                    return Err(validation(format!(
                        "check '{}' targets a non-attempt artifact",
                        artifact.artifact_id
                    )));
                }
            }
            ExperienceEvent::Lesson(lesson) => {
                validate_embedded_lesson(lesson)?;
                validate_lesson_refs(&by_id, artifact, lesson)?;
            }
            ExperienceEvent::Correction {
                supersedes_id,
                replacement,
                ..
            } => {
                validate_embedded_lesson(replacement)?;
                validate_lesson_refs(&by_id, artifact, replacement)?;
                let target = ensure_ref(&by_id, supersedes_id, "superseded lesson", artifact)?;
                if lesson_from_event(required_event(target)?).is_none() {
                    return Err(validation(format!(
                        "correction '{}' supersedes a non-lesson artifact",
                        artifact.artifact_id
                    )));
                }
                if event_time(artifact) < event_time(target) {
                    return Err(validation(format!(
                        "correction '{}' predates its superseded lesson",
                        artifact.artifact_id
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_lesson_refs(
    by_id: &HashMap<&str, &TaskArtifact>,
    artifact: &TaskArtifact,
    lesson: &ConditionalLesson,
) -> Result<()> {
    ensure_ref(by_id, &lesson.problem_id, "problem", artifact)?;
    let check = ensure_ref(
        by_id,
        &lesson.supporting_check_id,
        "supporting check",
        artifact,
    )?;
    let ExperienceEvent::Check(receipt) = required_event(check)? else {
        return Err(validation(format!(
            "lesson '{}' has a non-check supporting reference",
            artifact.artifact_id
        )));
    };
    if !receipt.supports_lesson() {
        return Err(validation(format!(
            "lesson '{}' is supported by an unsuccessful or incomplete check",
            artifact.artifact_id
        )));
    }
    for conflict in &lesson.conflicts_with {
        let target = ensure_ref(by_id, conflict, "conflicting lesson", artifact)?;
        if lesson_from_event(required_event(target)?).is_none() {
            return Err(validation(format!(
                "lesson '{}' conflicts with a non-lesson artifact",
                artifact.artifact_id
            )));
        }
    }
    Ok(())
}

fn ensure_ref<'a>(
    by_id: &HashMap<&str, &'a TaskArtifact>,
    reference: &str,
    kind: &str,
    artifact: &TaskArtifact,
) -> Result<&'a TaskArtifact> {
    by_id.get(reference).copied().ok_or_else(|| {
        validation(format!(
            "artifact '{}' has missing {kind} reference '{reference}'",
            artifact.artifact_id
        ))
    })
}

pub(crate) fn validate_artifact_shape(artifact: &TaskArtifact) -> Result<()> {
    let event = required_event(artifact)?;
    let (expected_kind, expected_role, expected_status, expected_reply, expected_relation) =
        match event {
            ExperienceEvent::Problem {
                description,
                symptom_terms,
            } => {
                require_text("description", description)?;
                validate_terms(symptom_terms)?;
                (ArtifactKind::TaskProgress, ROLE_PROBLEM, "open", None, None)
            }
            ExperienceEvent::Attempt {
                problem_id,
                previous_attempt_id,
                action,
                ordinal,
            } => {
                require_text("action", action)?;
                if *ordinal == 0 {
                    return Err(validation("attempt ordinal must be at least 1"));
                }
                (
                    ArtifactKind::RunFinish,
                    ROLE_ATTEMPT,
                    "recorded",
                    Some(previous_attempt_id.as_deref().unwrap_or(problem_id)),
                    Some("attempts_problem"),
                )
            }
            ExperienceEvent::Check(receipt) => {
                validate_receipt(receipt)?;
                (
                    ArtifactKind::Verification,
                    ROLE_CHECK,
                    receipt.status(),
                    Some(receipt.attempt_id.as_str()),
                    Some("checks_attempt"),
                )
            }
            ExperienceEvent::Lesson(lesson) => {
                validate_embedded_lesson(lesson)?;
                (
                    ArtifactKind::Decision,
                    ROLE_LESSON,
                    "recorded",
                    Some(lesson.supporting_check_id.as_str()),
                    Some("learned_from_check"),
                )
            }
            ExperienceEvent::Correction {
                problem_id,
                supersedes_id,
                replacement,
                ..
            } => {
                if problem_id != &replacement.problem_id {
                    return Err(validation(
                        "correction problem_id must match replacement.problem_id",
                    ));
                }
                validate_embedded_lesson(replacement)?;
                (
                    ArtifactKind::Revision,
                    ROLE_CORRECTION,
                    "recorded",
                    Some(supersedes_id.as_str()),
                    Some("supersedes_lesson"),
                )
            }
        };
    if artifact.artifact_kind != expected_kind
        || artifact.artifact_role.as_deref() != Some(expected_role)
        || artifact.status.as_deref() != Some(expected_status)
        || artifact.reply_to_artifact_id.as_deref() != expected_reply
        || artifact.relation_kind.as_deref() != expected_relation
    {
        return Err(validation(format!(
            "artifact '{}' has envelope fields inconsistent with its experience event",
            artifact.artifact_id
        )));
    }
    if artifact.promotion_state != PromotionState::Canonical {
        return Err(validation(format!(
            "artifact '{}' must use canonical promotion_state",
            artifact.artifact_id
        )));
    }
    let expected_verification =
        matches!(event, ExperienceEvent::Check(_)).then_some("recorded_check");
    if artifact.verification_status.as_deref() != expected_verification
        || artifact.supports_claim.is_some()
        || artifact.approval_state.is_some()
    {
        return Err(validation(format!(
            "artifact '{}' carries unsupported trust fields",
            artifact.artifact_id
        )));
    }
    let mut expected_related = match event {
        ExperienceEvent::Problem { .. } => Vec::new(),
        ExperienceEvent::Attempt {
            problem_id,
            previous_attempt_id,
            ..
        } => {
            let mut ids = vec![problem_id.clone()];
            ids.extend(previous_attempt_id.clone());
            ids
        }
        ExperienceEvent::Check(receipt) => vec![receipt.attempt_id.clone()],
        ExperienceEvent::Lesson(lesson) => lesson_related_ids(lesson),
        ExperienceEvent::Correction {
            supersedes_id,
            replacement,
            ..
        } => {
            let mut ids = lesson_related_ids(replacement);
            ids.push(supersedes_id.clone());
            ids
        }
    };
    expected_related.sort();
    expected_related.dedup();
    let mut actual_related = artifact.related_artifact_ids.clone();
    actual_related.sort();
    actual_related.dedup();
    if actual_related != expected_related {
        return Err(validation(format!(
            "artifact '{}' has related_artifact_ids inconsistent with its experience event",
            artifact.artifact_id
        )));
    }
    Ok(())
}

pub(crate) fn lesson_related_ids(lesson: &ConditionalLesson) -> Vec<String> {
    let mut ids = vec![
        lesson.problem_id.clone(),
        lesson.supporting_check_id.clone(),
    ];
    ids.extend(lesson.conflicts_with.clone());
    ids.sort();
    ids.dedup();
    ids
}

pub(crate) fn validate_embedded_lesson(lesson: &ConditionalLesson) -> Result<()> {
    require_text("problem_id", &lesson.problem_id)?;
    require_text("supporting_check_id", &lesson.supporting_check_id)?;
    require_text("guidance", &lesson.guidance)?;
    validate_terms(&lesson.symptom_terms)?;
    validate_applicability(&lesson.applicability)
}

pub(crate) fn current_lesson_heads(
    artifacts: &[TaskArtifact],
) -> Result<(Vec<TaskArtifact>, Vec<Vec<String>>)> {
    let by_id = artifacts
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact))
        .collect::<HashMap<_, _>>();
    let mut children: HashMap<&str, Vec<&TaskArtifact>> = HashMap::new();
    let roots = artifacts
        .iter()
        .filter(|artifact| matches!(artifact.experience, Some(ExperienceEvent::Lesson(_))))
        .collect::<Vec<_>>();
    for artifact in artifacts {
        if let Some(ExperienceEvent::Correction { supersedes_id, .. }) = &artifact.experience {
            if by_id.contains_key(supersedes_id.as_str()) {
                children
                    .entry(supersedes_id.as_str())
                    .or_default()
                    .push(artifact);
            }
        }
    }

    let mut heads = Vec::new();
    let mut conflicts = Vec::new();
    for root in roots {
        let mut terminals = Vec::new();
        collect_terminals(root, &children, &mut HashSet::new(), &mut terminals)?;
        let max_time = terminals
            .iter()
            .map(|artifact| event_time(artifact))
            .max()
            .expect("root yields at least one terminal");
        let mut latest = terminals
            .into_iter()
            .filter(|artifact| event_time(artifact) == max_time)
            .collect::<Vec<_>>();
        latest.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
        if latest.len() == 1 {
            heads.push(latest[0].clone());
        } else {
            conflicts.push(
                latest
                    .iter()
                    .map(|artifact| artifact.artifact_id.clone())
                    .collect(),
            );
        }
    }
    sort_artifacts(&mut heads);
    conflicts.sort();
    Ok((heads, conflicts))
}

fn collect_terminals<'a>(
    artifact: &'a TaskArtifact,
    children: &HashMap<&str, Vec<&'a TaskArtifact>>,
    visiting: &mut HashSet<String>,
    terminals: &mut Vec<&'a TaskArtifact>,
) -> Result<()> {
    if !visiting.insert(artifact.artifact_id.clone()) {
        return Err(validation("correction chain contains a cycle"));
    }
    match children.get(artifact.artifact_id.as_str()) {
        Some(next) if !next.is_empty() => {
            for child in next {
                collect_terminals(child, children, visiting, terminals)?;
            }
        }
        _ => terminals.push(artifact),
    }
    visiting.remove(&artifact.artifact_id);
    Ok(())
}

pub(crate) fn sort_artifacts(artifacts: &mut [TaskArtifact]) {
    artifacts.sort_by(|left, right| {
        event_time(left)
            .cmp(&event_time(right))
            .then_with(|| left.timestamp_created.cmp(&right.timestamp_created))
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TenantId;

    #[test]
    fn overflowing_attempt_ordinal_is_a_validation_error() {
        let tenant = TenantId::new("ordinal_test").unwrap();
        let mut previous = TaskArtifact::new(ArtifactKind::RunFinish, tenant.clone(), "task");
        previous.experience = Some(ExperienceEvent::Attempt {
            problem_id: "problem".to_string(),
            previous_attempt_id: None,
            ordinal: u32::MAX,
            action: "previous".to_string(),
        });
        let mut child = TaskArtifact::new(ArtifactKind::RunFinish, tenant, "task");
        child.experience = Some(ExperienceEvent::Attempt {
            problem_id: "problem".to_string(),
            previous_attempt_id: Some(previous.artifact_id.clone()),
            ordinal: 2,
            action: "child".to_string(),
        });

        let error = current_attempt(&[child, previous]).unwrap_err();
        assert!(error.to_string().contains("attempt ordinal overflow"));
    }
}
