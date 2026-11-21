//! A crash between the writes of a note leaves an unfinished case. The same
//! submission finishes it, with and without `--compact`.

use std::path::Path;

use super::*;
use crate::store::metadata::MetadataStore;
use crate::store::persistent::{PersistentStore, PersistentStoreConfig};
use crate::types::{ProjectId, TenantId};

const T0: i64 = 1_800_000_000_000;
const TENANT: &str = "finish_tenant";
const PROJECT: &str = "finish_project";
const SHARED_ID: &str = "EXP-9";

fn open(path: &Path) -> PersistentStore {
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

fn tenant() -> TenantId {
    TenantId::new(TENANT).unwrap()
}

fn context(time: i64, uri: Option<&str>) -> ExperienceContext {
    let mut context = ExperienceContext {
        tenant_id: tenant(),
        project_id: ProjectId::from(PROJECT),
        agent_id: Some("finish-test".to_string()),
        session_id: None,
        observed_at_ms: Some(time),
        provenance: Default::default(),
    };
    context.provenance.uri = uri.map(str::to_string);
    context
}

fn case(problem: &str) -> CaseFile {
    CaseFile::parse(
        serde_json::json!({
            "problem": problem,
            "symptoms": ["slow startup"],
            "failed": ["restarted the daemon"],
            "fix": "Set the cache directory first.",
            "check": {"method": "observed", "result": "startup took under a second"},
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap()
}

fn chunk_rows(store: &PersistentStore) -> usize {
    let (active, deleted, candidates) = store.metadata().count_by_status(&tenant()).unwrap();
    active + deleted + candidates
}

/// Records written before the crash: 0 is the problem alone, 1 adds the
/// failed attempt, 2 adds the fix attempt, 3 adds the check.
async fn crash_after(
    store: &PersistentStore,
    stage: usize,
    uri: Option<&str>,
    problem: &str,
    searchable: bool,
) {
    let file = case(problem);
    let mut clock = T0;
    let mut tick = || {
        clock += 1;
        context(clock - 1, uri)
    };
    let problem = record_unlocked(
        store,
        tick(),
        RecordEvent::Problem {
            description: file.problem.clone(),
            symptom_terms: file.symptoms.clone(),
        },
        searchable,
    )
    .await
    .unwrap();
    let mut attempt_id = None;
    for action in [&file.failed[0], &file.fix].into_iter().take(stage) {
        let attempt = record_unlocked(
            store,
            tick(),
            RecordEvent::Attempt {
                problem_id: problem.artifact.artifact_id.clone(),
                previous_attempt_id: attempt_id.clone(),
                action: action.clone(),
            },
            searchable,
        )
        .await
        .unwrap();
        attempt_id = Some(attempt.artifact.artifact_id);
    }
    if stage == 3 {
        record_observed_check_unlocked(
            store,
            tick(),
            ObservedCheck {
                attempt_id: attempt_id.unwrap(),
                result: file.check.result.clone(),
                command: None,
            },
            searchable,
        )
        .await
        .unwrap();
    }
}

async fn experience_tasks(store: &PersistentStore) -> Vec<String> {
    let mut tasks = store
        .list_tasks(&tenant(), Some(PROJECT), usize::MAX / 2)
        .await
        .unwrap()
        .into_iter()
        .map(|task| task.task_id)
        .filter(|id| id.starts_with(TASK_PREFIX))
        .collect::<Vec<_>>();
    tasks.sort();
    tasks
}

async fn lesson_rows(store: &PersistentStore) -> usize {
    let filters = TaskSearchFilters {
        artifact_role: Some(ROLE_LESSON.to_string()),
        project_id: Some(PROJECT.to_string()),
        ..Default::default()
    };
    store
        .search_task_projection_chunk_ids(&tenant(), &filters, EXHAUSTIVE_LIMIT)
        .await
        .unwrap()
        .len()
}

/// A note after a crash at `stage` finishes the stored case.
async fn assert_finishes(stage: usize, compact: bool, shared: bool) {
    let label = format!("stage {stage}, compact {compact}, shared id {shared}");
    let data = tempfile::tempdir().unwrap();
    let store = open(data.path());
    let uri = shared.then(|| format!("{SHARED_ID_PREFIX}{SHARED_ID}"));
    crash_after(&store, stage, uri.as_deref(), "Startup is slow", !compact).await;
    let tasks = experience_tasks(&store).await;
    assert_eq!(tasks.len(), 1, "{label}");
    assert_eq!(lesson_rows(&store).await, 0, "{label}");
    let rows_before = chunk_rows(&store);

    // The retry asks for the same time as the first try, which is earlier
    // than the stored records.
    let retry = || {
        record_case(
            &store,
            context(T0, None),
            case("Startup is slow"),
            shared.then_some(SHARED_ID),
            compact,
        )
    };
    let finished = retry().await.unwrap();

    assert!(!finished.replay, "{label}");
    assert_eq!(experience_tasks(&store).await, tasks, "{label}: one case");
    let artifacts = store
        .list_task_artifacts(&tenant(), &tasks[0])
        .await
        .unwrap();
    assert_eq!(artifacts.len(), 5, "{label}: no duplicate record");
    assert_eq!(finished.attempt_ids.len(), 2, "{label}");
    // A full note rows each record it writes now; a compact note rows the
    // lesson only.
    let expected = if compact { 1 } else { 4 - stage };
    assert_eq!(finished.rows_written, expected, "{label}");
    assert_eq!(chunk_rows(&store) - rows_before, expected, "{label}");
    assert_eq!(lesson_rows(&store).await, 1, "{label}");
    let case = resolve_case(&store, &tenant(), &finished.problem_id)
        .await
        .unwrap();
    assert_eq!(case.current_lessons.len(), 1, "{label}");
    assert_eq!(
        case.current_lessons[0].artifact_id, finished.lesson_id,
        "{label}"
    );

    let again = retry().await.unwrap();
    assert!(again.replay, "{label}");
    assert_eq!(again.rows_written, 0, "{label}");
    assert_eq!(again.lesson_id, finished.lesson_id, "{label}");
    assert_eq!(
        store
            .list_task_artifacts(&tenant(), &tasks[0])
            .await
            .unwrap()
            .len(),
        5,
        "{label}: a replay writes nothing"
    );
}

async fn assert_finishes_every_way(stages: &[usize]) {
    for &stage in stages {
        for compact in [false, true] {
            for shared in [false, true] {
                assert_finishes(stage, compact, shared).await;
            }
        }
    }
}

#[tokio::test]
async fn a_retry_finishes_a_case_that_stopped_after_the_problem() {
    assert_finishes_every_way(&[0]).await;
}

#[tokio::test]
async fn a_retry_finishes_a_case_that_stopped_after_an_attempt() {
    assert_finishes_every_way(&[1, 2]).await;
}

#[tokio::test]
async fn a_retry_finishes_a_case_that_stopped_after_the_check() {
    assert_finishes_every_way(&[3]).await;
}

/// Other content under the same ID is not finished: the stored case stays as
/// it is and the note adds a case, as it did before. The ID then names the
/// finished case.
#[tokio::test]
async fn an_unfinished_case_with_other_content_is_left_alone() {
    let data = tempfile::tempdir().unwrap();
    let store = open(data.path());
    let uri = format!("{SHARED_ID_PREFIX}{SHARED_ID}");
    crash_after(&store, 3, Some(&uri), "Another problem", false).await;

    let note = record_case(
        &store,
        context(T0, None),
        case("Startup is slow"),
        Some(SHARED_ID),
        true,
    )
    .await
    .unwrap();

    assert!(!note.replay);
    assert_eq!(note.rows_written, 1);
    assert_eq!(experience_tasks(&store).await.len(), 2);
    let again = record_case(
        &store,
        context(T0, None),
        case("Startup is slow"),
        Some(SHARED_ID),
        true,
    )
    .await
    .unwrap();
    assert!(again.replay);
    assert_eq!(again.lesson_id, note.lesson_id);
}

/// An unfinished case under another ID is another submission.
#[tokio::test]
async fn an_unfinished_case_under_another_id_is_not_finished() {
    let data = tempfile::tempdir().unwrap();
    let store = open(data.path());
    let uri = format!("{SHARED_ID_PREFIX}EXP-1");
    crash_after(&store, 3, Some(&uri), "Startup is slow", false).await;

    let note = record_case(
        &store,
        context(T0, None),
        case("Startup is slow"),
        Some("EXP-2"),
        true,
    )
    .await
    .unwrap();

    assert!(!note.replay);
    assert_eq!(experience_tasks(&store).await.len(), 2);
}
