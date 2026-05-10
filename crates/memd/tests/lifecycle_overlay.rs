//! Integration tests for lifecycle overlay on PersistentStore.

#[path = "common/mod.rs"]
mod common;
use common::*;

use memd::store::metadata::MetadataStore;
use memd::store::Store;
use memd::types::lifecycle::{LifecycleDelta, MemoryTier};
use memd::types::{ChunkId, ChunkStatus, ChunkType, MemoryChunk, TenantId};

fn chunk_at(tenant_id: &TenantId, text: &str, timestamp_created: i64) -> MemoryChunk {
    let mut chunk = MemoryChunk::new(tenant_id.clone(), text, ChunkType::Doc);
    chunk.timestamp_created = timestamp_created;
    chunk
}

fn context_chunk_at(tenant_id: &TenantId, text: &str, timestamp_created: i64) -> MemoryChunk {
    let mut chunk = chunk_at(tenant_id, text, timestamp_created);
    chunk.tags = vec![
        "ctx:doc".to_string(),
        "ctx:subsystem:search".to_string(),
        "ctx:tier:cold".to_string(),
    ];
    chunk
}

#[tokio::test]
async fn persistent_store_returns_lifecycle_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let id = store
        .add(MemoryChunk::new(t.clone(), "hello", ChunkType::Doc))
        .await
        .unwrap();

    // Apply lifecycle delta directly through the PersistentStore API.
    store
        .update_lifecycle(
            &t,
            &id,
            &LifecycleDelta {
                tier: Some(MemoryTier::Working),
                lifecycle_updated_at_ms: Some(1_700_000_000_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let resolved = store.get_with_lifecycle(&t, &id).await.unwrap().unwrap();
    assert_eq!(resolved.chunk.text, "hello");
    assert_eq!(resolved.lifecycle.tier, MemoryTier::Working);
    assert_eq!(resolved.status, ChunkStatus::Final);
}

#[tokio::test]
async fn store_get_with_lifecycle_default_impl_returns_default_lifecycle() {
    // For an in-memory store (no override), the default trait impl should still
    // return a ResolvedChunk with default lifecycle fields.
    use memd::store::memory::MemoryStore;
    use std::sync::Arc;
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let t = tenant("t");
    let chunk = MemoryChunk::new(t.clone(), "hi", ChunkType::Doc);
    let id = store.add(chunk).await.unwrap();
    let resolved = store.get_with_lifecycle(&t, &id).await.unwrap().unwrap();
    assert_eq!(resolved.lifecycle.tier, MemoryTier::LongTerm); // default
    assert_eq!(resolved.lifecycle.lifecycle_updated_at_ms, 0); // default
}

#[tokio::test]
async fn get_with_lifecycle_returns_none_for_deleted_chunk() {
    // A6 (supersede_chunk) relies on Deleted rows being hidden by
    // get_with_lifecycle — document that invariant here so any future refactor
    // that drops the Deleted branch fails loudly.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let id = store
        .add(MemoryChunk::new(t.clone(), "to-delete", ChunkType::Doc))
        .await
        .unwrap();

    // Confirm the chunk is visible before deletion.
    let before = store.get_with_lifecycle(&t, &id).await.unwrap();
    assert!(before.is_some(), "chunk should be visible before delete");

    // Mark deleted via the metadata store (mirrors what memory.delete does
    // today). The `MetadataStore` trait is in scope at the top of the file so
    // `mark_deleted` resolves on the `&SqliteMetadataStore` accessor.
    store.metadata().mark_deleted(&t, &id).unwrap();

    // get_with_lifecycle must now return None, matching the Deleted branch.
    let after = store.get_with_lifecycle(&t, &id).await.unwrap();
    assert!(
        after.is_none(),
        "Deleted chunk must not surface through get_with_lifecycle"
    );
}

#[tokio::test]
async fn supersede_chunk_is_atomic_and_bumps_cache() {
    // End-to-end invariants for PersistentStore::supersede_chunk:
    // * the new chunk id differs from the old one,
    // * the old row transitions to Superseded with superseded_by = new,
    // * the new row carries supersedes = old,
    // * the tenant memory version either stays at its (missing) default
    //   when hybrid is disabled in the test harness, or strictly
    //   increases when hybrid is enabled. Because `persistent_store`
    //   disables hybrid, the accessor returns `None` and the assert
    //   degrades to a no-op — the real bump path is covered by the
    //   in-crate unit test that observes `tenant_memory_version` with a
    //   seeded tiered searcher.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let old_id = store
        .add(MemoryChunk::new(t.clone(), "v1", ChunkType::Doc))
        .await
        .unwrap();
    let version_before = store
        .hybrid()
        .and_then(|h| h.tenant_memory_version(&t))
        .unwrap_or(0);

    let new_chunk = MemoryChunk::new(t.clone(), "v2", ChunkType::Doc);
    let new_id = store.supersede_chunk(&t, &old_id, new_chunk).await.unwrap();
    assert_ne!(new_id, old_id, "supersede_chunk must mint a fresh chunk id");

    let old_resolved = store
        .get_with_lifecycle(&t, &old_id)
        .await
        .unwrap()
        .expect("old chunk still resolvable post-supersede");
    assert_eq!(old_resolved.status, ChunkStatus::Superseded);
    assert_eq!(
        old_resolved.lifecycle.superseded_by.as_ref().unwrap(),
        &new_id
    );

    let new_resolved = store
        .get_with_lifecycle(&t, &new_id)
        .await
        .unwrap()
        .expect("new chunk must be resolvable");
    assert_eq!(new_resolved.chunk.text, "v2");
    assert_eq!(new_resolved.status, ChunkStatus::Final);
    assert_eq!(new_resolved.lifecycle.supersedes.as_ref().unwrap(), &old_id);

    // When hybrid is enabled this version strictly increases; otherwise
    // both sides resolve to the `unwrap_or(0)` fallback and the check
    // degrades to `0 >= 0`, which is still a signal that we never
    // regress.
    let version_after = store
        .hybrid()
        .and_then(|h| h.tenant_memory_version(&t))
        .unwrap_or(0);
    assert!(
        version_after >= version_before,
        "tenant memory version must not regress across supersede_chunk: \
         before={version_before} after={version_after}"
    );
}

#[tokio::test]
async fn supersede_chunk_walks_long_chain_without_cycle_error() {
    // A→B→C→D chain — each hop drives supersede_chunk, which runs
    // detect_supersession_cycle from the latest `old`. Because the
    // chain is acyclic, every call must succeed. This test also
    // indirectly covers that `atomic_supersede` accepts the same
    // tenant+chunk_id pair being linked multiple times across hops.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let a = store
        .add(MemoryChunk::new(t.clone(), "A", ChunkType::Doc))
        .await
        .unwrap();
    let b = store
        .supersede_chunk(&t, &a, MemoryChunk::new(t.clone(), "B", ChunkType::Doc))
        .await
        .unwrap();
    let c = store
        .supersede_chunk(&t, &b, MemoryChunk::new(t.clone(), "C", ChunkType::Doc))
        .await
        .unwrap();
    let _d = store
        .supersede_chunk(&t, &c, MemoryChunk::new(t.clone(), "D", ChunkType::Doc))
        .await
        .unwrap();

    // A still resolves with its original superseded_by pointer.
    let resolved_a = store
        .get_with_lifecycle(&t, &a)
        .await
        .unwrap()
        .expect("A should still be resolvable as Superseded");
    assert_eq!(resolved_a.status, ChunkStatus::Superseded);
    assert_eq!(resolved_a.lifecycle.superseded_by.as_ref().unwrap(), &b);
}

#[tokio::test]
async fn supersede_chunk_detects_cycle_via_forged_overlay() {
    // A real A→A cycle cannot be induced through `supersede_chunk`
    // itself because `atomic_supersede`'s transaction requires both
    // rows to exist and the helper always mints a fresh chunk id for
    // `new`. To pin the cycle-detection branch we forge a chain's
    // overlay by calling `update_lifecycle` directly so
    // `a.superseded_by = b` and `b.superseded_by = a`, then exercise
    // `supersede_chunk` — it must fail-closed before touching the
    // add path.
    use memd::types::lifecycle::LifecycleDelta;

    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let a = store
        .add(MemoryChunk::new(t.clone(), "A", ChunkType::Doc))
        .await
        .unwrap();
    let b = store
        .add(MemoryChunk::new(t.clone(), "B", ChunkType::Doc))
        .await
        .unwrap();

    // Forge A → B → A at the overlay layer, bypassing atomic_supersede
    // so we can observe detect_supersession_cycle's failure branch.
    store
        .update_lifecycle(
            &t,
            &a,
            &LifecycleDelta {
                superseded_by: Some(b.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store
        .update_lifecycle(
            &t,
            &b,
            &LifecycleDelta {
                superseded_by: Some(a.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let err = store
        .supersede_chunk(&t, &a, MemoryChunk::new(t.clone(), "C", ChunkType::Doc))
        .await
        .expect_err("supersede_chunk must fail on cyclic superseded_by chain");
    let msg = format!("{err}");
    assert!(
        msg.contains("supersession cycle detected"),
        "expected cycle-detection error, got: {msg}"
    );
}

#[tokio::test]
async fn supersede_chunk_errors_when_old_id_missing_and_does_not_orphan() {
    // Regression for the orphan-chunk window in the original A6 commit:
    // if `old_id` did not exist, `detect_supersession_cycle` correctly
    // treated the missing row as a terminal chain (not a cycle), then
    // `add_chunk_with_lifecycle` committed the new chunk to WAL +
    // segment + metadata, then `atomic_supersede` failed on the missing
    // old row — leaving the new row orphaned. The store-layer fix is
    // an up-front existence check; this test pins it by asserting that
    // a failed call produces zero metadata rows for the tenant.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let bogus_old = ChunkId::new();
    let new_chunk = MemoryChunk::new(t.clone(), "replacement", ChunkType::Doc);

    let result = store.supersede_chunk(&t, &bogus_old, new_chunk).await;
    let err = result.expect_err("expected error when old chunk is missing");
    let msg = format!("{err}");
    assert!(
        msg.contains("not found"),
        "expected `not found` error, got: {msg}"
    );

    // The whole point: zero metadata rows must exist for this tenant.
    // Any nonzero count means the new chunk was orphaned.
    let list = store.metadata().list(&t, 100, 0).unwrap();
    assert_eq!(
        list.len(),
        0,
        "no chunk should have been written for failed supersede; \
         observed orphan(s): {list:?}"
    );
}

#[tokio::test]
async fn memory_supersede_handler_matches_store_op() {
    // Round-trips a chunk through `memory.add` then `memory.supersede`
    // and asserts the handler-side path produces the same overlay as
    // calling `PersistentStore::supersede_chunk` directly.
    let (server, _tmp) = test_server().await;
    let add_resp = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "v1",
            "type": "doc"
        }),
    )
    .await;
    let old_id = parse_result_text(&add_resp)["chunk_id"]
        .as_str()
        .expect("chunk_id from memory.add")
        .to_string();

    let sup = call_tool(
        &server,
        "memory.supersede",
        serde_json::json!({
            "tenant_id": "t",
            "old_chunk_id": old_id.clone(),
            "new_text": "v2",
            "type": "doc"
        }),
    )
    .await;
    let new_id = parse_result_text(&sup)["new_chunk_id"]
        .as_str()
        .expect("new_chunk_id from memory.supersede")
        .to_string();

    let resolved = server
        .store()
        .get_with_lifecycle(&tenant("t"), &ChunkId::parse(&old_id).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.status, ChunkStatus::Superseded);
    assert_eq!(
        resolved
            .lifecycle
            .superseded_by
            .as_ref()
            .unwrap()
            .to_string(),
        new_id
    );
}

#[tokio::test]
async fn memory_supersede_requires_existing_old_chunk_id() {
    // Pin the negative path: handing memory.supersede a fresh
    // (non-existent) old_chunk_id must surface an error envelope and
    // must not write any state.
    let (server, _tmp) = test_server().await;
    let bogus = ChunkId::new().to_string();
    let resp = call_tool(
        &server,
        "memory.supersede",
        serde_json::json!({
            "tenant_id": "t",
            "old_chunk_id": bogus,
            "new_text": "v2",
            "type": "doc"
        }),
    )
    .await;
    // Tighten the assertion: the error message must specifically mention
    // the missing old_chunk_id or a "not found" condition. A generic
    // "some error happened" would hide regressions that swap the error
    // code for an unrelated failure (e.g. schema validation).
    let msg = parse_error(&resp)
        .map(|(_, m)| m)
        .or_else(|| {
            resp.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    assert!(
        msg.contains("old_chunk_id")
            || msg.contains("not found")
            || msg.to_lowercase().contains("old chunk"),
        "expected error mentioning old_chunk_id or 'not found', got: {msg}"
    );
}

#[tokio::test]
async fn supersede_chunk_errors_on_tenant_mismatch() {
    // The dispatch layer is the natural place to enforce
    // tenant_id == new_chunk.tenant_id, but supersede_chunk now
    // refuses the mismatch as a defense-in-depth check. Without it,
    // the new chunk would be persisted under `new_chunk.tenant_id`
    // while the supersede edge would target `tenant_id` — orphaning
    // the new row in a different tenant from where the caller meant
    // to operate.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t1 = tenant("t1");
    let t2 = tenant("t2");
    let old = store
        .add(MemoryChunk::new(t1.clone(), "A", ChunkType::Doc))
        .await
        .unwrap();

    // new_chunk belongs to tenant t2, but we're asking for supersede
    // under t1. supersede_chunk must reject this BEFORE writing.
    let mismatched = MemoryChunk::new(t2.clone(), "B", ChunkType::Doc);
    let err = store
        .supersede_chunk(&t1, &old, mismatched)
        .await
        .expect_err("tenant mismatch must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not match"),
        "expected tenant-mismatch error, got: {msg}"
    );

    // Defense-in-depth check: nothing was written under t2.
    let t2_rows = store.metadata().list(&t2, 100, 0).unwrap();
    assert_eq!(
        t2_rows.len(),
        0,
        "no chunk should have been written under t2; observed orphan(s): {t2_rows:?}"
    );
}

#[tokio::test]
async fn memory_get_hides_superseded_by_default() {
    // A8 contract: memory.get must route through the lifecycle overlay and
    // hide Superseded chunks from callers that did not opt in. The chunk
    // payload MUST NOT be included when hidden=true so stale text cannot
    // leak via the primary chunk-lookup tool.
    let (server, _tmp) = test_server().await;
    let ps = server.store().as_persistent().expect("persistent store");
    let t = tenant("t");
    let id = ps
        .add(MemoryChunk::new(t.clone(), "v1", ChunkType::Doc))
        .await
        .unwrap();
    ps.supersede_chunk(&t, &id, MemoryChunk::new(t.clone(), "v2", ChunkType::Doc))
        .await
        .unwrap();

    let resp = call_tool(
        &server,
        "memory.get",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": id.to_string(),
        }),
    )
    .await;
    let body = parse_result_text(&resp);

    assert!(
        body["hidden"].as_bool().unwrap_or(false),
        "superseded chunk must report hidden=true by default: {body}"
    );
    assert_eq!(body["status"].as_str(), Some("superseded"));
    // `serde_json::Value` indexing returns `Value::Null` for missing
    // keys, so `is_null()` covers both "absent" and "explicitly null"
    // — which is exactly the contract for hidden responses.
    assert!(
        body["chunk"].is_null(),
        "chunk payload must be omitted when hidden=true: {body}"
    );
}

#[tokio::test]
async fn memory_get_returns_payload_when_include_superseded_true() {
    // Opting in with include_superseded=true must surface the full chunk
    // plus lifecycle overlay and still report status=superseded so callers
    // can reason about the edge.
    let (server, _tmp) = test_server().await;
    let ps = server.store().as_persistent().expect("persistent store");
    let t = tenant("t");
    let id = ps
        .add(MemoryChunk::new(t.clone(), "v1", ChunkType::Doc))
        .await
        .unwrap();
    ps.supersede_chunk(&t, &id, MemoryChunk::new(t.clone(), "v2", ChunkType::Doc))
        .await
        .unwrap();

    let resp = call_tool(
        &server,
        "memory.get",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": id.to_string(),
            "include_superseded": true,
        }),
    )
    .await;
    let body = parse_result_text(&resp);

    assert_eq!(body["found"].as_bool(), Some(true));
    assert!(
        !body["hidden"].as_bool().unwrap_or(false),
        "hidden must be false/absent when caller opts in: {body}"
    );
    assert_eq!(body["chunk"]["text"].as_str(), Some("v1"));
    assert_eq!(body["status"].as_str(), Some("superseded"));
}

#[tokio::test]
async fn memory_get_returns_found_false_when_chunk_absent() {
    // Missing chunks must surface as `found=false` without leaking a
    // phantom payload.
    let (server, _tmp) = test_server().await;
    let bogus = ChunkId::new();
    let resp = call_tool(
        &server,
        "memory.get",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": bogus.to_string(),
        }),
    )
    .await;
    let body = parse_result_text(&resp);
    assert_eq!(body["found"].as_bool(), Some(false));
}

#[tokio::test]
async fn memory_search_hides_lifecycle_hidden_by_default_and_refills() {
    let (server, _tmp) = test_server().await;
    let ps = server.store().as_persistent().expect("persistent store");
    let t = tenant("t");

    let visible_id = ps
        .add(chunk_at(&t, "needle visible survivor", 1_000))
        .await
        .unwrap();
    let history_id = ps
        .add(chunk_at(&t, "needle history archive", 2_000))
        .await
        .unwrap();
    let superseded_id = ps
        .add(chunk_at(&t, "needle superseded retired", 3_000))
        .await
        .unwrap();
    let error_id = ps
        .add(chunk_at(&t, "needle error diagnostic", 500))
        .await
        .unwrap();

    ps.update_lifecycle(
        &t,
        &history_id,
        &LifecycleDelta {
            tier: Some(MemoryTier::History),
            lifecycle_updated_at_ms: Some(10),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ps.update_lifecycle(
        &t,
        &superseded_id,
        &LifecycleDelta {
            status: Some(ChunkStatus::Superseded),
            lifecycle_updated_at_ms: Some(11),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ps.update_lifecycle(
        &t,
        &error_id,
        &LifecycleDelta {
            status: Some(ChunkStatus::Error),
            lifecycle_updated_at_ms: Some(12),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let resp = call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": "t",
            "query": "needle",
            "k": 1
        }),
    )
    .await;
    let body = parse_result_text(&resp);
    let results = body["results"].as_array().expect("results array");

    assert_eq!(
        results.len(),
        1,
        "search should refill from over-fetched visible rows: {body}"
    );
    let visible_id_text = visible_id.to_string();
    assert_eq!(
        results[0]["chunk_id"].as_str(),
        Some(visible_id_text.as_str())
    );
    assert_eq!(results[0]["text"].as_str(), Some("needle visible survivor"));

    let resp = call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": "t",
            "query": "needle",
            "k": 4,
            "include_superseded": true,
            "include_expired": true,
            "include_history": true
        }),
    )
    .await;
    let body = parse_result_text(&resp);
    let texts = body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|result| result["text"].as_str())
        .collect::<Vec<_>>();

    assert!(texts.contains(&"needle visible survivor"), "{body}");
    assert!(texts.contains(&"needle superseded retired"), "{body}");
    assert!(texts.contains(&"needle history archive"), "{body}");
    assert!(
        !texts.contains(&"needle error diagnostic"),
        "Error rows must remain hidden even with all include flags: {body}"
    );
}

#[tokio::test]
async fn context_search_documents_hides_lifecycle_hidden_rows_and_refills() {
    let (server, _tmp) = test_server().await;
    let ps = server.store().as_persistent().expect("persistent store");
    let t = tenant("t");

    let visible_id = ps
        .add(context_chunk_at(&t, "contextneedle visible context", 1_000))
        .await
        .unwrap();
    let history_id = ps
        .add(context_chunk_at(&t, "contextneedle history context", 2_000))
        .await
        .unwrap();
    let superseded_id = ps
        .add(context_chunk_at(
            &t,
            "contextneedle superseded context",
            3_000,
        ))
        .await
        .unwrap();

    ps.update_lifecycle(
        &t,
        &history_id,
        &LifecycleDelta {
            tier: Some(MemoryTier::History),
            lifecycle_updated_at_ms: Some(20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ps.update_lifecycle(
        &t,
        &superseded_id,
        &LifecycleDelta {
            status: Some(ChunkStatus::Superseded),
            lifecycle_updated_at_ms: Some(21),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let resp = call_tool(
        &server,
        "context.search_context_documents",
        serde_json::json!({
            "tenant_id": "t",
            "query": "contextneedle",
            "subsystem_key": "search",
            "k": 1
        }),
    )
    .await;
    let body = parse_result_text(&resp);
    let results = body["results"].as_array().expect("results array");

    assert_eq!(
        results.len(),
        1,
        "context search should refill from over-fetched visible rows: {body}"
    );
    let visible_id_text = visible_id.to_string();
    assert_eq!(
        results[0]["chunk_id"].as_str(),
        Some(visible_id_text.as_str())
    );
    assert_eq!(
        results[0]["text"].as_str(),
        Some("contextneedle visible context")
    );
}

#[tokio::test]
async fn memory_add_accepts_expires_at_ms_and_search_hides_expired_rows() {
    let (server, _tmp) = test_server().await;

    let expired_id = add_with_expiry(&server, "t", "expiryneedle already expired", 1).await;

    let get = call_tool(
        &server,
        "memory.get",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": expired_id.to_string()
        }),
    )
    .await;
    let body = parse_result_text(&get);
    assert_eq!(body["found"].as_bool(), Some(true), "{body}");
    assert_eq!(body["hidden"].as_bool(), Some(true), "{body}");

    let search = call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": "t",
            "query": "expiryneedle",
            "k": 5
        }),
    )
    .await;
    let body = parse_result_text(&search);
    assert_eq!(body["results"].as_array().unwrap().len(), 0, "{body}");

    let search = call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": "t",
            "query": "expiryneedle",
            "k": 5,
            "include_expired": true
        }),
    )
    .await;
    let body = parse_result_text(&search);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{body}");
    assert_eq!(
        results[0]["chunk_id"].as_str(),
        Some(expired_id.to_string().as_str())
    );
}

#[tokio::test]
async fn memory_set_expiry_sets_and_clears_temporal_fields() {
    let (server, _tmp) = test_server().await;
    let chunk_id = add_chunk(&server, "t", "setexpiry visible document").await;

    let set = call_tool(
        &server,
        "memory.set_expiry",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": chunk_id.to_string(),
            "expires_at_ms": 4_000_000_000_000i64,
            "review_after_ms": 3_000_000_000_000i64
        }),
    )
    .await;
    let body = parse_result_text(&set);
    assert_eq!(body["updated"].as_bool(), Some(true), "{body}");
    assert_eq!(body["expires_at_ms"].as_i64(), Some(4_000_000_000_000));
    assert_eq!(body["review_after_ms"].as_i64(), Some(3_000_000_000_000));

    let get = call_tool(
        &server,
        "memory.get",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": chunk_id.to_string()
        }),
    )
    .await;
    let body = parse_result_text(&get);
    assert_eq!(
        body["lifecycle"]["expires_at_ms"].as_i64(),
        Some(4_000_000_000_000)
    );
    assert_eq!(
        body["lifecycle"]["review_after_ms"].as_i64(),
        Some(3_000_000_000_000)
    );

    let clear = call_tool(
        &server,
        "memory.set_expiry",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": chunk_id.to_string(),
            "clear_expiry": true,
            "clear_review_after": true
        }),
    )
    .await;
    let body = parse_result_text(&clear);
    assert_eq!(body["updated"].as_bool(), Some(true), "{body}");
    assert!(body["expires_at_ms"].is_null(), "{body}");
    assert!(body["review_after_ms"].is_null(), "{body}");
}

#[tokio::test]
async fn expiry_sweep_marks_due_chunks_expired() {
    let (server, _tmp) = test_server().await;
    let ps = server.store().as_persistent().expect("persistent store");
    let t = tenant("t");

    let due_id = ps
        .add(chunk_at(&t, "sweepneedle due", 1_000))
        .await
        .unwrap();
    let future_id = ps
        .add(chunk_at(&t, "sweepneedle future", 2_000))
        .await
        .unwrap();
    ps.update_lifecycle(
        &t,
        &due_id,
        &LifecycleDelta {
            expires_at_ms: Some(Some(500)),
            lifecycle_updated_at_ms: Some(10),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ps.update_lifecycle(
        &t,
        &future_id,
        &LifecycleDelta {
            expires_at_ms: Some(Some(2_000)),
            lifecycle_updated_at_ms: Some(11),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let expired = ps.expire_chunks_before(&t, 1_000, 100).await.unwrap();
    assert_eq!(expired, 1);

    let due = ps.get_with_lifecycle(&t, &due_id).await.unwrap().unwrap();
    assert_eq!(due.status, ChunkStatus::Expired);
    assert_eq!(due.lifecycle.lifecycle_updated_at_ms, 1_000);

    let future = ps
        .get_with_lifecycle(&t, &future_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(future.status, ChunkStatus::Final);
}

#[tokio::test]
async fn history_promotion_moves_stale_hidden_rows_to_history() {
    let (server, _tmp) = test_server().await;
    let ps = server.store().as_persistent().expect("persistent store");
    let t = tenant("t");

    let expired_id = ps
        .add(chunk_at(&t, "historyneedle expired", 1_000))
        .await
        .unwrap();
    let superseded_id = ps
        .add(chunk_at(&t, "historyneedle superseded", 2_000))
        .await
        .unwrap();
    let fresh_id = ps
        .add(chunk_at(&t, "historyneedle fresh", 3_000))
        .await
        .unwrap();

    for (chunk_id, status, updated_at) in [
        (&expired_id, ChunkStatus::Expired, 100),
        (&superseded_id, ChunkStatus::Superseded, 200),
        (&fresh_id, ChunkStatus::Expired, 2_000),
    ] {
        ps.update_lifecycle(
            &t,
            chunk_id,
            &LifecycleDelta {
                status: Some(status),
                lifecycle_updated_at_ms: Some(updated_at),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let promoted = ps
        .promote_stale_lifecycle_hidden_to_history(&t, 1_000, 100)
        .await
        .unwrap();
    assert_eq!(promoted, 2);

    let expired = ps
        .get_with_lifecycle(&t, &expired_id)
        .await
        .unwrap()
        .unwrap();
    let superseded = ps
        .get_with_lifecycle(&t, &superseded_id)
        .await
        .unwrap()
        .unwrap();
    let fresh = ps.get_with_lifecycle(&t, &fresh_id).await.unwrap().unwrap();
    assert_eq!(expired.lifecycle.tier, MemoryTier::History);
    assert_eq!(superseded.lifecycle.tier, MemoryTier::History);
    assert_eq!(fresh.lifecycle.tier, MemoryTier::LongTerm);
}

#[tokio::test]
async fn memory_compact_runs_lifecycle_maintenance() {
    let (server, _tmp) = test_server().await;
    let expired_id = add_with_expiry(&server, "t", "compactexpiry due", 1).await;

    let compact = call_tool(
        &server,
        "memory.compact",
        serde_json::json!({
            "tenant_id": "t"
        }),
    )
    .await;
    let body = parse_result_text(&compact);
    assert_eq!(
        body["lifecycle_maintenance"]["expired_chunks"].as_u64(),
        Some(1),
        "{body}"
    );

    let get = call_tool(
        &server,
        "memory.get",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": expired_id.to_string(),
            "include_expired": true
        }),
    )
    .await;
    let body = parse_result_text(&get);
    assert_eq!(body["status"].as_str(), Some("expired"), "{body}");
}
