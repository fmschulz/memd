mod common;

use std::path::Path;
use std::sync::Arc;

use memd::experience::{
    export_bundle, import_bundle, record, record_check, resolve_case, retrieve_lessons,
    validate_check_target, CheckReceipt, ConditionalLesson, ExperienceBundleV1, ExperienceContext,
    ExperienceEvent, ExperienceResolutionState, LessonApplicability, LessonQuery, LessonRecall,
    LessonVisibility, RecallAbstention, RecordEvent, SourceFingerprint, SourceFingerprintCoverage,
};
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::{MemoryStore, Store};
use memd::task_memory::{
    build_task_projections_minimal, ArtifactKind, ExecutionContext, TaskArtifact,
};
use memd::types::{ProjectId, PromotionState, TenantId};

const START_MS: i64 = 1_800_000_000_000;

#[test]
fn task_artifact_without_experience_keeps_legacy_json_shape() {
    let artifact = TaskArtifact::new_task_start(tenant("legacy"));
    let json = serde_json::to_value(&artifact).unwrap();
    assert!(json.get("experience").is_none());

    let restored: TaskArtifact = serde_json::from_value(json).unwrap();
    assert!(restored.experience.is_none());
}

#[tokio::test]
async fn causal_records_resolve_current_attempt_and_correction() {
    let store = MemoryStore::new();
    let problem = record_problem(&store, "project_a", START_MS).await;
    let attempt = record_attempt(&store, &problem, None, START_MS + 1).await;

    let target = validate_check_target(
        &store,
        &problem.tenant_id,
        &problem.project_id,
        &attempt.artifact_id,
    )
    .await
    .unwrap();
    assert_eq!(target.ordinal, 1);

    let check = record_check(
        &store,
        context("project_a", START_MS + 2),
        successful_receipt(&attempt.artifact_id, START_MS + 2),
    )
    .await
    .unwrap();
    assert_eq!(check.verification_status.as_deref(), Some("recorded_check"));

    let lesson = record(
        &store,
        context("project_a", START_MS + 3),
        RecordEvent::Lesson(lesson(
            &problem,
            &check,
            "Use the locked dependency graph",
            LessonVisibility::Project,
        )),
    )
    .await
    .unwrap();
    let case = resolve_case(&store, &problem.tenant_id, &problem.artifact_id)
        .await
        .unwrap();
    assert_eq!(case.resolution_state, ExperienceResolutionState::Resolved);
    assert_eq!(case.current_lessons[0].artifact_id, lesson.artifact_id);

    let correction = record(
        &store,
        context("project_a", START_MS + 4),
        RecordEvent::Correction {
            problem_id: problem.artifact_id.clone(),
            supersedes_id: lesson.artifact_id.clone(),
            replacement: lesson_with_guidance(
                &problem,
                &check,
                "Regenerate the lockfile before using the dependency graph",
                LessonVisibility::Project,
            ),
        },
    )
    .await
    .unwrap();
    let case = resolve_case(&store, &problem.tenant_id, &problem.artifact_id)
        .await
        .unwrap();
    assert_eq!(case.current_lessons[0].artifact_id, correction.artifact_id);

    let stale = record(
        &store,
        context("project_a", START_MS + 5),
        RecordEvent::Correction {
            problem_id: problem.artifact_id.clone(),
            supersedes_id: lesson.artifact_id,
            replacement: lesson_with_guidance(
                &problem,
                &check,
                "Late stale guidance",
                LessonVisibility::Project,
            ),
        },
    )
    .await
    .unwrap_err();
    assert!(stale.to_string().contains("not a current lesson head"));

    validate_check_target(
        &store,
        &problem.tenant_id,
        &problem.project_id,
        &attempt.artifact_id,
    )
    .await
    .unwrap();
    let second_attempt = record_attempt(
        &store,
        &problem,
        Some(attempt.artifact_id.clone()),
        START_MS + 6,
    )
    .await;
    assert!(matches!(
        second_attempt.experience,
        Some(ExperienceEvent::Attempt { ordinal: 2, .. })
    ));
    let stale_preflight = validate_check_target(
        &store,
        &problem.tenant_id,
        &problem.project_id,
        &attempt.artifact_id,
    )
    .await
    .unwrap_err();
    assert!(stale_preflight
        .to_string()
        .contains("not the current attempt"));

    let historical_check = record_check(
        &store,
        context("project_a", START_MS + 7),
        successful_receipt(&attempt.artifact_id, START_MS + 7),
    )
    .await
    .unwrap();
    assert_eq!(
        historical_check.reply_to_artifact_id.as_deref(),
        Some(attempt.artifact_id.as_str())
    );
    let case = resolve_case(&store, &problem.tenant_id, &problem.artifact_id)
        .await
        .unwrap();
    assert_eq!(case.problem.status.as_deref(), Some("open"));
}

#[tokio::test]
async fn lessons_require_complete_successful_receipts_and_exact_conditions() {
    let store = MemoryStore::new();
    let problem = record_problem(&store, "project_a", START_MS).await;
    let attempt = record_attempt(&store, &problem, None, START_MS + 1).await;
    let mut changed_receipt = successful_receipt(&attempt.artifact_id, START_MS + 2);
    changed_receipt.source_before = Some(fingerprint("a"));
    changed_receipt.source_after = Some(fingerprint("b"));
    let changed_check = record_check(&store, context("project_a", START_MS + 2), changed_receipt)
        .await
        .unwrap();
    assert_eq!(changed_check.status.as_deref(), Some("source_unverified"));
    let rejected = record(
        &store,
        context("project_a", START_MS + 3),
        RecordEvent::Lesson(lesson(
            &problem,
            &changed_check,
            "This must not resolve",
            LessonVisibility::Shared,
        )),
    )
    .await
    .unwrap_err();
    assert!(rejected.to_string().contains("supporting check must have"));

    let check = record_check(
        &store,
        context("project_a", START_MS + 4),
        successful_receipt(&attempt.artifact_id, START_MS + 4),
    )
    .await
    .unwrap();
    let shared_lesson = record(
        &store,
        context("project_a", START_MS + 5),
        RecordEvent::Lesson(lesson(
            &problem,
            &check,
            "Use cargo's locked dependency graph",
            LessonVisibility::Shared,
        )),
    )
    .await
    .unwrap();

    let local_only = recall(&store, "project_b", false, None, None, None).await;
    assert_eq!(
        local_only,
        LessonRecall::Abstain {
            reason: RecallAbstention::NoMatch
        }
    );
    let unknown = recall(&store, "project_b", true, None, None, None).await;
    assert_eq!(
        unknown,
        LessonRecall::Abstain {
            reason: RecallAbstention::UnknownRequiredCondition {
                fields: vec![
                    "machine".to_string(),
                    "tool".to_string(),
                    "version".to_string()
                ]
            }
        }
    );
    let matched = recall(
        &store,
        "project_b",
        true,
        Some("nelli-gpu"),
        Some("cargo"),
        Some("1.98.0"),
    )
    .await;
    let LessonRecall::Matches { lessons } = matched else {
        panic!("exact conditions should retrieve the shared lesson");
    };
    assert_eq!(lessons[0].artifact.artifact_id, shared_lesson.artifact_id);

    let wrong_machine = recall(
        &store,
        "project_b",
        true,
        Some("fw13"),
        Some("cargo"),
        Some("1.98.0"),
    )
    .await;
    assert_eq!(
        wrong_machine,
        LessonRecall::Abstain {
            reason: RecallAbstention::NoMatch
        }
    );

    let stop_words_only = retrieve_lessons(
        &store,
        LessonQuery {
            tenant_id: problem.tenant_id.clone(),
            project_id: ProjectId::from("project_b"),
            symptom: "the and not a".to_string(),
            machine: Some("nelli-gpu".to_string()),
            tool: Some("cargo".to_string()),
            version: Some("1.98.0".to_string()),
            include_shared: true,
            limit: 5,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        stop_words_only,
        LessonRecall::Abstain {
            reason: RecallAbstention::NoMatch
        }
    );
}

#[tokio::test]
async fn unknown_source_and_signal_termination_remain_non_supporting_receipts() {
    let store = MemoryStore::new();
    let problem = record_problem(&store, "project_a", START_MS).await;
    let attempt = record_attempt(&store, &problem, None, START_MS + 1).await;

    let mut unknown_source = successful_receipt(&attempt.artifact_id, START_MS + 2);
    unknown_source.source_before = None;
    unknown_source.source_after = None;
    let unknown_check = record_check(&store, context("project_a", START_MS + 2), unknown_source)
        .await
        .unwrap();
    assert_eq!(unknown_check.status.as_deref(), Some("source_unverified"));
    let rejected = record(
        &store,
        context("project_a", START_MS + 3),
        RecordEvent::Lesson(lesson(
            &problem,
            &unknown_check,
            "Unknown source cannot support this lesson",
            LessonVisibility::Project,
        )),
    )
    .await
    .unwrap_err();
    assert!(rejected.to_string().contains("unchanged source evidence"));

    let mut signaled = successful_receipt(&attempt.artifact_id, START_MS + 4);
    signaled.exit_code = None;
    let signal_check = record_check(&store, context("project_a", START_MS + 4), signaled)
        .await
        .unwrap();
    assert_eq!(signal_check.status.as_deref(), Some("failed"));
}

#[tokio::test]
async fn explicit_current_conflicts_abstain() {
    let store = MemoryStore::new();
    let problem = record_problem(&store, "project_a", START_MS).await;
    let attempt = record_attempt(&store, &problem, None, START_MS + 1).await;
    let check = record_check(
        &store,
        context("project_a", START_MS + 2),
        successful_receipt(&attempt.artifact_id, START_MS + 2),
    )
    .await
    .unwrap();
    let first = record(
        &store,
        context("project_a", START_MS + 3),
        RecordEvent::Lesson(lesson(
            &problem,
            &check,
            "Pin cargo dependencies",
            LessonVisibility::Project,
        )),
    )
    .await
    .unwrap();
    let mut second_lesson = lesson(
        &problem,
        &check,
        "Update cargo dependencies",
        LessonVisibility::Project,
    );
    second_lesson.conflicts_with = vec![first.artifact_id.clone()];
    let second = record(
        &store,
        context("project_a", START_MS + 4),
        RecordEvent::Lesson(second_lesson),
    )
    .await
    .unwrap();

    let case = resolve_case(&store, &problem.tenant_id, &problem.artifact_id)
        .await
        .unwrap();
    assert_eq!(case.resolution_state, ExperienceResolutionState::Conflicted);
    let recalled = recall(
        &store,
        "project_a",
        false,
        Some("nelli-gpu"),
        Some("cargo"),
        Some("1.98.0"),
    )
    .await;
    let LessonRecall::Abstain {
        reason: RecallAbstention::Conflict { lesson_ids },
    } = recalled
    else {
        panic!("conflicting lessons must abstain");
    };
    assert_eq!(
        lesson_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
        [first.artifact_id.clone(), second.artifact_id]
            .into_iter()
            .collect()
    );

    record(
        &store,
        context("project_a", START_MS + 5),
        RecordEvent::Correction {
            problem_id: problem.artifact_id.clone(),
            supersedes_id: first.artifact_id,
            replacement: lesson(
                &problem,
                &check,
                "Pin only the dependencies required by the failing package",
                LessonVisibility::Project,
            ),
        },
    )
    .await
    .unwrap();
    let case = resolve_case(&store, &problem.tenant_id, &problem.artifact_id)
        .await
        .unwrap();
    assert_eq!(case.resolution_state, ExperienceResolutionState::Resolved);
    assert!(case.lesson_conflicts.is_empty());
}

#[tokio::test]
async fn bundle_roundtrip_is_lossless_idempotent_and_never_executes_argv() {
    let source = MemoryStore::new();
    let problem = record_problem(&source, "project_a", START_MS).await;
    let attempt = record_attempt(&source, &problem, None, START_MS + 1).await;
    let sentinel_dir = tempfile::tempdir().unwrap();
    let sentinel = sentinel_dir.path().join("must-not-exist");
    let mut receipt = successful_receipt(&attempt.artifact_id, START_MS + 2);
    receipt.argv = vec!["touch".to_string(), sentinel.to_string_lossy().into_owned()];
    let check = record_check(&source, context("project_a", START_MS + 2), receipt)
        .await
        .unwrap();
    record(
        &source,
        context("project_a", START_MS + 3),
        RecordEvent::Lesson(lesson(
            &problem,
            &check,
            "Do not execute imported commands",
            LessonVisibility::Project,
        )),
    )
    .await
    .unwrap();

    let bundle = export_bundle(
        &source,
        &problem.tenant_id,
        std::slice::from_ref(&problem.artifact_id),
    )
    .await
    .unwrap();
    let encoded = serde_json::to_string(&bundle).unwrap();
    let decoded: ExperienceBundleV1 = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, bundle);

    let destination = MemoryStore::new();
    let first = import_bundle(&destination, &problem.tenant_id, bundle.clone())
        .await
        .unwrap();
    assert_eq!(first.inserted_artifact_ids.len(), bundle.artifacts.len());
    assert!(!sentinel.exists());
    let mut evidence = TaskArtifact::new(
        ArtifactKind::Evidence,
        problem.tenant_id.clone(),
        problem.task_id.clone(),
    );
    evidence.project_id = problem.project_id.clone();
    evidence.summary = Some("General evidence may share the task".to_string());
    destination
        .add_task_artifact(evidence.clone(), build_task_projections_minimal(&evidence))
        .await
        .unwrap();
    let second = import_bundle(&destination, &problem.tenant_id, bundle.clone())
        .await
        .unwrap();
    assert!(second.inserted_artifact_ids.is_empty());
    assert_eq!(second.replayed_artifact_ids.len(), bundle.artifacts.len());
    let reexported = export_bundle(
        &destination,
        &problem.tenant_id,
        std::slice::from_ref(&problem.artifact_id),
    )
    .await
    .unwrap();
    assert_eq!(reexported, bundle);

    let mut missing = bundle.clone();
    missing
        .artifacts
        .retain(|artifact| artifact.artifact_id != check.artifact_id);
    let missing_store = MemoryStore::new();
    assert!(import_bundle(&missing_store, &problem.tenant_id, missing)
        .await
        .unwrap_err()
        .to_string()
        .contains("missing supporting check"));

    let mut promoted = bundle.clone();
    promoted.artifacts[0].promotion_state = PromotionState::Verified;
    let promoted_store = MemoryStore::new();
    assert!(import_bundle(&promoted_store, &problem.tenant_id, promoted)
        .await
        .unwrap_err()
        .to_string()
        .contains("canonical promotion_state"));

    let mut wrong_scope = bundle;
    let attempt = wrong_scope
        .artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id == attempt.artifact_id)
        .unwrap();
    attempt.project_id = ProjectId::from("project_b");
    let wrong_scope_store = MemoryStore::new();
    assert!(
        import_bundle(&wrong_scope_store, &problem.tenant_id, wrong_scope)
            .await
            .unwrap_err()
            .to_string()
            .contains("crosses its problem scope")
    );
}

#[tokio::test]
async fn imported_correction_chain_cannot_bypass_stale_head_guard() {
    let base = MemoryStore::new();
    let problem = record_problem(&base, "project_a", START_MS).await;
    let attempt = record_attempt(&base, &problem, None, START_MS + 1).await;
    let check = record_check(
        &base,
        context("project_a", START_MS + 2),
        successful_receipt(&attempt.artifact_id, START_MS + 2),
    )
    .await
    .unwrap();
    let original = record(
        &base,
        context("project_a", START_MS + 3),
        RecordEvent::Lesson(lesson(
            &problem,
            &check,
            "Original guidance",
            LessonVisibility::Project,
        )),
    )
    .await
    .unwrap();
    let base_bundle = export_bundle(
        &base,
        &problem.tenant_id,
        std::slice::from_ref(&problem.artifact_id),
    )
    .await
    .unwrap();

    let local = MemoryStore::new();
    import_bundle(&local, &problem.tenant_id, base_bundle.clone())
        .await
        .unwrap();
    let local_head = record(
        &local,
        context("project_a", START_MS + 10),
        RecordEvent::Correction {
            problem_id: problem.artifact_id.clone(),
            supersedes_id: original.artifact_id.clone(),
            replacement: lesson(
                &problem,
                &check,
                "Current local guidance",
                LessonVisibility::Project,
            ),
        },
    )
    .await
    .unwrap();

    let remote = MemoryStore::new();
    import_bundle(&remote, &problem.tenant_id, base_bundle)
        .await
        .unwrap();
    let old_remote = record(
        &remote,
        context("project_a", START_MS + 5),
        RecordEvent::Correction {
            problem_id: problem.artifact_id.clone(),
            supersedes_id: original.artifact_id,
            replacement: lesson(
                &problem,
                &check,
                "Older remote guidance",
                LessonVisibility::Project,
            ),
        },
    )
    .await
    .unwrap();
    record(
        &remote,
        context("project_a", START_MS + 20),
        RecordEvent::Correction {
            problem_id: problem.artifact_id.clone(),
            supersedes_id: old_remote.artifact_id,
            replacement: lesson(
                &problem,
                &check,
                "New remote descendant",
                LessonVisibility::Project,
            ),
        },
    )
    .await
    .unwrap();
    let remote_bundle = export_bundle(
        &remote,
        &problem.tenant_id,
        std::slice::from_ref(&problem.artifact_id),
    )
    .await
    .unwrap();

    let error = import_bundle(&local, &problem.tenant_id, remote_bundle)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("targets stale ancestor"));
    let case = resolve_case(&local, &problem.tenant_id, &problem.artifact_id)
        .await
        .unwrap();
    assert_eq!(case.current_lessons[0].artifact_id, local_head.artifact_id);
}

#[tokio::test]
async fn recall_abstains_when_only_one_tied_branch_matches() {
    let base = MemoryStore::new();
    let problem = record_problem(&base, "project_a", START_MS).await;
    let attempt = record_attempt(&base, &problem, None, START_MS + 1).await;
    let check = record_check(
        &base,
        context("project_a", START_MS + 2),
        successful_receipt(&attempt.artifact_id, START_MS + 2),
    )
    .await
    .unwrap();
    let original = record(
        &base,
        context("project_a", START_MS + 3),
        RecordEvent::Lesson(lesson(
            &problem,
            &check,
            "Original guidance",
            LessonVisibility::Project,
        )),
    )
    .await
    .unwrap();
    let base_bundle = export_bundle(
        &base,
        &problem.tenant_id,
        std::slice::from_ref(&problem.artifact_id),
    )
    .await
    .unwrap();

    let mut branch_ids = Vec::new();
    let mut merged = base_bundle.clone();
    for (guidance, symptom_terms) in [
        (
            "Cargo branch",
            vec!["cargo dependency resolution failure".to_string()],
        ),
        ("Python branch", vec!["python syntax error".to_string()]),
    ] {
        let branch_store = MemoryStore::new();
        import_bundle(&branch_store, &problem.tenant_id, base_bundle.clone())
            .await
            .unwrap();
        let mut replacement = lesson(&problem, &check, guidance, LessonVisibility::Project);
        replacement.symptom_terms = symptom_terms;
        let branch = record(
            &branch_store,
            context("project_a", START_MS + 5),
            RecordEvent::Correction {
                problem_id: problem.artifact_id.clone(),
                supersedes_id: original.artifact_id.clone(),
                replacement,
            },
        )
        .await
        .unwrap();
        branch_ids.push(branch.artifact_id.clone());
        merged.artifacts.push(branch);
    }

    let target = MemoryStore::new();
    import_bundle(&target, &problem.tenant_id, merged)
        .await
        .unwrap();
    let recalled = recall(
        &target,
        "project_a",
        false,
        Some("nelli-gpu"),
        Some("cargo"),
        Some("1.98.0"),
    )
    .await;
    let LessonRecall::Abstain {
        reason: RecallAbstention::Conflict { lesson_ids },
    } = recalled
    else {
        panic!("one matching tied branch must still abstain");
    };
    branch_ids.sort();
    assert_eq!(lesson_ids, branch_ids);
}

#[tokio::test]
async fn persistent_import_rejects_global_artifact_and_task_id_collisions() {
    let source = MemoryStore::new();
    let problem = record_problem(&source, "project_a", START_MS).await;
    let bundle = export_bundle(
        &source,
        &problem.tenant_id,
        std::slice::from_ref(&problem.artifact_id),
    )
    .await
    .unwrap();

    let collision_dir = tempfile::tempdir().unwrap();
    let collision_store = persistent_store(collision_dir.path());
    let mut collision = problem.clone();
    collision.tenant_id = tenant("other_tenant");
    collision.task_id = "other-task".to_string();
    collision_store
        .add_task_artifact(
            collision.clone(),
            build_task_projections_minimal(&collision),
        )
        .await
        .unwrap();
    let error = import_bundle(&collision_store, &problem.tenant_id, bundle.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("artifact_id collision"));

    let task_dir = tempfile::tempdir().unwrap();
    let task_store = persistent_store(task_dir.path());
    let mut task_collision = problem.clone();
    task_collision.artifact_id = uuid::Uuid::now_v7().to_string();
    task_collision.tenant_id = tenant("other_tenant");
    task_store
        .add_task_artifact(
            task_collision.clone(),
            build_task_projections_minimal(&task_collision),
        )
        .await
        .unwrap();
    let error = import_bundle(&task_store, &problem.tenant_id, bundle)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("task_id collision"));
}

#[tokio::test]
async fn concurrent_attempts_leave_one_valid_causal_head() {
    let store = Arc::new(MemoryStore::new());
    let problem = record_problem(store.as_ref(), "project_a", START_MS).await;
    let first = record_attempt(store.as_ref(), &problem, None, START_MS + 1).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let mut tasks = Vec::new();
    for offset in [2, 3] {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let problem_id = problem.artifact_id.clone();
        let previous_attempt_id = first.artifact_id.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            record(
                store.as_ref(),
                context("project_a", START_MS + offset),
                RecordEvent::Attempt {
                    problem_id,
                    previous_attempt_id: Some(previous_attempt_id),
                    action: format!("Concurrent attempt {offset}"),
                },
            )
            .await
        }));
    }

    let mut successes = 0;
    let mut rejections = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => successes += 1,
            Err(error) if error.to_string().contains("current attempt") => rejections += 1,
            Err(error) => panic!("unexpected concurrent attempt error: {error}"),
        }
    }
    assert_eq!((successes, rejections), (1, 1));
    let case = resolve_case(store.as_ref(), &problem.tenant_id, &problem.artifact_id)
        .await
        .unwrap();
    assert_eq!(case.events.len(), 3);
}

#[tokio::test]
async fn concurrent_different_imports_never_overwrite_an_artifact() {
    let source = MemoryStore::new();
    let problem = record_problem(&source, "project_a", START_MS).await;
    let first_bundle = export_bundle(
        &source,
        &problem.tenant_id,
        std::slice::from_ref(&problem.artifact_id),
    )
    .await
    .unwrap();
    let mut second_bundle = first_bundle.clone();
    let changed = second_bundle
        .artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id == problem.artifact_id)
        .unwrap();
    changed.summary = Some("Different problem payload".to_string());
    let Some(ExperienceEvent::Problem { description, .. }) = &mut changed.experience else {
        panic!("bundle problem artifact lost its event");
    };
    *description = "Different problem payload".to_string();

    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(persistent_store(directory.path()));
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for bundle in [first_bundle.clone(), second_bundle.clone()] {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let tenant = problem.tenant_id.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            import_bundle(store.as_ref(), &tenant, bundle).await
        }));
    }

    let mut successes = 0;
    let mut collisions = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => successes += 1,
            Err(error) if error.to_string().contains("artifact_id collision") => collisions += 1,
            Err(error) => panic!("unexpected concurrent import error: {error}"),
        }
    }
    assert_eq!((successes, collisions), (1, 1));
    let stored = store
        .get_task_artifact(&problem.tenant_id, &problem.artifact_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        stored == first_bundle.artifacts[0] || stored == second_bundle.artifacts[0],
        "one complete incoming artifact must win"
    );
}

#[tokio::test]
async fn memory_store_rejects_direct_experience_overwrite() {
    let store = MemoryStore::new();
    let problem = record_problem(&store, "project_a", START_MS).await;
    let mut replacement = problem.clone();
    replacement.summary = Some("Different problem payload".to_string());
    let Some(ExperienceEvent::Problem { description, .. }) = &mut replacement.experience else {
        panic!("recorded problem lost its event");
    };
    *description = "Different problem payload".to_string();

    let error = store
        .add_task_artifact(
            replacement.clone(),
            build_task_projections_minimal(&replacement),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("artifact_id collision"));
    assert_eq!(
        store
            .get_task_artifact(&problem.tenant_id, &problem.artifact_id)
            .await
            .unwrap(),
        Some(problem)
    );
}

async fn record_problem<S: Store>(store: &S, project: &str, time: i64) -> TaskArtifact {
    record(
        store,
        context(project, time),
        RecordEvent::Problem {
            description: "Cargo resolution fails on the build host".to_string(),
            symptom_terms: vec!["cargo dependency resolution".to_string()],
        },
    )
    .await
    .unwrap()
}

async fn record_attempt<S: Store>(
    store: &S,
    problem: &TaskArtifact,
    previous_attempt_id: Option<String>,
    time: i64,
) -> TaskArtifact {
    record(
        store,
        context(problem.project_id.as_option().unwrap(), time),
        RecordEvent::Attempt {
            problem_id: problem.artifact_id.clone(),
            previous_attempt_id,
            action: "Regenerate the dependency lockfile".to_string(),
        },
    )
    .await
    .unwrap()
}

fn lesson(
    problem: &TaskArtifact,
    check: &TaskArtifact,
    guidance: &str,
    visibility: LessonVisibility,
) -> ConditionalLesson {
    ConditionalLesson {
        problem_id: problem.artifact_id.clone(),
        supporting_check_id: check.artifact_id.clone(),
        guidance: guidance.to_string(),
        symptom_terms: vec!["cargo dependency resolution".to_string()],
        applicability: LessonApplicability {
            machine: Some("nelli-gpu".to_string()),
            tool: Some("cargo".to_string()),
            version: Some("1.98.0".to_string()),
        },
        visibility,
        conflicts_with: Vec::new(),
    }
}

fn lesson_with_guidance(
    problem: &TaskArtifact,
    check: &TaskArtifact,
    guidance: &str,
    visibility: LessonVisibility,
) -> ConditionalLesson {
    lesson(problem, check, guidance, visibility)
}

fn successful_receipt(attempt_id: &str, time: i64) -> CheckReceipt {
    CheckReceipt {
        attempt_id: attempt_id.to_string(),
        cwd: "/tmp/project".to_string(),
        argv: vec!["cargo".to_string(), "check".to_string()],
        started_at_ms: time,
        finished_at_ms: time + 1,
        timed_out: false,
        exit_code: Some(0),
        stdout_sha256: Some("a".repeat(64)),
        stderr_sha256: Some("b".repeat(64)),
        source_before: Some(fingerprint("c")),
        source_after: Some(fingerprint("c")),
        execution: ExecutionContext {
            origin_host: Some("nelli-gpu".to_string()),
            cwd: Some("/tmp/project".to_string()),
            observed_at_ms: time,
            ..Default::default()
        },
    }
}

fn fingerprint(character: &str) -> SourceFingerprint {
    SourceFingerprint {
        sha256: character.repeat(64),
        coverage: SourceFingerprintCoverage::TrackedRepository,
    }
}

async fn recall<S: Store>(
    store: &S,
    project: &str,
    include_shared: bool,
    machine: Option<&str>,
    tool: Option<&str>,
    version: Option<&str>,
) -> LessonRecall {
    retrieve_lessons(
        store,
        LessonQuery {
            tenant_id: tenant("experience_test"),
            project_id: ProjectId::from(project),
            symptom: "cargo dependency resolution failure".to_string(),
            machine: machine.map(str::to_string),
            tool: tool.map(str::to_string),
            version: version.map(str::to_string),
            include_shared,
            limit: 5,
        },
    )
    .await
    .unwrap()
}

fn context(project: &str, time: i64) -> ExperienceContext {
    ExperienceContext {
        tenant_id: tenant("experience_test"),
        project_id: ProjectId::from(project),
        agent_id: Some("integration-test".to_string()),
        session_id: Some("session-1".to_string()),
        observed_at_ms: Some(time),
        provenance: Default::default(),
    }
}

fn tenant(value: &str) -> TenantId {
    TenantId::new(value).unwrap()
}

fn persistent_store(path: &Path) -> PersistentStore {
    PersistentStore::open(PersistentStoreConfig {
        data_dir: path.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })
    .unwrap()
}
