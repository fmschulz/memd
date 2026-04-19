//! Integration tests for the B1 visibility filter on memory.search.
//!
//! The fast path here is the text-search fallback (dense + hybrid are
//! disabled in `common::persistent_store`), which is deterministic enough
//! to assert "this chunk is / is not in the result set" without having to
//! pin a specific rank ordering.

#[path = "common/mod.rs"]
mod common;
use common::*;

use memd::store::Store;
use memd::types::{ChunkId, ChunkStatus, ChunkType, MemoryChunk};
use serde_json::json;

fn hit_ids(resp: &serde_json::Value) -> Vec<String> {
    parse_result_text(resp)["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("chunk_id").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn search_hides_superseded_by_default_and_refills_to_k() {
    // Add 12 chunks that all match the query "payload", then supersede the
    // first 3 we added. With `k=10` and default visibility (hide
    // superseded), the result set must contain 10 hits and none of the
    // superseded ids — oversample-and-refill keeps the page full.
    let (server, _tmp) = test_server().await;

    let mut ids = Vec::new();
    for i in 0..12 {
        let resp = call_tool(
            &server,
            "memory.add",
            json!({
                "tenant_id": "t",
                "text": format!("payload row {}", i),
                "type": "doc"
            }),
        )
        .await;
        ids.push(
            parse_result_text(&resp)["chunk_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }

    let superseded: Vec<String> = ids.iter().take(3).cloned().collect();
    for old in &superseded {
        // Replacement text must also contain "payload" so it matches the
        // same BM25 query as the originals — that is what makes the
        // oversample-and-refill visible: 9 originals + 3 replacements =
        // 12 visible hits for `query=payload`, which refills to k=10.
        let _ = call_tool(
            &server,
            "memory.supersede",
            json!({
                "tenant_id": "t",
                "old_chunk_id": old,
                "new_text": format!("payload replacement for {}", old),
                "type": "doc"
            }),
        )
        .await;
    }

    let resp = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": "t",
            "query": "payload",
            "k": 10
        }),
    )
    .await;
    let hits = hit_ids(&resp);
    for s in &superseded {
        assert!(
            !hits.contains(s),
            "default search must hide superseded chunk {s}: hits={hits:?}"
        );
    }
    // Oversample-and-refill: we seeded enough visible chunks (the 9
    // un-superseded originals + 3 replacements = 12 visible rows) to
    // return a full page of 10.
    assert!(
        hits.len() == 10,
        "oversample-and-refill must preserve k=10: got {}: {:?}",
        hits.len(),
        hits
    );
}

#[tokio::test]
async fn search_returns_superseded_when_include_flag_set() {
    // Same seed, but call with `include_superseded=true` — at least one
    // superseded id must appear in the result set.
    let (server, _tmp) = test_server().await;

    let mut ids = Vec::new();
    for i in 0..6 {
        let resp = call_tool(
            &server,
            "memory.add",
            json!({
                "tenant_id": "t",
                "text": format!("payload row {}", i),
                "type": "doc"
            }),
        )
        .await;
        ids.push(
            parse_result_text(&resp)["chunk_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let superseded_id = ids[0].clone();
    let _ = call_tool(
        &server,
        "memory.supersede",
        json!({
            "tenant_id": "t",
            "old_chunk_id": superseded_id.clone(),
            "new_text": "replacement",
            "type": "doc"
        }),
    )
    .await;

    let resp = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": "t",
            "query": "payload",
            "k": 20,
            "include_superseded": true
        }),
    )
    .await;
    let hits = hit_ids(&resp);
    assert!(
        hits.contains(&superseded_id),
        "include_superseded=true must surface {superseded_id}: hits={hits:?}"
    );
}

#[tokio::test]
async fn search_hides_history_tier_by_default() {
    // Direct store access is used to set the History tier on a chunk
    // without going through a future Track C promotion path. The search
    // handler must hide it unless include_history is set.
    use memd::store::persistent::PersistentStoreConfig;
    use memd::types::lifecycle::{LifecycleDelta, MemoryTier};

    let tmp = tempfile::tempdir().unwrap();
    let cfg = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = std::sync::Arc::new(memd::store::persistent::PersistentStore::open(cfg).unwrap());
    let server_cfg = memd::config::Config {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let server = std::sync::Arc::new(memd::mcp::server::McpServer::new(
        server_cfg,
        store.clone(),
    ));

    let t = tenant("t");
    let visible = store
        .add(MemoryChunk::new(t.clone(), "visible payload", ChunkType::Doc))
        .await
        .unwrap();
    let buried = store
        .add(MemoryChunk::new(t.clone(), "buried payload", ChunkType::Doc))
        .await
        .unwrap();
    store
        .update_lifecycle(
            &t,
            &buried,
            &LifecycleDelta {
                tier: Some(MemoryTier::History),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Default search — buried must be hidden.
    let resp = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": "t",
            "query": "payload",
            "k": 20
        }),
    )
    .await;
    let hits = hit_ids(&resp);
    assert!(
        hits.contains(&visible.to_string()),
        "visible chunk must appear in default search: {hits:?}"
    );
    assert!(
        !hits.contains(&buried.to_string()),
        "history-tier chunk must be hidden by default: {hits:?}"
    );

    // include_history=true — buried reappears.
    let resp = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": "t",
            "query": "payload",
            "k": 20,
            "include_history": true
        }),
    )
    .await;
    let hits = hit_ids(&resp);
    assert!(
        hits.contains(&buried.to_string()),
        "include_history=true must surface history-tier chunk: {hits:?}"
    );
}

#[tokio::test]
async fn search_all_permissive_flags_disable_oversample() {
    // When the caller opts into every hide reason the filter is a no-op
    // and must not multiply fetch_k. We can't directly assert the
    // internal fetch_k count from the handler, but we can assert
    // functional behavior: with all three include_* flags true and k
    // large, every chunk (superseded or not) appears.
    let (server, _tmp) = test_server().await;

    let mut ids = Vec::new();
    for i in 0..5 {
        let resp = call_tool(
            &server,
            "memory.add",
            json!({
                "tenant_id": "t",
                "text": format!("payload row {}", i),
                "type": "doc"
            }),
        )
        .await;
        ids.push(
            parse_result_text(&resp)["chunk_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let _ = call_tool(
        &server,
        "memory.supersede",
        json!({
            "tenant_id": "t",
            "old_chunk_id": ids[0].clone(),
            "new_text": "replacement",
            "type": "doc"
        }),
    )
    .await;

    let resp = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": "t",
            "query": "payload",
            "k": 20,
            "include_superseded": true,
            "include_expired": true,
            "include_history": true
        }),
    )
    .await;
    let hits = hit_ids(&resp);
    // Every seeded id plus the replacement (6 visible total) must be in hits.
    assert!(
        hits.contains(&ids[0]),
        "all-permissive must surface superseded {}: {hits:?}",
        ids[0]
    );
    for id in &ids[1..] {
        assert!(
            hits.contains(id),
            "all-permissive must surface live {id}: {hits:?}"
        );
    }
}

#[tokio::test]
async fn search_hides_chunk_expired_by_wall_clock() {
    // A chunk with `expires_at_ms <= now` but still `status=Final` must
    // be hidden by `VisibilityPolicy::is_visible_at`'s wall-clock arm.
    use memd::store::persistent::PersistentStoreConfig;
    use memd::types::lifecycle::LifecycleDelta;

    let tmp = tempfile::tempdir().unwrap();
    let cfg = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = std::sync::Arc::new(memd::store::persistent::PersistentStore::open(cfg).unwrap());
    let server_cfg = memd::config::Config {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let server = std::sync::Arc::new(memd::mcp::server::McpServer::new(
        server_cfg,
        store.clone(),
    ));

    let t = tenant("t");
    let _visible = store
        .add(MemoryChunk::new(t.clone(), "visible payload", ChunkType::Doc))
        .await
        .unwrap();
    let expired = store
        .add(MemoryChunk::new(t.clone(), "expiring payload", ChunkType::Doc))
        .await
        .unwrap();
    // Set expires_at_ms to the epoch so it is unambiguously in the past.
    store
        .update_lifecycle(
            &t,
            &expired,
            &LifecycleDelta {
                expires_at_ms: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Default search — expired is hidden.
    let resp = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": "t",
            "query": "payload",
            "k": 20
        }),
    )
    .await;
    let hits = hit_ids(&resp);
    assert!(
        !hits.contains(&expired.to_string()),
        "wall-clock-expired chunk must be hidden by default: {hits:?}"
    );

    // include_expired=true — expired reappears.
    let resp = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": "t",
            "query": "payload",
            "k": 20,
            "include_expired": true
        }),
    )
    .await;
    let hits = hit_ids(&resp);
    assert!(
        hits.contains(&expired.to_string()),
        "include_expired=true must surface expired chunk: {hits:?}"
    );
}

// Suppress unused-import warnings for items only used in cross-test helpers.
#[allow(dead_code)]
fn _unused_marker(_: ChunkId, _: ChunkStatus) {}
