//! Phase 1 end-to-end test: insert raw `task:kind:task_finish`
//! chunks plus a `task:role:highlight_library` digest, refresh
//! `memory.md`, and assert (a) the digest is surfaced, (b) covered
//! finishes are suppressed, (c) a user-tagged high-priority finish
//! survives suppression.
//!
//! The bundled `MemoryStore` ranks results by lexical overlap, so
//! chunk text deliberately echoes the `memory-md` query phrases to
//! guarantee the finishes are actually retrieved before suppression
//! runs — otherwise the suppression assertion would pass trivially.

use memd::cli::{run_cli, CliCommand};
use memd::store::Store;
use memd::{ChunkType, MemoryChunk, MemoryStore, ProjectId, TenantId};
use std::path::PathBuf;
use tempfile::tempdir;

/// Text echoing the `project_failures` query so the mock store
/// retrieves the chunk with a high lexical score.
fn finish_text(task: &str) -> String {
    format!(
        "Project recurring failures bugs timeouts blockers fixes how to solve: task {task} \
         finished after resolving the blocker."
    )
}

#[tokio::test]
async fn highlight_library_outranks_and_suppresses_raw_finishes() {
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

    // The digest must appear.
    assert!(
        content.contains("Highlight library for phase1_project"),
        "digest missing from memory.md\n---\n{content}"
    );

    // All 5 covered finishes should be suppressed even though their
    // text scores highly against the failures query.
    assert!(
        !content.contains("finished after resolving the blocker"),
        "covered finishes should be suppressed:\n---\n{content}"
    );
}

#[tokio::test]
async fn user_explicit_priority_high_survives_suppression_e2e() {
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

    // task_finish for T2 (no explicit priority) — also covered, should
    // be suppressed.
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
        "priority:9 finish must survive suppression:\n{content}"
    );
    assert!(
        !content.contains("Routine maintenance, low importance"),
        "covered finish without explicit priority must be suppressed:\n{content}"
    );
}

async fn run_memory_md(store: &MemoryStore, tenant: &str, project: &str) -> String {
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
        },
    )
    .await
    .unwrap();
    std::fs::read_to_string(dir.path().join("memory.md")).unwrap()
}
