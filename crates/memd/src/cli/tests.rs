use super::args::parse_chunk_type;
use super::paths::{discover_project_data_dir_from, resolve_export_markdown_data_dirs_from};
use super::*;
use crate::store::persistent::{PersistentStore, PersistentStoreConfig};
use crate::store::MemoryStore;
use crate::types::{ChunkType, MemoryChunk, ProjectId};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::{tempdir, TempDir};

#[test]
fn parse_chunk_types() {
    assert!(matches!(parse_chunk_type("code"), Ok(ChunkType::Code)));
    assert!(matches!(parse_chunk_type("DOC"), Ok(ChunkType::Doc)));
    assert!(matches!(parse_chunk_type("Trace"), Ok(ChunkType::Trace)));
    assert!(parse_chunk_type("invalid").is_err());
}

fn unique_test_file(ext: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("memd_export_test_{now}.{ext}"))
}

fn make_persistent_store() -> (PersistentStore, TempDir) {
    let dir = tempdir().unwrap();
    let store = PersistentStore::open(PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    })
    .unwrap();
    (store, dir)
}

fn segment_payloads_contain(root: &std::path::Path, tenant: &TenantId, needle: &str) -> bool {
    let segments_dir = root.join("tenants").join(tenant.as_str()).join("segments");
    let Ok(entries) = std::fs::read_dir(segments_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let payload = entry.path().join("payload.bin");
        std::fs::read(payload)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
            .unwrap_or(false)
    })
}

#[tokio::test]
async fn cli_add_rejects_low_signal_progress_with_reason() {
    let store = MemoryStore::new();
    let result = run_cli(
        &store,
        None,
        CliCommand::Add {
            tenant_id: Some("quality_gate_cli".to_string()),
            text: "starting to inspect the files".to_string(),
            chunk_type: ChunkType::Summary,
            project_id: None,
            tags: Some(vec!["kind:progress".to_string()]),
            source_uri: None,
            source_path: None,
            warm: WarmMode::Off,
        },
    )
    .await;

    let err = result.expect_err("CLI add should reject low-signal progress");
    assert!(err
        .to_string()
        .contains("memory.add rejected by quality gate"));
}

#[tokio::test]
async fn cli_add_returns_every_split_chunk_id() {
    let store = MemoryStore::new();
    let rendered = cli_add_rendered(
        &store,
        None,
        CliAddRenderOptions {
            tenant_id: "cli_split_ids".to_string(),
            text: "A deterministic sentence for split identity verification. ".repeat(80),
            chunk_type: ChunkType::Doc,
            project_id: Some("identity-contract".to_string()),
            tags: None,
            source_uri: None,
            source_path: None,
        },
    )
    .await
    .unwrap();
    let response: Value = serde_json::from_str(&rendered).unwrap();
    let stored_ids = response["stored_chunk_ids"].as_array().unwrap();
    assert!(stored_ids.len() > 1);
    assert_eq!(stored_ids[0], response["chunk_id"]);
}

#[tokio::test]
async fn export_markdown_writes_human_readable_output() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("export_tenant").unwrap();
    let chunk = MemoryChunk::new(tenant, "export me", ChunkType::Doc)
        .with_tags(vec!["ctx:doc".to_string(), "quality".to_string()])
        .with_project(ProjectId::from("demo_project"));
    store.add(chunk).await.unwrap();

    let output_path = unique_test_file("md");
    run_cli(
        &store,
        None,
        CliCommand::Export {
            tenant_id: Some("export_tenant".to_string()),
            format: ExportFormat::Markdown,
            output: Some(output_path.clone()),
            page_size: 100,
        },
    )
    .await
    .unwrap();

    let content = std::fs::read_to_string(&output_path).unwrap();
    assert!(content.contains("# memd export"));
    assert!(content.contains("export me"));
    assert!(content.contains("demo_project"));
    let _ = std::fs::remove_file(output_path);
}

#[tokio::test]
async fn export_json_writes_chunk_array() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("export_json_tenant").unwrap();
    let chunk = MemoryChunk::new(tenant, "json export chunk", ChunkType::Decision);
    store.add(chunk).await.unwrap();

    let output_path = unique_test_file("json");
    run_cli(
        &store,
        None,
        CliCommand::Export {
            tenant_id: Some("export_json_tenant".to_string()),
            format: ExportFormat::Json,
            output: Some(output_path.clone()),
            page_size: 100,
        },
    )
    .await
    .unwrap();

    let content = std::fs::read_to_string(&output_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let rows = parsed.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["text"], "json export chunk");
    let _ = std::fs::remove_file(output_path);
}

#[tokio::test]
async fn agent_context_builds_cli_prefetch_payload() {
    let (store, data_dir) = make_persistent_store();
    let tenant = TenantId::new("agent_context_tenant").unwrap();
    let chunk = MemoryChunk::new(
            tenant,
            "experience_id=mt-schema-defaults-v1 repair rule: shared defaults belong in one schema layer",
            ChunkType::Research,
        )
        .with_project(ProjectId::from("schema_defaults"));
    store.add(chunk).await.unwrap();

    let payload = cli_agent_context_payload(
        &store,
        "agent_context_tenant",
        Some("schema_defaults"),
        Some("task-schema-defaults"),
        Some("thread-schema-defaults"),
        &[
            "mt-schema-defaults-v1 repair rules".to_string(),
            "private-agent-context-query-7a19".to_string(),
        ],
        5,
        1200,
        CliQueryMode::Generic,
        false,
        false,
    )
    .await
    .unwrap();

    assert_eq!(payload["interface"], "cli_prefetch");
    assert!(payload["retrieval_episode_id"].as_str().is_some());
    assert!(payload["result_count"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(payload["ranking_policy"]["mode"], "off");
    let markdown = render_agent_context(&payload, ExportFormat::Markdown).unwrap();
    assert!(markdown.contains("mt-schema-defaults-v1"));
    assert!(markdown.contains("interface: `cli_only`"));

    let dir = tempdir().unwrap();
    write_cli_log(Some(dir.path()), "memd_search", &payload).unwrap();
    let files = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(files.iter().any(|name| name.starts_with("memd_search_")));
    assert!(files.iter().any(|name| name == "memd_search_log.jsonl"));

    for name in ["metadata.db", "metadata.db-wal"] {
        let path = data_dir.path().join(name);
        if path.exists() {
            let bytes = std::fs::read(path).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("private-agent-context-query-7a19"));
        }
    }
}

#[tokio::test]
async fn memory_md_writes_project_and_global_takeaways() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("memory_md_tenant").unwrap();
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "Project architecture configuration deployment key decisions tradeoffs: \
                     use project-scoped metadata before payload reads.",
                ChunkType::Decision,
            )
            .with_project(ProjectId::from("memory_md_project"))
            .with_tags(vec!["kind:decision".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();
    store
        .add(
            MemoryChunk::new(
                tenant,
                "Machine wide reusable takeaways best practices recurring issues important paths \
                     how to solve: stop stale warm workers before replacing the bundled binary. \
                     Agent action: Check for stale warm workers before replacing a bundled binary.",
                ChunkType::Summary,
            )
            .with_tags(vec!["kind:finish".to_string(), "priority:7".to_string()]),
        )
        .await
        .unwrap();

    let dir = tempdir().unwrap();
    run_cli(
        &store,
        None,
        CliCommand::MemoryMd {
            tenant_id: Some("memory_md_tenant".to_string()),
            project_id: Some("memory_md_project".to_string()),
            project_dir: dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 10,
            candidate_k: 10,
            explain_output: None,
        },
    )
    .await
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join("memory.md")).unwrap();
    assert!(content.contains("## Project Fact Library"));
    assert!(content.contains("use project-scoped metadata before payload reads"));
    assert!(content.contains("memory_md_project"));
    assert!(content.contains("## Machine-Wide Fact Library"));
    assert!(content.contains("stop stale warm workers before replacing the bundled binary"));
    assert!(content.contains("priority:"));
}

#[tokio::test]
async fn memory_md_writes_candidate_explanation_report() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("memory_md_explain_tenant").unwrap();
    store
        .add(
            MemoryChunk::new(
                tenant,
                "Project architecture configuration deployment key decisions tradeoffs: \
                     Decision: keep candidate explanations for memory.md auditability.",
                ChunkType::Decision,
            )
            .with_project(ProjectId::from("memory_md_explain_project"))
            .with_tags(vec!["kind:decision".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();

    let dir = tempdir().unwrap();
    run_cli(
        &store,
        None,
        CliCommand::MemoryMd {
            tenant_id: Some("memory_md_explain_tenant".to_string()),
            project_id: Some("memory_md_explain_project".to_string()),
            project_dir: dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 0,
            candidate_k: 10,
            explain_output: Some(PathBuf::from("memory-explain.json")),
        },
    )
    .await
    .unwrap();

    let report_path = dir.path().join("memory-explain.json");
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_path).unwrap()).unwrap();
    let project_rows = report["project"].as_array().unwrap();
    let displayed = project_rows
        .iter()
        .find(|row| row["display_status"] == "displayed")
        .expect("expected at least one displayed candidate explanation");
    assert_eq!(displayed["display_rank"], 1);
    assert_eq!(displayed["filter_reason"], serde_json::Value::Null);
    assert!(displayed["priority_breakdown"]["total"].as_f64().unwrap() > 0.0);
    assert_eq!(displayed["source"], "scan");
    assert_eq!(displayed["mode"], "scan");
    assert_eq!(displayed["query"], "");
}

#[tokio::test]
async fn memory_md_omits_global_takeaways_when_limit_zero() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("memory_md_default_tenant").unwrap();
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "Project architecture configuration deployment key decisions tradeoffs: \
                     keep session startup scoped to project lessons.",
                ChunkType::Decision,
            )
            .with_project(ProjectId::from("memory_md_default_project"))
            .with_tags(vec!["kind:decision".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();
    store
        .add(
            MemoryChunk::new(
                tenant,
                "Machine wide reusable takeaways best practices recurring issues important paths \
                     how to solve: this should require explicit global-limit.",
                ChunkType::Summary,
            )
            .with_tags(vec!["kind:finish".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();

    let dir = tempdir().unwrap();
    run_cli(
        &store,
        None,
        CliCommand::MemoryMd {
            tenant_id: Some("memory_md_default_tenant".to_string()),
            project_id: Some("memory_md_default_project".to_string()),
            project_dir: dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 0,
            candidate_k: 10,
            explain_output: None,
        },
    )
    .await
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join("memory.md")).unwrap();
    assert!(content.contains("## Project Fact Library"));
    assert!(content.contains("keep session startup scoped to project lessons"));
    assert!(content.contains("memory_md_default_project"));
    assert!(!content.contains("## Machine-Wide Fact Library"));
    assert!(!content.contains("this should require explicit global-limit"));
}

#[tokio::test]
async fn eval_memory_md_passes_actionable_fixture() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("memory_md_eval_tenant").unwrap();
    let project = ProjectId::from("memory_md_eval_project");
    for (text, tags, chunk_type) in [
            (
                "Project architecture configuration deployment key decisions tradeoffs: \
                 Decision: keep project scopes explicit. Rationale: aliases hide drift.",
                vec!["kind:decision".to_string(), "priority:9".to_string()],
                ChunkType::Decision,
            ),
            (
                "Project recurring failures bugs timeouts blockers fixes how to solve: \
                 Validation: cargo test -p memd passed after the memory-md evaluator.",
                vec!["kind:finish".to_string(), "priority:8".to_string()],
                ChunkType::Summary,
            ),
            (
                "Project recurring failures bugs timeouts blockers fixes how to solve: \
                 Root cause: generated wrappers used to occupy startup memory.",
                vec!["kind:finish".to_string(), "priority:8".to_string()],
                ChunkType::Summary,
            ),
            (
                "Project takeaways best practices key decisions recurring issues important files paths \
                 how to solve: Command: memd eval-memory-md --tenant-id t --project-id p.",
                vec!["kind:run".to_string(), "priority:8".to_string()],
                ChunkType::Trace,
            ),
            (
                "Project takeaways best practices key decisions recurring issues important files paths \
                 how to solve: Follow-up: keep the useful-top-10 threshold at 0.8.",
                vec!["kind:finish".to_string(), "priority:8".to_string()],
                ChunkType::Summary,
            ),
        ] {
            store
                .add(
                    MemoryChunk::new(tenant.clone(), text.to_string(), chunk_type)
                        .with_project(project.clone())
                        .with_tags(tags),
                )
                .await
                .unwrap();
        }

    let dir = tempdir().unwrap();
    run_cli(
        &store,
        None,
        CliCommand::EvalMemoryMd {
            tenant_id: Some("memory_md_eval_tenant".to_string()),
            project_id: Some("memory_md_eval_project".to_string()),
            project_dir: dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            candidate_k: 20,
            top_n: 10,
            min_useful_ratio: 0.8,
            max_generated_wrappers: 0,
            agent_usefulness: false,
            gold_file: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn eval_memory_md_agent_usefulness_passes_synthetic_fixture() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("memory_md_agent_eval_tenant").unwrap();
    let project = ProjectId::from("memory_md_agent_eval_project");
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "Project architecture configuration deployment key decisions tradeoffs: \
                     Validation: startup state fixture has concrete work. \
                     Agent action: Verify the source-backed next action before resuming.",
                ChunkType::Summary,
            )
            .with_project(project.clone())
            .with_tags(vec!["kind:finish".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();

    let dir = tempdir().unwrap();
    let tasks_dir = dir.path().join("tasks");
    std::fs::create_dir_all(&tasks_dir).unwrap();
    std::fs::write(
        tasks_dir.join("todo.md"),
        "# Synthetic Work\n\n- [ ] verify startup state fixture\n",
    )
    .unwrap();

    run_cli(
        &store,
        None,
        CliCommand::EvalMemoryMd {
            tenant_id: Some("memory_md_agent_eval_tenant".to_string()),
            project_id: Some("memory_md_agent_eval_project".to_string()),
            project_dir: dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            candidate_k: 20,
            top_n: 10,
            min_useful_ratio: 0.8,
            max_generated_wrappers: 0,
            agent_usefulness: true,
            gold_file: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn eval_memory_md_gold_file_passes_synthetic_project() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("memory_md_gold_eval_tenant").unwrap();
    let project = ProjectId::from("memory_md_gold_eval_project");
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "Project architecture configuration deployment key decisions tradeoffs: \
                     Validation: gold-file startup fixture has concrete work. \
                     Agent action: Verify the gold-file project before accepting startup context.",
                ChunkType::Summary,
            )
            .with_project(project.clone())
            .with_tags(vec!["kind:finish".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();

    let dir = tempdir().unwrap();
    let tasks_dir = dir.path().join("tasks");
    std::fs::create_dir_all(&tasks_dir).unwrap();
    std::fs::write(
        tasks_dir.join("todo.md"),
        "# Gold Work\n\n- [ ] verify gold fixture\n",
    )
    .unwrap();
    let gold_path = dir.path().join("gold.json");
    std::fs::write(
        &gold_path,
        format!(
            r#"{{
  "projects": [
    {{
      "name": "gold",
      "project_dir": "{}",
      "must_contain": ["Project Fact Library"],
      "must_not_contain": ["Latest Project State"],
      "max_fragments": 0,
      "max_unrelated_machine_items": 1
    }}
  ]
}}"#,
            dir.path().display()
        ),
    )
    .unwrap();

    run_cli(
        &store,
        None,
        CliCommand::EvalMemoryMd {
            tenant_id: Some("memory_md_gold_eval_tenant".to_string()),
            project_id: Some("memory_md_gold_eval_project".to_string()),
            project_dir: dir.path().to_path_buf(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            candidate_k: 20,
            top_n: 10,
            min_useful_ratio: 0.8,
            max_generated_wrappers: 0,
            agent_usefulness: true,
            gold_file: Some(gold_path),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn eval_retrieval_passes_known_useful_fixture() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("retrieval_eval_tenant").unwrap();
    let project = ProjectId::from("retrieval_eval_project");
    let useful_a = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "alpha retrieval gate exactneedle decision: keep fixed useful chunk ids.",
                ChunkType::Decision,
            )
            .with_project(project.clone())
            .with_tags(vec!["kind:decision".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();
    let useful_b = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "alpha retrieval gate exactneedle validation: precision at k is measured.",
                ChunkType::Summary,
            )
            .with_project(project.clone())
            .with_tags(vec!["kind:finish".to_string(), "priority:8".to_string()]),
        )
        .await
        .unwrap();

    let dir = tempdir().unwrap();
    let queries = dir.path().join("retrieval_queries.jsonl");
    std::fs::write(
        &queries,
        format!(
            "{}\n",
            json!({
                "label": "alpha_gate",
                "query": "alpha retrieval gate exactneedle",
                "useful_chunk_ids": [useful_a.to_string(), useful_b.to_string()],
            })
        ),
    )
    .unwrap();

    run_cli(
        &store,
        None,
        CliCommand::EvalRetrieval {
            tenant_id: "retrieval_eval_tenant".to_string(),
            project_id: Some("retrieval_eval_project".to_string()),
            project_dir: dir.path().to_path_buf(),
            queries: Some(queries),
            k: 3,
            min_precision_at_k: 0.6,
            min_hit_rate_at_k: 1.0,
            min_known_recall_at_k: 0.0,
            min_mrr: 0.0,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn eval_retrieval_fails_when_precision_threshold_is_not_met() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("retrieval_eval_fail_tenant").unwrap();
    let project = ProjectId::from("retrieval_eval_fail_project");
    let useful = store
        .add(
            MemoryChunk::new(
                tenant,
                "beta retrieval gate exactneedle useful decision.",
                ChunkType::Decision,
            )
            .with_project(project)
            .with_tags(vec!["kind:decision".to_string(), "priority:9".to_string()]),
        )
        .await
        .unwrap();

    let dir = tempdir().unwrap();
    let queries = dir.path().join("retrieval_queries.jsonl");
    std::fs::write(
        &queries,
        format!(
            "{}\n",
            json!({
                "label": "beta_gate",
                "query": "beta retrieval gate exactneedle",
                "useful_chunk_ids": [useful.to_string()],
            })
        ),
    )
    .unwrap();

    let err = run_cli(
        &store,
        None,
        CliCommand::EvalRetrieval {
            tenant_id: "retrieval_eval_fail_tenant".to_string(),
            project_id: Some("retrieval_eval_fail_project".to_string()),
            project_dir: dir.path().to_path_buf(),
            queries: Some(queries),
            k: 5,
            min_precision_at_k: 1.0,
            min_hit_rate_at_k: 1.0,
            min_known_recall_at_k: 0.0,
            min_mrr: 0.0,
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("max_possible_precision_at_5"), "{err}");
}

#[tokio::test]
async fn generic_search_suppresses_generated_digest_wrappers() {
    let store = MemoryStore::new();
    let tenant = TenantId::new("search_digest_suppression_tenant").unwrap();
    let project = ProjectId::from("search_digest_suppression_project");
    let generated = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "gamma exactneedle generated wrapper should not surface in generic search",
                ChunkType::Summary,
            )
            .with_project(project.clone())
            .with_tags(vec![
                "task:status:generated".to_string(),
                "task:role:highlight_library".to_string(),
                "priority:9".to_string(),
            ]),
        )
        .await
        .unwrap();
    let useful = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "gamma exactneedle durable decision should surface in generic search",
                ChunkType::Decision,
            )
            .with_project(project.clone())
            .with_tags(vec!["kind:decision".to_string(), "priority:8".to_string()]),
        )
        .await
        .unwrap();

    let payload = search::cli_search_payload_silent(
        &store,
        "search_digest_suppression_tenant".to_string(),
        Some("search_digest_suppression_project".to_string()),
        "gamma exactneedle".to_string(),
        5,
        true,
        Some(4000),
        CliQueryMode::Generic,
        false,
        false,
        false,
    )
    .await
    .unwrap();
    let ids = payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["chunk_id"].as_str())
        .collect::<Vec<_>>();
    let generated = generated.to_string();
    let useful = useful.to_string();
    assert!(!ids.contains(&generated.as_str()), "{ids:?}");
    assert!(ids.contains(&useful.as_str()), "{ids:?}");
}

#[tokio::test]
async fn purge_dry_run_reports_candidates_without_mutating() {
    use crate::store::metadata::MetadataStore;

    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("purge_dry_run_tenant").unwrap();
    let project = ProjectId::from("purge_project");
    let hidden = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "expired progress that should be purge eligible",
                ChunkType::Summary,
            )
            .with_project(project.clone()),
        )
        .await
        .unwrap();
    let durable = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "durable decision that must survive purge planning",
                ChunkType::Decision,
            )
            .with_project(project.clone()),
        )
        .await
        .unwrap();
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &hidden,
            &crate::types::LifecycleDelta {
                status: Some(crate::types::ChunkStatus::Expired),
                lifecycle_updated_at_ms: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

    let report = purge::run_purge(
        &store,
        purge::PurgeOptions {
            tenant_id: tenant.to_string(),
            project_id: Some("purge_project".to_string()),
            older_than_days: 1,
            limit: 100,
            include_unreadable_active: false,
            archive: None,
            apply: false,
            vacuum_metadata: false,
            rewrite_segments: false,
        },
    )
    .await
    .unwrap();

    assert_eq!(report["status"], "dry_run");
    assert_eq!(report["candidate_count"], 1);
    assert!(store.get(&tenant, &hidden).await.unwrap().is_some());
    assert!(store.get(&tenant, &durable).await.unwrap().is_some());
}

#[tokio::test]
async fn purge_dry_run_can_include_unreadable_active_metadata() {
    use crate::store::metadata::MetadataStore;

    let (store, dir) = make_persistent_store();
    let tenant = TenantId::new("purge_unreadable_dry_run_tenant").unwrap();
    let project = ProjectId::from("purge_project");
    let unreadable = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "live metadata row whose segment payload disappeared",
                ChunkType::Summary,
            )
            .with_project(project.clone()),
        )
        .await
        .unwrap();
    let healthy = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "healthy live payload must not be an unreadable purge candidate",
                ChunkType::Decision,
            )
            .with_project(project),
        )
        .await
        .unwrap();
    let meta = store
        .metadata()
        .get(&tenant, &unreadable)
        .unwrap()
        .expect("unreadable metadata");
    let conn = rusqlite::Connection::open(dir.path().join("metadata.db")).unwrap();
    conn.execute(
        "UPDATE chunks SET segment_id = ?1 WHERE tenant_id = ?2 AND chunk_id = ?3",
        rusqlite::params![
            (meta.segment_id + 10_000) as i64,
            tenant.as_str(),
            unreadable.to_string()
        ],
    )
    .unwrap();
    drop(conn);

    let default_report = purge::run_purge(
        &store,
        purge::PurgeOptions {
            tenant_id: tenant.to_string(),
            project_id: Some("purge_project".to_string()),
            older_than_days: 1,
            limit: 100,
            include_unreadable_active: false,
            archive: None,
            apply: false,
            vacuum_metadata: false,
            rewrite_segments: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(default_report["candidate_count"], 0);

    let report = purge::run_purge(
        &store,
        purge::PurgeOptions {
            tenant_id: tenant.to_string(),
            project_id: Some("purge_project".to_string()),
            older_than_days: 1,
            limit: 100,
            include_unreadable_active: true,
            archive: None,
            apply: false,
            vacuum_metadata: false,
            rewrite_segments: false,
        },
    )
    .await
    .unwrap();

    assert_eq!(report["status"], "dry_run");
    assert_eq!(report["candidate_count"], 1);
    assert_eq!(report["hidden_candidate_count"], 0);
    assert_eq!(report["unreadable_active_candidate_count"], 1);
    assert_eq!(report["include_unreadable_active"], true);
    assert!(report["candidate_ids"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id.as_str() == Some(&unreadable.to_string())));
    assert!(store.get(&tenant, &healthy).await.unwrap().is_some());
}

#[tokio::test]
async fn purge_apply_requires_archive_before_deleting() {
    use crate::store::metadata::MetadataStore;

    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("purge_archive_required_tenant").unwrap();
    let hidden = store
        .add(MemoryChunk::new(
            tenant.clone(),
            "expired chunk needs archive before purge",
            ChunkType::Summary,
        ))
        .await
        .unwrap();
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &hidden,
            &crate::types::LifecycleDelta {
                status: Some(crate::types::ChunkStatus::Expired),
                lifecycle_updated_at_ms: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

    let err = purge::run_purge(
        &store,
        purge::PurgeOptions {
            tenant_id: tenant.to_string(),
            project_id: None,
            older_than_days: 1,
            limit: 100,
            include_unreadable_active: false,
            archive: None,
            apply: true,
            vacuum_metadata: false,
            rewrite_segments: false,
        },
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("requires --archive"),
        "unexpected error: {err}"
    );
    assert!(store.get(&tenant, &hidden).await.unwrap().is_some());
}

#[tokio::test]
async fn purge_apply_archives_and_removes_unreadable_active_metadata() {
    use crate::store::metadata::MetadataStore;

    let (store, dir) = make_persistent_store();
    let tenant = TenantId::new("purge_unreadable_apply_tenant").unwrap();
    let project = ProjectId::from("purge_project");
    let unreadable_text = "unreadable live payload should be represented by metadata archive";
    let unreadable = store
        .add(
            MemoryChunk::new(tenant.clone(), unreadable_text, ChunkType::Summary)
                .with_project(project.clone()),
        )
        .await
        .unwrap();
    let healthy = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "healthy live payload must survive unreadable purge",
                ChunkType::Decision,
            )
            .with_project(project),
        )
        .await
        .unwrap();
    let meta = store
        .metadata()
        .get(&tenant, &unreadable)
        .unwrap()
        .expect("unreadable metadata");
    let conn = rusqlite::Connection::open(dir.path().join("metadata.db")).unwrap();
    conn.execute(
        "UPDATE chunks SET segment_id = ?1 WHERE tenant_id = ?2 AND chunk_id = ?3",
        rusqlite::params![
            (meta.segment_id + 10_000) as i64,
            tenant.as_str(),
            unreadable.to_string()
        ],
    )
    .unwrap();
    drop(conn);
    let archive = dir.path().join("purge-unreadable-archive.json");

    let report = purge::run_purge(
        &store,
        purge::PurgeOptions {
            tenant_id: tenant.to_string(),
            project_id: Some("purge_project".to_string()),
            older_than_days: 1,
            limit: 100,
            include_unreadable_active: true,
            archive: Some(archive.clone()),
            apply: true,
            vacuum_metadata: false,
            rewrite_segments: false,
        },
    )
    .await
    .unwrap();

    assert_eq!(report["status"], "completed");
    assert_eq!(report["candidate_count"], 1);
    assert_eq!(report["unreadable_active_candidate_count"], 1);
    assert_eq!(report["soft_deleted_before_purge"], 1);
    assert_eq!(report["hard_deleted_metadata_rows"], 1);
    assert_eq!(report["archive_verification"]["status"], "verified");
    assert_eq!(report["archive_verification"]["record_count"], 1);
    assert_eq!(report["archive_verification"]["payload_missing_count"], 1);
    assert!(report["archive_verification"]["archive_sha256"]
        .as_str()
        .is_some_and(|hash| hash.len() == 64));
    assert!(store
        .metadata()
        .get(&tenant, &unreadable)
        .unwrap()
        .is_none());
    assert!(store.get(&tenant, &healthy).await.unwrap().is_some());

    let archive_text = std::fs::read_to_string(&archive).unwrap();
    assert!(archive_text.contains("memd_purge_archive_v1"));
    assert!(archive_text.contains("unreadable_active_payload"));
    assert!(archive_text.contains("\"payload_available\": false"));
    assert!(archive_text.contains(unreadable_text));
}

#[tokio::test]
async fn purge_apply_archives_and_removes_hidden_metadata() {
    use crate::store::metadata::MetadataStore;

    let (store, dir) = make_persistent_store();
    let tenant = TenantId::new("purge_apply_tenant").unwrap();
    let project = ProjectId::from("purge_project");
    let hidden = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "expired chunk payload must be archived before purge",
                ChunkType::Summary,
            )
            .with_project(project.clone()),
        )
        .await
        .unwrap();
    let durable = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "durable decision must remain searchable after purge",
                ChunkType::Decision,
            )
            .with_project(project),
        )
        .await
        .unwrap();
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &hidden,
            &crate::types::LifecycleDelta {
                status: Some(crate::types::ChunkStatus::Expired),
                lifecycle_updated_at_ms: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    let archive = dir.path().join("purge-archive.json");

    let report = purge::run_purge(
        &store,
        purge::PurgeOptions {
            tenant_id: tenant.to_string(),
            project_id: Some("purge_project".to_string()),
            older_than_days: 1,
            limit: 100,
            include_unreadable_active: false,
            archive: Some(archive.clone()),
            apply: true,
            vacuum_metadata: true,
            rewrite_segments: false,
        },
    )
    .await
    .unwrap();

    assert_eq!(report["status"], "completed");
    assert_eq!(report["candidate_count"], 1);
    assert_eq!(report["hard_deleted_metadata_rows"], 1);
    assert_eq!(report["archive_verification"]["status"], "verified");
    assert_eq!(
        report["archive_verification"]["tenant_id"].as_str(),
        Some(tenant.as_str())
    );
    assert_eq!(
        report["archive_verification"]["project_id"].as_str(),
        Some("purge_project")
    );
    assert_eq!(report["archive_verification"]["record_count"], 1);
    assert_eq!(report["archive_verification"]["payload_available_count"], 1);
    let archive_text = std::fs::read_to_string(&archive).unwrap();
    assert!(archive_text.contains("memd_purge_archive_v1"));
    assert!(archive_text.contains("expired chunk payload must be archived before purge"));
    assert!(store.get(&tenant, &hidden).await.unwrap().is_none());
    assert!(store.get(&tenant, &durable).await.unwrap().is_some());
    let rows = store
        .metadata()
        .list_recent_for_project(&tenant, Some("purge_project"), 10)
        .unwrap();
    assert!(!rows.iter().any(|row| row.chunk_id == hidden));
    assert!(rows.iter().any(|row| row.chunk_id == durable));
}

#[tokio::test]
async fn purge_apply_rebuilds_hnsw_for_hidden_dense_entries() {
    use crate::embeddings::{Embedder, MockEmbedder};
    use crate::store::dense::{DenseSearchConfig, DenseSearcher};
    use crate::store::metadata::MetadataStore;

    let dir = tempdir().unwrap();
    let mut store = PersistentStore::open(PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_async_indexing: false,
        ..Default::default()
    })
    .unwrap();
    let embedder = Arc::new(MockEmbedder::new());
    let dense = Arc::new(DenseSearcher::with_embedder(
        Arc::clone(&embedder) as Arc<dyn Embedder>,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    store.set_dense_searcher_for_tests(Arc::clone(&dense));

    let tenant = TenantId::new("purge_hnsw_rebuild_tenant").unwrap();
    let project = ProjectId::from("purge_project");
    let hidden = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "expired dense hnsw payload must be excluded during purge rebuild",
                ChunkType::Summary,
            )
            .with_project(project.clone()),
        )
        .await
        .unwrap();
    let durable = store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "durable dense hnsw payload must remain searchable after purge rebuild",
                ChunkType::Decision,
            )
            .with_project(project),
        )
        .await
        .unwrap();
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &hidden,
            &crate::types::LifecycleDelta {
                status: Some(crate::types::ChunkStatus::Expired),
                lifecycle_updated_at_ms: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

    let hidden_set = HashSet::from([hidden.clone()]);
    let durable_set = HashSet::from([durable.clone()]);
    assert!(dense.has_valid_embeddings_for_chunks(&tenant, &hidden_set));
    assert!(dense.has_valid_embeddings_for_chunks(&tenant, &durable_set));

    let archive = dir.path().join("purge-hnsw-archive.json");
    let report = purge::run_purge(
        &store,
        purge::PurgeOptions {
            tenant_id: tenant.to_string(),
            project_id: Some("purge_project".to_string()),
            older_than_days: 1,
            limit: 100,
            include_unreadable_active: false,
            archive: Some(archive),
            apply: true,
            vacuum_metadata: false,
            rewrite_segments: false,
        },
    )
    .await
    .unwrap();

    assert_eq!(report["status"], "completed");
    assert_eq!(report["candidate_count"], 1);
    assert_eq!(report["compaction"]["hnsw_rebuilt"], true);
    assert_eq!(
        report["compaction"]["hnsw_rebuild"]["embeddings_excluded"],
        1
    );
    assert!(!dense.has_valid_embeddings_for_chunks(&tenant, &hidden_set));
    assert!(dense.has_valid_embeddings_for_chunks(&tenant, &durable_set));
    assert!(store.get(&tenant, &hidden).await.unwrap().is_none());
    assert!(store.get(&tenant, &durable).await.unwrap().is_some());

    let dense_results = dense
        .search(&tenant, "durable dense hnsw payload", 10)
        .await
        .unwrap();
    assert!(dense_results
        .iter()
        .any(|result| result.chunk_id == durable));
    assert!(!dense_results.iter().any(|result| result.chunk_id == hidden));
}

#[tokio::test]
async fn purge_apply_rewrites_segments_to_reclaim_hidden_payload_bytes() {
    use crate::store::metadata::MetadataStore;

    let (store, dir) = make_persistent_store();
    let tenant = TenantId::new("purge_segment_rewrite_tenant").unwrap();
    let project = ProjectId::from("purge_project");
    let hidden_text = format!(
        "expired segment rewrite payload unique_hidden_marker {}",
        "x".repeat(900)
    );
    let durable_text = "durable segment rewrite payload unique_durable_marker";
    let hidden = store
        .add(
            MemoryChunk::new(tenant.clone(), hidden_text.clone(), ChunkType::Summary)
                .with_project(project.clone()),
        )
        .await
        .unwrap();
    let durable = store
        .add(
            MemoryChunk::new(tenant.clone(), durable_text, ChunkType::Decision)
                .with_project(project),
        )
        .await
        .unwrap();
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &hidden,
            &crate::types::LifecycleDelta {
                status: Some(crate::types::ChunkStatus::Expired),
                lifecycle_updated_at_ms: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

    let archive = dir.path().join("purge-segment-rewrite-archive.json");
    let report = purge::run_purge(
        &store,
        purge::PurgeOptions {
            tenant_id: tenant.to_string(),
            project_id: Some("purge_project".to_string()),
            older_than_days: 1,
            limit: 100,
            include_unreadable_active: false,
            archive: Some(archive),
            apply: true,
            vacuum_metadata: false,
            rewrite_segments: true,
        },
    )
    .await
    .unwrap();

    assert_eq!(report["status"], "completed");
    assert_eq!(report["hard_deleted_metadata_rows"], 1);
    assert_eq!(report["segment_rewrite"]["segments_rewritten"], 1);
    assert_eq!(report["segment_rewrite"]["chunks_moved"], 1);
    assert!(
        report["segment_rewrite"]["bytes_reclaimed"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(store.get(&tenant, &hidden).await.unwrap().is_none());
    assert!(store.get(&tenant, &durable).await.unwrap().is_some());
    assert!(!segment_payloads_contain(
        dir.path(),
        &tenant,
        "unique_hidden_marker"
    ));
    assert!(segment_payloads_contain(
        dir.path(),
        &tenant,
        "unique_durable_marker"
    ));
}

#[tokio::test]
async fn call_invokes_former_tool_operations_without_server() {
    let store = MemoryStore::new();

    let add_value = cli_call_tool(
        &store,
        None,
        "memory.add",
        json!({
            "tenant_id": "call_tenant",
            "project_id": "call_project",
            "type": "doc",
            "text": "call parity marker: local executable operation",
            "tags": ["kind:parity"]
        }),
    )
    .await
    .unwrap();
    let add_payload = unwrap_content_payload(add_value).unwrap();
    let chunk_id = add_payload["chunk_id"].as_str().unwrap().to_string();

    let get_value = cli_call_tool(
        &store,
        None,
        "memory.get",
        json!({
            "tenant_id": "call_tenant",
            "chunk_id": chunk_id
        }),
    )
    .await
    .unwrap();
    let get_payload = unwrap_content_payload(get_value).unwrap();
    assert!(get_payload["chunk"]["text"]
        .as_str()
        .is_some_and(|text| text.contains("local executable operation")));

    let task_value = cli_call_tool(
        &store,
        None,
        "task.start",
        json!({
            "tenant_id": "call_tenant",
            "project_id": "call_project",
            "goal": "prove CLI call parity"
        }),
    )
    .await
    .unwrap();
    let task_payload = unwrap_content_payload(task_value).unwrap();
    assert!(task_payload["task_id"].as_str().is_some());
}

#[test]
fn warm_socket_path_is_stable_per_data_dir_without_version_input() {
    let dir = tempdir().unwrap();
    let config = WarmProcessConfig {
        data_dir: dir.path().join("data"),
        config_path: None,
        embedding_model: "all-minilm".to_string(),
        search_variant: "hybrid-feature".to_string(),
    };

    let same = warm_socket_path(&config);
    assert_eq!(same, warm_socket_path(&config));
    assert!(same.ends_with("memd.sock"));

    // The path is a pure function of the data dir: nothing else —
    // no version, protocol, model, or variant — feeds the hash, so
    // a new binary can reach a worker left by an old one.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(config.data_dir.display().to_string().as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    let expected = config
        .data_dir
        .join("warm")
        .join(&hex[..16])
        .join("memd.sock");
    assert_eq!(same, expected);

    // Model/variant changes do NOT move the socket...
    let mut dense = config.clone();
    dense.search_variant = "dense-only".to_string();
    assert_eq!(same, warm_socket_path(&dense));

    // ...but a different data dir does.
    let mut other = config.clone();
    other.data_dir = dir.path().join("other-data");
    assert_ne!(same, warm_socket_path(&other));
}

#[test]
fn warm_socket_path_uses_short_temp_path_for_long_data_dirs() {
    let config = WarmProcessConfig {
        data_dir: PathBuf::from("/tmp").join("a".repeat(180)),
        config_path: None,
        embedding_model: "all-minilm".to_string(),
        search_variant: "hybrid-feature".to_string(),
    };

    let socket = warm_socket_path(&config);
    assert!(socket.to_string_lossy().len() < 100);
    assert!(socket.starts_with(std::env::temp_dir().join("memd-warm")));
}

#[tokio::test]
async fn batch_jsonl_runs_multiple_calls_through_one_store() {
    let store = MemoryStore::new();
    let input = r#"
{"tool":"memory.add","arguments":{"tenant_id":"batch_tenant","project_id":"batch_project","type":"doc","text":"batch marker one"}}
{"tool":"memory.stats","arguments":{"tenant_id":"batch_tenant"}}
"#;

    let rendered = run_batch_jsonl(&store, None, input, false).await.unwrap();
    let rows = rendered
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["ok"], true);
    assert_eq!(rows[1]["ok"], true);
    assert_eq!(rows[1]["tool"], "memory.stats");
    assert!(rows[1]["result"]["total_chunks"].as_u64().unwrap_or(0) >= 1);
}

#[tokio::test]
async fn init_writes_cli_guardrails() {
    let store = MemoryStore::new();
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    run_cli(
        &store,
        None,
        CliCommand::Init {
            tenant_id: "demo_tenant".to_string(),
            project_dir: project_dir.clone(),
            project_id: Some("demo_project".to_string()),
            memd_command: "memd".to_string(),
            memd_data_dir: Some(PathBuf::from("/tmp/memd-data")),
            write_agent_files: true,
        },
    )
    .await
    .unwrap();

    let guardrails =
        std::fs::read_to_string(project_dir.join(".memd/memory_guardrails.md")).unwrap();
    assert!(guardrails.contains("demo_tenant"));
    assert!(guardrails.contains("memory-md"));
    assert!(guardrails.contains("memory.md"));
    assert!(guardrails.contains("memd agent-context"));
    assert!(guardrails.contains("memd add"));
    assert!(guardrails.contains(".memd/project_scope.json"));
    assert!(!project_dir.join(".memd/mcp_config_claude.json").exists());
    assert!(!project_dir.join(".memd/mcp_config_codex.toml").exists());

    let tenant_scope: Value = serde_json::from_str(
        &std::fs::read_to_string(project_dir.join(".memd/tenant_scope.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(tenant_scope["write_tenant"], "demo_tenant");
    // Pins the stop-writing behaviour: read-scope plumbing was
    // removed, so fresh scope files must not carry these keys.
    assert!(tenant_scope.get("read_tenants").is_none());
    assert!(tenant_scope.get("scope").is_none());

    let project_scope: Value = serde_json::from_str(
        &std::fs::read_to_string(project_dir.join(".memd/project_scope.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(project_scope["tenant_id"], "demo_tenant");
    assert_eq!(project_scope["project_id"], "demo_project");
    assert!(project_scope.get("read_tenants").is_none());
    assert_eq!(project_scope["interface"], "cli");
    assert_eq!(project_scope["cli_command"], "memd");

    let agents = std::fs::read_to_string(project_dir.join("AGENTS.md")).unwrap();
    assert!(agents.contains("memd-guardrails:start"));
}

#[tokio::test]
async fn init_upserts_guardrail_block_without_duplication() {
    let store = MemoryStore::new();
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    for tenant in ["tenant_one", "tenant_two"] {
        run_cli(
            &store,
            None,
            CliCommand::Init {
                tenant_id: tenant.to_string(),
                project_dir: project_dir.clone(),
                project_id: Some("shared_project".to_string()),
                memd_command: "memd".to_string(),
                memd_data_dir: None,
                write_agent_files: true,
            },
        )
        .await
        .unwrap();
    }

    let agents = std::fs::read_to_string(project_dir.join("AGENTS.md")).unwrap();
    let marker_count = agents.matches("memd-guardrails:start").count();
    assert_eq!(marker_count, 1);
    assert!(agents.contains("tenant_two"));
}

// --- Item 4: export-markdown --data-dir auto-discovery ---

#[tokio::test]
async fn init_local_scope_persists_data_dir_in_tenant_scope() {
    // Pins the behaviour-change introduced for Item 4: `data_dir`
    // is now recorded in `tenant_scope.json` for every scope mode,
    // not just `global`, so `memd export-markdown` can auto-discover
    // it without forcing the user to pass `--data-dir`.
    let store = MemoryStore::new();
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    run_cli(
        &store,
        None,
        CliCommand::Init {
            tenant_id: "t_local".to_string(),
            project_dir: project_dir.clone(),
            project_id: Some("p".to_string()),
            memd_command: "memd".to_string(),
            memd_data_dir: Some(PathBuf::from("/tmp/memd-data-local")),
            write_agent_files: false,
        },
    )
    .await
    .unwrap();

    let tenant_scope: Value = serde_json::from_str(
        &std::fs::read_to_string(project_dir.join(".memd/tenant_scope.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(tenant_scope["data_dir"], "/tmp/memd-data-local");
}

#[test]
fn discover_project_data_dir_returns_none_when_no_memd_dir() {
    let dir = tempdir().unwrap();
    assert!(discover_project_data_dir_from(dir.path()).is_none());
}

#[test]
fn discover_project_data_dir_returns_data_dir_from_tenant_scope() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
    std::fs::write(
            dir.path().join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/abs/path/to/data"}"#,
        )
        .unwrap();
    let discovered = discover_project_data_dir_from(dir.path()).unwrap();
    assert_eq!(discovered, PathBuf::from("/abs/path/to/data"));
}

#[test]
fn discover_project_data_dir_returns_none_when_field_missing() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
    std::fs::write(
        dir.path().join(".memd/tenant_scope.json"),
        r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"]}"#,
    )
    .unwrap();
    assert!(discover_project_data_dir_from(dir.path()).is_none());
}

#[test]
fn discover_project_data_dir_returns_none_on_malformed_json() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
    std::fs::write(dir.path().join(".memd/tenant_scope.json"), "{not json").unwrap();
    assert!(discover_project_data_dir_from(dir.path()).is_none());
}

#[test]
fn discover_project_data_dir_walks_up_to_nearest_ancestor() {
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    let nested = project.join("a").join("b").join("c");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(project.join(".memd")).unwrap();
    std::fs::write(
            project.join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/discovered"}"#,
        )
        .unwrap();
    let discovered = discover_project_data_dir_from(&nested).unwrap();
    assert_eq!(discovered, PathBuf::from("/discovered"));
}

#[test]
fn discover_project_data_dir_resolves_relative_path_against_memd_parent() {
    // When `data_dir` in the JSON is a relative path, resolve it
    // relative to the directory containing `.memd/`, not relative
    // to the caller's CWD. This matches what `memd init` intends
    // when a user passes a project-relative `--data-dir`.
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(project.join(".memd")).unwrap();
    std::fs::write(
            project.join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"subdir/data"}"#,
        )
        .unwrap();
    let discovered = discover_project_data_dir_from(&project).unwrap();
    assert_eq!(discovered, project.join("subdir").join("data"));
}

#[test]
fn resolve_export_markdown_data_dirs_prefers_explicit_arg() {
    // When --data-dir is explicit, the guard checks ONLY that path
    // (single-element vec). The caller's declared intent overrides
    // any ambient discovery and the home default.
    let explicit = PathBuf::from("/explicit/path");
    let resolved = resolve_export_markdown_data_dirs(Some(&explicit)).unwrap();
    assert_eq!(resolved, vec![explicit]);
}

#[test]
fn resolve_export_markdown_data_dirs_from_uses_discovery_alongside_home_default() {
    // Regression for Codex Item 4 HIGH: when --data-dir is absent,
    // discovery must AUGMENT the home default, not replace it. An
    // ancestor config with `data_dir` = `/foo` must not silently
    // turn off the guard for `$HOME/.memd/data`.
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
    std::fs::write(
            dir.path().join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/discovered/data"}"#,
        )
        .unwrap();
    let resolved = resolve_export_markdown_data_dirs_from(None, Some(dir.path())).unwrap();
    let home_default = dirs::home_dir().unwrap().join(".memd").join("data");
    assert!(
        resolved.contains(&PathBuf::from("/discovered/data")),
        "expected discovered path in list, got {:?}",
        resolved
    );
    assert!(
        resolved.contains(&home_default),
        "expected home default in list, got {:?}",
        resolved
    );
}

#[test]
fn resolve_export_markdown_data_dirs_from_explicit_beats_discovery() {
    // Explicit --data-dir is a single-element vec; neither
    // discovery nor home default is appended. The caller takes
    // responsibility for the path they asked the guard to check.
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
    std::fs::write(
            dir.path().join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/not-used"}"#,
        )
        .unwrap();
    let explicit = PathBuf::from("/explicit/wins");
    let resolved =
        resolve_export_markdown_data_dirs_from(Some(&explicit), Some(dir.path())).unwrap();
    assert_eq!(resolved, vec![explicit]);
}

#[test]
fn resolve_export_markdown_data_dirs_from_falls_back_to_home_when_no_project() {
    let dir = tempdir().unwrap();
    let resolved = resolve_export_markdown_data_dirs_from(None, Some(dir.path())).unwrap();
    let home_default = dirs::home_dir().unwrap().join(".memd").join("data");
    assert_eq!(resolved, vec![home_default]);
}

#[test]
fn discover_project_data_dir_inner_broken_config_stops_walk() {
    // Regression for Codex Item 4 MEDIUM #2: an inner project
    // whose `.memd/tenant_scope.json` is missing `data_dir` must
    // NOT silently inherit the outer project's value. Discovery
    // treats the first-found `.memd/tenant_scope.json` as the
    // project boundary.
    let dir = tempdir().unwrap();
    let outer = dir.path().join("outer");
    let inner = outer.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::create_dir_all(outer.join(".memd")).unwrap();
    std::fs::create_dir_all(inner.join(".memd")).unwrap();
    // Outer has a valid config…
    std::fs::write(
            outer.join(".memd/tenant_scope.json"),
            r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"],"data_dir":"/outer-data"}"#,
        )
        .unwrap();
    // …but the inner project's config is missing data_dir.
    std::fs::write(
        inner.join(".memd/tenant_scope.json"),
        r#"{"primary_tenant":"t","write_tenant":"t","scope":"local","read_tenants":["t"]}"#,
    )
    .unwrap();
    assert!(
        discover_project_data_dir_from(&inner).is_none(),
        "inner broken config must stop walk and not return outer's data_dir"
    );
}

#[test]
fn resolve_data_dir_absolutizes_relative_explicit_arg() {
    // Regression for Codex Item 4 MEDIUM #3: `memd init` must
    // persist an absolute path even when the caller passed a
    // relative `--memd-data-dir`. Without this, later auto-
    // discovery would reinterpret the relative value against the
    // project root, which differs from the user's CWD at init
    // time.
    let relative = PathBuf::from("rel/data");
    let resolved = resolve_data_dir(Some(&relative)).unwrap();
    assert!(
        resolved.is_absolute(),
        "resolved must be absolute; got {}",
        resolved.display()
    );
    assert!(
        resolved.ends_with("rel/data"),
        "resolved must still end in the supplied segments; got {}",
        resolved.display()
    );
}

#[test]
fn resolve_data_dir_leaves_absolute_explicit_arg_unchanged() {
    let absolute = PathBuf::from("/already/abs/data");
    let resolved = resolve_data_dir(Some(&absolute)).unwrap();
    assert_eq!(resolved, absolute);
}

// --- Item 3: G3 symlink hardening ---

#[test]
fn reject_if_any_symlink_inside_outdir_accepts_regular_files() {
    // Baseline — a normal file tree under outdir passes.
    let dir = tempdir().unwrap();
    let outdir = dir.path().to_path_buf();
    std::fs::create_dir_all(outdir.join("a/b")).unwrap();
    std::fs::write(outdir.join("a/b/c.md"), "content").unwrap();
    let target = outdir.join("a/b/c.md");
    reject_if_any_symlink_inside_outdir(&target, &outdir).unwrap();
}

#[test]
fn reject_if_any_symlink_inside_outdir_tolerates_nonexistent_components() {
    // Non-existent components are fine — create_dir_all will
    // materialise them freshly, so they can't be symlinks.
    let dir = tempdir().unwrap();
    let outdir = dir.path().to_path_buf();
    let target = outdir.join("never").join("existed").join("yet.md");
    reject_if_any_symlink_inside_outdir(&target, &outdir).unwrap();
}

#[cfg(unix)]
#[test]
fn reject_if_any_symlink_inside_outdir_refuses_leaf_symlink() {
    // Attacker-planted leaf symlink inside outdir must be refused.
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let outdir = dir.path().join("outdir");
    std::fs::create_dir_all(outdir.join("a/b")).unwrap();
    let victim = dir.path().join("victim.md");
    std::fs::write(&victim, "pre-existing victim content").unwrap();
    symlink(&victim, outdir.join("a/b/leaf.md")).unwrap();

    let target = outdir.join("a/b/leaf.md");
    let err = reject_if_any_symlink_inside_outdir(&target, &outdir).unwrap_err();
    assert!(
        matches!(err, crate::error::MemdError::ValidationError(_)),
        "expected ValidationError, got {err:?}"
    );
    // Critical: the victim file must NOT have been touched.
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "pre-existing victim content"
    );
}

#[cfg(unix)]
#[test]
fn reject_if_any_symlink_inside_outdir_refuses_intermediate_symlink() {
    // Attacker-planted directory symlink mid-path must be refused.
    // Without the guard, create_dir_all would happily step through
    // the symlink and std::fs::write would hit the attacker's dir.
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let outdir = dir.path().join("outdir");
    std::fs::create_dir_all(&outdir).unwrap();
    let victim_dir = dir.path().join("victim_dir");
    std::fs::create_dir_all(&victim_dir).unwrap();
    symlink(&victim_dir, outdir.join("sub")).unwrap();

    let target = outdir.join("sub").join("x.md");
    let err = reject_if_any_symlink_inside_outdir(&target, &outdir).unwrap_err();
    assert!(matches!(err, crate::error::MemdError::ValidationError(_)));
    assert!(
        !target.exists() || !victim_dir.join("x.md").exists(),
        "victim dir must not have been written into",
    );
}

#[cfg(unix)]
#[test]
fn reject_if_any_symlink_inside_outdir_permits_symlinked_outdir_itself() {
    // The outdir ITSELF is allowed to be a symlink — users may
    // legitimately point `--outdir` at a symlinked exports dir
    // they own. We only refuse symlinks planted BELOW outdir.
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let real_outdir = dir.path().join("real");
    std::fs::create_dir_all(&real_outdir).unwrap();
    let symlink_outdir = dir.path().join("linked");
    symlink(&real_outdir, &symlink_outdir).unwrap();

    let target = symlink_outdir.join("sub").join("x.md");
    reject_if_any_symlink_inside_outdir(&target, &symlink_outdir).unwrap();
}

#[cfg(unix)]
#[test]
fn reject_if_any_symlink_inside_outdir_fails_closed_on_permission_denied() {
    // Regression for Codex Item 3 LOW: abnormal filesystem states
    // (PermissionDenied, ELOOP, other I/O errors) must fail closed,
    // not silently skip the guard. An attacker-crafted directory
    // mode that denies symlink_metadata access must not become a
    // way to bypass the check.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let outdir = dir.path().join("outdir");
    std::fs::create_dir_all(outdir.join("locked")).unwrap();
    // Make the "locked" directory unreadable so symlink_metadata on
    // its children fails with EACCES, not ENOENT.
    std::fs::set_permissions(
        outdir.join("locked"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let target = outdir.join("locked").join("inner").join("x.md");
    let result = reject_if_any_symlink_inside_outdir(&target, &outdir);

    // Restore perms so tempdir cleanup works regardless of outcome.
    std::fs::set_permissions(
        outdir.join("locked"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();

    let err = result.expect_err("must fail closed on EACCES");
    assert!(matches!(err, crate::error::MemdError::ValidationError(_)));
}
