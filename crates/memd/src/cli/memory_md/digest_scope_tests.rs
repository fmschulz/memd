//! Selection and health-header tests against a real persistent store. The
//! store runs without dense or hybrid search, so no embedding model loads.

use super::collect::memory_state_from_scan;
use super::rank::{scan_takeaway_candidates_capped, ScanCaps, Takeaway};
use super::*;
use crate::experience::{record_case, CaseFile, ExperienceContext, NoteOutcome};
use crate::store::metadata::MetadataStore;
use crate::store::persistent::{PersistentStore, PersistentStoreConfig};
use crate::store::Store;
use crate::task_memory::{build_task_projections_minimal, TaskArtifact};
use crate::types::{ChunkStatus, ChunkType, LifecycleDelta, MemoryChunk, ProjectId};

const TENANT: &str = "digest_scope";

fn open_store(dir: &Path) -> PersistentStore {
    PersistentStore::open(PersistentStoreConfig {
        data_dir: dir.to_path_buf(),
        segment_max_chunks: 2_000,
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })
    .unwrap()
}

fn fact(project: Option<&str>, text: &str, tags: &[&str], timestamp: i64) -> MemoryChunk {
    let chunk_type = if tags.contains(&"kind:decision") {
        ChunkType::Decision
    } else {
        ChunkType::Summary
    };
    let mut chunk = MemoryChunk::new(TenantId::new(TENANT).unwrap(), text, chunk_type)
        .with_tags(tags.iter().map(|tag| tag.to_string()).collect());
    if let Some(project) = project {
        chunk = chunk.with_project(ProjectId::from(project));
    }
    chunk.timestamp_created = timestamp;
    chunk
}

async fn record_experience(store: &PersistentStore, project: &str, problem: &str, id: &str) {
    record_lesson(
        store,
        project,
        problem,
        &format!("Apply the {project} workaround before launching."),
        json!({}),
        1_800_000_000_000,
        id,
    )
    .await;
}

/// Record one experience case through the real code path.
async fn record_lesson<S: Store + ?Sized>(
    store: &S,
    project: &str,
    problem: &str,
    fix: &str,
    applies_when: Value,
    observed_at_ms: i64,
    id: &str,
) -> NoteOutcome {
    let case = CaseFile::parse(
        json!({
            "problem": problem,
            "symptoms": ["slow startup"],
            "failed": ["restarted the daemon"],
            "fix": fix,
            "check": {"method": "observed", "result": "startup took under a second"},
            "applies_when": applies_when,
            "visibility": "shared",
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    record_case(
        store,
        experience_context(project, observed_at_ms),
        case,
        Some(id),
        false,
    )
    .await
    .unwrap()
}

fn experience_context(project: &str, observed_at_ms: i64) -> ExperienceContext {
    ExperienceContext {
        tenant_id: TenantId::new(TENANT).unwrap(),
        project_id: ProjectId::from(project),
        agent_id: Some("digest-test".to_string()),
        session_id: None,
        observed_at_ms: Some(observed_at_ms),
        provenance: Default::default(),
    }
}

mod lesson_block_tests;

/// Store a generated digest artifact with its retrieval projection, as the
/// digest writers do.
async fn record_generated_digest(store: &PersistentStore, project: &str, role: &str) {
    let mut artifact = TaskArtifact::new_digest(
        TenantId::new(TENANT).unwrap(),
        format!("task-{project}-{role}"),
        role,
        role,
    );
    artifact.project_id = ProjectId::from(project);
    artifact.summary = Some(format!(
        "Digest {role} for {project} covers 2 ranked lessons."
    ));
    let projections = build_task_projections_minimal(&artifact);
    store
        .add_task_artifact(artifact, projections)
        .await
        .unwrap();
}

/// Digest without the lessons block, for tests of fact selection.
async fn run_digest(store: &PersistentStore, project_dir: &Path, project: &str) -> (String, Value) {
    run_digest_in(store, project_dir, None, project).await
}

async fn run_digest_in(
    store: &PersistentStore,
    project_dir: &Path,
    repo_dir: Option<PathBuf>,
    project: &str,
) -> (String, Value) {
    let (_, memory_md, explain) = refresh_digest(store, project_dir, repo_dir, project, 0).await;
    (memory_md, explain)
}

/// Run one refresh. Returns the command result, `memory.md` and the explain
/// report.
async fn refresh_digest<S: Store>(
    store: &S,
    project_dir: &Path,
    repo_dir: Option<PathBuf>,
    project: &str,
    lesson_limit: usize,
) -> (Value, String, Value) {
    let explain = project_dir.join("explain.json");
    let result = refresh_memory_md_with_health(
        store,
        None,
        MemoryMdOptions {
            tenant_id: Some(TENANT.to_string()),
            project_id: Some(project.to_string()),
            project_dir: project_dir.to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 10,
            lesson_limit,
            candidate_k: 40,
            explain_output: Some(explain.clone()),
            repo_dir,
        },
    )
    .await
    .unwrap();
    (
        result,
        fs::read_to_string(project_dir.join("memory.md")).unwrap(),
        serde_json::from_str(&fs::read_to_string(explain).unwrap()).unwrap(),
    )
}

fn scan_count(explain: &Value, section: &str, field: &str) -> u64 {
    explain["scan"][section][field].as_u64().unwrap_or_else(|| {
        panic!(
            "explain scan.{section}.{field} missing: {}",
            explain["scan"]
        )
    })
}

fn has_experience_tag(explain: &Value, section: &str) -> bool {
    explain[section].as_array().unwrap().iter().any(|row| {
        row["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag.as_str().unwrap().starts_with("task:role:experience_"))
    })
}

const PROJECT_FACTS: [&str; 5] = [
    "Decision: store metadata in sqlite with WAL journaling.",
    "Decision: reject writes while the writer lock is held elsewhere.",
    "Decision: embed chunks with the bge-small candle model by default.",
    "Decision: keep segment files append-only until compaction runs.",
    "Decision: expire history-tier rows after the retention window.",
];

/// Cause A: the digest read the newest 10,000 rows of the whole tenant, so a
/// project with old curated facts lost them behind another project's newer
/// rows.
#[tokio::test]
async fn old_project_facts_survive_newer_rows_of_other_projects() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let chunks = PROJECT_FACTS
        .iter()
        .enumerate()
        .map(|(index, text)| {
            fact(
                Some("memd"),
                text,
                &["kind:decision", "priority:9"],
                1 + index as i64,
            )
        })
        .collect::<Vec<_>>();
    store.add_batch(chunks).await.unwrap();
    for batch in 0..11 {
        let chunks = (0..1_000)
            .map(|index| {
                fact(
                    Some("other"),
                    &format!("Filler row {batch} {index}"),
                    &[],
                    1_000_000 + (batch * 1_000 + index) as i64,
                )
            })
            .collect::<Vec<_>>();
        store.add_batch(chunks).await.unwrap();
    }

    let (memory_md, _) = run_digest(&store, project_dir.path(), "memd").await;

    for text in PROJECT_FACTS {
        assert!(
            memory_md.contains(text),
            "missing `{text}` in:\n{memory_md}"
        );
    }
}

/// Causes B and C: generated experience projections are not facts. They stay
/// out of both sections, and the explain output counts them.
#[tokio::test]
async fn experience_projections_are_not_candidates() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    store
        .add(fact(
            Some("alpha"),
            "Decision: pin the toolchain in rust-toolchain.toml.",
            &["kind:decision", "priority:9"],
            10,
        ))
        .await
        .unwrap();
    record_experience(
        &store,
        "alpha",
        "Cargo registry index is read-only",
        "EXP-1",
    )
    .await;
    record_experience(&store, "beta", "Lustre quota blocks the build", "EXP-2").await;

    let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;

    assert!(memory_md.contains("pin the toolchain"), "{memory_md}");
    for leaked in [
        "Task decision status recorded",
        "Cargo registry",
        "Lustre quota",
        "workaround before launching",
    ] {
        assert!(
            !memory_md.contains(leaked),
            "`{leaked}` leaked:\n{memory_md}"
        );
    }
    assert!(!has_experience_tag(&explain, "project"));
    assert!(!has_experience_tag(&explain, "machine_wide"));
    assert!(scan_count(&explain, "project", "excluded_experience_projections") > 0);
    assert!(scan_count(&explain, "machine_wide", "excluded_experience_projections") > 0);
    assert_eq!(scan_count(&explain, "project", "excluded_by_status"), 0);
}

/// Cause D: the header counted superseded rows and experience projections.
#[tokio::test]
async fn health_header_counts_visible_facts_only() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let tenant = TenantId::new(TENANT).unwrap();
    let ids = store
        .add_batch(vec![
            fact(
                Some("alpha"),
                "Decision: first fact stays.",
                &["kind:decision"],
                1,
            ),
            fact(
                Some("alpha"),
                "Decision: second fact is replaced.",
                &["kind:decision"],
                2,
            ),
            fact(
                Some("alpha"),
                "Decision: third fact is deleted.",
                &["kind:decision"],
                3,
            ),
            fact(
                Some("alpha"),
                "Decision: fourth fact replaces the second.",
                &["kind:decision"],
                4,
            ),
            fact(
                Some("beta"),
                "Decision: other project fact stays.",
                &["kind:decision"],
                5,
            ),
        ])
        .await
        .unwrap();
    record_experience(
        &store,
        "alpha",
        "Cargo registry index is read-only",
        "EXP-1",
    )
    .await;
    record_generated_digest(&store, "alpha", "project_brief").await;
    record_generated_digest(&store, "beta", "highlight_library").await;
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &ids[1],
            &LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(store.delete(&tenant, &ids[2]).await.unwrap());

    let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;

    assert!(
        memory_md.contains(&format!(
            "- facts: 2 visible in project alpha, 3 visible in tenant {TENANT}\n"
        )),
        "{memory_md}"
    );
    assert!(!memory_md.contains("chunks: "), "{memory_md}");
    assert!(scan_count(&explain, "project", "excluded_by_status") >= 1);
    assert!(scan_count(&explain, "project", "excluded_experience_projections") > 0);
    assert!(scan_count(&explain, "project", "excluded_generated_digests") >= 1);
    assert!(scan_count(&explain, "machine_wide", "excluded_generated_digests") >= 1);
}

/// Machine-wide facts have no project and still reach the digest.
#[tokio::test]
async fn project_less_global_fact_stays_machine_wide() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    store
        .add(fact(
            None,
            "Decision: use uv for every Python environment on this machine.",
            &["kind:decision", "priority:9", "global"],
            10,
        ))
        .await
        .unwrap();

    let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;

    let machine_wide = memory_md
        .split("## Machine-Wide Fact Library")
        .nth(1)
        .unwrap_or_else(|| panic!("no machine-wide section:\n{memory_md}"));
    assert!(machine_wide.contains("use uv for every Python environment"));
    assert_eq!(scan_count(&explain, "machine_wide", "visible_rows"), 1);
}

/// Excluded rows cost no payload read: a broken payload of a projection row is
/// invisible, while a broken payload of a fact is reported.
#[tokio::test]
async fn excluded_rows_cost_no_payload_read() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let ids = store
        .add_batch(vec![
            fact(
                Some("alpha"),
                "Decision: readable fact one.",
                &["kind:decision"],
                1,
            ),
            fact(
                Some("alpha"),
                "Decision: unreadable fact two.",
                &["kind:decision"],
                2,
            ),
        ])
        .await
        .unwrap();
    record_experience(
        &store,
        "alpha",
        "Cargo registry index is read-only",
        "EXP-1",
    )
    .await;
    record_generated_digest(&store, "alpha", "highlight_library").await;
    let conn = rusqlite::Connection::open(data.path().join("metadata.db")).unwrap();
    let break_projections = |filter: &str| {
        conn.execute(
            &format!(
                "UPDATE chunks SET segment_id = 99999
                 WHERE chunk_id IN (
                     SELECT l.chunk_id FROM artifact_links l
                     JOIN task_artifacts a ON a.artifact_id = l.artifact_id
                     WHERE l.link_kind = 'retrieval_projection' AND {filter})"
            ),
            [],
        )
        .unwrap()
    };
    assert!(break_projections("a.status = 'generated'") > 0);

    let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;
    assert_eq!(scan_count(&explain, "project", "unreadable_rows"), 0);
    assert!(!memory_md.contains("memory degraded"), "{memory_md}");
    assert!(scan_count(&explain, "project", "excluded_generated_digests") > 0);

    assert!(break_projections("a.status IS NOT 'generated'") > 0);
    let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;
    assert_eq!(scan_count(&explain, "project", "unreadable_rows"), 0);
    assert!(!memory_md.contains("memory degraded"), "{memory_md}");

    conn.execute(
        "UPDATE chunks SET segment_id = 99999 WHERE chunk_id = ?1",
        [ids[1].to_string()],
    )
    .unwrap();
    let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;
    assert_eq!(scan_count(&explain, "project", "unreadable_rows"), 1);
    assert!(
        memory_md.contains("memory degraded: 1 active chunks"),
        "{memory_md}"
    );
}

/// A scan that reaches its cap keeps the newest rows of the section and says
/// so, in the explain data and (for the project) in the header.
#[tokio::test]
async fn capped_scan_keeps_the_newest_rows_and_sets_truncated() {
    let data = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let tenant = TenantId::new(TENANT).unwrap();
    let chunks = (0..5)
        .map(|index| {
            fact(
                Some("alpha"),
                &format!("Decision: project fact {index}."),
                &["kind:decision"],
                10 + index,
            )
        })
        .chain((0..4).map(|index| {
            fact(
                Some("beta"),
                &format!("Decision: machine fact {index}."),
                &["kind:decision"],
                20 + index,
            )
        }))
        .collect::<Vec<_>>();
    store.add_batch(chunks).await.unwrap();

    let outcome = scan_takeaway_candidates_capped(
        &store,
        &tenant,
        Some("alpha"),
        ScanCaps {
            project: 2,
            machine_wide: 3,
        },
    )
    .await
    .unwrap();

    let texts = |takeaways: &[Takeaway]| {
        takeaways
            .iter()
            .map(|takeaway| takeaway.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        texts(&outcome.project),
        vec!["Decision: project fact 4.", "Decision: project fact 3."]
    );
    assert_eq!(outcome.machine_wide.len(), 3);
    assert!(outcome.project_scan.truncated);
    assert_eq!(outcome.project_scan.read_rows, 2);
    assert_eq!(outcome.project_scan.visible_rows, 5);
    assert!(outcome.machine_scan.truncated);
    assert_eq!(outcome.machine_scan.visible_rows, 4);

    let state = memory_state_from_scan(&outcome.project_scan, &outcome.machine_scan);
    assert_eq!(state.scan_warnings.len(), 1);
    assert!(state.scan_warnings[0].starts_with("project scan truncated: read the newest 2 of 5"));

    let uncapped = scan_takeaway_candidates(&store, &tenant, Some("alpha"))
        .await
        .unwrap();
    assert!(!uncapped.project_scan.truncated);
    assert!(!uncapped.machine_scan.truncated);
}

/// A session in a subdirectory checks coverage against the repository docs.
#[tokio::test]
async fn repo_dir_docs_suppress_covered_facts() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let session_dir = repo.path().join("subdir");
    fs::create_dir_all(&session_dir).unwrap();
    let covered = "Decision: rotate segment files after compaction using the writer lock protocol before restarting workers.";
    fs::create_dir_all(repo.path().join("tasks")).unwrap();
    fs::write(repo.path().join("tasks/todo.md"), covered).unwrap();
    let store = open_store(data.path());
    store
        .add(fact(Some("alpha"), covered, &["kind:decision"], 10))
        .await
        .unwrap();

    let (without, _) = run_digest_in(&store, &session_dir, None, "alpha").await;
    assert!(without.contains(covered), "{without}");

    let (with, explain) = run_digest_in(
        &store,
        &session_dir,
        Some(repo.path().to_path_buf()),
        "alpha",
    )
    .await;
    assert!(!with.contains(covered), "{with}");
    assert!(explain["project"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["filter_reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("covered_by_repo:tasks/todo.md"))));
}

/// Repository coverage runs before the dedupe window: covered candidates do
/// not use up the 200 places, so an uncovered fact below them still shows.
#[tokio::test]
async fn covered_candidates_do_not_fill_the_dedupe_window() {
    let data = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = open_store(data.path());
    let covered = (0..DEDUPE_WINDOW + 5)
        .map(|index| {
            format!(
                "Decision: covered fact number {index} keeps token{index}a token{index}b token{index}c token{index}d token{index}e."
            )
        })
        .collect::<Vec<_>>();
    fs::create_dir_all(project_dir.path().join("tasks")).unwrap();
    fs::write(project_dir.path().join("tasks/todo.md"), covered.join("\n")).unwrap();
    let uncovered =
        "Validation: cargo test passed after the zebra migration frobnicates quokka entries.";
    let mut chunks = covered
        .iter()
        .enumerate()
        .map(|(index, text)| fact(Some("alpha"), text, &["kind:decision"], 100 + index as i64))
        .collect::<Vec<_>>();
    chunks.push(fact(Some("alpha"), uncovered, &[], 1));
    store.add_batch(chunks).await.unwrap();

    let (memory_md, _) = run_digest(&store, project_dir.path(), "alpha").await;

    assert!(memory_md.contains(uncovered), "{memory_md}");
    assert!(!memory_md.contains("covered fact number"), "{memory_md}");
}

mod split_projection_tests {
    //! A projection long enough to split is stored as several physical rows.
    //! Every row must stay out of the digest scan.

    use super::*;
    use crate::task_memory::build_task_projections;

    const HEADER: &str = "- facts: 1 visible in project alpha, 1 visible in tenant digest_scope\n";

    async fn add_real_fact(store: &PersistentStore) -> String {
        store
            .add(fact(
                Some("alpha"),
                "Decision: the one real fact stays.",
                &["kind:decision"],
                10,
            ))
            .await
            .unwrap()
            .to_string()
    }

    async fn record_long_digest(store: &PersistentStore) {
        let mut artifact = TaskArtifact::new_digest(
            TenantId::new(TENANT).unwrap(),
            "task-long-digest",
            "highlight_library",
            "highlight_library",
        );
        artifact.project_id = ProjectId::from("alpha");
        artifact.summary = Some(
            "This is a long sentence that should trigger chunk splitting behavior. ".repeat(60),
        );
        let projections = build_task_projections(&artifact);
        store
            .add_task_artifact(artifact, projections)
            .await
            .unwrap();
    }

    /// Physical rows other than the real fact.
    fn extra_rows(conn: &rusqlite::Connection, fact_id: &str) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT chunk_id FROM chunks WHERE chunk_id != ?1")
            .unwrap();
        statement
            .query_map([fact_id], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    }

    fn break_payloads(conn: &rusqlite::Connection, chunk_ids: &[String]) {
        for chunk_id in chunk_ids {
            conn.execute(
                "UPDATE chunks SET segment_id = 99999 WHERE chunk_id = ?1",
                [chunk_id],
            )
            .unwrap();
        }
    }

    #[tokio::test]
    async fn long_generated_digest_is_excluded_row_by_row() {
        let data = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let store = open_store(data.path());
        let fact_id = add_real_fact(&store).await;
        record_long_digest(&store).await;
        let conn = rusqlite::Connection::open(data.path().join("metadata.db")).unwrap();
        let rows = extra_rows(&conn, &fact_id);
        assert!(rows.len() > 1, "the digest must split: {}", rows.len());
        break_payloads(&conn, &rows);

        let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;

        assert_eq!(scan_count(&explain, "project", "unreadable_rows"), 0);
        assert_eq!(
            scan_count(&explain, "project", "excluded_generated_digests"),
            rows.len() as u64
        );
        assert_eq!(scan_count(&explain, "project", "visible_rows"), 1);
        assert!(memory_md.contains(HEADER), "{memory_md}");
        assert!(!memory_md.contains("memory degraded"), "{memory_md}");
    }

    #[tokio::test]
    async fn long_experience_lesson_is_excluded_row_by_row() {
        let data = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let store = open_store(data.path());
        let fact_id = add_real_fact(&store).await;
        record_lesson(
            &store,
            "alpha",
            "A very long lesson",
            &"\u{3042}".repeat(1000),
            json!({}),
            1_800_000_000_000,
            "EXP-long",
        )
        .await;
        let conn = rusqlite::Connection::open(data.path().join("metadata.db")).unwrap();
        let rows = extra_rows(&conn, &fact_id);
        assert!(rows.len() > 1, "the lesson must split: {}", rows.len());
        break_payloads(&conn, &rows);

        let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;

        assert_eq!(scan_count(&explain, "project", "unreadable_rows"), 0);
        assert_eq!(
            scan_count(&explain, "project", "excluded_experience_projections"),
            rows.len() as u64
        );
        assert_eq!(scan_count(&explain, "project", "visible_rows"), 1);
        assert!(memory_md.contains(HEADER), "{memory_md}");
    }

    /// A store written before part links existed: the continuation rows have
    /// no link. The SQL cannot see them, but the header's project count is the
    /// number of facts the scan admitted, so it stays true.
    #[tokio::test]
    async fn unlinked_continuation_rows_do_not_inflate_the_project_count() {
        let data = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let store = open_store(data.path());
        add_real_fact(&store).await;
        record_long_digest(&store).await;
        let conn = rusqlite::Connection::open(data.path().join("metadata.db")).unwrap();
        // Imitate an old store: drop the part links, keep the primary link.
        conn.execute(
            "DELETE FROM artifact_links WHERE link_kind = 'retrieval_projection_part'",
            [],
        )
        .unwrap();

        let (memory_md, explain) = run_digest(&store, project_dir.path(), "alpha").await;

        // The old rows are read and dropped as generated wrappers.
        assert!(scan_count(&explain, "project", "visible_rows") > 1);
        assert_eq!(scan_count(&explain, "project", "admitted_rows"), 1);
        assert!(
            memory_md.contains("- facts: 1 visible in project alpha, "),
            "{memory_md}"
        );
    }
}
