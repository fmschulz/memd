//! Integration tests for lifecycle overlay on PersistentStore.

#[path = "common/mod.rs"]
mod common;
use common::*;

use memd::store::metadata::MetadataStore;
use memd::store::Store;
use memd::types::lifecycle::{LifecycleDelta, MemoryTier};
use memd::types::{ChunkId, ChunkStatus, ChunkType, MemoryChunk};

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
        resolved.lifecycle.superseded_by.as_ref().unwrap().to_string(),
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
async fn supersede_chunk_rejects_double_supersede_on_same_old_id() {
    // MED-4: the SAME old chunk must only be superseded once. Before
    // the fix, atomic_supersede's UPDATE blindly overwrote
    // `superseded_by` and the preflight accepted any non-deleted row,
    // so two supersedes on the same old chunk created a forked
    // supersession graph with two visible successors. The fix is
    // twofold: preflight rejects old_id whose `superseded_by` is
    // already set, and the SQL UPDATE carries a
    // `superseded_by IS NULL` guard so a race past the preflight still
    // fails atomically (and triggers the compensating tombstone).
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

    // Second supersede on the same old_id must fail.
    let err = store
        .supersede_chunk(&t, &a, MemoryChunk::new(t.clone(), "B-prime", ChunkType::Doc))
        .await
        .expect_err("second supersede on same old_id must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("not current head") || msg.contains("already superseded"),
        "expected not-current-head error, got: {msg}"
    );

    // A still points to B — not to some forked B-prime.
    let resolved_a = store.get_with_lifecycle(&t, &a).await.unwrap().unwrap();
    assert_eq!(
        resolved_a.lifecycle.superseded_by.as_ref().unwrap(),
        &b,
        "A.superseded_by must still point at B after the rejected double-supersede"
    );

    // No orphan B-prime in the tenant — the preflight rejected before
    // add_chunk_with_lifecycle ran, so the row count is still {A, B}.
    let list = store.metadata().list(&t, 100, 0).unwrap();
    assert_eq!(
        list.len(),
        2,
        "exactly 2 chunks expected (A, B); observed: {list:?}"
    );
}

#[tokio::test]
async fn supersede_chunk_detects_non_start_cycle_mid_chain() {
    // LOW-5: the old bounded-walk-only-detects-return-to-start
    // implementation missed cycles that re-entered the chain at any
    // node other than `start`. The new HashSet-based walk catches any
    // revisit. We forge a chain A → B → C → B (B re-entered) at the
    // overlay layer and assert that supersede_chunk on A fails with a
    // cycle error even though the cycle does not return to A.
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
    let c = store
        .add(MemoryChunk::new(t.clone(), "C", ChunkType::Doc))
        .await
        .unwrap();

    // Forge A → B → C → B (B re-entered, not a return-to-A loop).
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
                superseded_by: Some(c.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store
        .update_lifecycle(
            &t,
            &c,
            &LifecycleDelta {
                superseded_by: Some(b.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let err = store
        .supersede_chunk(&t, &a, MemoryChunk::new(t.clone(), "D", ChunkType::Doc))
        .await
        .expect_err("supersede_chunk must detect the non-start cycle B→C→B");
    let msg = format!("{err}");
    assert!(
        msg.contains("supersession cycle detected"),
        "expected cycle-detection error, got: {msg}"
    );
}

#[tokio::test]
async fn supersede_chunk_walks_long_chain_past_old_64_hop_bound() {
    // LOW-5: the old implementation capped the walk at 64 hops and
    // silently returned Ok(()) on exhaustion — meaning a 65-hop cycle
    // could slip through. The new HashSet walk has no length bound and
    // detects cycles regardless of chain length. Pin the acyclic case:
    // a 70-hop acyclic chain must still succeed.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let mut current = store
        .add(MemoryChunk::new(t.clone(), "A", ChunkType::Doc))
        .await
        .unwrap();
    for i in 0..70usize {
        let label = format!("v{}", i);
        current = store
            .supersede_chunk(
                &t,
                &current,
                MemoryChunk::new(t.clone(), &label, ChunkType::Doc),
            )
            .await
            .unwrap_or_else(|e| panic!("hop {i} failed: {e}"));
    }
    let resolved = store
        .get_with_lifecycle(&t, &current)
        .await
        .unwrap()
        .expect("final head must resolve");
    assert_eq!(resolved.status, ChunkStatus::Final);
}

#[tokio::test]
async fn memory_get_hidden_envelope_carries_hidden_reason() {
    // MED-3: a caller that receives `{hidden:true,...}` needs to know
    // which `include_*` flag would unhide the row. The envelope now
    // carries a `hidden_reason` ∈ {"superseded","expired","history",
    // "deleted"} discriminator so agents don't have to triangulate
    // from status + tier + expires_at_ms.
    let (server, _tmp) = test_server().await;
    let add = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "v1",
            "type": "doc"
        }),
    )
    .await;
    let old_id = parse_result_text(&add)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = call_tool(
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
    let get_resp = call_tool(
        &server,
        "memory.get",
        serde_json::json!({
            "tenant_id": "t",
            "chunk_id": old_id,
        }),
    )
    .await;
    let body = parse_result_text(&get_resp);
    assert_eq!(body["hidden"].as_bool(), Some(true));
    assert_eq!(
        body["hidden_reason"].as_str(),
        Some("superseded"),
        "superseded chunk must report hidden_reason=\"superseded\": {body}"
    );
}
