use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    current_lesson_heads, event_time, required_event, sort_artifacts, validate_artifact_shape,
    validate_case, validation, ExperienceEvent,
};
use crate::error::Result;
use crate::store::Store;
use crate::task_memory::{build_task_projections_minimal, TaskArtifact};
use crate::types::TenantId;

const BUNDLE_FORMAT: &str = "memd-experience";
const BUNDLE_VERSION: u32 = 1;

/// Lossless, versioned bundle of canonical experience artifacts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperienceBundleV1 {
    pub format: String,
    pub version: u32,
    pub tenant_id: TenantId,
    pub artifacts: Vec<TaskArtifact>,
}

/// Outcome of an idempotent bundle import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub inserted_artifact_ids: Vec<String>,
    pub replayed_artifact_ids: Vec<String>,
}

/// Export complete causal histories for the requested problem IDs.
pub async fn export_bundle<S: Store + ?Sized>(
    store: &S,
    tenant_id: &TenantId,
    problem_ids: &[String],
) -> Result<ExperienceBundleV1> {
    if problem_ids.is_empty() {
        return Err(validation("at least one problem_id is required"));
    }
    let mut artifacts = HashMap::new();
    for problem_id in problem_ids {
        let case = super::resolve_case(store, tenant_id, problem_id).await?;
        for artifact in case.events {
            artifacts.insert(artifact.artifact_id.clone(), artifact);
        }
    }
    let mut artifacts = artifacts.into_values().collect::<Vec<_>>();
    sort_artifacts(&mut artifacts);
    Ok(ExperienceBundleV1 {
        format: BUNDLE_FORMAT.to_string(),
        version: BUNDLE_VERSION,
        tenant_id: tenant_id.clone(),
        artifacts,
    })
}

/// Import a canonical bundle without retargeting or executing stored commands.
///
/// The entire bundle is checked before the first write. Retrying after a store
/// failure is safe because exact artifacts are treated as replay while any ID
/// collision with different content is rejected.
pub async fn import_bundle<S: Store + ?Sized>(
    store: &S,
    expected_tenant: &TenantId,
    bundle: ExperienceBundleV1,
) -> Result<ImportReport> {
    let _guard = super::EXPERIENCE_MUTATIONS.lock().await;
    import_bundle_unlocked(store, expected_tenant, bundle).await
}

async fn import_bundle_unlocked<S: Store + ?Sized>(
    store: &S,
    expected_tenant: &TenantId,
    bundle: ExperienceBundleV1,
) -> Result<ImportReport> {
    validate_bundle_header(expected_tenant, &bundle)?;
    let bundle_by_id = unique_bundle_artifacts(&bundle)?;
    let replayed = check_global_artifact_collisions(store, &bundle_by_id).await?;
    check_global_task_collisions(store, expected_tenant, &bundle.artifacts).await?;
    validate_combined_cases(store, expected_tenant, &bundle.artifacts, &replayed).await?;

    let mut pending = bundle
        .artifacts
        .into_iter()
        .filter(|artifact| !replayed.contains(&artifact.artifact_id))
        .collect::<Vec<_>>();
    let ordered = topological_order(&mut pending)?;
    let mut inserted_artifact_ids = Vec::with_capacity(ordered.len());
    for artifact in ordered {
        let projections = build_task_projections_minimal(&artifact);
        store
            .add_task_artifact(artifact.clone(), projections)
            .await?;
        inserted_artifact_ids.push(artifact.artifact_id);
    }

    let mut replayed_artifact_ids = replayed.into_iter().collect::<Vec<_>>();
    inserted_artifact_ids.sort();
    replayed_artifact_ids.sort();
    Ok(ImportReport {
        inserted_artifact_ids,
        replayed_artifact_ids,
    })
}

fn validate_bundle_header(expected_tenant: &TenantId, bundle: &ExperienceBundleV1) -> Result<()> {
    if bundle.format != BUNDLE_FORMAT || bundle.version != BUNDLE_VERSION {
        return Err(validation(format!(
            "unsupported experience bundle '{}'/v{}; expected '{BUNDLE_FORMAT}'/v{BUNDLE_VERSION}",
            bundle.format, bundle.version
        )));
    }
    if &bundle.tenant_id != expected_tenant {
        return Err(validation(format!(
            "bundle tenant '{}' does not match target tenant '{}'",
            bundle.tenant_id, expected_tenant
        )));
    }
    if bundle.artifacts.is_empty() {
        return Err(validation("experience bundle contains no artifacts"));
    }
    Ok(())
}

fn unique_bundle_artifacts(bundle: &ExperienceBundleV1) -> Result<HashMap<String, TaskArtifact>> {
    let mut by_id = HashMap::new();
    for artifact in &bundle.artifacts {
        if artifact.tenant_id != bundle.tenant_id {
            return Err(validation(format!(
                "artifact '{}' belongs to tenant '{}', not bundle tenant '{}'",
                artifact.artifact_id, artifact.tenant_id, bundle.tenant_id
            )));
        }
        validate_artifact_shape(artifact)?;
        if by_id
            .insert(artifact.artifact_id.clone(), artifact.clone())
            .is_some()
        {
            return Err(validation(format!(
                "bundle contains duplicate artifact_id '{}'",
                artifact.artifact_id
            )));
        }
    }
    Ok(by_id)
}

async fn check_global_artifact_collisions<S: Store + ?Sized>(
    store: &S,
    bundle_by_id: &HashMap<String, TaskArtifact>,
) -> Result<HashSet<String>> {
    let mut replayed = HashSet::new();
    for (artifact_id, incoming) in bundle_by_id {
        let Some(existing) = store.get_task_artifact_global(artifact_id).await? else {
            continue;
        };
        if existing.tenant_id == incoming.tenant_id && existing == *incoming {
            replayed.insert(artifact_id.clone());
            continue;
        }
        return Err(validation(format!(
            "artifact_id collision for '{artifact_id}' in tenant '{}'",
            existing.tenant_id
        )));
    }
    Ok(replayed)
}

async fn check_global_task_collisions<S: Store + ?Sized>(
    store: &S,
    expected_tenant: &TenantId,
    artifacts: &[TaskArtifact],
) -> Result<()> {
    let task_ids = artifacts
        .iter()
        .map(|artifact| artifact.task_id.as_str())
        .collect::<HashSet<_>>();
    for task_id in task_ids {
        let Some(task) = store.get_task_global(task_id).await? else {
            continue;
        };
        if &task.tenant_id != expected_tenant {
            return Err(validation(format!(
                "task_id collision for '{}' in tenant '{}'",
                task.task_id, task.tenant_id
            )));
        }
    }
    Ok(())
}

async fn validate_combined_cases<S: Store + ?Sized>(
    store: &S,
    tenant_id: &TenantId,
    incoming: &[TaskArtifact],
    replayed: &HashSet<String>,
) -> Result<()> {
    let mut by_task: HashMap<&str, Vec<TaskArtifact>> = HashMap::new();
    for artifact in incoming {
        by_task
            .entry(artifact.task_id.as_str())
            .or_default()
            .push(artifact.clone());
    }

    for (task_id, mut imported) in by_task {
        let existing = store
            .list_task_artifacts(tenant_id, task_id)
            .await?
            .into_iter()
            .filter(|artifact| artifact.experience.is_some())
            .collect::<Vec<_>>();
        reject_stale_override(&existing, &imported, replayed)?;
        let mut combined = existing;
        combined.extend(
            imported
                .drain(..)
                .filter(|artifact| !replayed.contains(&artifact.artifact_id)),
        );
        sort_artifacts(&mut combined);
        validate_case(&combined)?;
        current_lesson_heads(&combined)?;
    }
    Ok(())
}

fn reject_stale_override(
    existing: &[TaskArtifact],
    imported: &[TaskArtifact],
    replayed: &HashSet<String>,
) -> Result<()> {
    if existing.is_empty() {
        return Ok(());
    }
    let (heads, conflicts) = current_lesson_heads(existing)?;
    let existing_by_id = existing
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact))
        .collect::<HashMap<_, _>>();
    let imported_by_id = imported
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact))
        .collect::<HashMap<_, _>>();
    let mut current_by_root: HashMap<String, Vec<&TaskArtifact>> = HashMap::new();
    for head in &heads {
        current_by_root
            .entry(lesson_root_id(&head.artifact_id, &existing_by_id)?)
            .or_default()
            .push(head);
    }
    for conflict_id in conflicts.iter().flatten() {
        let artifact = existing_by_id
            .get(conflict_id.as_str())
            .ok_or_else(|| validation(format!("conflicting head '{conflict_id}' is missing")))?;
        current_by_root
            .entry(lesson_root_id(conflict_id, &existing_by_id)?)
            .or_default()
            .push(artifact);
    }
    for correction in imported {
        if replayed.contains(&correction.artifact_id) {
            continue;
        }
        let ExperienceEvent::Correction { supersedes_id, .. } = required_event(correction)? else {
            continue;
        };
        let Some(existing_target) =
            first_existing_correction_target(supersedes_id, &existing_by_id, &imported_by_id)?
        else {
            continue;
        };
        let root = lesson_root_id(&existing_target.artifact_id, &existing_by_id)?;
        let Some(current) = current_by_root.get(&root) else {
            continue;
        };
        if current
            .iter()
            .any(|head| head.artifact_id == existing_target.artifact_id)
        {
            continue;
        }
        let newest = current
            .iter()
            .max_by_key(|head| event_time(head))
            .expect("current lesson root has at least one head");
        if event_time(correction) >= event_time(newest) {
            return Err(validation(format!(
                "correction '{}' targets stale ancestor '{}' and could override current head '{}'",
                correction.artifact_id, existing_target.artifact_id, newest.artifact_id
            )));
        }
    }
    Ok(())
}

fn first_existing_correction_target<'a>(
    target_id: &str,
    existing_by_id: &HashMap<&str, &'a TaskArtifact>,
    imported_by_id: &HashMap<&str, &TaskArtifact>,
) -> Result<Option<&'a TaskArtifact>> {
    let mut current_id = target_id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current_id.to_string()) {
            return Err(validation("imported correction chain contains a cycle"));
        }
        if let Some(existing) = existing_by_id.get(current_id) {
            return Ok(Some(*existing));
        }
        let Some(imported) = imported_by_id.get(current_id) else {
            return Ok(None);
        };
        let ExperienceEvent::Correction { supersedes_id, .. } = required_event(imported)? else {
            return Ok(None);
        };
        current_id = supersedes_id;
    }
}

fn lesson_root_id(artifact_id: &str, by_id: &HashMap<&str, &TaskArtifact>) -> Result<String> {
    let mut current = artifact_id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current.to_string()) {
            return Err(validation("correction chain contains a cycle"));
        }
        let artifact = by_id
            .get(current)
            .ok_or_else(|| validation(format!("lesson ancestor '{current}' does not exist")))?;
        match required_event(artifact)? {
            ExperienceEvent::Lesson(_) => return Ok(current.to_string()),
            ExperienceEvent::Correction { supersedes_id, .. } => current = supersedes_id,
            _ => {
                return Err(validation(format!(
                    "artifact '{current}' is not in a lesson correction chain"
                )));
            }
        }
    }
}

fn topological_order(pending: &mut Vec<TaskArtifact>) -> Result<Vec<TaskArtifact>> {
    let imported_ids = pending
        .iter()
        .map(|artifact| artifact.artifact_id.clone())
        .collect::<HashSet<_>>();
    let mut written = HashSet::new();
    let mut ordered = Vec::with_capacity(pending.len());
    pending.sort_by(|left, right| {
        event_time(left)
            .cmp(&event_time(right))
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });
    while !pending.is_empty() {
        let Some(index) = pending.iter().position(|artifact| {
            causal_reference_ids(required_event(artifact).expect("shape prevalidated"))
                .into_iter()
                .all(|reference| !imported_ids.contains(reference) || written.contains(reference))
        }) else {
            return Err(validation(
                "experience bundle contains a causal cycle or unresolved reference",
            ));
        };
        let artifact = pending.remove(index);
        written.insert(artifact.artifact_id.clone());
        ordered.push(artifact);
    }
    Ok(ordered)
}

fn causal_reference_ids(event: &ExperienceEvent) -> Vec<&str> {
    match event {
        ExperienceEvent::Problem { .. } => Vec::new(),
        ExperienceEvent::Attempt {
            problem_id,
            previous_attempt_id,
            ..
        } => {
            let mut refs = vec![problem_id.as_str()];
            if let Some(previous) = previous_attempt_id.as_deref() {
                refs.push(previous);
            }
            refs
        }
        ExperienceEvent::Check(receipt) => vec![receipt.attempt_id.as_str()],
        ExperienceEvent::Lesson(lesson) => vec![
            lesson.problem_id.as_str(),
            lesson.supporting_check_id.as_str(),
        ],
        ExperienceEvent::Correction {
            problem_id,
            supersedes_id,
            replacement,
        } => vec![
            problem_id.as_str(),
            supersedes_id.as_str(),
            replacement.supporting_check_id.as_str(),
        ],
    }
}
