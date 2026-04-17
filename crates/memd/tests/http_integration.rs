//! Phase 4.2 black-box HTTP integration harness.
//!
//! Spins up a real memd daemon over HTTP against an in-memory store
//! and exercises the full MCP surface the way a real client would:
//! initialize → tool discovery → writes → reads → focused artifact
//! tools → default tenant resolution → mixed concurrency. The point
//! is to catch **seam bugs** between the protocol layer, handler
//! dispatch, and storage layer — the dominant class of defects the
//! Codex reviews across Phases 1-3 surfaced, which the in-process
//! unit tests do not consistently reach.
//!
//! These tests are deliberately coarse (no mocking) so that a
//! refactor in any layer has to keep the observable behavior stable.

use std::sync::Arc;
use std::time::Duration;

use memd::config::Config;
use memd::mcp::McpServer;
use memd::store::MemoryStore;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::task::spawn_blocking;
use tokio::time::{sleep, timeout};

/// Start a persistent in-process HTTP daemon backed by `MemoryStore`.
/// Returns `(url, handle)`. Drop the handle (or let the test end) to
/// shut down the axum task.
///
/// Uses `serve_http_server` with a pre-bound `TcpListener` so we
/// never race on the port between bind and serve.
async fn spawn_test_http_daemon() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let mut config = Config::default();
    config.server.transport = "http".to_string();

    let store = Arc::new(MemoryStore::new());
    let server = McpServer::new(config, store);

    let handle = tokio::spawn(async move {
        memd::mcp::serve_http_server(listener, server, "/mcp")
            .await
            .expect("http server should run");
    });

    // Give the axum task a tick to actually accept connections.
    tokio::task::yield_now().await;
    sleep(Duration::from_millis(50)).await;

    (format!("http://{}/mcp", addr), handle)
}

/// POST a JSON-RPC body and return (status, parsed_body).
async fn jsonrpc_call(url: &str, body: Value) -> (u16, Value) {
    let url = url.to_string();
    let body_str = body.to_string();
    spawn_blocking(move || {
        let response = ureq::post(&url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .timeout(Duration::from_secs(5))
            .send_string(&body_str)
            .expect("HTTP post must succeed");
        let status = response.status();
        let text = response.into_string().unwrap_or_default();
        let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
        (status, parsed)
    })
    .await
    .unwrap()
}

/// Convenience: call a `tools/call` wrapping `args` for `tool_name`.
async fn call_tool(url: &str, request_id: u64, tool_name: &str, args: Value) -> Value {
    let (status, body) = jsonrpc_call(
        url,
        json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": args}
        }),
    )
    .await;
    assert_eq!(status, 200, "{} returned {}: {}", tool_name, status, body);
    assert!(
        body.get("error").is_none(),
        "{} returned JSON-RPC error: {}",
        tool_name,
        body
    );
    let content = body["result"]["content"][0]["text"]
        .as_str()
        .expect("tools/call result must have content[0].text");
    serde_json::from_str(content).expect("tool payload must parse")
}

/// Initialize the server with a standard clientInfo.
async fn initialize(url: &str) {
    let (status, body) = jsonrpc_call(
        url,
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "harness", "version": "4.2"}
            }
        }),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["result"]["protocolVersion"].is_string());
}

// ---------- tests ----------

/// End-to-end: initialize → task.start → task.progress →
/// artifact.verification (focused tool) → memory.search →
/// task.resume. Exercises the full lifecycle through the real HTTP
/// transport. Catches seam bugs between protocol parsing, handler
/// dispatch, and storage writes.
#[tokio::test]
async fn end_to_end_task_lifecycle_over_http() {
    let (url, _handle) = spawn_test_http_daemon().await;
    initialize(&url).await;

    // task.start with minimal required fields (Phase 2.2: only goal).
    let start = call_tool(
        &url,
        1,
        "task.start",
        json!({
            "tenant_id": "e2e",
            "project_id": "lifecycle",
            "agent_id": "author",
            "goal": "end-to-end lifecycle smoke test"
        }),
    )
    .await;
    let task_id = start["task_id"].as_str().unwrap().to_string();
    let parent_artifact_id = start["artifact_id"].as_str().unwrap().to_string();
    assert!(!task_id.is_empty());

    // task.progress with opt-in fields.
    let progress = call_tool(
        &url,
        2,
        "task.progress",
        json!({
            "tenant_id": "e2e",
            "task_id": task_id,
            "project_id": "lifecycle",
            "summary": "first progress checkpoint",
            "blockers": ["none"],
            "next_step": "continue"
        }),
    )
    .await;
    assert!(progress["artifact_id"].as_str().unwrap().len() > 0);

    // artifact.verification (focused tool introduced in Phase 2.3)
    // from a distinct agent countersigns the parent task.
    let verify = call_tool(
        &url,
        3,
        "artifact.verification",
        json!({
            "tenant_id": "e2e",
            "task_id": task_id,
            "agent_id": "reviewer",
            "reply_to_artifact_id": parent_artifact_id,
            "supports_claim": true,
            "summary": "independently reproduced"
        }),
    )
    .await;
    assert!(!verify["artifact_id"].as_str().unwrap().is_empty());

    // memory.search for content we just wrote — should land at least
    // the progress summary.
    let search = call_tool(
        &url,
        4,
        "memory.search",
        json!({
            "tenant_id": "e2e",
            "query": "first progress checkpoint",
            "k": 5
        }),
    )
    .await;
    assert!(
        search["results"]
            .as_array()
            .map(|r| !r.is_empty())
            .unwrap_or(false),
        "memory.search should find the progress chunk: {}",
        search
    );

    // task.resume should now summarize what we did. The response
    // shape is `TaskResumeResult` which carries a nested `artifact`
    // (the persisted digest artifact) plus a `resume` view.
    let resume = call_tool(
        &url,
        5,
        "task.resume",
        json!({
            "tenant_id": "e2e",
            "task_id": task_id
        }),
    )
    .await;
    assert!(
        resume["artifact"]["artifact_id"].is_string(),
        "task.resume must include a digest artifact; got {}",
        resume
    );
    assert!(
        resume["resume"].is_object(),
        "task.resume must include a resume view; got {}",
        resume
    );
}

/// Phase 2.1: `tenant_id` is optional on every tool schema. Omit it
/// and the server must resolve to the default tenant without error.
#[tokio::test]
async fn default_tenant_resolution_over_http() {
    let (url, _handle) = spawn_test_http_daemon().await;
    initialize(&url).await;

    // Omit tenant_id entirely. Phase 2.1 resolver picks up the literal
    // "default" fallback (no env, no pinned file on CI).
    let start = call_tool(
        &url,
        10,
        "task.start",
        json!({
            "goal": "default-tenant resolution smoke",
            "agent_id": "author"
        }),
    )
    .await;
    assert!(!start["task_id"].as_str().unwrap().is_empty());

    // Passing an empty string should also resolve to default.
    let empty = call_tool(
        &url,
        11,
        "task.start",
        json!({
            "tenant_id": "",
            "goal": "empty-string tenant resolution",
            "agent_id": "author"
        }),
    )
    .await;
    assert!(!empty["task_id"].as_str().unwrap().is_empty());
}

/// Phase 2.3 focused artifact tools — each one must dispatch through
/// HTTP with its specific kind injected. Drives review/revision/
/// decision/verification and checks each persists with the right
/// kind visible via artifact.get.
#[tokio::test]
async fn focused_artifact_wrappers_over_http() {
    let (url, _handle) = spawn_test_http_daemon().await;
    initialize(&url).await;

    let start = call_tool(
        &url,
        20,
        "task.start",
        json!({
            "tenant_id": "focused_http",
            "goal": "focused tools via HTTP",
            "agent_id": "author"
        }),
    )
    .await;
    let task_id = start["task_id"].as_str().unwrap().to_string();
    let parent_id = start["artifact_id"].as_str().unwrap().to_string();

    for (id, tool, extra) in [
        (21u64, "artifact.review", json!({"summary": "review body"})),
        (
            22,
            "artifact.revision",
            json!({"summary": "revision body", "reply_to_artifact_id": parent_id.clone()}),
        ),
        (
            23,
            "artifact.decision",
            json!({"summary": "decision body", "why_chosen": "ends in fewer queries"}),
        ),
        (
            24,
            "artifact.verification",
            json!({
                "summary": "verification body",
                "reply_to_artifact_id": parent_id.clone(),
                "supports_claim": true
            }),
        ),
    ] {
        let mut args = extra;
        let obj = args.as_object_mut().unwrap();
        obj.insert("tenant_id".into(), json!("focused_http"));
        obj.insert("task_id".into(), json!(&task_id));
        obj.insert("agent_id".into(), json!(format!("agent-{}", tool)));
        let resp = call_tool(&url, id, tool, args).await;
        let artifact_id = resp["artifact_id"].as_str().unwrap();
        let fetched = call_tool(
            &url,
            id + 100,
            "artifact.get",
            json!({"tenant_id": "focused_http", "artifact_id": artifact_id}),
        )
        .await;
        let kind = fetched["artifact"]["artifact_kind"].as_str().unwrap();
        assert_eq!(
            kind,
            match tool {
                "artifact.review" => "review",
                "artifact.revision" => "revision",
                "artifact.decision" => "decision",
                "artifact.verification" => "verification",
                _ => unreachable!(),
            },
            "{} must inject its own artifact_kind; got {}",
            tool,
            kind
        );
    }
}

/// Phase 3.1 + 4.2: concurrent read/write on the shared daemon must
/// all succeed. Fires 8 interleaved memory.add + memory.search calls
/// against the same tenant and asserts every response is 200 with no
/// JSON-RPC error.
#[tokio::test]
async fn mixed_concurrency_over_http() {
    let (url, _handle) = spawn_test_http_daemon().await;
    initialize(&url).await;

    let mut handles = Vec::new();
    for i in 0..8 {
        let u = url.clone();
        handles.push(tokio::spawn(async move {
            call_tool(
                &u,
                1000 + i,
                "memory.add",
                json!({
                    "tenant_id": "concurrent_http",
                    "text": format!("concurrent payload {}", i),
                    "type": "doc"
                }),
            )
            .await
        }));
        let u = url.clone();
        handles.push(tokio::spawn(async move {
            call_tool(
                &u,
                2000 + i,
                "memory.search",
                json!({
                    "tenant_id": "concurrent_http",
                    "query": "concurrent",
                    "k": 5
                }),
            )
            .await
        }));
    }

    for handle in handles {
        let payload = handle.await.unwrap();
        assert!(
            payload.is_object() || payload.is_array(),
            "mixed concurrency response must be a structured JSON value"
        );
    }
}

/// Phase 1.5: `ping` is a request (not a notification) and must
/// return `{}` with the id echoed. Verify over the HTTP transport.
#[tokio::test]
async fn ping_over_http_returns_empty_object() {
    let (url, _handle) = spawn_test_http_daemon().await;
    initialize(&url).await;

    let (status, body) =
        jsonrpc_call(&url, json!({"jsonrpc": "2.0", "id": 999, "method": "ping"})).await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], json!(999));
    assert_eq!(body["result"], json!({}));
    assert!(body.get("error").is_none());
}

/// Phase 1.5: HTTP notifications (no `id`) must return 202 Accepted
/// with an empty body, even when the handler would produce an error.
/// No JSON-RPC error bodies on the wire for notifications.
#[tokio::test]
async fn notifications_over_http_return_202_no_body() {
    let (url, _handle) = spawn_test_http_daemon().await;
    initialize(&url).await;

    let url_clone = url.clone();
    let (status, body) = spawn_blocking(move || {
        let response = ureq::post(&url_clone)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .timeout(Duration::from_secs(5))
            .send_string(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .expect("post must succeed");
        let status = response.status();
        let text = response.into_string().unwrap_or_default();
        (status, text)
    })
    .await
    .unwrap();

    assert_eq!(
        status, 202,
        "JSON-RPC notifications must return 202 Accepted"
    );
    assert!(
        body.trim().is_empty(),
        "notification response must have an empty body; got {:?}",
        body
    );
}

/// Phase 4.1: spin the background digest sweeper at a fast interval,
/// write evidence via HTTP, and verify the sweeper drains the dirty
/// tracker without an explicit `memory.compact` call. Because the
/// dirty tracker is a process-global singleton, this test scopes its
/// assertion to a tenant/project combo no other test touches.
#[tokio::test]
async fn background_sweeper_drains_dirty_without_manual_compact() {
    // Force a very fast sweep interval for this test. Must be set
    // before spawning the daemon because it's read once at startup.
    // SAFETY: this is a single-threaded test and the env mutation
    // happens before the sweeper reads it.
    unsafe {
        std::env::set_var("MEMD_DIGEST_SWEEP_INTERVAL_SEC", "1");
    }
    let (url, _handle) = spawn_test_http_daemon().await;
    initialize(&url).await;

    // Pick tenant/project strings unique to this test so sibling
    // parallel-running tests do not race us on the tracker.
    let tenant = "sweeper_4_2_harness";
    let project = "sweeper_4_2_harness_proj";

    let start = call_tool(
        &url,
        30,
        "task.start",
        json!({
            "tenant_id": tenant,
            "project_id": project,
            "goal": "sweeper drain smoke",
            "agent_id": "author"
        }),
    )
    .await;
    let task_id = start["task_id"].as_str().unwrap().to_string();

    call_tool(
        &url,
        31,
        "task.add_evidence",
        json!({
            "tenant_id": tenant,
            "task_id": task_id,
            "project_id": project,
            "summary": "sweeper sentinel evidence",
            "evidence_kind": "integration_smoke",
            "supports_claim": true
        }),
    )
    .await;

    // Poll for up to 5s waiting for the sweeper to drain the three
    // digest keys (evidence/highlight/project_brief) we just flagged.
    let drained = timeout(Duration::from_secs(5), async {
        loop {
            let still_dirty_after_sweep = memd::task_memory::digest_dirty::global().contains(
                &memd::task_memory::digest_dirty::DigestDirtyKey {
                    tenant_id: tenant.to_string(),
                    project_id: Some(project.to_string()),
                    role: memd::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY.to_string(),
                },
            );
            if !still_dirty_after_sweep {
                return ();
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await;

    // Restore env for sibling tests.
    unsafe {
        std::env::remove_var("MEMD_DIGEST_SWEEP_INTERVAL_SEC");
    }
    drained.expect("background sweeper must drain the evidence-library dirty key");
}
