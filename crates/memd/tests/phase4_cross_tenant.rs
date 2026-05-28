//! Phase 4 end-to-end tests for cross-tenant takeaways + shared
//! tenant promotion.

use memd::cli::{run_cli, CliCommand};
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::{ChunkType, MemoryChunk, ProjectId, TenantId};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use tempfile::tempdir;

static CONSOLIDATOR_ENV_MUTEX: Mutex<()> = Mutex::new(());

fn with_consolidator_env<'a>() -> MutexGuard<'a, ()> {
    CONSOLIDATOR_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
async fn cross_tenant_section_appears_when_flag_set() {
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));

    // Tenant A holds a high-priority consolidated lesson; tenant B is
    // the "home" tenant we're rendering memory.md for. The lesson
    // must surface in the Cross-Tenant Takeaways section.
    // Text contains the GLOBAL_QUERIES `find_failures` phrase verbatim
    // so the in-memory store's text-fallback substring search matches.
    let cross_text = "Cross-tenant lesson — cross project recurring failures timeouts blockers \
                      fixes how to solve: tenant scoped cache keys.";
    let other = TenantId::new("other_tenant").unwrap();
    store
        .add(
            MemoryChunk::new(other.clone(), cross_text, ChunkType::Summary)
                .with_project(ProjectId::from("other_project"))
                .with_tags(vec![
                    "kind:consolidated".to_string(),
                    "priority:9".to_string(),
                ]),
        )
        .await
        .unwrap();

    let home = TenantId::new("home_tenant").unwrap();
    store
        .add(
            MemoryChunk::new(
                home.clone(),
                "Home tenant cross project recurring failures timeouts blockers fixes how to solve log",
                ChunkType::Summary,
            )
            .with_project(ProjectId::from("home_project"))
            .with_tags(vec!["kind:finish".to_string()]),
        )
        .await
        .unwrap();

    let md_dir = tempdir().unwrap();

    // First without the flag — no Cross-Tenant section.
    run_cli(
        &store,
        None,
        CliCommand::MemoryMd {
            tenant_id: Some("home_tenant".to_string()),
            project_id: Some("home_project".to_string()),
            project_dir: md_dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 10,
            candidate_k: 40,
            cross_tenant: false,
            explain_output: None,
        },
    )
    .await
    .unwrap();
    let off = std::fs::read_to_string(md_dir.path().join("memory.md")).unwrap();
    assert!(
        !off.contains("Cross-Tenant Takeaways"),
        "cross-tenant section must be opt-in:\n{off}"
    );

    // Now with the flag — the cross-tenant lesson surfaces.
    run_cli(
        &store,
        None,
        CliCommand::MemoryMd {
            tenant_id: Some("home_tenant".to_string()),
            project_id: Some("home_project".to_string()),
            project_dir: md_dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 10,
            candidate_k: 40,
            cross_tenant: true,
            explain_output: None,
        },
    )
    .await
    .unwrap();
    let on = std::fs::read_to_string(md_dir.path().join("memory.md")).unwrap();
    assert!(
        on.contains("## Cross-Tenant Takeaways"),
        "cross-tenant section missing when flag set:\n{on}"
    );
    assert!(
        on.contains("Cross-tenant lesson"),
        "cross-tenant lesson missing:\n{on}"
    );
    assert!(
        !off.contains("Cross-tenant lesson"),
        "cross-tenant lesson must NOT appear in the project sections of memory.md when \
         --cross-tenant is off (it lives only in the cross-tenant section):\n{off}"
    );
}

#[tokio::test]
async fn shared_tenant_promotion_fires_when_multiproject() {
    let _env_guard = with_consolidator_env();
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t").unwrap();

    // Seed 12 chunks split across two projects (6 + 6) so the mock
    // consolidator's `supersedes` list spans both projects.
    let mut ids = Vec::new();
    for i in 0..12 {
        let project = if i < 6 { "proj_a" } else { "proj_b" };
        let id = store
            .add(
                MemoryChunk::new(
                    tenant.clone(),
                    format!("Run {i} in {project}: tenant scoped cache keys hit the bug."),
                    ChunkType::Summary,
                )
                .with_project(ProjectId::from(project))
                .with_tags(vec!["kind:finish".to_string()]),
            )
            .await
            .unwrap();
        ids.push(id.to_string());
    }

    // Mock returns one consolidated lesson superseding all 12 (so its
    // sources span proj_a and proj_b).
    let entry = serde_json::json!({
        "text": "Cross-project consolidated lesson: tenant scoped cache keys.",
        "supersedes": ids,
        "kind": "consolidated",
        "priority": 8,
    });
    let response = serde_json::to_string(&vec![entry]).unwrap();
    std::env::set_var("MEMD_CONSOLIDATOR", "mock");
    std::env::set_var("MEMD_CONSOLIDATOR_MOCK_RESPONSE", &response);
    // Consolidation runs without a project filter so the region
    // ingests both projects.
    let result = run_consolidate(&store, dir.path(), &tenant).await;
    std::env::remove_var("MEMD_CONSOLIDATOR");
    std::env::remove_var("MEMD_CONSOLIDATOR_MOCK_RESPONSE");
    assert_eq!(result["consolidated"], 1);
    assert_eq!(
        result["promoted_to_shared"], 1,
        "multi-project supersedes should promote to shared tenant"
    );

    // The shared tenant now holds a cross-tenant-promoted lesson.
    let shared = TenantId::new("shared").unwrap();
    let shared_chunks = store
        .list_chunks_for_project(&shared, None, 100, 0)
        .await
        .unwrap();
    assert!(
        shared_chunks
            .iter()
            .any(|c| c.tags.iter().any(|t| t == "kind:cross_tenant_promoted")
                && c.tags.iter().any(|t| t == "source_tenant:t")),
        "shared tenant must hold the promoted lesson, got: {shared_chunks:?}"
    );
}

#[tokio::test]
async fn shared_promotion_requires_explicit_opt_in() {
    let _env_guard = with_consolidator_env();
    // Without `--promote-to-shared`, consolidation must never write
    // anything to the shared tenant, even when a single lesson spans
    // multiple projects.
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t").unwrap();

    let mut ids = Vec::new();
    for i in 0..12 {
        let project = if i < 6 { "proj_a" } else { "proj_b" };
        let id = store
            .add(
                MemoryChunk::new(
                    tenant.clone(),
                    format!("Run {i} in {project}: tenant scoped cache keys hit the bug."),
                    ChunkType::Summary,
                )
                .with_project(ProjectId::from(project))
                .with_tags(vec!["kind:finish".to_string()]),
            )
            .await
            .unwrap();
        ids.push(id.to_string());
    }
    let entry = serde_json::json!({
        "text": "Cross-project consolidated lesson v2.",
        "supersedes": ids,
        "kind": "consolidated",
        "priority": 8,
    });
    let response = serde_json::to_string(&vec![entry]).unwrap();
    std::env::set_var("MEMD_CONSOLIDATOR", "mock");
    std::env::set_var("MEMD_CONSOLIDATOR_MOCK_RESPONSE", &response);

    run_cli(
        &store,
        None,
        CliCommand::Consolidate {
            tenant_id: Some("t".to_string()),
            project_id: None,
            project_dir: dir.path().to_path_buf(),
            max_region: 50,
            dry_run: false,
            background: false,
            force: false,
            promote_to_shared: false,
        },
    )
    .await
    .expect("consolidate run");

    std::env::remove_var("MEMD_CONSOLIDATOR");
    std::env::remove_var("MEMD_CONSOLIDATOR_MOCK_RESPONSE");

    let shared = TenantId::new("shared").unwrap();
    let after = store
        .list_chunks_for_project(&shared, None, 100, 0)
        .await
        .unwrap_or_default()
        .len();
    assert_eq!(
        after, 0,
        "promotion must NOT fire without --promote-to-shared"
    );
}

#[tokio::test]
async fn cross_tenant_dedup_removes_near_duplicates() {
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));

    // Two other tenants emit near-duplicate consolidated lessons whose
    // text contains the global failures query verbatim.
    let dup_text = "Cross-tenant lesson — cross project recurring failures timeouts blockers \
                    fixes how to solve: tenant scoped cache keys are the durable fix.";
    for tid in ["alpha_tenant", "beta_tenant"] {
        let t = TenantId::new(tid).unwrap();
        store
            .add(
                MemoryChunk::new(t, dup_text, ChunkType::Summary)
                    .with_project(ProjectId::from("p"))
                    .with_tags(vec![
                        "kind:consolidated".to_string(),
                        "priority:9".to_string(),
                    ]),
            )
            .await
            .unwrap();
    }

    // Home tenant with a baseline chunk to keep memory-md non-empty.
    let home = TenantId::new("home_tenant").unwrap();
    store
        .add(
            MemoryChunk::new(
                home,
                "Home tenant cross project recurring failures timeouts blockers fixes how to solve log",
                ChunkType::Summary,
            )
            .with_project(ProjectId::from("home_project"))
            .with_tags(vec!["kind:finish".to_string()]),
        )
        .await
        .unwrap();

    let md_dir = tempdir().unwrap();
    run_cli(
        &store,
        None,
        CliCommand::MemoryMd {
            tenant_id: Some("home_tenant".to_string()),
            project_id: Some("home_project".to_string()),
            project_dir: md_dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 10,
            candidate_k: 40,
            cross_tenant: true,
            explain_output: None,
        },
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(md_dir.path().join("memory.md")).unwrap();
    let occurrences = content.matches("Cross-tenant lesson").count();
    assert_eq!(
        occurrences, 1,
        "near-duplicate cross-tenant lessons must be deduped, got {occurrences}:\n{content}"
    );
}

async fn run_consolidate(
    store: &PersistentStore,
    project_dir: &std::path::Path,
    _tenant: &TenantId,
) -> serde_json::Value {
    // Drive the CLI dispatcher so we exercise the same path that ships.
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    // run_cli prints JSON to stdout; we redirect by re-running the
    // underlying handler. For simplicity, call the same code via the
    // public Consolidate command and rely on its return JSON in the
    // log, then parse the consolidated chunk metadata via the store.
    run_cli(
        store,
        None,
        CliCommand::Consolidate {
            tenant_id: Some("t".to_string()),
            project_id: None,
            project_dir: project_dir.to_path_buf(),
            max_region: 50,
            dry_run: false,
            background: false,
            force: false,
            promote_to_shared: true,
        },
    )
    .await
    .expect("consolidate run");
    // Reconstruct the result shape by inspecting the store.
    let _ = captured;
    let kind_consolidated_count = store
        .list_chunks_for_project(&TenantId::new("t").unwrap(), None, 200, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|c| c.tags.iter().any(|t| t == "kind:consolidated"))
        .count();
    let promoted_count = store
        .list_chunks_for_project(&TenantId::new("shared").unwrap(), None, 200, 0)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.tags.iter().any(|t| t == "kind:cross_tenant_promoted"))
        .count();
    serde_json::json!({
        "consolidated": kind_consolidated_count,
        "promoted_to_shared": promoted_count,
    })
}
