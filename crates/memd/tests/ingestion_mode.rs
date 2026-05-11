//! Track E integration tests for ingestion_mode persistence and the
//! conversation-mode default review window.

mod common;
use common::*;

use memd::store::metadata::MetadataStore;
use memd::store::Store;
use memd::types::{ChunkId, IngestionMode};

#[tokio::test]
async fn add_with_mode_conversation_persists_label_to_metadata() {
    let (server, _tmp) = test_server().await;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "session note",
            "type": "doc",
            "mode": "conversation",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");

    let ps = server.store().as_persistent().expect("persistent");
    let meta = ps
        .metadata()
        .get(&tenant("t"), &id)
        .expect("metadata.get")
        .expect("inserted row");
    assert_eq!(meta.ingestion_mode, IngestionMode::Conversation);
}

#[tokio::test]
async fn add_with_mode_document_default_persists_document() {
    let (server, _tmp) = test_server().await;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "doc chunk",
            "type": "doc",
            // mode omitted → default Document
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");

    let ps = server.store().as_persistent().expect("persistent");
    let meta = ps
        .metadata()
        .get(&tenant("t"), &id)
        .expect("metadata.get")
        .expect("inserted row");
    assert_eq!(meta.ingestion_mode, IngestionMode::Document);
}

#[tokio::test]
async fn add_with_mode_invalid_value_rejected() {
    let (server, _tmp) = test_server().await;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "x",
            "type": "doc",
            "mode": "shoutcast",
        }),
    )
    .await;
    let err = parse_error(&r).expect("error envelope");
    assert!(
        err.1.to_lowercase().contains("ingestion mode") || err.1.to_lowercase().contains("mode"),
        "expected 'mode'-related error message, got: {}",
        err.1
    );
}

#[tokio::test]
async fn add_with_mode_conversation_applies_default_14d_review_window() {
    // E2: when mode=conversation AND review_after_ms is omitted, the
    // handler defaults review_after_ms to now() + 14 days. Explicit
    // review_after_ms always wins; mode=document never applies the
    // default.
    let (server, _tmp) = test_server().await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "session note",
            "type": "doc",
            "mode": "conversation",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");

    let ps = server.store().as_persistent().expect("persistent");
    let resolved = ps
        .get_with_lifecycle(&tenant("t"), &id)
        .await
        .expect("get_with_lifecycle")
        .expect("row");
    let review = resolved
        .lifecycle
        .review_after_ms
        .expect("conversation mode must default review_after_ms");
    let fourteen_days_ms: i64 = 14 * 24 * 3600 * 1000;
    let expected = now_ms + fourteen_days_ms;
    let drift = (review - expected).abs();
    assert!(
        drift < 5_000,
        "review_after_ms drift > 5s: review={review}, expected≈{expected}, drift={drift}"
    );
}

#[tokio::test]
async fn add_with_mode_document_does_not_set_default_review_window() {
    let (server, _tmp) = test_server().await;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "doc",
            "type": "doc",
            "mode": "document",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");
    let ps = server.store().as_persistent().expect("persistent");
    let resolved = ps
        .get_with_lifecycle(&tenant("t"), &id)
        .await
        .expect("get_with_lifecycle")
        .expect("row");
    assert!(
        resolved.lifecycle.review_after_ms.is_none(),
        "document mode must not auto-populate review_after_ms"
    );
}

// Codex round-1 E2 LOW: prove the post-default `any_lifecycle` flip
// works in the BATCH path. A batch with no expires_at_ms /
// review_after_ms but mode=conversation must still route through
// `add_chunk_with_lifecycle` so the 14d default lands per chunk.
#[tokio::test]
async fn add_batch_with_mode_conversation_applies_default_review_window() {
    let (server, _tmp) = test_server().await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let r = call_tool(
        &server,
        "memory.add_batch",
        serde_json::json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "session note one", "type": "doc", "mode": "conversation" },
                { "text": "session note two", "type": "doc", "mode": "conversation" },
            ],
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let ids: Vec<String> = body["chunk_ids"]
        .as_array()
        .expect("chunk_ids")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2);

    let ps = server.store().as_persistent().expect("persistent");
    let fourteen_days_ms: i64 = 14 * 24 * 3600 * 1000;
    for id_str in &ids {
        let id = ChunkId::parse(id_str).expect("valid");
        let resolved = ps
            .get_with_lifecycle(&tenant("t"), &id)
            .await
            .unwrap()
            .unwrap();
        let review = resolved
            .lifecycle
            .review_after_ms
            .expect("conversation batch row must default review_after_ms");
        let drift = (review - (now_ms + fourteen_days_ms)).abs();
        assert!(drift < 5_000, "review drift > 5s for {id}");

        // ingestion_mode lives on ChunkMetadata, not on ResolvedChunk;
        // pull it via metadata.get directly.
        let meta = ps
            .metadata()
            .get(&tenant("t"), &id)
            .expect("metadata.get")
            .expect("row");
        assert_eq!(meta.ingestion_mode, IngestionMode::Conversation);
    }
}

#[tokio::test]
async fn add_with_mode_conversation_explicit_review_after_ms_wins() {
    let (server, _tmp) = test_server().await;
    let explicit = 1_900_000_000_000_i64;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "session note",
            "type": "doc",
            "mode": "conversation",
            "review_after_ms": explicit,
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");
    let ps = server.store().as_persistent().expect("persistent");
    let resolved = ps
        .get_with_lifecycle(&tenant("t"), &id)
        .await
        .expect("get_with_lifecycle")
        .expect("row");
    assert_eq!(
        resolved.lifecycle.review_after_ms,
        Some(explicit),
        "explicit review_after_ms must override the conversation default"
    );
}
