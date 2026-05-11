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
    let server = std::sync::Arc::new(TestServer::new(server_cfg, store.clone()));

    let t = tenant("t");
    let visible = store
        .add(MemoryChunk::new(
            t.clone(),
            "visible payload",
            ChunkType::Doc,
        ))
        .await
        .unwrap();
    let buried = store
        .add(MemoryChunk::new(
            t.clone(),
            "buried payload",
            ChunkType::Doc,
        ))
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
    // When the caller opts into every hide reason `resolve_visibility_
    // and_oversample` sets oversample_factor=1 (no pre-visibility
    // oversample). The filter itself still runs — it has to, to catch
    // Deleted/Error rows and delete-race drops — but with oversample=1
    // the handler does not multiply fetch_k. We can't directly assert
    // internal fetch_k from the handler, so we check functional behavior
    // instead: with all three include_* flags true and k large, every
    // seeded chunk (superseded included) appears in the results.
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
    let server = std::sync::Arc::new(TestServer::new(server_cfg, store.clone()));

    let t = tenant("t");
    let _visible = store
        .add(MemoryChunk::new(
            t.clone(),
            "visible payload",
            ChunkType::Doc,
        ))
        .await
        .unwrap();
    let expired = store
        .add(MemoryChunk::new(
            t.clone(),
            "expiring payload",
            ChunkType::Doc,
        ))
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

#[tokio::test]
async fn list_lifecycle_hidden_returns_superseded_expired_and_history() {
    // B2 correctness anchor: the `MetadataStore::list_lifecycle_hidden`
    // helper must surface every row the visibility policy hides by
    // lifecycle state (status=Superseded/Expired OR tier=History). The
    // compaction runner unions this list into the HNSW-rebuild excluded
    // set so the rebuilt index no longer carries weight for rows
    // already invisible to callers.
    //
    // We exercise the metadata helper directly rather than the full
    // compaction path — the runner's HNSW rebuild path requires a
    // populated DenseSearcher, which test_server() deliberately disables
    // for fast iteration. The wiring under test (runner.rs calling
    // list_lifecycle_hidden and unioning it with get_deleted_chunk_ids)
    // is a trivial set union; the risk is that the metadata helper
    // misses a hide category. Assert that once over the three known
    // categories is enough to pin the contract.
    use memd::store::metadata::MetadataStore;
    use memd::types::lifecycle::{LifecycleDelta, MemoryTier};

    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    // Seed 5 chunks; we'll mutate 3 of them into different hidden states
    // and leave 2 alone (visible).
    let mut ids = Vec::new();
    for _ in 0..5 {
        let id = store
            .add(MemoryChunk::new(t.clone(), "payload", ChunkType::Doc))
            .await
            .unwrap();
        ids.push(id);
    }

    // (a) status = Superseded via atomic supersede.
    let _ = store
        .supersede_chunk(
            &t,
            &ids[0],
            MemoryChunk::new(t.clone(), "v2", ChunkType::Doc),
        )
        .await
        .unwrap();
    // (b) status = Expired via direct overlay write — mimics what
    // ExpirySweep (Track C3) will do when it lands.
    store
        .update_lifecycle(
            &t,
            &ids[1],
            &LifecycleDelta {
                status: Some(ChunkStatus::Expired),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // (c) tier = History via direct overlay write.
    store
        .update_lifecycle(
            &t,
            &ids[2],
            &LifecycleDelta {
                tier: Some(MemoryTier::History),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Compaction runner uses this helper — the contract under test is
    // "every lifecycle-hidden category shows up". The three mutated ids
    // must all be present; the two untouched chunks must not be.
    let hidden = store.metadata().list_lifecycle_hidden(&t).unwrap();
    let hidden_set: std::collections::HashSet<_> = hidden.iter().cloned().collect();
    assert!(
        hidden_set.contains(&ids[0]),
        "superseded chunk {} missing from list_lifecycle_hidden: {hidden_set:?}",
        ids[0]
    );
    assert!(
        hidden_set.contains(&ids[1]),
        "expired-status chunk {} missing from list_lifecycle_hidden: {hidden_set:?}",
        ids[1]
    );
    assert!(
        hidden_set.contains(&ids[2]),
        "history-tier chunk {} missing from list_lifecycle_hidden: {hidden_set:?}",
        ids[2]
    );
    assert!(
        !hidden_set.contains(&ids[3]),
        "untouched visible chunk {} leaked into list_lifecycle_hidden: {hidden_set:?}",
        ids[3]
    );
    assert!(
        !hidden_set.contains(&ids[4]),
        "untouched visible chunk {} leaked into list_lifecycle_hidden: {hidden_set:?}",
        ids[4]
    );
}

// Suppress unused-import warnings for items only used in cross-test helpers.
#[allow(dead_code)]
fn _unused_marker(_: ChunkId, _: ChunkStatus) {}
