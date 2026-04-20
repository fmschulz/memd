//! Shared test helpers for integration tests that exercise the full MCP server.
//!
//! Usage from any integration test under `crates/memd/tests/`:
//!
//! ```ignore
//! mod common;
//! use common::*;
//! ```
//!
//! All helpers are async-aware where needed. The module deliberately stays
//! small — anything specific to a single test suite should live next to that
//! suite's assertions, not here.
//!
//! Some helpers (e.g. `backdate_timestamp_created`) require `--features test-support`.

#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use memd::config::Config;
use memd::mcp::server::McpServer;
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::types::{ChunkId, TenantId};

/// Open a fresh `PersistentStore` backed by the given data directory.
///
/// The store is wrapped in an `Arc` so integration tests can share it with
/// a freshly constructed `McpServer`. The caller owns the tempdir — it must
/// outlive the returned store, otherwise the backing files disappear
/// underneath SQLite/segments.
///
/// NOTE: the real `PersistentStore::open` takes a `PersistentStoreConfig`
/// synchronously (not `Config`, and not `async`). We expose `async` here
/// anyway so call sites stay uniform even if construction ever becomes
/// asynchronous; the returned store is identical to calling `open` directly.
pub async fn persistent_store(data_dir: &Path) -> Arc<PersistentStore> {
    let config = PersistentStoreConfig {
        data_dir: data_dir.to_path_buf(),
        // Unit/integration tests run without the heavy dense + hybrid
        // initialisation so tempdir-backed stores spin up in milliseconds.
        // Tests that need vector search opt in by constructing their own
        // `PersistentStoreConfig` directly.
        enable_dense_search: false,
        enable_hybrid_search: false,
        // Suppress the spawn_startup_*_backfill side effects so per-test
        // state stays deterministic — otherwise the background passes
        // race with the test's own writes / metadata mutations.
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    };
    Arc::new(PersistentStore::open(config).expect("persistent store"))
}

/// Spin up an isolated `McpServer<PersistentStore>` under a fresh tempdir.
///
/// Returns the server plus the `TempDir`. The caller MUST keep the
/// `TempDir` alive for the duration of the test — dropping it removes the
/// backing data directory while the store still references it.
pub async fn test_server() -> (Arc<McpServer<PersistentStore>>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = persistent_store(tmp.path()).await;
    let cfg = Config {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    (Arc::new(McpServer::new(cfg, store)), tmp)
}

/// Shorthand for `TenantId` construction in tests.
pub fn tenant(id: &str) -> TenantId {
    TenantId::new(id).expect("valid tenant id")
}

/// Invoke an MCP tool by name with the given arguments.
///
/// Returns the raw JSON-RPC response as `serde_json::Value`. Callers use
/// `parse_result_text` to unwrap the `content[0].text` JSON payload.
///
/// NOTE: the server's typed `handle_request` / `handle_tools_call` methods
/// are module-private. The only public dispatch entry point is
/// `handle_jsonrpc(line: &str) -> Option<Response>`. We serialize the
/// request, drive it through that path, and deserialize the response back
/// to a `Value` so tests can assert against a plain JSON shape without
/// pulling in protocol types.
pub async fn call_tool<S: memd::store::Store>(
    server: &Arc<McpServer<S>>,
    name: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    let line = req.to_string();
    let response = server
        .handle_jsonrpc(&line)
        .await
        .expect("tools/call is a request, not a notification — expected Some(Response)");
    serde_json::to_value(&response).expect("response is serializable to JSON")
}

/// Unwrap the MCP content wrapper (`result.content[0].text` -> parsed JSON).
pub fn parse_result_text(resp: &serde_json::Value) -> serde_json::Value {
    let text = resp
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|i| i.get("text"))
        .and_then(|t| t.as_str())
        .expect("content[0].text");
    serde_json::from_str(text).expect("content text is valid JSON")
}

/// Extract a JSON-RPC error envelope from a tool response. Tests that expect
/// a failure path call this instead of `parse_result_text`.
pub fn parse_error(resp: &serde_json::Value) -> Option<(i64, String)> {
    let err = resp.get("error")?;
    let code = err.get("code")?.as_i64()?;
    let message = err.get("message")?.as_str()?.to_string();
    Some((code, message))
}

/// Add a chunk via `memory.add`. Returns the resulting `ChunkId`.
pub async fn add_chunk<S: memd::store::Store>(
    server: &Arc<McpServer<S>>,
    tenant_id: &str,
    text: &str,
) -> ChunkId {
    let r = call_tool(
        server,
        "memory.add",
        serde_json::json!({ "tenant_id": tenant_id, "text": text, "type": "doc" }),
    )
    .await;
    let id = parse_result_text(&r)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();
    ChunkId::parse(&id).expect("valid chunk id")
}

/// Add a chunk with an explicit `expires_at_ms`. Consumed by Track C tests.
///
/// The `expires_at_ms` argument is passed through to the handler even if
/// the schema doesn't accept it today — Task C1 widens the schema, and
/// this helper is consumed by C1's test. Until then the field is ignored
/// (or rejected) by the handler and the helper will surface that in test
/// assertions.
pub async fn add_with_expiry<S: memd::store::Store>(
    server: &Arc<McpServer<S>>,
    tenant_id: &str,
    text: &str,
    expires_at_ms: i64,
) -> ChunkId {
    let r = call_tool(
        server,
        "memory.add",
        serde_json::json!({
            "tenant_id": tenant_id,
            "text": text,
            "type": "doc",
            "expires_at_ms": expires_at_ms,
        }),
    )
    .await;
    let id = parse_result_text(&r)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();
    ChunkId::parse(&id).expect("valid chunk id")
}

/// Back-date a chunk's `timestamp_created` to `ts_ms` via direct SQL.
///
/// Used by Track C history-promotion tests to make the clock
/// deterministic. This is a TEST-ONLY helper — it wraps
/// `SqliteMetadataStore::force_timestamp_created` which is itself gated
/// behind the `test-support` feature on the `memd` crate. Consumers
/// must therefore run their tests with `--features test-support`
/// (e.g. `cargo test -p memd --features test-support`) to reach this
/// helper; otherwise the wrapper compiles out entirely.
#[cfg(feature = "test-support")]
pub fn backdate_timestamp_created(
    metadata: &memd::store::metadata::SqliteMetadataStore,
    chunk_id: &ChunkId,
    ts_ms: i64,
) -> memd::error::Result<()> {
    metadata.force_timestamp_created(chunk_id, ts_ms)
}
