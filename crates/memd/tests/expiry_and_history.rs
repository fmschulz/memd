//! Integration tests for Track C (temporal overlay + lazy hiding +
//! expiry sweep + history promotion + memory.set_expiry).

mod common;

use common::*;
use memd::store::Store;
use memd::types::{ChunkId, TenantId};
use serde_json::json;

#[tokio::test]
async fn memory_add_persists_temporal_overlay_fields() {
    let (server, _tmp) = test_server().await;
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "sprint note",
            "type": "doc",
            "expires_at_ms": 1_900_000_000_000_i64,
            "review_after_ms": 1_800_000_000_000_i64,
        }),
    )
    .await;
    let id_str = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();
    let id = ChunkId::parse(&id_str).expect("valid chunk id");
    let tenant = TenantId::new("t").expect("valid tenant id");
    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("get_with_lifecycle ok")
        .expect("chunk present");
    assert_eq!(resolved.lifecycle.expires_at_ms, Some(1_900_000_000_000));
    assert_eq!(resolved.lifecycle.review_after_ms, Some(1_800_000_000_000));
}

#[tokio::test]
async fn memory_add_without_temporal_fields_leaves_overlay_empty() {
    let (server, _tmp) = test_server().await;
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "plain note",
            "type": "doc",
        }),
    )
    .await;
    let id_str = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();
    let id = ChunkId::parse(&id_str).expect("valid chunk id");
    let tenant = TenantId::new("t").expect("valid tenant id");
    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("get_with_lifecycle ok")
        .expect("chunk present");
    assert!(resolved.lifecycle.expires_at_ms.is_none());
    assert!(resolved.lifecycle.review_after_ms.is_none());
}

#[tokio::test]
async fn memory_add_batch_validation_failure_leaves_no_partial_writes() {
    // When any chunk in a lifecycle-enabled batch fails validation,
    // no rows should be written — validation must run before the first
    // store write. Regression test for Codex C1 round-1 HIGH.
    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Count rows already present in this tenant (new tempdir = 0).
    let before = server
        .store()
        .list_chunks(&tenant, 1024, 0)
        .await
        .expect("list ok")
        .len();

    // First chunk is valid + carries temporal fields (forces the
    // lifecycle path). Second chunk uses an invalid episode_id that
    // validate_episode_id rejects, so the whole batch must fail before
    // any chunk is persisted.
    let resp = call_tool(
        &server,
        "memory.add_batch",
        json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "first",  "type": "doc", "expires_at_ms": 1_900_000_000_000_i64 },
                { "text": "second", "type": "doc", "episode_id": "bad id with spaces" }
            ]
        }),
    )
    .await;
    assert!(
        parse_error(&resp).is_some(),
        "batch should return an error when any chunk fails validation, got: {resp}"
    );

    let after = server
        .store()
        .list_chunks(&tenant, 1024, 0)
        .await
        .expect("list ok")
        .len();
    assert_eq!(
        before, after,
        "no chunks must be persisted when batch validation fails"
    );
}

#[tokio::test]
async fn memory_add_batch_persists_temporal_overlay_fields_per_chunk() {
    let (server, _tmp) = test_server().await;
    let resp = call_tool(
        &server,
        "memory.add_batch",
        json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "with expiry", "type": "doc", "expires_at_ms": 1_900_000_000_000_i64 },
                { "text": "plain",        "type": "doc" }
            ]
        }),
    )
    .await;
    let ids = parse_result_text(&resp)["chunk_ids"]
        .as_array()
        .expect("chunk_ids")
        .clone();
    assert_eq!(ids.len(), 2);
    let tenant = TenantId::new("t").expect("valid tenant id");
    let id_a = ChunkId::parse(ids[0].as_str().unwrap()).expect("valid chunk id");
    let id_b = ChunkId::parse(ids[1].as_str().unwrap()).expect("valid chunk id");
    let ra = server
        .store()
        .get_with_lifecycle(&tenant, &id_a)
        .await
        .unwrap()
        .unwrap();
    let rb = server
        .store()
        .get_with_lifecycle(&tenant, &id_b)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ra.lifecycle.expires_at_ms, Some(1_900_000_000_000));
    assert!(ra.lifecycle.review_after_ms.is_none());
    assert!(rb.lifecycle.expires_at_ms.is_none());
    assert!(rb.lifecycle.review_after_ms.is_none());
}
