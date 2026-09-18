#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use memd::consolidate::journal::LineageRelation;
use memd::consolidate::prompt::ConsolidatedEntry;
use memd::consolidate::service::execute_consolidation_with_identity;
use memd::consolidate::ConsolidatorIdentity;
use memd::store::metadata::MetadataStore;
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::types::{ChunkStatus, ChunkType, MemoryChunk, ProjectId, Source, TenantId};
use serde_json::Value;

fn memd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_memd")
}

fn open_store(data_dir: &Path) -> PersistentStore {
    PersistentStore::open(PersistentStoreConfig {
        data_dir: data_dir.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: true,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })
    .unwrap()
}

fn write_scope(project_dir: &Path, tenant_id: &str, project_id: &str) {
    std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
    std::fs::write(
        project_dir.join(".memd/project_scope.json"),
        serde_json::to_vec(&serde_json::json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "interface": "cli",
            "cli_command": "memd",
            "agent_context_output": ".memd/context.md",
            "project_dir": project_dir,
        }))
        .unwrap(),
    )
    .unwrap();
}

struct StagedRun {
    run_id: String,
    source_id: String,
    candidate_id: String,
    candidate_text: String,
}

async fn stage_run(
    store: &PersistentStore,
    tenant_id: &str,
    project_id: &str,
    label: &str,
) -> StagedRun {
    let tenant = TenantId::new(tenant_id).unwrap();
    let source = MemoryChunk::new(
        tenant.clone(),
        format!("source text and metadata for {label}"),
        ChunkType::Doc,
    )
    .with_project(ProjectId::from(project_id))
    .with_source(Source::from_path(format!("notes/{label}.md")))
    .with_tags(vec![format!("source:{label}")]);
    let source_id = store.add(source).await.unwrap();
    let candidate_text = format!("candidate inspection token {label}");
    let staged = execute_consolidation_with_identity(
        store,
        &tenant,
        Some(project_id),
        &[ConsolidatedEntry {
            text: candidate_text.clone(),
            agent_action: format!("reuse {label} only with its source"),
            evidence: vec![source_id.to_string()],
            confidence: 0.91,
            supersedes: vec![source_id.to_string()],
            priority: 8,
        }],
        LineageRelation::Supersedes,
        &ConsolidatorIdentity {
            adapter: "review-test-adapter".to_string(),
            command: Some("review-test --fixed".to_string()),
            model: Some("review-test-model".to_string()),
            version: Some("1.2.3".to_string()),
        },
        &[],
        "review test prompt",
        "review test response",
        false,
    )
    .await
    .unwrap();
    StagedRun {
        run_id: staged.run_id.to_string(),
        source_id: source_id.to_string(),
        candidate_id: staged.candidate_chunk_ids[0].to_string(),
        candidate_text,
    }
}

fn cli(data_dir: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(memd_bin())
        .current_dir(cwd)
        .env("HOME", cwd)
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--search-variant", "bm25-only"])
        .args(args)
        .output()
        .unwrap()
}

fn success_json(output: Output, label: &str) -> Value {
    assert!(
        output.status.success(),
        "{label} failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_scope_failure(output: Output, label: &str) {
    assert!(
        !output.status.success(),
        "{label} unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not found in requested scope"),
        "unexpected {label} error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct WorkerGuard {
    data_dir: PathBuf,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let _ = Command::new(memd_bin())
            .arg("--data-dir")
            .arg(&self.data_dir)
            .args(["warm", "stop"])
            .output();
    }
}

fn start_worker(data_dir: &Path, cwd: &Path) -> WorkerGuard {
    success_json(cli(data_dir, cwd, &["warm", "start"]), "start warm worker");
    WorkerGuard {
        data_dir: data_dir.to_path_buf(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn review_cli_is_scoped_read_only_and_routes_decisions_through_worker() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    let project_dir = temp.path().join("target-project");
    write_scope(&project_dir, "target_tenant", "target_project");
    let store = open_store(&data_dir);

    let foreign = stage_run(&store, "foreign_tenant", "foreign_project", "foreign").await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    let accepted = stage_run(&store, "target_tenant", "target_project", "accepted").await;
    tokio::time::sleep(Duration::from_millis(2)).await;
    let rejected = stage_run(&store, "target_tenant", "target_project", "rejected").await;

    // Scope-file inheritance and SQL filtering happen before LIMIT: the older
    // foreign run cannot consume the target scope's one-row window.
    let scoped_list = success_json(
        cli(
            &data_dir,
            &project_dir,
            &["consolidate-review", "--list", "--limit", "1"],
        ),
        "scoped list while writer active",
    );
    assert_eq!(scoped_list["count"], 1);
    assert_eq!(scoped_list["staged_runs"][0]["run_id"], accepted.run_id);
    let admin_list = success_json(
        cli(
            &data_dir,
            temp.path(),
            &["consolidate-review", "--list", "--limit", "10"],
        ),
        "unscoped administrative list",
    );
    assert_eq!(admin_list["count"], 3);
    assert_eq!(admin_list["staged_runs"][0]["run_id"], foreign.run_id);
    let wrong_scope_list = success_json(
        cli(
            &data_dir,
            &project_dir,
            &[
                "consolidate-review",
                "--list",
                "--tenant-id",
                "wrong_tenant",
            ],
        ),
        "wrong-scope list",
    );
    assert_eq!(wrong_scope_list["count"], 0);

    let inspection = success_json(
        cli(
            &data_dir,
            &project_dir,
            &["consolidate-review", &accepted.run_id],
        ),
        "inspection while writer active",
    );
    assert_eq!(inspection["run"]["state"], "validated");
    assert_eq!(
        inspection["run"]["consolidator"]["adapter"],
        "review-test-adapter"
    );
    assert_eq!(
        inspection["candidates"][0]["candidate"]["payload"]["status"],
        "candidate"
    );
    assert!(inspection["candidates"][0]["candidate"]["payload"]["text"]
        .as_str()
        .unwrap()
        .contains(&accepted.candidate_text));
    assert_eq!(
        inspection["sources"][0]["payload"]["source"]["path"],
        "notes/accepted.md"
    );
    assert_eq!(
        inspection["lineage"][0]["source_chunk_id"],
        accepted.source_id
    );
    assert_eq!(inspection["lineage"][0]["relation"], "supersedes");

    let candidate_get = success_json(
        cli(
            &data_dir,
            &project_dir,
            &[
                "get",
                "--tenant-id",
                "target_tenant",
                "--chunk-id",
                &accepted.candidate_id,
            ],
        ),
        "ordinary candidate get",
    );
    assert!(candidate_get.is_null());
    let candidate_search = success_json(
        cli(
            &data_dir,
            &project_dir,
            &[
                "search",
                "--warm",
                "off",
                "--tenant-id",
                "target_tenant",
                "--project-id",
                "target_project",
                "--query",
                &accepted.candidate_text,
                "--k",
                "10",
            ],
        ),
        "ordinary candidate search",
    );
    assert!(!candidate_search
        .to_string()
        .contains(&accepted.candidate_text));
    let tenant = TenantId::new("target_tenant").unwrap();
    assert_eq!(
        store
            .metadata()
            .get(
                &tenant,
                &memd::types::ChunkId::parse(&accepted.source_id).unwrap()
            )
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
    assert_eq!(
        store
            .metadata()
            .get(
                &tenant,
                &memd::types::ChunkId::parse(&accepted.candidate_id).unwrap()
            )
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Candidate
    );
    assert_scope_failure(
        cli(
            &data_dir,
            &project_dir,
            &[
                "consolidate-review",
                &accepted.run_id,
                "--tenant-id",
                "wrong_tenant",
            ],
        ),
        "wrong-tenant inspection",
    );

    drop(store);
    assert_scope_failure(
        cli(
            &data_dir,
            &project_dir,
            &[
                "consolidate-review",
                &accepted.run_id,
                "--project-id",
                "wrong_project",
                "--accept",
                "--warm",
                "off",
            ],
        ),
        "wrong-project decision",
    );

    let _worker = start_worker(&data_dir, &project_dir);
    let accepted_result = success_json(
        cli(
            &data_dir,
            &project_dir,
            &[
                "consolidate-review",
                &accepted.run_id,
                "--accept",
                "--warm",
                "required",
            ],
        ),
        "accept through worker",
    );
    assert_eq!(accepted_result["state"], "committed");
    let rejected_result = success_json(
        cli(
            &data_dir,
            &project_dir,
            &[
                "consolidate-review",
                &rejected.run_id,
                "--reject",
                "--warm",
                "required",
            ],
        ),
        "reject through worker",
    );
    assert_eq!(rejected_result["state"], "rejected");

    let post_decision = success_json(
        cli(
            &data_dir,
            &project_dir,
            &["consolidate-review", &rejected.run_id],
        ),
        "inspection while warm worker active",
    );
    assert_eq!(post_decision["run"]["state"], "rejected");
    assert_eq!(
        post_decision["candidates"][0]["candidate"]["payload"]["status"],
        "error"
    );
}
