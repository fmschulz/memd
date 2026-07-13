#[path = "common/mod.rs"]
mod common;
use common::*;

use memd::store::Store;
use memd::types::{ChunkId, ChunkType};
use serde_json::json;

#[tokio::test]
async fn equivalent_single_and_batch_writes_persist_identical_policy_tags() {
    let (server, _tmp) = test_server().await;
    let text = "Decision: use tenant-scoped cache keys. Rationale: global keys leak context.";

    let single = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "prepared_single",
            "project_id": "p",
            "text": text,
            "type": "decision",
            "tags": ["kind:decision", "ctx:subsystem:cache"]
        }),
    )
    .await;
    let single_id = ChunkId::parse(parse_result_text(&single)["chunk_id"].as_str().unwrap())
        .expect("single chunk id");

    let batch = call_tool(
        &server,
        "memory.add_batch",
        json!({
            "tenant_id": "prepared_batch",
            "chunks": [{
                "project_id": "p",
                "text": text,
                "type": "decision",
                "tags": ["kind:decision", "ctx:subsystem:cache"]
            }]
        }),
    )
    .await;
    let batch_id = ChunkId::parse(parse_result_text(&batch)["chunk_ids"][0].as_str().unwrap())
        .expect("batch chunk id");

    let single_chunk = server
        .store()
        .get(&tenant("prepared_single"), &single_id)
        .await
        .unwrap()
        .unwrap();
    let batch_chunk = server
        .store()
        .get(&tenant("prepared_batch"), &batch_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(batch_chunk.tags, single_chunk.tags);
    assert!(single_chunk.tags.iter().any(|tag| tag == "priority:7"));
}

#[tokio::test]
async fn equivalent_cli_and_call_writes_share_retention_and_tags() {
    let (server, _tmp) = test_server().await;
    let text = "Validation: scoped cache-key behavior passed the regression suite.";
    memd::cli::run_cli(
        server.store(),
        None,
        memd::cli::CliCommand::Add {
            tenant_id: Some("prepared_cli".to_string()),
            text: text.to_string(),
            chunk_type: ChunkType::Summary,
            project_id: Some("p".to_string()),
            tags: Some(vec!["kind:progress".to_string()]),
            source_uri: None,
            source_path: None,
            warm: memd::cli::WarmMode::Off,
        },
    )
    .await
    .unwrap();
    let called = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "prepared_call",
            "project_id": "p",
            "text": text,
            "type": "summary",
            "tags": ["kind:progress"]
        }),
    )
    .await;
    let call_id = ChunkId::parse(parse_result_text(&called)["chunk_id"].as_str().unwrap()).unwrap();
    let cli_tenant = tenant("prepared_cli");
    let cli_chunk = server
        .store()
        .list_chunks_for_project(&cli_tenant, Some("p"), 10, 0)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let call_chunk = server
        .store()
        .get(&tenant("prepared_call"), &call_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cli_chunk.tags, call_chunk.tags);
    let cli_lifecycle = server
        .store()
        .get_with_lifecycle(&cli_tenant, &cli_chunk.chunk_id)
        .await
        .unwrap()
        .unwrap()
        .lifecycle;
    let call_lifecycle = server
        .store()
        .get_with_lifecycle(&tenant("prepared_call"), &call_id)
        .await
        .unwrap()
        .unwrap()
        .lifecycle;
    assert_eq!(cli_lifecycle.tier, call_lifecycle.tier);
    let cli_expiry_delta = cli_lifecycle.expires_at_ms.unwrap() - cli_chunk.timestamp_created;
    let call_expiry_delta = call_lifecycle.expires_at_ms.unwrap() - call_chunk.timestamp_created;
    assert!((cli_expiry_delta - call_expiry_delta).abs() <= 100);
    let cli_review_delta = cli_lifecycle.review_after_ms.unwrap() - cli_chunk.timestamp_created;
    let call_review_delta = call_lifecycle.review_after_ms.unwrap() - call_chunk.timestamp_created;
    assert!((cli_review_delta - call_review_delta).abs() <= 100);
}

#[tokio::test]
async fn supersede_replacement_cannot_bypass_write_admission() {
    let (server, _tmp) = test_server().await;
    let added = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "prepared_supersede",
            "project_id": "p",
            "text": "Validation: old cache-key decision was recorded.",
            "type": "decision"
        }),
    )
    .await;
    let old_id = parse_result_text(&added)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();

    let replacement = call_tool(
        &server,
        "memory.supersede",
        json!({
            "tenant_id": "prepared_supersede",
            "old_chunk_id": old_id,
            "project_id": "p",
            "new_text": "starting to inspect the code",
            "type": "summary",
            "tags": ["kind:progress"]
        }),
    )
    .await;

    assert!(
        replacement.get("error").is_some(),
        "low-signal replacement bypassed admission: {replacement}"
    );
}

#[tokio::test]
async fn episode_summary_contains_prepared_actionable_guidance() {
    let (server, _tmp) = test_server().await;
    for text in ["cache key failure reproduced", "tenant-scoped key fixed it"] {
        let response = call_tool(
            &server,
            "memory.add",
            json!({
                "tenant_id": "prepared_episode",
                "project_id": "p",
                "episode_id": "e1",
                "text": text,
                "type": "doc"
            }),
        )
        .await;
        parse_result_text(&response);
    }

    let consolidated = call_tool(
        &server,
        "memory.consolidate_episode",
        json!({
            "tenant_id": "prepared_episode",
            "episode_id": "e1",
            "retain_source_chunks": false
        }),
    )
    .await;
    let summary_id = ChunkId::parse(
        parse_result_text(&consolidated)["summary_chunk_id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let summary = server
        .store()
        .get(&tenant("prepared_episode"), &summary_id)
        .await
        .unwrap()
        .unwrap();

    assert!(
        summary.text.contains("Agent action:"),
        "episode synthesis lacks actionable guidance: {}",
        summary.text
    );
}
