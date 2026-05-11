//! Contract tests for StructMem-inspired event consolidation.
//!
//! The first test locks in the primitive that exists today: caller-supplied
//! event/entry tags survive through `memory.add` and `memory.search`.
//! Ignored tests below are executable acceptance criteria for the planned
//! sibling expansion, deterministic packet, and agent-authored synthesis tools.

#[path = "common/mod.rs"]
mod common;
use common::*;

use serde_json::{json, Value};

fn result_has_tags(result: &Value, required: &[&str]) -> bool {
    let Some(observed) = result["tags"].as_array() else {
        return required.is_empty();
    };
    required.iter().all(|required_tag| {
        observed
            .iter()
            .any(|observed_tag| observed_tag.as_str() == Some(*required_tag))
    })
}

#[tokio::test]
async fn conversation_event_tags_survive_add_and_search() {
    let (server, _tmp) = test_server().await;
    let tenant_id = "event_contract";
    let project_id = "structmem_contract";
    let episode_id = "session_2026_04_24";
    let event_tag = "event:session_2026_04_24_turn_1";

    let factual = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "episode_id": episode_id,
            "text": "aurora bridge alpha factual entry: Caroline attended Pride Fest in 2022.",
            "type": "doc",
            "mode": "conversation",
            "tags": [event_tag, "entry:factual", "speaker:caroline", "turn:1"]
        }),
    )
    .await;
    parse_result_text(&factual);

    let relational = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "episode_id": episode_id,
            "text": "aurora bridge alpha relational entry: Melanie was interested in the same Pride Fest event.",
            "type": "doc",
            "mode": "conversation",
            "tags": [event_tag, "entry:relational", "speaker:melanie", "turn:1"]
        }),
    )
    .await;
    parse_result_text(&relational);

    let search = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "query": "aurora bridge alpha",
            "k": 5,
            "filters": { "episode_id": episode_id }
        }),
    )
    .await;
    let body = parse_result_text(&search);
    let results = body["results"].as_array().expect("results array");

    assert!(
        results.iter().any(|result| {
            result["episode_id"].as_str() == Some(episode_id)
                && result_has_tags(result, &[event_tag, "entry:factual"])
        }),
        "expected factual event entry in search results: {body}"
    );
    assert!(
        results.iter().any(|result| {
            result["episode_id"].as_str() == Some(episode_id)
                && result_has_tags(result, &[event_tag, "entry:relational"])
        }),
        "expected relational event entry in search results: {body}"
    );
}

#[tokio::test]
async fn search_with_event_sibling_expansion_returns_factual_and_relational_siblings() {
    let (server, _tmp) = test_server().await;
    let tenant_id = "event_contract_expand";
    let project_id = "structmem_contract";
    let episode_id = "session_expand";
    let event_tag = "event:session_expand_turn_1";

    let factual = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "episode_id": episode_id,
            "text": "solstice factual entry: Caroline attended Pride Fest in 2022.",
            "type": "doc",
            "mode": "conversation",
            "tags": [event_tag, "entry:factual"]
        }),
    )
    .await;
    let factual_id = parse_result_text(&factual)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();

    let relational = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "episode_id": episode_id,
            "text": "relational entry: Melanie showed interest in Caroline's Pride Fest memory.",
            "type": "doc",
            "mode": "conversation",
            "tags": [event_tag, "entry:relational"]
        }),
    )
    .await;
    let relational_id = parse_result_text(&relational)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();

    let cross_project = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": tenant_id,
            "project_id": "other_project",
            "episode_id": episode_id,
            "text": "cross-project entry sharing the event tag but outside the request scope.",
            "type": "doc",
            "mode": "conversation",
            "tags": [event_tag, "entry:relational"]
        }),
    )
    .await;
    let cross_project_id = parse_result_text(&cross_project)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();

    let search = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "query": "solstice",
            "k": 1,
            "expand_event_siblings": true
        }),
    )
    .await;
    let body = parse_result_text(&search);
    let first = body["results"]
        .as_array()
        .and_then(|results| results.first())
        .expect("first result");
    let sibling_ids: Vec<&str> = first["expanded_siblings"]
        .as_array()
        .expect("expanded_siblings array")
        .iter()
        .filter_map(|sibling| sibling["chunk_id"].as_str())
        .collect();

    assert!(
        sibling_ids.contains(&factual_id.as_str())
            || first["chunk_id"].as_str() == Some(factual_id.as_str()),
        "expanded result must include the factual source: {body}"
    );
    assert!(
        sibling_ids.contains(&relational_id.as_str())
            || first["chunk_id"].as_str() == Some(relational_id.as_str()),
        "expanded result must include the relational sibling: {body}"
    );
    assert!(
        !sibling_ids.contains(&cross_project_id.as_str()),
        "expanded siblings must not cross project boundaries: {body}"
    );
}

#[tokio::test]
#[ignore = "phase 3 acceptance: enable after memory.prepare_event_consolidation is implemented"]
async fn prepare_event_consolidation_returns_deterministic_grounded_packet() {
    let (server, _tmp) = test_server().await;
    let tenant_id = "event_contract_prepare";
    let project_id = "structmem_contract";

    for (episode_id, event_tag, text) in [
        (
            "recent_session",
            "event:recent_session_turn_1",
            "ember factual entry: Caroline described attending Pride Fest in 2022.",
        ),
        (
            "recent_session",
            "event:recent_session_turn_1",
            "ember relational entry: Melanie connected the Pride Fest story to Caroline.",
        ),
        (
            "historical_session",
            "event:historical_session_turn_9",
            "historical entry: Caroline and Melanie discussed festival plans.",
        ),
    ] {
        let response = call_tool(
            &server,
            "memory.add",
            json!({
                "tenant_id": tenant_id,
                "project_id": project_id,
                "episode_id": episode_id,
                "text": text,
                "type": "doc",
                "mode": "conversation",
                "tags": [event_tag, "entry:factual"]
            }),
        )
        .await;
        parse_result_text(&response);
    }

    let args = json!({
        "tenant_id": tenant_id,
        "project_id": project_id,
        "episode_id": "recent_session",
        "buffer_limit": 60,
        "seed_k": 15,
        "include_existing_syntheses": true
    });
    let first = parse_result_text(
        &call_tool(&server, "memory.prepare_event_consolidation", args.clone()).await,
    );
    let second =
        parse_result_text(&call_tool(&server, "memory.prepare_event_consolidation", args).await);

    assert_eq!(first, second, "packet must be deterministic for fixed data");
    assert!(
        first["buffer"]
            .as_array()
            .map(|buffer| !buffer.is_empty())
            .unwrap_or(false),
        "packet must include recent episode buffer: {first}"
    );
    assert!(
        first["expanded_events"]
            .as_array()
            .map(|events| {
                events.iter().all(|event| {
                    event["source_chunk_ids"]
                        .as_array()
                        .map(|ids| !ids.is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false),
        "every expanded event must carry source_chunk_ids: {first}"
    );
}

#[tokio::test]
#[ignore = "phase 4 acceptance: enable after memory.commit_event_synthesis is implemented"]
async fn commit_event_synthesis_rejects_empty_source_set() {
    let (server, _tmp) = test_server().await;

    let response = call_tool(
        &server,
        "memory.commit_event_synthesis",
        json!({
            "tenant_id": "event_contract_commit",
            "project_id": "structmem_contract",
            "episode_id": "session_commit",
            "agent_id": "codex",
            "summary": "Unsupported synthesis",
            "content": "Facts:\nUnsupported claim.",
            "source_chunk_ids": []
        }),
    )
    .await;
    let (_code, message) = parse_error(&response).expect("error envelope");
    assert!(
        message.to_lowercase().contains("source_chunk_ids"),
        "empty source set must be rejected with a source-specific error: {message}"
    );
}

#[tokio::test]
#[ignore = "phase 4 acceptance: enable after memory.commit_event_synthesis is implemented"]
async fn commit_event_synthesis_rejects_cross_project_sources() {
    let (server, _tmp) = test_server().await;
    let tenant_id = "event_contract_scope";

    let source = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": tenant_id,
            "project_id": "alpha",
            "episode_id": "session_scope",
            "text": "scope source chunk",
            "type": "doc",
            "mode": "conversation",
            "tags": ["event:session_scope_turn_1", "entry:factual"]
        }),
    )
    .await;
    let source_id = parse_result_text(&source)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();

    let response = call_tool(
        &server,
        "memory.commit_event_synthesis",
        json!({
            "tenant_id": tenant_id,
            "project_id": "beta",
            "episode_id": "session_scope",
            "agent_id": "codex",
            "summary": "Cross-project synthesis",
            "content": "Facts:\nThis should not commit.",
            "source_chunk_ids": [source_id]
        }),
    )
    .await;
    let (_code, message) = parse_error(&response).expect("error envelope");
    assert!(
        message.to_lowercase().contains("project"),
        "cross-project source must be rejected with a scope error: {message}"
    );
}

#[tokio::test]
#[ignore = "phase 5 acceptance: enable after event synthesis retrieval mode is implemented"]
async fn committed_event_synthesis_is_searchable_and_provenance_marked() {
    let (server, _tmp) = test_server().await;
    let tenant_id = "event_contract_synthesis";
    let project_id = "structmem_contract";
    let episode_id = "session_synthesis";

    let source = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "episode_id": episode_id,
            "text": "Caroline and Melanie discussed attending Pride Fest together.",
            "type": "doc",
            "mode": "conversation",
            "tags": ["event:session_synthesis_turn_1", "entry:factual"]
        }),
    )
    .await;
    let source_id = parse_result_text(&source)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();

    let commit = call_tool(
        &server,
        "memory.commit_event_synthesis",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "episode_id": episode_id,
            "agent_id": "codex",
            "summary": "Pride Fest relationship synthesis",
            "content": "Facts:\nCaroline and Melanie likely attended Pride Fest together.\nUncertainty:\nThe relation is inferred from adjacent event entries.",
            "source_chunk_ids": [source_id],
            "parameters": { "seed_k": 15, "buffer_limit": 60, "model": "agent-owned" }
        }),
    )
    .await;
    parse_result_text(&commit);

    let search = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "query": "attended pride fest together",
            "mode": "event_synthesis",
            "k": 3
        }),
    )
    .await;
    let body = parse_result_text(&search);
    let synthesis = body["results"]
        .as_array()
        .and_then(|results| results.first())
        .expect("synthesis search result");

    assert_eq!(synthesis["chunk_type"].as_str(), Some("summary"));
    assert_eq!(synthesis["promotion_state"].as_str(), Some("summarized"));
    let source_tag = format!("source_chunk:{source_id}");
    assert!(
        result_has_tags(
            synthesis,
            &[
                "entry:synthesis",
                "event_synthesis:true",
                source_tag.as_str()
            ]
        ),
        "synthesis result must expose derived/provenance tags: {body}"
    );
    assert_eq!(
        synthesis["source"]["tool_name"].as_str(),
        Some("memory.commit_event_synthesis")
    );
}
