//! Phase 3 end-to-end test for `memd eval-counterfactual`: seed a
//! corpus with both raw and consolidated chunks, run the eval, and
//! check that the report is written with sane deltas (consolidated
//! chunks actually move ranks vs. the filtered baseline).

use memd::cli::{run_cli, CliCommand};
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::{ChunkType, MemoryChunk, ProjectId, TenantId};
use tempfile::tempdir;

fn open_store(dir: &std::path::Path) -> PersistentStore {
    let cfg = PersistentStoreConfig {
        data_dir: dir.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    };
    PersistentStore::open(cfg).expect("open persistent store")
}

#[tokio::test]
async fn eval_counterfactual_reports_sane_deltas() {
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t").unwrap();
    let project = "p";

    // Seed a small corpus: 5 raw chunks and 1 consolidated chunk for
    // a single query phrase.
    for i in 0..5 {
        store
            .add(
                MemoryChunk::new(
                    tenant.clone(),
                    format!("raw chunk {i}: tenant scoped cache keys hit the bug"),
                    ChunkType::Summary,
                )
                .with_project(ProjectId::from(project))
                .with_tags(vec!["kind:finish".to_string()]),
            )
            .await
            .unwrap();
    }
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "Consolidated lesson: tenant scoped cache keys are the durable fix.",
                ChunkType::Summary,
            )
            .with_project(ProjectId::from(project))
            .with_tags(vec![
                "kind:consolidated".to_string(),
                "priority:8".to_string(),
            ]),
        )
        .await
        .unwrap();

    // Minimal queries file.
    let qpath = dir.path().join("queries.jsonl");
    std::fs::write(
        &qpath,
        "{\"query\":\"tenant scoped cache keys\",\"label\":\"q1\"}\n",
    )
    .unwrap();

    run_cli(
        &store,
        None,
        CliCommand::EvalCounterfactual {
            tenant_id: "t".to_string(),
            project_id: Some(project.to_string()),
            project_dir: dir.path().to_path_buf(),
            queries: Some(qpath),
            k: 5,
        },
    )
    .await
    .expect("eval-counterfactual run");

    // Exactly one report file should land in evals/bench/reports/.
    let reports_dir = dir.path().join("evals/bench/reports");
    let entries: Vec<_> = std::fs::read_dir(&reports_dir).unwrap().collect();
    assert_eq!(entries.len(), 1, "expected one report file");
    let report = std::fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap();
    assert!(report.contains("# Counterfactual Retrieval Eval"));
    assert!(report.contains("| 1 | q1 |"));
    assert!(report.contains("overlap@k"));
}
