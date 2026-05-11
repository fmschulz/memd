//! Shared test helpers for integration tests that exercise local operations.
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
use memd::mcp::*;
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::types::{ChunkId, TenantId};
use memd::MetricsCollector;

pub struct TestServer<S> {
    _config: Config,
    store: Arc<S>,
}

impl<S> TestServer<S> {
    pub fn new(config: Config, store: Arc<S>) -> Self {
        Self {
            _config: config,
            store,
        }
    }

    pub fn store(&self) -> &S {
        self.store.as_ref()
    }
}

/// Open a fresh `PersistentStore` backed by the given data directory.
///
/// The store is wrapped in an `Arc` so integration tests can share it with
/// a freshly constructed `TestServer`. The caller owns the tempdir — it must
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

/// Spin up an isolated `TestServer<PersistentStore>` under a fresh tempdir.
///
/// Returns the server plus the `TempDir`. The caller MUST keep the
/// `TempDir` alive for the duration of the test — dropping it removes the
/// backing data directory while the store still references it.
pub async fn test_server() -> (Arc<TestServer<PersistentStore>>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = persistent_store(tmp.path()).await;
    let cfg = Config {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    (Arc::new(TestServer::new(cfg, store)), tmp)
}

/// Shorthand for `TenantId` construction in tests.
pub fn tenant(id: &str) -> TenantId {
    TenantId::new(id).expect("valid tenant id")
}

/// Invoke a local operation by name with the given arguments.
///
/// Returns the raw JSON-RPC response as `serde_json::Value`. Callers use
/// `parse_result_text` to unwrap the `content[0].text` JSON payload.
pub async fn call_tool<S: memd::store::Store>(
    server: &Arc<TestServer<S>>,
    name: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let result = dispatch_operation(server.store(), name, args).await;
    match result {
        Ok(value) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": value
        }),
        Err(error) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": error.code(),
                "message": error.to_string()
            }
        }),
    }
}

async fn dispatch_operation<S: memd::store::Store>(
    store: &S,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpError> {
    let metrics = MetricsCollector::default();

    match name {
        "memory.search" => handle_memory_search(store, parse_params(name, args)?).await,
        "memory.add" => handle_memory_add(store, None, parse_params(name, args)?).await,
        "memory.add_batch" => handle_memory_add_batch(store, None, parse_params(name, args)?).await,
        "memory.get" => handle_memory_get(store, parse_params(name, args)?).await,
        "memory.delete" => handle_memory_delete(store, parse_params(name, args)?).await,
        "memory.feedback" => handle_memory_feedback(store, parse_params(name, args)?).await,
        "memory.stats" => handle_memory_stats(store, None, parse_params(name, args)?).await,
        "memory.health" => handle_memory_health(store, &metrics, parse_params(name, args)?).await,
        "memory.metrics" => {
            let params: MetricsParams = parse_params(name, args)?;
            handle_memory_metrics(&metrics, store.get_index_stats(None), params)
        }
        "memory.compact" => handle_memory_compact(store, parse_params(name, args)?).await,
        "memory.dream" => handle_memory_dream(store, None, parse_params(name, args)?).await,
        "memory.supersede" => {
            let (value, _) =
                handle_memory_supersede(store, None, parse_params(name, args)?).await?;
            Ok(value)
        }
        "memory.set_expiry" => {
            handle_memory_set_expiry(store, None, parse_params(name, args)?).await
        }
        "memory.find_near_duplicates" => {
            handle_memory_find_near_duplicates(store, parse_params(name, args)?).await
        }
        "memory.export_markdown" => {
            handle_memory_export_markdown(store, parse_params(name, args)?).await
        }
        "memory.export_omf" => handle_memory_export_omf(store, parse_params(name, args)?).await,
        "memory.preview_omf_import" => {
            handle_memory_preview_omf_import(store, parse_params(name, args)?).await
        }
        "memory.import_omf" => {
            let (value, _) =
                handle_memory_import_omf(store, None, parse_params(name, args)?).await?;
            Ok(value)
        }
        "memory.consolidate_episode" => {
            handle_memory_consolidate_episode(store, parse_params(name, args)?).await
        }
        "task.start" => handle_task_start(store, None, parse_params(name, args)?).await,
        "task.progress" => handle_task_progress(store, None, parse_params(name, args)?).await,
        "task.run_start" => handle_task_run_start(store, None, parse_params(name, args)?).await,
        "task.run_finish" => handle_task_run_finish(store, None, parse_params(name, args)?).await,
        "task.add_evidence" => {
            handle_task_add_evidence(store, None, parse_params(name, args)?).await
        }
        "task.finish" => handle_task_finish(store, None, parse_params(name, args)?).await,
        "task.get" => handle_task_get(store, parse_params(name, args)?).await,
        "task.search" => handle_task_search(store, parse_params(name, args)?).await,
        "task.resume" => handle_task_resume(store, parse_params(name, args)?).await,
        "artifact.create" => handle_artifact_create(store, None, parse_params(name, args)?).await,
        "artifact.review" | "artifact.revision" | "artifact.decision" | "artifact.verification" => {
            let kind = match name {
                "artifact.review" => "review",
                "artifact.revision" => "revision",
                "artifact.decision" => "decision",
                "artifact.verification" => "verification",
                _ => unreachable!(),
            };
            let args = inject_artifact_kind(name, args, kind)?;
            handle_artifact_create(store, None, parse_params(name, args)?).await
        }
        "artifact.get" => handle_artifact_get(store, parse_params(name, args)?).await,
        "artifact.search" => handle_artifact_search(store, parse_params(name, args)?).await,
        "artifact.find_related" | "artifact.verify" => {
            handle_artifact_verify(store, parse_params(name, args)?).await
        }
        "artifact.find_failures" => {
            handle_artifact_find_failures(store, parse_params(name, args)?).await
        }
        "artifact.find_decisions" => {
            handle_artifact_find_decisions(store, parse_params(name, args)?).await
        }
        "artifact.find_evidence" => {
            handle_artifact_find_evidence(store, parse_params(name, args)?).await
        }
        "artifact.find_highlights" => {
            handle_artifact_find_highlights(store, parse_params(name, args)?).await
        }
        "artifact.list_thread" => {
            handle_artifact_list_thread(store, parse_params(name, args)?).await
        }
        _ => Err(McpError::MethodNotFound(format!(
            "unknown operation '{name}'"
        ))),
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(
    name: &str,
    args: serde_json::Value,
) -> Result<T, McpError> {
    serde_json::from_value(args)
        .map_err(|error| McpError::InvalidParams(format!("{name}: {error}")))
}

fn inject_artifact_kind(
    name: &str,
    mut args: serde_json::Value,
    kind: &str,
) -> Result<serde_json::Value, McpError> {
    let Some(obj) = args.as_object_mut() else {
        return Err(McpError::InvalidParams(format!(
            "{name}: expected object parameters"
        )));
    };
    if let Some(existing) = obj.get("artifact_kind") {
        if existing.as_str() != Some(kind) {
            return Err(McpError::InvalidParams(format!(
                "{name} forbids an overriding artifact_kind; got {existing}"
            )));
        }
    }
    obj.insert(
        "artifact_kind".to_string(),
        serde_json::Value::String(kind.to_string()),
    );
    Ok(args)
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
    server: &Arc<TestServer<S>>,
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
    server: &Arc<TestServer<S>>,
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
