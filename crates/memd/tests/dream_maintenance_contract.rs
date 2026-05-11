mod common;

use common::*;
use memd::index::sparse::SparseIndex;
use memd::store::metadata::MetadataStore;
use serde_json::{json, Value};
use std::collections::BTreeSet;

async fn add_digest_projection_on<S: memd::store::Store>(
    server: &std::sync::Arc<TestServer<S>>,
    tenant: &str,
    project: &str,
    text: &str,
) -> String {
    let resp = call_tool(
        server,
        "memory.add",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "text": text,
            "type": "summary",
            "tags": ["task:kind:digest", "task:projection:digest"]
        }),
    )
    .await;
    parse_result_text(&resp)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn dream_on<S: memd::store::Store>(
    server: &std::sync::Arc<TestServer<S>>,
    tenant: &str,
    project: &str,
    dry_run: bool,
) -> Value {
    let resp = call_tool(
        server,
        "memory.dream",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "dry_run": dry_run,
            "physical": {
                "run_store_compaction": false,
                "prune_sparse_index": false
            }
        }),
    )
    .await;
    parse_result_text(&resp)
}

async fn dream_with_physical_on<S: memd::store::Store>(
    server: &std::sync::Arc<TestServer<S>>,
    tenant: &str,
    project: &str,
    physical: Value,
) -> Value {
    let resp = call_tool(
        server,
        "memory.dream",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "dry_run": false,
            "physical": physical
        }),
    )
    .await;
    parse_result_text(&resp)
}

async fn search_ids_on<S: memd::store::Store>(
    server: &std::sync::Arc<TestServer<S>>,
    tenant: &str,
    project: &str,
    query: &str,
    include_superseded: bool,
) -> Vec<String> {
    let resp = call_tool(
        server,
        "memory.search",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "query": query,
            "k": 10,
            "include_superseded": include_superseded,
            "include_history": include_superseded
        }),
    )
    .await;
    parse_result_text(&resp)["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["chunk_id"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn dream_dry_run_reports_duplicate_projection_reclaim_without_mutating() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_dry_run_t";
    let project = "dream_dry_run_p";
    let text = "duplicate digest sentinel dry run";
    let first = add_digest_projection_on(&server, tenant, project, text).await;
    let second = add_digest_projection_on(&server, tenant, project, text).await;

    let before = search_ids_on(&server, tenant, project, "sentinel", false).await;
    assert_eq!(before.len(), 2);

    let report = dream_on(&server, tenant, project, true).await;
    assert_eq!(report["status"], "dry_run");
    assert_eq!(report["planned_actions"].as_array().unwrap().len(), 1);
    assert_eq!(report["applied_actions"].as_array().unwrap().len(), 0);
    assert!(
        report["reclaimed"]["estimated_hidden_payload_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );

    let after = search_ids_on(&server, tenant, project, "sentinel", false).await;
    assert_eq!(after.len(), 2);
    assert!(after.contains(&first));
    assert!(after.contains(&second));
}

#[tokio::test]
async fn dream_apply_retires_duplicate_digest_projections() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_apply_t";
    let project = "dream_apply_p";
    let text = "duplicate digest sentinel apply";
    let old = add_digest_projection_on(&server, tenant, project, text).await;
    let survivor = add_digest_projection_on(&server, tenant, project, text).await;

    let report = dream_on(&server, tenant, project, false).await;
    assert_eq!(report["status"], "completed");
    assert_eq!(report["applied_actions"].as_array().unwrap().len(), 1);

    let visible = search_ids_on(&server, tenant, project, "sentinel", false).await;
    assert_eq!(visible, vec![survivor.clone()]);

    let all = search_ids_on(&server, tenant, project, "sentinel", true).await;
    assert_eq!(all.len(), 2);
    assert!(all.contains(&old));
    assert!(all.contains(&survivor));
}

#[tokio::test]
async fn dream_skips_unreadable_projection_payloads() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let tenant = "dream_unreadable_t";
    let project = "dream_unreadable_p";
    let text = "duplicate digest unreadable segment sentinel";
    let first_chunk_id: String;

    {
        let store = persistent_store(tmp.path()).await;
        let cfg = memd::config::Config {
            data_dir: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let server = std::sync::Arc::new(TestServer::new(cfg, store));
        first_chunk_id = add_digest_projection_on(&server, tenant, project, text).await;
        add_digest_projection_on(&server, tenant, project, text).await;
    }

    // Reopen once so WAL recovery finalizes the active segment and truncates
    // the WAL. Corrupting after this point mirrors a live store where metadata
    // points at a segment whose reader cannot be opened.
    let recovered_segment_id = {
        let store = persistent_store(tmp.path()).await;
        let cfg = memd::config::Config {
            data_dir: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let _server = std::sync::Arc::new(TestServer::new(cfg, store.clone()));
        let tenant_id = common::tenant(tenant);
        let chunk_id = memd::types::ChunkId::parse(&first_chunk_id).unwrap();
        store
            .metadata()
            .get(&tenant_id, &chunk_id)
            .unwrap()
            .unwrap()
            .segment_id
    };

    let segment_dir = tmp
        .path()
        .join("tenants")
        .join(tenant)
        .join("segments")
        .join(format!("seg_{recovered_segment_id:06}"));
    let _ = std::fs::remove_file(segment_dir.join("meta"));
    let _ = std::fs::remove_file(segment_dir.join("payload.idx"));

    let store = persistent_store(tmp.path()).await;
    let cfg = memd::config::Config {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let server = std::sync::Arc::new(TestServer::new(cfg, store));
    let report = dream_on(&server, tenant, project, true).await;

    assert_eq!(report["status"], "dry_run");
    assert_eq!(report["planned_actions"].as_array().unwrap().len(), 0);
    assert_eq!(report["applied_actions"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn dream_apply_creates_traceable_takeaway_report() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_report_t";
    let project = "dream_report_p";
    let start = call_tool(
        &server,
        "task.start",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "goal": "seed canonical evidence for dream report",
            "agent_id": "tester"
        }),
    )
    .await;
    let task_id = parse_result_text(&start)["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    let evidence = call_tool(
        &server,
        "task.add_evidence",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "task_id": task_id,
            "summary": "canonical evidence should remain traceable",
            "evidence_kind": "contract",
            "supports_claim": true,
            "agent_id": "tester"
        }),
    )
    .await;
    let evidence_id = parse_result_text(&evidence)["artifact_id"]
        .as_str()
        .unwrap()
        .to_string();

    let text = "duplicate digest sentinel report";
    add_digest_projection_on(&server, tenant, project, text).await;
    add_digest_projection_on(&server, tenant, project, text).await;
    let report = dream_on(&server, tenant, project, false).await;
    let report_ids = report["summary_artifacts"].as_array().unwrap();
    assert!(
        report_ids
            .iter()
            .any(|id| id.as_str().unwrap().contains("dream_report")),
        "dream_report artifact id must be returned: {report_ids:?}"
    );

    let search = call_tool(
        &server,
        "artifact.search",
        json!({
            "tenant_id": tenant,
            "query": "Dream maintenance report",
            "filters": {
                "project_id": project,
                "artifact_kind": "digest",
                "artifact_role": "dream_report"
            },
            "k": 5
        }),
    )
    .await;
    let payload = parse_result_text(&search);
    let result = &payload["results"].as_array().unwrap()[0];
    assert_eq!(result["trust_tier"], "compiled_digest_hint");
    let related = result["artifact"]["related_artifact_ids"]
        .as_array()
        .unwrap();
    assert!(
        related.iter().any(|id| id.as_str() == Some(&evidence_id)),
        "dream_report should link to source evidence {evidence_id}; got {related:?}"
    );
}

#[tokio::test]
async fn dream_apply_is_idempotent() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_idempotent_t";
    let project = "dream_idempotent_p";
    let text = "duplicate digest sentinel idempotent";
    add_digest_projection_on(&server, tenant, project, text).await;
    add_digest_projection_on(&server, tenant, project, text).await;

    let first = dream_on(&server, tenant, project, false).await;
    assert_eq!(first["applied_actions"].as_array().unwrap().len(), 1);
    let second = dream_on(&server, tenant, project, false).await;
    assert_eq!(second["planned_actions"].as_array().unwrap().len(), 0);
    assert_eq!(second["applied_actions"].as_array().unwrap().len(), 0);
    assert!(second["summary_artifacts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn dream_does_not_retire_canonical_evidence_by_default() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_canonical_t";
    let project = "dream_canonical_p";
    let start = call_tool(
        &server,
        "task.start",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "goal": "protect canonical evidence",
            "agent_id": "tester"
        }),
    )
    .await;
    let task_id = parse_result_text(&start)["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    for _ in 0..2 {
        call_tool(
            &server,
            "task.add_evidence",
            json!({
                "tenant_id": tenant,
                "project_id": project,
                "task_id": task_id.clone(),
                "summary": "same canonical evidence summary",
                "evidence_kind": "contract",
                "supports_claim": true,
                "agent_id": "tester"
            }),
        )
        .await;
    }
    let text = "duplicate digest sentinel canonical";
    add_digest_projection_on(&server, tenant, project, text).await;
    add_digest_projection_on(&server, tenant, project, text).await;

    let report = dream_on(&server, tenant, project, false).await;
    assert_eq!(report["applied_actions"].as_array().unwrap().len(), 1);

    let evidence_search = call_tool(
        &server,
        "artifact.search",
        json!({
            "tenant_id": tenant,
            "query": "same canonical evidence summary",
            "filters": {"project_id": project, "artifact_kind": "evidence"},
            "k": 10
        }),
    )
    .await;
    let evidence_results = parse_result_text(&evidence_search)["results"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(evidence_results, 2);
}

#[tokio::test]
async fn dream_respects_project_scope() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_scope_t";
    let text = "duplicate digest sentinel scope";
    add_digest_projection_on(&server, tenant, "project_a", text).await;
    add_digest_projection_on(&server, tenant, "project_a", text).await;
    add_digest_projection_on(&server, tenant, "project_b", text).await;
    add_digest_projection_on(&server, tenant, "project_b", text).await;

    let report = dream_on(&server, tenant, "project_a", false).await;
    assert_eq!(report["applied_actions"].as_array().unwrap().len(), 1);

    let a_visible = search_ids_on(&server, tenant, "project_a", "scope", false).await;
    let b_visible = search_ids_on(&server, tenant, "project_b", "scope", false).await;
    assert_eq!(a_visible.len(), 1);
    assert_eq!(b_visible.len(), 2);
}

#[tokio::test]
async fn dream_physical_compaction_prunes_sparse_index_after_retirement() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = std::sync::Arc::new(
        memd::store::persistent::PersistentStore::open(
            memd::store::persistent::PersistentStoreConfig {
                data_dir: tmp.path().to_path_buf(),
                enable_dense_search: false,
                enable_hybrid_search: true,
                enable_tiered_search: false,
                backfill_hnsw_on_startup: false,
                backfill_canonical_text_on_startup: false,
                ..Default::default()
            },
        )
        .expect("persistent store"),
    );
    let cfg = memd::config::Config {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let server = std::sync::Arc::new(TestServer::new(cfg, store));
    let tenant = "dream_sparse_t";
    let project = "dream_sparse_p";
    let text = "duplicate digest sparseprune sentinel";
    let first = add_digest_projection_on(&server, tenant, project, text).await;
    let second = add_digest_projection_on(&server, tenant, project, text).await;

    let tenant_id = common::tenant(tenant);
    let sparse = server
        .store()
        .sparse_index()
        .expect("test persistent store has sparse index");
    for chunk_id in [&first, &second] {
        sparse
            .insert(
                &tenant_id,
                &memd::types::ChunkId::parse(chunk_id).unwrap(),
                &[text.to_string()],
            )
            .unwrap();
    }
    let before_ids = sparse
        .search(&tenant_id, "sparseprune", 10)
        .unwrap()
        .into_iter()
        .map(|hit| hit.chunk_id.to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(before_ids.len(), 2);

    let report = dream_with_physical_on(
        &server,
        tenant,
        project,
        json!({
            "run_store_compaction": false,
            "prune_sparse_index": true
        }),
    )
    .await;
    assert_eq!(report["physical"]["sparse_pruned_chunks"], 1);
    let retired = report["applied_actions"][0]["chunk_id"].as_str().unwrap();

    let after_ids = sparse
        .search(&tenant_id, "sparseprune", 10)
        .unwrap()
        .into_iter()
        .map(|hit| hit.chunk_id.to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(after_ids.len(), 1);
    assert!(!after_ids.contains(retired));
}

#[tokio::test]
async fn dream_metadata_vacuum_reports_no_metadata_reclaim_without_purge() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_vacuum_t";
    let project = "dream_vacuum_p";
    let text = "duplicate digest vacuum sentinel";
    add_digest_projection_on(&server, tenant, project, text).await;
    add_digest_projection_on(&server, tenant, project, text).await;

    let report = dream_with_physical_on(
        &server,
        tenant,
        project,
        json!({
            "run_store_compaction": false,
            "prune_sparse_index": false,
            "vacuum_metadata": true
        }),
    )
    .await;

    assert_eq!(report["physical"]["metadata_vacuum_ran"], true);
    assert_eq!(report["reclaimed"]["metadata_bytes"].as_u64().unwrap(), 0);
    assert!(
        report["reclaimed"]["estimated_hidden_payload_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning
            .as_str()
            .unwrap()
            .contains("did not reclaim append-only segment bytes")));
}

#[tokio::test]
async fn dream_segment_rewrite_is_blocked_until_supported() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_rewrite_t";
    let project = "dream_rewrite_p";
    let text = "duplicate digest sentinel rewrite";
    let first = add_digest_projection_on(&server, tenant, project, text).await;
    let second = add_digest_projection_on(&server, tenant, project, text).await;

    let resp = call_tool(
        &server,
        "memory.dream",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "dry_run": false,
            "physical": {
                "rewrite_segments": true,
                "run_store_compaction": false,
                "prune_sparse_index": false
            }
        }),
    )
    .await;
    let report = parse_result_text(&resp);
    assert_eq!(report["status"], "blocked");
    assert_eq!(report["applied_actions"].as_array().unwrap().len(), 0);
    assert!(report["planned_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "rewrite_segments_unsupported"));
    for chunk_id in [first, second] {
        let get = call_tool(
            &server,
            "memory.get",
            json!({"tenant_id": tenant, "chunk_id": chunk_id}),
        )
        .await;
        let payload = parse_result_text(&get);
        assert_eq!(payload["found"], true);
        assert!(payload.get("hidden").is_none());
    }
}

#[tokio::test]
async fn dream_report_improves_health_metrics() {
    let (server, _tmp) = test_server().await;
    let tenant = "dream_health_t";
    let project = "dream_health_p";
    let text = "duplicate digest sentinel health";
    add_digest_projection_on(&server, tenant, project, text).await;
    add_digest_projection_on(&server, tenant, project, text).await;

    let before = call_tool(
        &server,
        "memory.health",
        json!({"tenant_id": tenant, "project_id": project}),
    )
    .await;
    let before_health = parse_result_text(&before);
    assert!(
        before_health["duplicates"]["duplicate_row_ratio"]
            .as_f64()
            .unwrap()
            > 0.0
    );

    let report = dream_on(&server, tenant, project, false).await;
    let before_ratio = report["before"]["health"]["duplicates"]["duplicate_row_ratio"]
        .as_f64()
        .unwrap();
    let after_ratio = report["after"]["health"]["duplicates"]["duplicate_row_ratio"]
        .as_f64()
        .unwrap();
    let before_count = report["before"]["health"]["duplicates"]["duplicate_row_count"]
        .as_u64()
        .unwrap();
    let after_count = report["after"]["health"]["duplicates"]["duplicate_row_count"]
        .as_u64()
        .unwrap();
    let hidden_payload = report["reclaimed"]["estimated_hidden_payload_bytes"]
        .as_u64()
        .unwrap();
    println!(
        "dream smoke metrics: duplicate_row_count {} -> {}; duplicate_row_ratio {:.4} -> {:.4}; estimated_hidden_payload_bytes {}; superseded_chunks {}; history_chunks {}",
        before_count,
        after_count,
        before_ratio,
        after_ratio,
        hidden_payload,
        report["after"]["health"]["counts"]["superseded_chunks"]
            .as_u64()
            .unwrap(),
        report["after"]["health"]["counts"]["history_chunks"]
            .as_u64()
            .unwrap()
    );
    assert!(before_ratio > after_ratio);
    assert_eq!(after_count, 0);
    assert!(hidden_payload > 0);
    assert_eq!(report["after"]["health"]["counts"]["superseded_chunks"], 1);
    assert_eq!(report["after"]["health"]["counts"]["history_chunks"], 1);
}
