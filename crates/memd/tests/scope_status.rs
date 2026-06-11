//! In-band scope_status reporting: wrong-scope and degraded retrieval
//! must be distinguishable from "no memory exists" in every
//! memory.search payload, and a write that creates a brand-new tenant
//! must say so.

use memd::mcp::{handle_memory_add, handle_memory_search, AddParams, SearchParams};
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::{ChunkType, MemoryChunk, ProjectId, TenantId};
use serde_json::Value;
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

fn payload(response: &Value) -> Value {
    let text = response
        .get("content")
        .and_then(|c| c.get(0))
        .and_then(|i| i.get("text"))
        .and_then(Value::as_str)
        .expect("response content[0].text");
    serde_json::from_str(text).expect("result text is JSON")
}

async fn seed_alpha(store: &PersistentStore, tenant: &TenantId) {
    for i in 0..3 {
        store
            .add(
                MemoryChunk::new(
                    tenant.clone(),
                    format!("decision {i}: the ingestion service uses postgres"),
                    ChunkType::Summary,
                )
                .with_project(ProjectId::from("alpha")),
            )
            .await
            .unwrap();
    }
}

async fn seed_beta_sibling(store: &PersistentStore, tenant: &TenantId) {
    store
        .add(
            MemoryChunk::new(
                tenant.clone(),
                "beta decision: the ingestion service uses postgres on the sibling project too",
                ChunkType::Summary,
            )
            .with_project(ProjectId::from("beta")),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn project_scoped_miss_reports_wider_scope_hits() {
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t1").unwrap();
    seed_alpha(&store, &tenant).await;

    // Same tenant, different project: the scoped search finds nothing,
    // but the payload must say the hits exist one flag away.
    let response = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "t1".to_string(),
            query: "ingestion service uses postgres".to_string(),
            project_id: Some("beta".to_string()),
            k: 5,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let parsed = payload(&response);

    assert_eq!(parsed["results"].as_array().unwrap().len(), 0);
    let status = &parsed["scope_status"];
    assert_eq!(status["tenant_id"], "t1");
    assert_eq!(status["project_id"], "beta");
    let wider = status["wider_scope_hits"].as_u64().unwrap();
    assert!(wider >= 1, "expected wider_scope_hits >= 1, got {wider}");
    let hint = status["widen_hint"].as_str().unwrap();
    assert!(
        hint.contains("--project-id"),
        "widen_hint must name the flag to drop: {hint}"
    );
}

#[tokio::test]
async fn budget_trimmed_results_do_not_emit_false_widen_hint() {
    // Regression: wider_scope_hits must key on the pre-budget retrieval
    // count, not the post-budget result count. A project-scoped search
    // that genuinely returns >= k in-project hits but gets token-budget
    // trimmed to < k must NOT advise widening, even when sibling-project
    // content exists.
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t1").unwrap();

    // Plenty of in-project matches (>= k) plus sibling-project content
    // that would trigger the widen hint if the probe misfired.
    for i in 0..6 {
        store
            .add(
                MemoryChunk::new(
                    tenant.clone(),
                    format!("alpha note {i}: the ingestion service uses postgres for durable storage and the retry queue"),
                    ChunkType::Summary,
                )
                .with_project(ProjectId::from("alpha")),
            )
            .await
            .unwrap();
    }
    seed_beta_sibling(&store, &tenant).await;

    // Small token budget over k=3 forces the packer to drop rows, so the
    // post-budget result count falls below k even though the search found
    // >= k in-project hits.
    let response = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "t1".to_string(),
            query: "ingestion service uses postgres".to_string(),
            project_id: Some("alpha".to_string()),
            k: 3,
            compact: true,
            token_budget: Some(40),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let parsed = payload(&response);

    let trimmed = parsed["results"].as_array().unwrap().len();
    assert!(
        trimmed < 3,
        "test precondition: token budget should trim below k, got {trimmed}"
    );
    let status = &parsed["scope_status"];
    assert!(
        status["widen_hint"].is_null(),
        "budget trimming must not trigger a widen hint: {status}"
    );
    assert!(status["wider_scope_hits"].is_null());
}

#[tokio::test]
async fn project_scoped_hit_has_no_widen_hint() {
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t1").unwrap();
    seed_alpha(&store, &tenant).await;

    let response = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "t1".to_string(),
            query: "ingestion service uses postgres".to_string(),
            project_id: Some("alpha".to_string()),
            k: 3,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let parsed = payload(&response);

    assert_eq!(parsed["results"].as_array().unwrap().len(), 3);
    let status = &parsed["scope_status"];
    assert!(status["widen_hint"].is_null());
    assert!(status["wider_scope_hits"].is_null());
}

#[tokio::test]
async fn unknown_tenant_search_warns_instead_of_silent_empty() {
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t1").unwrap();
    seed_alpha(&store, &tenant).await;

    let response = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "t9".to_string(),
            query: "ingestion service uses postgres".to_string(),
            k: 5,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let parsed = payload(&response);

    assert_eq!(parsed["results"].as_array().unwrap().len(), 0);
    let warnings = parsed["scope_status"]["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("'t9'")),
        "expected a tenant-existence warning naming t9: {warnings:?}"
    );
}

#[tokio::test]
async fn degraded_retrieval_reports_text_fallback() {
    // Dense + hybrid disabled: queries degrade to substring matching,
    // and the payload must say so instead of hiding it on stderr.
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t1").unwrap();
    seed_alpha(&store, &tenant).await;

    let response = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "t1".to_string(),
            query: "ingestion service uses postgres".to_string(),
            k: 3,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let parsed = payload(&response);

    let status = &parsed["scope_status"];
    assert_eq!(status["retrieval_mode"], "text_fallback");
    let warnings = status["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap_or_default().contains("substring")));
}

#[tokio::test]
async fn first_write_to_new_tenant_is_reported() {
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));

    let first = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "fresh_tenant".to_string(),
            text: "first durable note for a brand-new tenant".to_string(),
            chunk_type: "summary".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(payload(&first)["created_tenant"], true);

    let second = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "fresh_tenant".to_string(),
            text: "second durable note for the same tenant".to_string(),
            chunk_type: "summary".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(payload(&second).get("created_tenant").is_none());
}
