//! The `Recent Lessons` block: lessons recorded through the real experience
//! code path, rendered from the lesson itself.

use super::*;
use crate::experience::{record, ConditionalLesson, LessonEvidence, LessonVisibility, RecordEvent};
use crate::store::MemoryStore;

const T0: i64 = 1_800_000_000_000;
const WRAPPERS: [&str; 4] = [
    "status recorded for task",
    "Artifact role",
    "Reply to",
    "task:id:",
];

fn block(memory_md: &str) -> Option<&str> {
    let block = memory_md.split("## Recent Lessons\n\n").nth(1)?;
    Some(block.split("\n## ").next().unwrap_or(block).trim_end())
}

fn assert_no_wrapper(block: &str) {
    for wrapper in WRAPPERS {
        assert!(!block.contains(wrapper), "`{wrapper}` in:\n{block}");
    }
}

#[tokio::test]
async fn one_case_renders_one_lesson_line() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let note = record_lesson(
        &store,
        "alpha",
        "Cargo registry index is read-only",
        "Set CARGO_HOME to a writable directory.",
        json!({"tool": "cargo", "version": "1.80", "machine": "nelli-gpu"}),
        T0,
        "EXP-1",
    )
    .await;

    let (result, memory_md, explain) =
        refresh_digest(&store, project_dir.path(), None, "alpha", 3).await;

    let block = block(&memory_md).unwrap_or_else(|| panic!("no block:\n{memory_md}"));
    println!("RENDERED BLOCK:\n## Recent Lessons\n\n{block}");
    assert_eq!(
        block,
        format!(
            "1. Set CARGO_HOME to a writable directory.\n   - when: slow startup; applies to: tool cargo 1.80, machine nelli-gpu; evidence: observed; case: `{}`\n\nMore lessons: memd experience find \"<error words>\"",
            note.problem_id
        )
    );
    assert_no_wrapper(block);
    assert!(
        memory_md.find("## Project Fact Library").unwrap()
            < memory_md.find("## Recent Lessons").unwrap()
    );
    assert_eq!(result["project_lessons"], 1);
    let entries = explain["lessons"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["problem_id"], note.problem_id);
    assert_eq!(entries[0]["lesson_id"], note.lesson_id);
    assert!(entries[0]["event_time_ms"].as_i64().unwrap() >= T0);
    assert!(entries[0]["skipped"].is_null());
}

#[tokio::test]
async fn four_cases_show_the_three_newest() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    for (index, name) in ["one", "two", "three", "four"].into_iter().enumerate() {
        record_lesson(
            &store,
            "alpha",
            &format!("Problem {name}"),
            &format!("Guidance {name}."),
            json!({}),
            T0 + 100_000 * index as i64,
            &format!("EXP-{name}"),
        )
        .await;
    }

    let (result, memory_md, explain) =
        refresh_digest(&store, project_dir.path(), None, "alpha", 3).await;

    let block = block(&memory_md).unwrap();
    let positions = ["Guidance four.", "Guidance three.", "Guidance two."].map(|text| {
        block
            .find(text)
            .unwrap_or_else(|| panic!("{text} in {block}"))
    });
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    assert!(!block.contains("Guidance one."));
    assert_no_wrapper(block);
    assert_eq!(result["project_lessons"], 3);
    let over_limit = explain["lessons"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["skipped"] == "over_limit")
        .count();
    assert_eq!(over_limit, 1);
}

#[tokio::test]
async fn corrected_lesson_shows_only_the_correction() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let note = record_lesson(
        &store,
        "alpha",
        "Dependency graph is stale",
        "Use the cached dependency graph.",
        json!({}),
        T0,
        "EXP-1",
    )
    .await;
    record(
        &store,
        experience_context("alpha", T0 + 50_000),
        RecordEvent::Correction {
            problem_id: note.problem_id.clone(),
            supersedes_id: note.lesson_id.clone(),
            replacement: ConditionalLesson {
                problem_id: note.problem_id.clone(),
                supporting_check_id: note.check_id.clone(),
                guidance: "Regenerate the lockfile first.".to_string(),
                symptom_terms: vec!["stale lockfile".to_string()],
                evidence: LessonEvidence::Observed,
                visibility: LessonVisibility::Project,
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();

    let (_, memory_md, explain) =
        refresh_digest(&store, project_dir.path(), None, "alpha", 3).await;

    let block = block(&memory_md).unwrap();
    assert!(block.contains("Regenerate the lockfile first."));
    assert!(!block.contains("Use the cached dependency graph."));
    assert_no_wrapper(block);
    let reasons = explain["lessons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["skipped"].clone())
        .collect::<Vec<_>>();
    assert!(reasons.contains(&json!("superseded")), "{reasons:?}");
    assert!(reasons.contains(&Value::Null), "{reasons:?}");
}

#[tokio::test]
async fn conflicting_lessons_hide_the_case() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let note = record_lesson(
        &store,
        "alpha",
        "Dependency graph is stale",
        "Pin the dependencies.",
        json!({}),
        T0,
        "EXP-1",
    )
    .await;
    record(
        &store,
        experience_context("alpha", T0 + 50_000),
        RecordEvent::Lesson(ConditionalLesson {
            problem_id: note.problem_id.clone(),
            supporting_check_id: note.check_id.clone(),
            guidance: "Update the dependencies.".to_string(),
            symptom_terms: vec!["stale lockfile".to_string()],
            evidence: LessonEvidence::Observed,
            visibility: LessonVisibility::Project,
            conflicts_with: vec![note.lesson_id.clone()],
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let (_, memory_md, explain) =
        refresh_digest(&store, project_dir.path(), None, "alpha", 3).await;

    assert!(block(&memory_md).is_none(), "{memory_md}");
    let entries = explain["lessons"].as_array().unwrap();
    assert!(!entries.is_empty());
    assert!(entries.iter().all(|entry| entry["skipped"] == "conflict"));
}

#[tokio::test]
async fn other_projects_and_empty_stores_show_no_block() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let (_, memory_md, explain) =
        refresh_digest(&store, project_dir.path(), None, "alpha", 3).await;
    assert!(block(&memory_md).is_none(), "{memory_md}");
    assert!(explain["lessons"].as_array().unwrap().is_empty());

    // A shared lesson of another project is not shown either.
    record_lesson(
        &store,
        "beta",
        "Lustre quota blocks the build",
        "Clean the scratch directory.",
        json!({}),
        T0,
        "EXP-2",
    )
    .await;
    let (_, memory_md, explain) =
        refresh_digest(&store, project_dir.path(), None, "alpha", 3).await;
    assert!(block(&memory_md).is_none(), "{memory_md}");
    assert!(!memory_md.contains("Clean the scratch directory."));
    assert!(explain["lessons"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn lesson_limit_zero_turns_the_block_off() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    record_lesson(
        &store,
        "alpha",
        "Cargo registry index is read-only",
        "Set CARGO_HOME to a writable directory.",
        json!({}),
        T0,
        "EXP-1",
    )
    .await;

    let (result, memory_md, explain) =
        refresh_digest(&store, project_dir.path(), None, "alpha", 0).await;

    assert!(block(&memory_md).is_none(), "{memory_md}");
    assert_eq!(result["project_lessons"], 0);
    assert!(explain["lessons"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn in_memory_store_shows_lessons_through_the_same_api() {
    let project_dir = tempfile::tempdir().unwrap();
    let store = MemoryStore::new();
    record_lesson(
        &store,
        "alpha",
        "Cargo registry index is read-only",
        "Set CARGO_HOME to a writable directory.",
        json!({"tool": "cargo"}),
        T0,
        "EXP-1",
    )
    .await;

    let (_, memory_md, _) = refresh_digest(&store, project_dir.path(), None, "alpha", 3).await;

    let block = block(&memory_md).unwrap_or_else(|| panic!("no block:\n{memory_md}"));
    assert!(block.contains("1. Set CARGO_HOME to a writable directory."));
    assert!(block.contains("applies to: tool cargo;"));
    assert_no_wrapper(block);
}
