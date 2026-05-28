//! End-to-end tests for generated digest handling in `memory.md`.
//! Insert raw `task:kind:task_finish` chunks plus a generated
//! `task:role:highlight_library` digest, refresh `memory.md`, and
//! assert generated digest wrappers are not displayed as takeaways.
//!
//! The bundled `MemoryStore` ranks results by lexical overlap, so
//! chunk text deliberately echoes the `memory-md` query phrases to
//! guarantee the finishes are actually retrieved before suppression
//! runs — otherwise the suppression assertion would pass trivially.

use memd::cli::{run_cli, CliCommand};
use memd::config::{ProjectAliasConfig, ProjectAliasScopeConfig};
use memd::ops::{handle_memory_add, AddParams};
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::{configure_operation_routing, ChunkType, MemoryChunk, MemoryStore, ProjectId, TenantId};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use tempfile::tempdir;

static ROUTING_MUTEX: Mutex<()> = Mutex::new(());

fn routing_guard<'a>() -> MutexGuard<'a, ()> {
    ROUTING_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Text echoing the `project_failures` query so the mock store
/// retrieves the chunk with a high lexical score.
fn finish_text(task: &str) -> String {
    format!(
        "Project recurring failures bugs timeouts blockers fixes how to solve: task {task} \
         finished after resolving the blocker."
    )
}

fn open_persistent_store(dir: &std::path::Path) -> PersistentStore {
    let config = PersistentStoreConfig {
        data_dir: dir.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    };
    PersistentStore::open(config).expect("open persistent store")
}

#[tokio::test]
async fn generated_highlight_library_is_hidden_without_suppressing_source_finishes() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("phase1_tenant").unwrap();
    let project = "phase1_project";

    // 5 raw task_finish chunks for tasks T1..T5, all covered by the
    // digest below.
    for i in 1..=5 {
        let task_id = format!("T{i}");
        store
            .add(
                MemoryChunk::new(tenant.clone(), finish_text(&task_id), ChunkType::Summary)
                    .with_project(ProjectId::from(project))
                    .with_tags(vec![
                        "kind:finish".to_string(),
                        "task:kind:task_finish".to_string(),
                        format!("task:id:{task_id}"),
                    ]),
            )
            .await
            .unwrap();
    }

    // 1 highlight_library digest covering all 5 tasks.
    let summary = "Highlight library for phase1_project contains 5 ranked lessons with \
         future-agent uplift.\nCovers tasks: task:id:T1, task:id:T2, task:id:T3, task:id:T4, task:id:T5";
    store
        .add(
            MemoryChunk::new(tenant.clone(), summary, ChunkType::Summary)
                .with_project(ProjectId::from(project))
                .with_tags(vec![
                    "task:role:highlight_library".to_string(),
                    "task:status:generated".to_string(),
                    "kind:finish".to_string(),
                ]),
        )
        .await
        .unwrap();

    let content = run_memory_md(&store, "phase1_tenant", project).await;

    assert!(
        !content.contains("Highlight library for phase1_project"),
        "generated digest wrapper must be hidden from memory.md\n---\n{content}"
    );

    assert!(
        content.contains("finished after resolving the blocker"),
        "source finishes should remain eligible when their generated digest is hidden:\n---\n{content}"
    );
}

#[tokio::test]
async fn generated_digest_hidden_and_user_priority_still_ranks() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("phase1_tenant_pri").unwrap();
    let project = "phase1_project_pri";

    // task_finish for T1 with explicit priority:9 — must survive.
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                format!(
                    "{} Operator flagged this lesson as a high-priority keeper.",
                    finish_text("T1")
                ),
                ChunkType::Summary,
            )
            .with_project(ProjectId::from(project))
            .with_tags(vec![
                "kind:finish".to_string(),
                "task:kind:task_finish".to_string(),
                "task:id:T1".to_string(),
                "priority:9".to_string(),
            ]),
        )
        .await
        .unwrap();

    // task_finish for T2 (no explicit priority) — covered by the
    // hidden generated digest, but still eligible as a source record.
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                format!("{} Routine maintenance, low importance.", finish_text("T2")),
                ChunkType::Summary,
            )
            .with_project(ProjectId::from(project))
            .with_tags(vec![
                "kind:finish".to_string(),
                "task:kind:task_finish".to_string(),
                "task:id:T2".to_string(),
            ]),
        )
        .await
        .unwrap();

    // Digest covering T1 and T2.
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "Highlight library for phase1_project_pri contains 2 ranked lessons with \
                 future-agent uplift.\nCovers tasks: task:id:T1, task:id:T2",
                ChunkType::Summary,
            )
            .with_project(ProjectId::from(project))
            .with_tags(vec![
                "task:role:highlight_library".to_string(),
                "task:status:generated".to_string(),
            ]),
        )
        .await
        .unwrap();

    let content = run_memory_md(&store, "phase1_tenant_pri", project).await;

    assert!(
        content.contains("Operator flagged this lesson as a high-priority keeper"),
        "priority:9 finish must still rank:\n{content}"
    );
    assert!(
        !content.contains("Highlight library for phase1_project_pri"),
        "generated digest wrapper must be hidden from memory.md:\n{content}"
    );
    assert!(
        content.contains("Routine maintenance, low importance"),
        "covered source finish should remain eligible when the generated digest is hidden:\n{content}"
    );
}

#[tokio::test]
async fn project_alias_allows_memory_md_to_use_hyphenated_bester_scope() {
    let _guard = routing_guard();
    configure_operation_routing(false, Vec::new());

    let store = MemoryStore::new();
    let tenant = TenantId::new("phase1_alias_tenant").unwrap();
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "Project recurring failures bugs timeouts blockers fixes how to solve: \
                 Bester Tailscale gateway restore lesson came from the hyphenated project.",
                ChunkType::Summary,
            )
            .with_project(ProjectId::from("bester-hosting"))
            .with_tags(vec!["kind:finish".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();

    let isolated = run_memory_md(&store, "phase1_alias_tenant", "bester_hosting").await;
    assert!(
        !isolated.contains("Bester Tailscale gateway restore lesson"),
        "underscore scope must not silently merge hyphenated memories without an explicit alias"
    );

    configure_operation_routing(
        false,
        vec![ProjectAliasConfig {
            tenant_id: "phase1_alias_tenant".to_string(),
            project_id: "bester_hosting".to_string(),
            aliases: vec![ProjectAliasScopeConfig {
                tenant_id: "phase1_alias_tenant".to_string(),
                project_id: Some("bester-hosting".to_string()),
                reason: Some("project_id_separator_drift".to_string()),
            }],
        }],
    );

    let aliased = run_memory_md(&store, "phase1_alias_tenant", "bester_hosting").await;
    configure_operation_routing(false, Vec::new());

    assert!(
        aliased.contains("Bester Tailscale gateway restore lesson"),
        "explicit alias should let pinned underscore scope retrieve useful hyphenated memories:\n{aliased}"
    );
}

#[tokio::test]
async fn ephemeral_progress_is_hidden_from_default_memory_md() {
    let dir = tempdir().unwrap();
    let store = open_persistent_store(dir.path());
    let tenant = TenantId::new("phase2_ephemeral_memory_md").unwrap();
    let project = "phase2_project";

    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "Project recurring failures bugs timeouts blockers fixes how to solve: \
                 durable validation lesson remains visible in memory.md.",
                ChunkType::Summary,
            )
            .with_project(ProjectId::from(project))
            .with_tags(vec!["kind:finish".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: tenant.to_string(),
            project_id: Some(project.to_string()),
            text: "starting to inspect the files for project recurring failures bugs timeouts \
                   blockers fixes how to solve"
                .to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:progress".to_string()],
            mode: Some("conversation".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let content = run_memory_md(&store, "phase2_ephemeral_memory_md", project).await;

    assert!(
        content.contains("durable validation lesson remains visible"),
        "durable lesson should still appear in memory.md:\n{content}"
    );
    assert!(
        !content.contains("starting to inspect the files"),
        "ephemeral progress should be hidden from default memory.md:\n{content}"
    );
}

async fn run_memory_md<S: Store>(store: &S, tenant: &str, project: &str) -> String {
    let dir = tempdir().unwrap();
    run_cli(
        store,
        None,
        CliCommand::MemoryMd {
            tenant_id: Some(tenant.to_string()),
            project_id: Some(project.to_string()),
            project_dir: dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 0,
            candidate_k: 40,
            cross_tenant: false,
            explain_output: None,
        },
    )
    .await
    .unwrap();
    std::fs::read_to_string(dir.path().join("memory.md")).unwrap()
}
