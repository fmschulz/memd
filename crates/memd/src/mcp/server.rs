//! MCP server implementation
//!
//! Handles JSON-RPC communication over stdio and streamable HTTP transports.
//! This is the primary interface for agent integration.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::http::header::{CONTENT_TYPE, ORIGIN};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

use super::error::McpError;
use super::handlers::{
    handle_artifact_create, handle_artifact_find_decisions, handle_artifact_find_evidence,
    handle_artifact_find_failures, handle_artifact_find_highlights, handle_artifact_get,
    handle_artifact_list_thread, handle_artifact_search, handle_artifact_verify,
    handle_context_brief_project, handle_context_find_relevant_context,
    handle_context_get_files_for_subsystem, handle_context_get_hot_context,
    handle_context_list_subsystems, handle_context_search_documents, handle_context_suggest_agent,
    handle_find_callers, handle_find_definition, handle_find_errors, handle_find_imports,
    handle_find_references, handle_find_tool_calls, handle_memory_add, handle_memory_add_batch,
    handle_memory_compact, handle_memory_consolidate_episode, handle_memory_delete,
    handle_memory_feedback, handle_memory_get, handle_memory_metrics, handle_memory_search,
    handle_memory_set_expiry, handle_memory_stats, handle_memory_supersede,
    handle_task_add_evidence, handle_task_finish, handle_task_get, handle_task_progress,
    handle_task_resume, handle_task_run_finish, handle_task_run_start, handle_task_search,
    handle_task_start, AddBatchParams, AddParams, ArtifactCreateParams, ArtifactGetParams,
    ArtifactLibraryParams, ArtifactListThreadParams, ArtifactVerifyParams, CompactParams,
    ConsolidateEpisodeParams, ContextFindRelevantContextParams, ContextGetFilesForSubsystemParams,
    ContextGetHotContextParams, ContextListSubsystemsParams, ContextSearchDocumentsParams,
    ContextSuggestAgentParams, DeleteParams, FeedbackParams, FindCallersParams,
    FindDefinitionParams, FindErrorsParams, FindImportsParams, FindReferencesParams,
    FindToolCallsParams, GetParams, MetricsParams, ProjectBriefParams, SearchParams,
    SetExpiryParams, StatsParams, SupersedeParams, TaskAddEvidenceParams, TaskFinishParams,
    TaskGetParams, TaskProgressParams, TaskResumeParams, TaskRunFinishParams, TaskRunStartParams,
    TaskSearchParams, TaskStartParams,
};
use super::protocol::{Request, Response, RpcError};
use super::tools::get_all_tools;
use crate::metrics::MetricsCollector;
use crate::store::{Store, TenantManager};
use crate::structural::{
    CallGraphIndexer, CallGraphSymbolRecord, StructuralStore, SymbolIndexer, SymbolQueryService,
    TraceQueryService,
};
use crate::types::TenantId;
use crate::Config;

/// Default MCP protocol version for the stdio path and legacy docs.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// Protocol versions accepted by the HTTP daemon for current Codex/Claude clients.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-11-25"];

/// Server name for capability negotiation
const SERVER_NAME: &str = "memd";

/// Server version
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

/// Shared state for the HTTP transport.
///
/// Phase 3.1: the server itself is wrapped in a plain `Arc`, not an
/// `Arc<AsyncMutex<_>>`. Every handler path on `McpServer` takes
/// `&self`; mutable state (`initialized`) is an `AtomicBool`, and the
/// actual storage layer (`Arc<S>` + internal locks) handles its own
/// concurrency. Removing the outer mutex is the prerequisite for
/// making the SQLite pool (3.3) buy anything.
struct HttpServerState<S: Store> {
    server: Arc<McpServer<S>>,
}

impl<S: Store> Clone for HttpServerState<S> {
    fn clone(&self) -> Self {
        Self {
            server: Arc::clone(&self.server),
        }
    }
}

/// MCP server that handles JSON-RPC requests over stdio
///
/// Phase 3.1: the server is designed to be shared across concurrent
/// HTTP clients via `Arc<McpServer<S>>` — no outer mutex. The only
/// field that changes after construction is `initialized`, which is an
/// `AtomicBool` so multiple requests can observe/update it without
/// serialization. All other mutable state is already behind internal
/// synchronization (`Arc`s or the store's own locking).
pub struct McpServer<S: Store> {
    config: Config,
    store: Arc<S>,
    tenant_manager: Option<TenantManager>,
    metrics: Arc<MetricsCollector>,
    structural_store: Option<Arc<StructuralStore>>,
    symbol_indexer: Option<Arc<SymbolIndexer>>,
    call_graph_indexer: Option<Arc<CallGraphIndexer>>,
    symbol_query_service: Option<Arc<SymbolQueryService>>,
    trace_query_service: Option<Arc<TraceQueryService>>,
    initialized: std::sync::atomic::AtomicBool,
}

impl<S: Store> McpServer<S> {
    /// Borrow the concrete store behind Arc<S>.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Create a new MCP server with the given configuration and store
    pub fn new(config: Config, store: Arc<S>) -> Self {
        // Create tenant manager from config data_dir
        let tenant_manager = config.data_dir_expanded().ok().map(TenantManager::new);

        // Apply server-level policies to the handler module. See
        // `handlers::set_cross_tenant_project_fallback` for rationale.
        // Skipped in `cfg(test)` builds so unit tests that deliberately
        // toggle the flag are not stomped by incidental server
        // construction in unrelated tests.
        #[cfg(not(test))]
        super::handlers::set_cross_tenant_project_fallback(
            config.server.allow_cross_tenant_project_fallback,
        );

        Self {
            config,
            store,
            tenant_manager,
            metrics: Arc::new(MetricsCollector::default()),
            structural_store: None,
            symbol_indexer: None,
            call_graph_indexer: None,
            symbol_query_service: None,
            trace_query_service: None,
            initialized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Create a new MCP server with custom metrics collector
    pub fn with_metrics(config: Config, store: Arc<S>, metrics: Arc<MetricsCollector>) -> Self {
        let tenant_manager = config.data_dir_expanded().ok().map(TenantManager::new);

        #[cfg(not(test))]
        super::handlers::set_cross_tenant_project_fallback(
            config.server.allow_cross_tenant_project_fallback,
        );

        Self {
            config,
            store,
            tenant_manager,
            metrics,
            structural_store: None,
            symbol_indexer: None,
            call_graph_indexer: None,
            symbol_query_service: None,
            trace_query_service: None,
            initialized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Set the structural store and indexers used to keep code navigation data current.
    pub fn with_structural_indexers(
        mut self,
        store: Arc<StructuralStore>,
        symbol_indexer: Arc<SymbolIndexer>,
        call_graph_indexer: Arc<CallGraphIndexer>,
    ) -> Self {
        self.structural_store = Some(store);
        self.symbol_indexer = Some(symbol_indexer);
        self.call_graph_indexer = Some(call_graph_indexer);
        self
    }

    /// Set the symbol query service for code navigation tools
    pub fn with_symbol_query_service(mut self, service: Arc<SymbolQueryService>) -> Self {
        self.symbol_query_service = Some(service);
        self
    }

    /// Set the trace query service for debugging tools
    pub fn with_trace_query_service(mut self, service: Arc<TraceQueryService>) -> Self {
        self.trace_query_service = Some(service);
        self
    }

    fn maybe_index_structural_chunk(
        &self,
        tenant_id: &str,
        project_id: Option<&str>,
        chunk_type: &str,
        source_path: Option<&str>,
        text: &str,
    ) {
        if !chunk_type.eq_ignore_ascii_case("code") {
            return;
        }
        let Some(source_path) = source_path.filter(|value| !value.trim().is_empty()) else {
            return;
        };
        let Some(structural_store) = self.structural_store.as_ref() else {
            return;
        };
        let Some(symbol_indexer) = self.symbol_indexer.as_ref() else {
            return;
        };
        let Some(call_graph_indexer) = self.call_graph_indexer.as_ref() else {
            return;
        };

        let path = Path::new(source_path);
        if crate::structural::detect_language(path).is_none() {
            return;
        }

        let tenant_id = match TenantId::new(tenant_id) {
            Ok(tenant_id) => tenant_id,
            Err(error) => {
                warn!(
                    tenant_id = tenant_id,
                    source_path = source_path,
                    error = %error,
                    "skipping structural indexing because tenant validation failed"
                );
                return;
            }
        };

        let parsed = match crate::structural::parse_file(path, text) {
            Ok(parsed) => parsed,
            Err(error) => {
                warn!(
                    tenant_id = %tenant_id,
                    source_path = source_path,
                    error = %error,
                    "skipping structural indexing because parsing failed"
                );
                return;
            }
        };

        if let Err(error) = symbol_indexer.index_file(
            &tenant_id,
            project_id,
            source_path,
            &parsed.tree,
            text.as_bytes(),
            parsed.language,
        ) {
            warn!(
                tenant_id = %tenant_id,
                source_path = source_path,
                error = %error,
                "skipping structural indexing because symbol indexing failed"
            );
            return;
        }

        let file_symbols = match structural_store.find_symbols_by_file(&tenant_id, source_path) {
            Ok(symbols) => symbols,
            Err(error) => {
                warn!(
                    tenant_id = %tenant_id,
                    source_path = source_path,
                    error = %error,
                    "skipping structural indexing because symbol lookup failed"
                );
                return;
            }
        };

        let call_graph_symbols = file_symbols
            .iter()
            .filter_map(|symbol| {
                symbol.symbol_id.map(|symbol_id| CallGraphSymbolRecord {
                    symbol_id,
                    name: symbol.name.clone(),
                    start_line: symbol.line_start,
                    end_line: symbol.line_end,
                })
            })
            .collect::<Vec<_>>();

        if let Err(error) = call_graph_indexer.index_file(
            &tenant_id,
            source_path,
            &parsed.tree,
            text.as_bytes(),
            parsed.language,
            &call_graph_symbols,
        ) {
            warn!(
                tenant_id = %tenant_id,
                source_path = source_path,
                error = %error,
                "skipping structural indexing because call graph indexing failed"
            );
        }
    }

    /// Get reference to metrics collector
    pub fn metrics(&self) -> &MetricsCollector {
        &self.metrics
    }

    /// Run the server loop, reading from stdin and writing to stdout
    ///
    /// This is the main event loop. It reads JSON-RPC requests line by line
    /// from stdin, processes them, and writes responses to stdout.
    pub async fn run(&mut self) -> crate::Result<()> {
        info!("MCP server starting");

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    error!("failed to read from stdin: {}", e);
                    break;
                }
            };

            // Skip empty lines
            if line.trim().is_empty() {
                continue;
            }

            debug!(request = %line, "received request");

            // Parse and handle the request. `None` means the frame was a
            // notification (no `id`) and the JSON-RPC spec forbids a reply.
            let response = match self.handle_jsonrpc(&line).await {
                Some(r) => r,
                None => {
                    debug!("notification handled, no response emitted");
                    continue;
                }
            };

            // Serialize and write response
            let json = match response.to_json() {
                Ok(j) => j,
                Err(e) => {
                    error!("failed to serialize response: {}", e);
                    continue;
                }
            };

            debug!(response = %json, "sending response");

            if writeln!(stdout, "{}", json).is_err() {
                error!("failed to write to stdout");
                break;
            }

            if stdout.flush().is_err() {
                error!("failed to flush stdout");
                break;
            }
        }

        info!("MCP server shutting down");
        Ok(())
    }

    /// Handle a single line of input (one JSON-RPC request).
    ///
    /// Returns `None` when the incoming message is a valid JSON-RPC
    /// notification (no `id`) — callers MUST NOT write anything back for
    /// notifications, per the JSON-RPC 2.0 spec. Parse errors always produce
    /// a response with `id = null` because we cannot know whether the
    /// unparseable frame was a notification.
    pub async fn handle_jsonrpc(&self, line: &str) -> Option<Response> {
        // Try to parse the request
        let request = match Request::parse(line) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "failed to parse request");
                return Some(Response::error(None, e.into()));
            }
        };

        let is_notification = request.is_notification();
        let response = self.handle_request(request).await;

        if is_notification {
            // JSON-RPC 2.0 §4.1: notifications MUST NOT receive any response.
            // Any error we produced while processing a notification is
            // logged at the handler but swallowed here.
            if let Some(ref error) = response.error {
                warn!(
                    code = error.code,
                    message = %error.message,
                    "suppressing error response for notification"
                );
            }
            return None;
        }

        Some(response)
    }

    /// Handle a parsed JSON-RPC request
    async fn handle_request(&self, request: Request) -> Response {
        let id = request.id.clone();

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params).await,
            // MCP `ping` is a request (not a notification) and expects an
            // empty object `{}` as the result with the caller's id echoed.
            "ping" => Ok(Value::Object(serde_json::Map::new())),
            "initialized" | "notifications/initialized" | "notifications/cancelled" => {
                // Client-originated notifications. The spec forbids a
                // response; we still produce a placeholder Response so
                // `handle_request`'s caller can keep a uniform type. The
                // stdio/HTTP dispatchers drop the value when the incoming
                // request was a notification.
                Ok(Value::Null)
            }
            "tools/list" => self.handle_tools_list().await,
            "tools/call" => self.handle_tools_call(request.params).await,
            "shutdown" => {
                info!("shutdown requested");
                Ok(Value::Null)
            }
            method => {
                warn!(method = %method, "unknown method");
                Err(McpError::MethodNotFound(format!(
                    "method '{}' not found",
                    method
                )))
            }
        };

        match result {
            Ok(value) => Response::success(id, value),
            Err(e) => Response::error(id, e.into()),
        }
    }

    /// Handle the 'initialize' request
    ///
    /// Returns server capabilities and protocol version.
    async fn handle_initialize(&self, params: Option<Value>) -> Result<Value, McpError> {
        use std::sync::atomic::Ordering;
        if self.initialized.swap(true, Ordering::AcqRel) {
            warn!("server already initialized");
        }

        let protocol_version = negotiate_protocol_version(params.as_ref());

        // Note on writer identity: in v0.3.1 we deliberately do NOT
        // propagate `clientInfo` into a process-global default
        // `agent_id`. McpServer is shared across all HTTP clients
        // behind an `Arc<AsyncMutex<_>>`; a shared default would let
        // one session overwrite another's identity and bypass the
        // distinct-writer countersignature rule introduced in 1.1.
        // Per-session identity (auto-populated without the bleed
        // hazard) lands in Phase 2 with the HTTP session model. For
        // v0.3.1, agent identity is caller-supplied: tools that want
        // countersignature promotion must pass an explicit
        // `agent_id`. We still log the advertised client identity here
        // so operators can see who connected.
        let advertised_client = params
            .as_ref()
            .and_then(|p| p.get("clientInfo"))
            .map(derive_client_agent_id);

        info!(
            protocol_version = protocol_version,
            server_name = SERVER_NAME,
            server_version = SERVER_VERSION,
            advertised_client = ?advertised_client,
            "server initialized"
        );

        Ok(json!({
            "protocolVersion": protocol_version,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        }))
    }

    /// Handle the 'tools/list' request
    ///
    /// Returns all available tool definitions.
    async fn handle_tools_list(&self) -> Result<Value, McpError> {
        let tools = get_all_tools();

        let tool_list: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema
                })
            })
            .collect();

        Ok(json!({
            "tools": tool_list
        }))
    }

    /// Handle the 'tools/call' request
    ///
    /// Dispatches to the appropriate tool handler using the actual store.
    async fn handle_tools_call(&self, params: Option<Value>) -> Result<Value, McpError> {
        let params = params.ok_or_else(|| McpError::InvalidParams("missing params".to_string()))?;

        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing 'name' field".to_string()))?;

        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

        info!(tool = %name, "tool call received");

        // Phase 4.4: bind the dispatch result so we can record a
        // rejection metric when any tool handler returns an error.
        // `record_rejection` is cheap (two atomic bumps + one
        // HashMap entry) and gives operators a per-tool / per-reason
        // count in `memory.metrics`.
        let tool_name_for_metrics = name.to_string();
        let dispatch_result = async {
            // Dispatch to tool handlers
            match name {
                "memory.search" => {
                    let params: SearchParams = serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid search params: {}", e))
                    })?;
                    handle_memory_search(&*self.store, params).await
                }
                "memory.add" => {
                    let params: AddParams = serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid add params: {}", e))
                    })?;
                    let tenant_id = params.tenant_id.clone();
                    let project_id = params.project_id.clone();
                    let chunk_type = params.chunk_type.clone();
                    let source_path = params
                        .source
                        .as_ref()
                        .and_then(|source| source.path.as_deref())
                        .map(str::to_string);
                    let text = params.text.clone();
                    let response =
                        handle_memory_add(&*self.store, self.tenant_manager.as_ref(), params)
                            .await?;
                    self.maybe_index_structural_chunk(
                        &tenant_id,
                        project_id.as_deref(),
                        &chunk_type,
                        source_path.as_deref(),
                        &text,
                    );
                    Ok(response)
                }
                "memory.add_batch" => {
                    let params: AddBatchParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid add_batch params: {}", e))
                        })?;
                    let tenant_id = params.tenant_id.clone();
                    let chunks_to_index = params
                        .chunks
                        .iter()
                        .map(|chunk| {
                            (
                                chunk.project_id.clone(),
                                chunk.chunk_type.clone(),
                                chunk.source.as_ref().and_then(|source| source.path.clone()),
                                chunk.text.clone(),
                            )
                        })
                        .collect::<Vec<_>>();
                    let response =
                        handle_memory_add_batch(&*self.store, self.tenant_manager.as_ref(), params)
                            .await?;
                    for (project_id, chunk_type, source_path, text) in chunks_to_index {
                        self.maybe_index_structural_chunk(
                            &tenant_id,
                            project_id.as_deref(),
                            &chunk_type,
                            source_path.as_deref(),
                            &text,
                        );
                    }
                    Ok(response)
                }
                "task.start" => {
                    let params: TaskStartParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid task.start params: {}", e))
                        })?;
                    handle_task_start(&*self.store, self.tenant_manager.as_ref(), params).await
                }
                "task.progress" => {
                    let params: TaskProgressParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid task.progress params: {}", e))
                        })?;
                    handle_task_progress(&*self.store, self.tenant_manager.as_ref(), params).await
                }
                "task.run_start" => {
                    let params: TaskRunStartParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid task.run_start params: {}", e))
                        })?;
                    handle_task_run_start(&*self.store, self.tenant_manager.as_ref(), params).await
                }
                "task.run_finish" => {
                    let params: TaskRunFinishParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid task.run_finish params: {}",
                                e
                            ))
                        })?;
                    handle_task_run_finish(&*self.store, self.tenant_manager.as_ref(), params).await
                }
                "task.add_evidence" => {
                    let params: TaskAddEvidenceParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid task.add_evidence params: {}",
                                e
                            ))
                        })?;
                    handle_task_add_evidence(&*self.store, self.tenant_manager.as_ref(), params)
                        .await
                }
                "task.finish" => {
                    let params: TaskFinishParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid task.finish params: {}", e))
                        })?;
                    handle_task_finish(&*self.store, self.tenant_manager.as_ref(), params).await
                }
                "task.get" => {
                    let params: TaskGetParams = serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid task.get params: {}", e))
                    })?;
                    handle_task_get(&*self.store, params).await
                }
                "task.search" => {
                    let params: TaskSearchParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid task.search params: {}", e))
                        })?;
                    handle_task_search(&*self.store, params).await
                }
                "task.resume" => {
                    let params: TaskResumeParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid task.resume params: {}", e))
                        })?;
                    handle_task_resume(&*self.store, params).await
                }
                "artifact.create" => {
                    warn!(
                        "artifact.create is deprecated; prefer the focused tools \
                     artifact.review / artifact.revision / artifact.decision / \
                     artifact.verification, which expose small per-kind schemas"
                    );
                    let params: ArtifactCreateParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.create params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_create(&*self.store, self.tenant_manager.as_ref(), params).await
                }
                // Phase 2.3 shortcuts: thin wrappers over `artifact.create`
                // with the `artifact_kind` fixed and a tight per-kind
                // schema. Any additional fields in the argument map flow
                // through to `ArtifactCreateParams` via serde(default).
                "artifact.review"
                | "artifact.revision"
                | "artifact.decision"
                | "artifact.verification" => {
                    let kind = match name {
                        "artifact.review" => "review",
                        "artifact.revision" => "revision",
                        "artifact.decision" => "decision",
                        "artifact.verification" => "verification",
                        _ => unreachable!(),
                    };
                    let mut arguments = arguments;
                    // Inject `artifact_kind` so the shared handler sees the
                    // right variant. If the caller supplied a conflicting
                    // `artifact_kind`, we reject rather than silently
                    // reinterpret — this keeps the intent tool-driven.
                    if let Some(obj) = arguments.as_object_mut() {
                        if let Some(existing) = obj.get("artifact_kind") {
                            if existing.as_str() != Some(kind) {
                                return Err(McpError::InvalidParams(format!(
                                    "{} forbids an overriding artifact_kind; got {}",
                                    name, existing
                                )));
                            }
                        }
                        obj.insert("artifact_kind".to_string(), Value::String(kind.to_string()));
                    }
                    let params: ArtifactCreateParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid {} params: {}", name, e))
                        })?;
                    handle_artifact_create(&*self.store, self.tenant_manager.as_ref(), params).await
                }
                "artifact.get" => {
                    let params: ArtifactGetParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid artifact.get params: {}", e))
                        })?;
                    handle_artifact_get(&*self.store, params).await
                }
                "artifact.search" => {
                    let params: TaskSearchParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.search params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_search(&*self.store, params).await
                }
                "artifact.find_related" => {
                    let params: ArtifactVerifyParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.find_related params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_verify(&*self.store, params).await
                }
                "artifact.verify" => {
                    warn!(
                        "artifact.verify is deprecated; use artifact.find_related. \
                     Note: the underlying implementation is substring retrieval, \
                     not true verification — a hit does not imply grounded support."
                    );
                    let params: ArtifactVerifyParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.verify params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_verify(&*self.store, params).await
                }
                "artifact.find_failures" => {
                    let params: ArtifactLibraryParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.find_failures params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_find_failures(&*self.store, params).await
                }
                "artifact.find_decisions" => {
                    let params: ArtifactLibraryParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.find_decisions params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_find_decisions(&*self.store, params).await
                }
                "artifact.find_evidence" => {
                    let params: ArtifactLibraryParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.find_evidence params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_find_evidence(&*self.store, params).await
                }
                "artifact.find_highlights" => {
                    let params: ArtifactLibraryParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.find_highlights params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_find_highlights(&*self.store, params).await
                }
                "artifact.list_thread" => {
                    let params: ArtifactListThreadParams = serde_json::from_value(arguments)
                        .map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid artifact.list_thread params: {}",
                                e
                            ))
                        })?;
                    handle_artifact_list_thread(&*self.store, params).await
                }
                "memory.get" => {
                    let params: GetParams = serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid get params: {}", e))
                    })?;
                    handle_memory_get(&*self.store, params).await
                }
                "memory.delete" => {
                    let params: DeleteParams = serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid delete params: {}", e))
                    })?;
                    handle_memory_delete(&*self.store, params).await
                }
                "memory.feedback" => {
                    let params: FeedbackParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid feedback params: {}", e))
                        })?;
                    handle_memory_feedback(&*self.store, params).await
                }
                "memory.stats" => {
                    let params: StatsParams = serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid stats params: {}", e))
                    })?;
                    handle_memory_stats(&*self.store, self.tenant_manager.as_ref(), params).await
                }
                "memory.metrics" => {
                    let params: MetricsParams = serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid metrics params: {}", e))
                    })?;
                    let index_stats = self.store.get_index_stats(None);
                    handle_memory_metrics(&self.metrics, index_stats, params)
                }
                "memory.compact" => {
                    let params: CompactParams = serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid compact params: {}", e))
                    })?;
                    handle_memory_compact(&*self.store, params).await
                }
                "memory.supersede" => {
                    let params: SupersedeParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid supersede params: {}",
                                e
                            ))
                        })?;
                    let (response, event) = handle_memory_supersede(
                        &*self.store,
                        self.tenant_manager.as_ref(),
                        params,
                    )
                    .await?;
                    // Run structural indexing for the newly-written
                    // chunk, mirroring memory.add. The handler returns
                    // the event so the dispatch arm stays the single
                    // place that owns post-write side effects.
                    self.maybe_index_structural_chunk(
                        &event.tenant_id,
                        event.project_id.as_deref(),
                        &event.chunk_type,
                        event.source_path.as_deref(),
                        &event.text,
                    );
                    Ok(response)
                }
                "memory.set_expiry" => {
                    let params: SetExpiryParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid set_expiry params: {}", e))
                        })?;
                    handle_memory_set_expiry(&*self.store, params).await
                }
                "memory.consolidate_episode" => {
                    let params: ConsolidateEpisodeParams = serde_json::from_value(arguments)
                        .map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid consolidate_episode params: {}",
                                e
                            ))
                        })?;
                    handle_memory_consolidate_episode(&*self.store, params).await
                }
                "context.list_subsystems" => {
                    let params: ContextListSubsystemsParams = serde_json::from_value(arguments)
                        .map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid list_subsystems params: {}",
                                e
                            ))
                        })?;
                    handle_context_list_subsystems(&*self.store, params).await
                }
                "context.get_files_for_subsystem" => {
                    let params: ContextGetFilesForSubsystemParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid get_files_for_subsystem params: {}",
                                e
                            ))
                        })?;
                    handle_context_get_files_for_subsystem(&*self.store, params).await
                }
                "context.search_context_documents" => {
                    let params: ContextSearchDocumentsParams = serde_json::from_value(arguments)
                        .map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid search_context_documents params: {}",
                                e
                            ))
                        })?;
                    handle_context_search_documents(&*self.store, params).await
                }
                "context.find_relevant_context" => {
                    let params: ContextFindRelevantContextParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid find_relevant_context params: {}",
                                e
                            ))
                        })?;
                    handle_context_find_relevant_context(&*self.store, params).await
                }
                "context.brief_project" => {
                    let params: ProjectBriefParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid context.brief_project params: {}",
                                e
                            ))
                        })?;
                    handle_context_brief_project(&*self.store, params).await
                }
                "context.suggest_agent" => {
                    let params: ContextSuggestAgentParams = serde_json::from_value(arguments)
                        .map_err(|e| {
                            McpError::InvalidParams(format!("invalid suggest_agent params: {}", e))
                        })?;
                    handle_context_suggest_agent(&*self.store, params).await
                }
                "context.get_hot_context" => {
                    let params: ContextGetHotContextParams = serde_json::from_value(arguments)
                        .map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid get_hot_context params: {}",
                                e
                            ))
                        })?;
                    handle_context_get_hot_context(&*self.store, params).await
                }
                "code.find_definition" => {
                    let params: FindDefinitionParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid find_definition params: {}",
                                e
                            ))
                        })?;
                    let query_service = self.symbol_query_service.as_ref().ok_or_else(|| {
                        McpError::ToolError("Structural index not initialized".to_string())
                    })?;
                    handle_find_definition(query_service, params)
                }
                "code.find_references" => {
                    let params: FindReferencesParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid find_references params: {}",
                                e
                            ))
                        })?;
                    let query_service = self.symbol_query_service.as_ref().ok_or_else(|| {
                        McpError::ToolError("Structural index not initialized".to_string())
                    })?;
                    handle_find_references(query_service, params)
                }
                "code.find_callers" => {
                    let params: FindCallersParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid find_callers params: {}", e))
                        })?;
                    let query_service = self.symbol_query_service.as_ref().ok_or_else(|| {
                        McpError::ToolError("Structural index not initialized".to_string())
                    })?;
                    handle_find_callers(query_service, params)
                }
                "code.find_imports" => {
                    let params: FindImportsParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid find_imports params: {}", e))
                        })?;
                    let query_service = self.symbol_query_service.as_ref().ok_or_else(|| {
                        McpError::ToolError("Structural index not initialized".to_string())
                    })?;
                    handle_find_imports(query_service, params)
                }
                "debug.find_tool_calls" => {
                    let params: FindToolCallsParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!(
                                "invalid find_tool_calls params: {}",
                                e
                            ))
                        })?;
                    let trace_service = self.trace_query_service.as_ref().ok_or_else(|| {
                        McpError::ToolError("Trace index not initialized".to_string())
                    })?;
                    handle_find_tool_calls(trace_service, params)
                }
                "debug.find_errors" => {
                    let params: FindErrorsParams =
                        serde_json::from_value(arguments).map_err(|e| {
                            McpError::InvalidParams(format!("invalid find_errors params: {}", e))
                        })?;
                    let trace_service = self.trace_query_service.as_ref().ok_or_else(|| {
                        McpError::ToolError("Trace index not initialized".to_string())
                    })?;
                    handle_find_errors(trace_service, params)
                }
                // Codex Phase 4 nit: classify unknown tool names as
                // `MethodNotFound` so the `rejections.by_reason`
                // metric separates "bad params for known tool" from
                // "tool name does not exist". Previously both landed
                // in `invalid-params`.
                _ => Err(McpError::MethodNotFound(format!(
                    "unknown tool '{}'",
                    name
                ))),
            }
        }
        .await;

        if let Err(ref err) = dispatch_result {
            self.metrics
                .record_rejection(&tool_name_for_metrics, err.reason_label());
        }
        dispatch_result
    }

    /// Get a reference to the config
    #[allow(dead_code)]
    pub fn config(&self) -> &Config {
        &self.config
    }
}

fn negotiate_protocol_version(params: Option<&Value>) -> &'static str {
    let requested = params
        .and_then(|value| value.get("protocolVersion"))
        .and_then(Value::as_str);

    match requested {
        Some(version) => SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .find(|candidate| **candidate == version)
            .copied()
            .unwrap_or_else(|| {
                SUPPORTED_PROTOCOL_VERSIONS
                    .last()
                    .copied()
                    .unwrap_or(DEFAULT_PROTOCOL_VERSION)
            }),
        None => DEFAULT_PROTOCOL_VERSION,
    }
}

fn is_supported_protocol_version(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

/// Build an `agent_id` default from MCP `initialize`'s `clientInfo`.
///
/// Format: `{name}@{version}`. When the client omits either field, fall
/// back to the first field present, or `mcp-client` as a last resort. The
/// returned value is used only when a tool call does not carry an
/// explicit `agent_id`.
fn derive_client_agent_id(client_info: &Value) -> String {
    let name = client_info
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let version = client_info
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match (name, version) {
        (Some(n), Some(v)) => format!("{}@{}", n, v),
        (Some(n), None) => n.to_string(),
        (None, Some(v)) => format!("mcp-client@{}", v),
        (None, None) => "mcp-client".to_string(),
    }
}

fn validate_http_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(origin) = headers.get(ORIGIN) else {
        return Ok(());
    };

    let Ok(origin) = origin.to_str() else {
        return Err(StatusCode::FORBIDDEN);
    };

    if origin == "null" {
        return Ok(());
    }

    let allowed = [
        "http://localhost",
        "https://localhost",
        "http://127.0.0.1",
        "https://127.0.0.1",
        "http://[::1]",
        "https://[::1]",
    ];
    if allowed.iter().any(|prefix| origin.starts_with(prefix)) {
        return Ok(());
    }

    Err(StatusCode::FORBIDDEN)
}

fn response_with_headers(
    status: StatusCode,
    content_type: Option<&'static str>,
    body: String,
    protocol_version: Option<&str>,
) -> HttpResponse {
    let mut response = (status, body).into_response();
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    if let Some(protocol_version) = protocol_version {
        if let Ok(value) = HeaderValue::from_str(protocol_version) {
            response
                .headers_mut()
                .insert(HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER), value);
        }
    }
    response
}

fn json_error_http_response(
    status: StatusCode,
    error: RpcError,
    protocol_version: Option<&str>,
) -> HttpResponse {
    let json = Response::error(None, error).to_json().unwrap_or_else(|_| {
        "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32603,\"message\":\"internal error\"}}"
            .to_string()
    });
    response_with_headers(status, Some("application/json"), json, protocol_version)
}

async fn handle_http_post<S: Store + Send + Sync + 'static>(
    State(state): State<HttpServerState<S>>,
    headers: HeaderMap,
    body: String,
) -> HttpResponse {
    if let Err(status) = validate_http_origin(&headers) {
        return response_with_headers(status, None, String::new(), None);
    }

    if let Some(protocol_version) = headers
        .get(MCP_PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        if !is_supported_protocol_version(protocol_version) {
            return json_error_http_response(
                StatusCode::BAD_REQUEST,
                RpcError::invalid_params(format!(
                    "unsupported MCP protocol version '{}'",
                    protocol_version
                )),
                None,
            );
        }
    }

    let rpc_value: Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(e) => {
            return json_error_http_response(
                StatusCode::BAD_REQUEST,
                RpcError::parse_error(e.to_string()),
                None,
            );
        }
    };

    let protocol_version = match rpc_value.get("method").and_then(Value::as_str) {
        Some("initialize") => Some(negotiate_protocol_version(rpc_value.get("params"))),
        _ => headers
            .get(MCP_PROTOCOL_VERSION_HEADER)
            .and_then(|value| value.to_str().ok()),
    };

    if rpc_value.get("method").is_some() {
        let request = match serde_json::from_value::<Request>(rpc_value.clone()) {
            Ok(request) => request,
            Err(e) => {
                return json_error_http_response(
                    StatusCode::BAD_REQUEST,
                    RpcError::invalid_request(e.to_string()),
                    protocol_version,
                );
            }
        };

        let is_notification = request.is_notification();
        // Phase 3.1: dispatch directly against the shared `Arc<McpServer>`.
        // Prior to this, every HTTP request serialized on an outer
        // `AsyncMutex<McpServer>`, which meant the SQLite pool (3.3)
        // and anything else inside the handler had no opportunity to
        // parallelize across clients. `handle_request` now takes
        // `&self`; the handlers either use their own internal locks
        // (the store is `Arc<S>`) or pure atomics.
        let response = state.server.handle_request(request).await;

        if is_notification {
            // JSON-RPC 2.0 §4.1: notifications MUST NOT receive any
            // response, not even an error. Log at `warn!` if the
            // handler produced an error object so operators still see
            // the issue, but return 202 Accepted with an empty body.
            if let Some(error) = &response.error {
                warn!(
                    code = error.code,
                    message = %error.message,
                    "suppressing error response for HTTP notification"
                );
            }
            return response_with_headers(
                StatusCode::ACCEPTED,
                None,
                String::new(),
                protocol_version,
            );
        }

        let json = match response.to_json() {
            Ok(json) => json,
            Err(e) => {
                return json_error_http_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    RpcError::internal_error(e.to_string()),
                    protocol_version,
                );
            }
        };

        return response_with_headers(
            StatusCode::OK,
            Some("application/json"),
            json,
            protocol_version,
        );
    }

    if rpc_value.get("id").is_some()
        && (rpc_value.get("result").is_some() || rpc_value.get("error").is_some())
    {
        return response_with_headers(StatusCode::ACCEPTED, None, String::new(), protocol_version);
    }

    json_error_http_response(
        StatusCode::BAD_REQUEST,
        RpcError::invalid_request("expected a JSON-RPC request, notification, or response"),
        protocol_version,
    )
}

async fn handle_http_get(headers: HeaderMap) -> HttpResponse {
    if let Err(status) = validate_http_origin(&headers) {
        return response_with_headers(status, None, String::new(), None);
    }

    response_with_headers(StatusCode::METHOD_NOT_ALLOWED, None, String::new(), None)
}

pub async fn run_http_server<S: Store + Send + Sync + 'static>(
    server: McpServer<S>,
    bind: &str,
    path: &str,
) -> crate::Result<()> {
    let listener = TcpListener::bind(bind).await?;
    serve_http_server(listener, server, path).await
}

/// Serve the MCP HTTP endpoint on a pre-bound `TcpListener`.
///
/// Exposed as `pub` so external integration tests (and alternative
/// bring-your-own-listener setups like systemd socket activation)
/// can reuse the exact same serve loop that `run_http_server` uses.
pub async fn serve_http_server<S: Store + Send + Sync + 'static>(
    listener: TcpListener,
    server: McpServer<S>,
    path: &str,
) -> crate::Result<()> {
    // NOTE: memd does not implement Server-Sent Events; responses are always
    // `application/json`. We used to advertise `Accept: text/event-stream`
    // here which confused strict clients that then negotiated for streaming.
    info!(
        bind = %listener.local_addr().map(|addr| addr.to_string()).unwrap_or_else(|_| "<unknown>".to_string()),
        path = path,
        "HTTP MCP server starting"
    );

    let state = HttpServerState {
        server: Arc::new(server),
    };

    // Phase 4.1: spawn the background digest sweeper. Interval
    // resolves from `$MEMD_DIGEST_SWEEP_INTERVAL_SEC` (default 10s;
    // 0 disables). The handle binds to this scope — when
    // `serve_http_server` returns (axum shutdown), the sweeper's
    // `Drop` aborts the background task so it does not leak past
    // the server's lifetime.
    let sweep_interval = super::digest_sweeper::resolve_sweep_interval_from_env();
    let store_for_sweeper = Arc::clone(&state.server.store);
    let _sweeper_handle =
        super::digest_sweeper::spawn_digest_sweeper(store_for_sweeper, sweep_interval);

    let app = Router::new()
        .route(path, post(handle_http_post::<S>).get(handle_http_get))
        .with_state(state);

    axum::serve(listener, app)
        .await
        .map_err(|e| crate::error::MemdError::ProtocolError(e.to_string()))
}

/// Run the MCP server with the given configuration
///
/// This is the main entry point for the MCP server.
/// Uses an in-memory store by default.
pub async fn run_server(config: Config) -> crate::Result<()> {
    use crate::store::MemoryStore;

    let store = Arc::new(MemoryStore::new());
    let mut server = McpServer::new(config, store);
    server.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use super::super::protocol::{error_codes, RequestId};
    use crate::config::Config;
    use crate::error::Result as MemdResult;
    use crate::metrics::IndexStats;
    use crate::metrics::QueryMetrics;
    use crate::store::{MemoryStore, PersistentStore, PersistentStoreConfig, Store, StoreStats};
    use crate::types::{ChunkId, MemoryChunk, TenantId};
    use async_trait::async_trait;
    use tempfile::tempdir;
    use tokio::task::{spawn_blocking, yield_now};

    struct IndexStatsStore;

    #[async_trait]
    impl Store for IndexStatsStore {
        async fn add(&self, _chunk: MemoryChunk) -> MemdResult<ChunkId> {
            Err(crate::error::MemdError::StorageError(
                "not used in test".to_string(),
            ))
        }

        async fn add_batch(&self, _chunks: Vec<MemoryChunk>) -> MemdResult<Vec<ChunkId>> {
            Ok(Vec::new())
        }

        async fn get(
            &self,
            _tenant_id: &TenantId,
            _chunk_id: &ChunkId,
        ) -> MemdResult<Option<MemoryChunk>> {
            Ok(None)
        }

        async fn search(
            &self,
            _tenant_id: &TenantId,
            _query: &str,
            _k: usize,
        ) -> MemdResult<Vec<MemoryChunk>> {
            Ok(Vec::new())
        }

        async fn delete(&self, _tenant_id: &TenantId, _chunk_id: &ChunkId) -> MemdResult<bool> {
            Ok(false)
        }

        async fn stats(&self, _tenant_id: &TenantId) -> MemdResult<StoreStats> {
            Ok(StoreStats::default())
        }

        fn get_index_stats(&self, _tenant_id: Option<&TenantId>) -> HashMap<String, IndexStats> {
            HashMap::from([(
                "test_tenant".to_string(),
                IndexStats {
                    chunks_indexed: 3,
                    embeddings_count: 3,
                    embedding_dimension: 384,
                    index_memory_bytes: 4096,
                },
            )])
        }
    }

    fn test_config_with_data_dir(data_dir: PathBuf) -> Config {
        Config {
            data_dir,
            log_level: "info".to_string(),
            log_format: "json".to_string(),
            server: crate::config::ServerConfig::default(),
        }
    }

    fn test_config() -> Config {
        // Use a temp directory to avoid permission issues in tests
        test_config_with_data_dir(std::env::temp_dir().join("memd_test"))
    }

    fn test_server() -> McpServer<MemoryStore> {
        let store = Arc::new(MemoryStore::new());
        McpServer::new(test_config(), store)
    }

    fn test_server_no_tenant_manager() -> McpServer<MemoryStore> {
        // Create server without tenant manager for simpler tests
        let store = Arc::new(MemoryStore::new());
        McpServer {
            config: test_config(),
            store,
            tenant_manager: None,
            metrics: Arc::new(MetricsCollector::default()),
            structural_store: None,
            symbol_indexer: None,
            call_graph_indexer: None,
            symbol_query_service: None,
            trace_query_service: None,
            initialized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    async fn spawn_http_test_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = test_server();
        let handle = tokio::spawn(async move {
            serve_http_server(listener, server, "/mcp").await.unwrap();
        });
        yield_now().await;
        (format!("http://{}/mcp", addr), handle)
    }

    async fn http_post_json(
        url: String,
        body: String,
        origin: Option<String>,
    ) -> Result<(u16, String), ureq::Error> {
        spawn_blocking(move || {
            let mut request = ureq::post(&url)
                .set("Accept", "application/json, text/event-stream")
                .timeout(Duration::from_secs(5));
            if let Some(origin) = origin.as_deref() {
                request = request.set("Origin", origin);
            }
            let response = request.send_string(&body)?;
            let status = response.status();
            let body = response.into_string().unwrap_or_default();
            Ok((status, body))
        })
        .await
        .unwrap()
    }

    async fn http_get(url: String, origin: Option<String>) -> Result<u16, ureq::Error> {
        spawn_blocking(move || {
            let mut request = ureq::get(&url)
                .set("Accept", "text/event-stream")
                .timeout(Duration::from_secs(5));
            if let Some(origin) = origin.as_deref() {
                request = request.set("Origin", origin);
            }
            let response = request.call()?;
            Ok(response.status())
        })
        .await
        .unwrap()
    }

    fn parse_tool_payload(result: &Value) -> serde_json::Value {
        let text = result["content"][0]["text"]
            .as_str()
            .expect("tool result should include text payload");
        serde_json::from_str(text).expect("tool payload should be valid JSON")
    }

    async fn run_memory_tool_flow<S: Store>(server: &McpServer<S>, tenant_id: &str) {
        let add_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.add",
                "arguments": {
                    "tenant_id": tenant_id,
                    "text": "end to end memory tool flow",
                    "type": "doc"
                }
            })))
            .await
            .expect("memory.add should succeed");

        let add_payload = parse_tool_payload(&add_result);
        let chunk_id = add_payload["chunk_id"]
            .as_str()
            .expect("add payload should include chunk_id")
            .to_string();

        let search_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.search",
                "arguments": {
                    "tenant_id": tenant_id,
                    "query": "end to end",
                    "k": 5
                }
            })))
            .await
            .expect("memory.search should succeed");

        let search_payload = parse_tool_payload(&search_result);
        let results = search_payload["results"]
            .as_array()
            .expect("search payload should include results array");
        assert!(results
            .iter()
            .any(|result| result["chunk_id"].as_str() == Some(chunk_id.as_str())));

        let feedback_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.feedback",
                "arguments": {
                    "tenant_id": tenant_id,
                    "query": "end to end",
                    "chunk_id": chunk_id,
                    "relevance": "relevant"
                }
            })))
            .await
            .expect("memory.feedback should succeed");
        let feedback_payload = parse_tool_payload(&feedback_result);
        assert_eq!(feedback_payload["stored"], true);

        let metrics_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.metrics",
                "arguments": {
                    "include_recent": false
                }
            })))
            .await
            .expect("memory.metrics should succeed");
        let metrics_payload = parse_tool_payload(&metrics_result);
        assert!(metrics_payload["index"].is_object());

        let compact_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.compact",
                "arguments": {
                    "tenant_id": tenant_id,
                    "force": false
                }
            })))
            .await
            .expect("memory.compact should succeed");
        let compact_payload = parse_tool_payload(&compact_result);
        let status = compact_payload["status"]
            .as_str()
            .expect("compact payload should include status");
        assert!(matches!(status, "completed" | "skipped"));
    }

    async fn run_memory_add_batch_tool_flow<S: Store>(server: &McpServer<S>, tenant_id: &str) {
        let add_batch_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.add_batch",
                "arguments": {
                    "tenant_id": tenant_id,
                    "chunks": [
                        {
                            "text": "batch document chunk",
                            "type": "doc",
                            "project_id": "batch_project"
                        },
                        {
                            "text": "batch code chunk",
                            "type": "code"
                        }
                    ]
                }
            })))
            .await
            .expect("memory.add_batch should succeed");

        let add_batch_payload = parse_tool_payload(&add_batch_result);
        let chunk_ids = add_batch_payload["chunk_ids"]
            .as_array()
            .expect("add_batch payload should include chunk_ids");
        assert_eq!(chunk_ids.len(), 2);
        assert!(chunk_ids.iter().all(|id| id.as_str().is_some()));

        let search_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.search",
                "arguments": {
                    "tenant_id": tenant_id,
                    "query": "batch",
                    "k": 10
                }
            })))
            .await
            .expect("memory.search should succeed after add_batch");

        let search_payload = parse_tool_payload(&search_result);
        let results = search_payload["results"]
            .as_array()
            .expect("search payload should include results");
        assert_eq!(results.len(), 2);
    }

    async fn run_episode_consolidation_flow<S: Store>(server: &McpServer<S>, tenant_id: &str) {
        server
            .handle_tools_call(Some(json!({
                "name": "memory.add_batch",
                "arguments": {
                    "tenant_id": tenant_id,
                    "chunks": [
                        {
                            "text": "Episode event one",
                            "type": "doc",
                            "episode_id": "ep_alpha"
                        },
                        {
                            "text": "Episode event two",
                            "type": "decision",
                            "episode_id": "ep_alpha"
                        }
                    ]
                }
            })))
            .await
            .expect("memory.add_batch should succeed");

        let consolidate_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.consolidate_episode",
                "arguments": {
                    "tenant_id": tenant_id,
                    "episode_id": "ep_alpha",
                    "max_chunks": 20,
                    "retain_source_chunks": false
                }
            })))
            .await
            .expect("memory.consolidate_episode should succeed");

        let consolidate_payload = parse_tool_payload(&consolidate_result);
        assert!(consolidate_payload["summary_chunk_id"].as_str().is_some());
        assert_eq!(consolidate_payload["source_chunk_count"], 2);
        assert_eq!(consolidate_payload["retained_source_chunks"], false);

        let search_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.search",
                "arguments": {
                    "tenant_id": tenant_id,
                    "query": "Episode ep_alpha summary",
                    "k": 10,
                    "filters": {
                        "episode_id": "ep_alpha",
                        "types": ["summary"]
                    }
                }
            })))
            .await
            .expect("memory.search should succeed");

        let search_payload = parse_tool_payload(&search_result);
        let results = search_payload["results"]
            .as_array()
            .expect("search payload should include results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["chunk_type"], "summary");
        assert_eq!(results[0]["episode_id"], "ep_alpha");
    }

    async fn run_task_tool_flow<S: Store>(server: &McpServer<S>, tenant_id: &str) {
        let start_result = server
            .handle_tools_call(Some(json!({
                "name": "task.start",
                "arguments": {
                    "tenant_id": tenant_id,
                    "project_id": "science_proj",
                    "goal": "Map the perturbation-responsive genes",
                    "motivation": "The pathway response is unresolved",
                    "hypothesis": "RpoS drives the induced genes",
                    "scientific_question": "Which genes increase after the perturbation?",
                    "dataset_refs": [
                        {
                            "name": "rna_seq",
                            "version": "v1"
                        }
                    ],
                    "entity_refs": [
                        {
                            "name": "RpoS",
                            "entity_type": "protein",
                            "role": "candidate regulator"
                        }
                    ],
                    "expected_outputs": ["differential expression table"]
                }
            })))
            .await
            .expect("task.start should succeed");

        let start_payload = parse_tool_payload(&start_result);
        let task_id = start_payload["task_id"]
            .as_str()
            .expect("task.start should return task_id")
            .to_string();
        assert!(start_payload["artifact_id"].as_str().is_some());
        assert!(start_payload["projection_chunk_ids"].as_array().is_some());

        server
            .handle_tools_call(Some(json!({
                    "name": "task.progress",
                    "arguments": {
                        "tenant_id": tenant_id,
                        "task_id": task_id.clone(),
                    "project_id": "science_proj",
                    "summary": "Initial QC exposed one low-depth replicate",
                    "blockers": ["One replicate is borderline"],
                    "failed_attempts": ["Default trimming removed too much signal"],
                    "next_step": "Re-run with lighter trimming"
                }
            })))
            .await
            .expect("task.progress should succeed");

        server
            .handle_tools_call(Some(json!({
                    "name": "task.run_start",
                    "arguments": {
                        "tenant_id": tenant_id,
                        "task_id": task_id.clone(),
                    "project_id": "science_proj",
                    "tool_name": "mmseqs",
                    "tool_version": "15",
                    "command": "mmseqs search db query out tmp",
                    "why_chosen": "Fast enough for iterative parameter sweeps",
                    "parameters": {"sensitivity": 7.5},
                    "inputs": ["query.faa"],
                    "summary": "Homology search for candidate regulators",
                    "dataset_refs": [{"name": "rna_seq", "version": "v1"}]
                }
            })))
            .await
            .expect("task.run_start should succeed");

        server
            .handle_tools_call(Some(json!({
                    "name": "task.run_finish",
                    "arguments": {
                        "tenant_id": tenant_id,
                        "task_id": task_id.clone(),
                    "project_id": "science_proj",
                    "status": "completed",
                    "tool_name": "mmseqs",
                    "tool_version": "15",
                    "command": "mmseqs search db query out tmp",
                    "outputs": ["hits.tsv"],
                    "metrics": {"top_hit_bitscore": 310.5},
                    "notes": "Recovered a strong candidate regulator",
                    "validation": ["Top hit was stable across reruns"]
                }
            })))
            .await
            .expect("task.run_finish should succeed");

        server
            .handle_tools_call(Some(json!({
                    "name": "task.add_evidence",
                    "arguments": {
                        "tenant_id": tenant_id,
                        "task_id": task_id.clone(),
                    "project_id": "science_proj",
                    "summary": "Top hit exceeded the curated threshold",
                    "evidence_kind": "metric",
                    "supports_claim": true,
                    "metric_name": "top_hit_bitscore",
                    "metric_value": 310.5
                }
            })))
            .await
            .expect("task.add_evidence should succeed");

        let finish_result = server
            .handle_tools_call(Some(json!({
                "name": "task.finish",
                "arguments": {
                    "tenant_id": tenant_id,
                    "project_id": "science_proj",
                    "task_id": task_id.clone(),
                    "what_worked": ["QC filtering stabilized the hit list"],
                    "what_failed": ["The first aligner preset over-trimmed reads"],
                    "validation": ["Independent replicate confirmed the top genes"],
                    "uncertainty": ["One replicate remains borderline"],
                    "followups": ["Collect one additional replicate"],
                    "confidence": 0.83
                }
            })))
            .await
            .expect("task.finish should succeed");

        let finish_payload = parse_tool_payload(&finish_result);
        assert!(finish_payload["artifact_id"].as_str().is_some());

        let get_result = server
            .handle_tools_call(Some(json!({
                "name": "task.get",
                "arguments": {
                    "tenant_id": tenant_id,
                    "task_id": task_id.clone()
                }
            })))
            .await
            .expect("task.get should succeed");
        let get_payload = parse_tool_payload(&get_result);
        let artifacts = get_payload["artifacts"]
            .as_array()
            .expect("task.get should include artifacts");
        assert_eq!(artifacts.len(), 6);

        let task_search_result = server
            .handle_tools_call(Some(json!({
                "name": "task.search",
                "arguments": {
                    "tenant_id": tenant_id,
                    "query": "parameter sweeps",
                    "k": 10,
                    "filters": {
                        "task_id": task_id.clone(),
                        "artifact_kind": "run_start",
                        "status": "started",
                        "dataset_name": "rna_seq",
                        "dataset_version": "v1",
                        "tool_name": "mmseqs",
                        "project_id": "science_proj"
                    }
                }
            })))
            .await
            .expect("task.search should succeed");
        let task_search_payload = parse_tool_payload(&task_search_result);
        let task_results = task_search_payload["results"]
            .as_array()
            .expect("task.search should include results");
        assert_eq!(task_results.len(), 1);

        let search_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.search",
                "arguments": {
                    "tenant_id": tenant_id,
                    "project_id": "science_proj",
                    "query": "over-trimmed reads",
                    "k": 10
                }
            })))
            .await
            .expect("memory.search should find task projection");

        let search_payload = parse_tool_payload(&search_result);
        let results = search_payload["results"]
            .as_array()
            .expect("search payload should include results");
        assert!(!results.is_empty());
        assert!(results.iter().any(|result| {
            result["tags"]
                .as_array()
                .map(|tags| {
                    tags.iter().any(|tag| {
                        tag.as_str()
                            .map(|value| value.starts_with("task:projection:failed"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        }));
    }

    #[tokio::test]
    async fn handle_initialize() {
        let mut server = test_server();
        let result = server.handle_initialize(None).await.unwrap();

        assert_eq!(result["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn handle_initialize_negotiates_supported_protocol_version() {
        let mut server = test_server();
        let result = server
            .handle_initialize(Some(json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            })))
            .await
            .unwrap();

        assert_eq!(result["protocolVersion"], "2025-11-25");
    }

    /// Phase 2.3: `artifact.verification` is a focused wrapper over
    /// `artifact.create` with `artifact_kind="verification"` injected
    /// by the dispatcher. A distinct-writer countersignature via this
    /// tool must promote trust just like the mega-schema path.
    #[tokio::test]
    async fn artifact_verification_tool_produces_verified_record_when_distinct_writer() {
        let mut server = test_server();
        let _ = server.handle_initialize(Some(json!({}))).await.unwrap();

        // Author starts a task as "alice".
        let start = server
            .handle_tools_call(Some(json!({
                "name": "task.start",
                "arguments": {
                    "tenant_id": "ver_tool",
                    "agent_id": "alice",
                    "goal": "verify via focused tool"
                }
            })))
            .await
            .unwrap();
        let text = start["content"][0]["text"].as_str().unwrap();
        let payload: serde_json::Value = serde_json::from_str(text).unwrap();
        let parent_artifact_id = payload["artifact_id"].as_str().unwrap().to_string();
        let parent_task_id = payload["task_id"].as_str().unwrap().to_string();

        // Distinct agent "reviewer" countersigns via the focused tool.
        let verify = server
            .handle_tools_call(Some(json!({
                "name": "artifact.verification",
                "arguments": {
                    "tenant_id": "ver_tool",
                    "task_id": parent_task_id,
                    "agent_id": "reviewer",
                    "reply_to_artifact_id": parent_artifact_id,
                    "supports_claim": true,
                    "summary": "independently reproduced"
                }
            })))
            .await
            .unwrap();
        let verify_text = verify["content"][0]["text"].as_str().unwrap();
        let verify_payload: serde_json::Value = serde_json::from_str(verify_text).unwrap();

        // Pull the artifact from the store and confirm promotion.
        let tenant = crate::types::TenantId::new("ver_tool").unwrap();
        let persisted = server
            .store
            .get_task_artifact(&tenant, verify_payload["artifact_id"].as_str().unwrap())
            .await
            .unwrap()
            .expect("verification artifact must persist");
        assert_eq!(persisted.agent_id.as_deref(), Some("reviewer"));
        assert_eq!(
            crate::task_memory::derive_artifact_trust_tier(&persisted),
            crate::task_memory::TrustTier::VerifiedRecord,
            "focused artifact.verification tool must drive the same countersignature promotion as artifact.create"
        );
    }

    /// Phase 3.1 regression: multiple concurrent HTTP requests must
    /// all succeed against a shared `Arc<McpServer>`. Before this
    /// change, every request serialized on an outer
    /// `AsyncMutex<McpServer>`. The concurrent-success check here is
    /// what compiles: the `Arc<McpServer>` is cloned across tasks and
    /// dispatched simultaneously.
    #[tokio::test]
    async fn http_concurrent_requests_share_server_without_outer_mutex() {
        let (url, _handle) = spawn_http_test_server().await;

        // Single initialize up front.
        let (status, _) = http_post_json(
            url.clone(),
            json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "concurrent-test", "version": "1.0"}
                }
            })
            .to_string(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);

        // Fan out 16 concurrent memory.search calls.
        let mut futures = Vec::new();
        for i in 0..16 {
            let u = url.clone();
            futures.push(tokio::spawn(async move {
                http_post_json(
                    u,
                    json!({
                        "jsonrpc": "2.0",
                        "id": i + 1,
                        "method": "tools/call",
                        "params": {
                            "name": "memory.search",
                            "arguments": {
                                "tenant_id": format!("concurrent_{}", i),
                                "query": format!("concurrency probe {}", i),
                                "k": 5
                            }
                        }
                    })
                    .to_string(),
                    None,
                )
                .await
            }));
        }

        for handle in futures {
            let (status, body) = handle.await.unwrap().unwrap();
            assert_eq!(status, 200, "concurrent request failed: {}", body);
        }
    }

    /// Codex Phase 3 coverage gap: the read-only concurrency test
    /// above proves the outer mutex is gone, but it does not cover
    /// mixed read/write safety. This test fires interleaved `memory.add`
    /// and `memory.search` calls against the shared `Arc<McpServer>`
    /// and verifies all requests succeed without deadlock, panics, or
    /// 5xx responses. Tenant id is also omitted on the search side, so
    /// the Phase 2.1 default-tenant resolver path gets exercised under
    /// concurrency as well.
    #[tokio::test]
    async fn http_mixed_read_write_concurrency() {
        let (url, _handle) = spawn_http_test_server().await;

        // Initialize once.
        let (status, _) = http_post_json(
            url.clone(),
            json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "mixed-rw", "version": "1.0"}
                }
            })
            .to_string(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);

        // Interleaved 8 writes + 8 reads against the same tenant.
        let mut futures = Vec::new();
        for i in 0..8 {
            let u = url.clone();
            futures.push(tokio::spawn(async move {
                http_post_json(
                    u,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 100 + i,
                        "method": "tools/call",
                        "params": {
                            "name": "memory.add",
                            "arguments": {
                                "tenant_id": "rw_mix",
                                "text": format!("concurrent write body {}", i),
                                "type": "doc"
                            }
                        }
                    })
                    .to_string(),
                    None,
                )
                .await
            }));
            let u = url.clone();
            futures.push(tokio::spawn(async move {
                http_post_json(
                    u,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 200 + i,
                        "method": "tools/call",
                        "params": {
                            "name": "memory.search",
                            "arguments": {
                                "tenant_id": "rw_mix",
                                "query": "concurrent",
                                "k": 5
                            }
                        }
                    })
                    .to_string(),
                    None,
                )
                .await
            }));
        }

        for handle in futures {
            let (status, body) = handle.await.unwrap().unwrap();
            assert_eq!(
                status, 200,
                "mixed read/write under concurrency must all return 200; body={}",
                body
            );
            // JSON-RPC response must not carry an error field on a
            // successful status. Parse and check.
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert!(
                parsed.get("error").is_none(),
                "concurrent request produced a JSON-RPC error: {}",
                body
            );
        }
    }

    /// Phase 2.3 (Codex coverage gap): the review/revision/decision
    /// wrappers must each inject their own `artifact_kind`, preserve
    /// their wrapper-specific fields, and reject overriding
    /// `artifact_kind` just like `artifact.verification` does.
    #[tokio::test]
    async fn focused_artifact_wrappers_dispatch_per_kind_and_preserve_fields() {
        let mut server = test_server();
        let _ = server.handle_initialize(Some(json!({}))).await.unwrap();

        // Seed a parent task so the wrappers have something to reply to.
        let start = server
            .handle_tools_call(Some(json!({
                "name": "task.start",
                "arguments": {
                    "tenant_id": "focused",
                    "agent_id": "author",
                    "goal": "exercise focused artifact wrappers"
                }
            })))
            .await
            .unwrap();
        let start_text = start["content"][0]["text"].as_str().unwrap();
        let start_payload: serde_json::Value = serde_json::from_str(start_text).unwrap();
        let parent_id = start_payload["artifact_id"].as_str().unwrap().to_string();
        let task_id = start_payload["task_id"].as_str().unwrap().to_string();

        let tenant = crate::types::TenantId::new("focused").unwrap();

        // artifact.review — verify kind injection + round-trip of `requested_action`.
        let review = server
            .handle_tools_call(Some(json!({
                "name": "artifact.review",
                "arguments": {
                    "tenant_id": "focused",
                    "task_id": task_id,
                    "agent_id": "reviewer",
                    "reply_to_artifact_id": parent_id,
                    "supports_claim": true,
                    "summary": "looks good to me",
                    "requested_action": "approve"
                }
            })))
            .await
            .unwrap();
        let review_payload: serde_json::Value =
            serde_json::from_str(review["content"][0]["text"].as_str().unwrap()).unwrap();
        let review_artifact = server
            .store
            .get_task_artifact(&tenant, review_payload["artifact_id"].as_str().unwrap())
            .await
            .unwrap()
            .expect("review artifact must persist");
        assert_eq!(
            review_artifact.artifact_kind,
            crate::task_memory::ArtifactKind::Review
        );
        assert_eq!(review_artifact.requested_action.as_deref(), Some("approve"));
        assert_eq!(review_artifact.agent_id.as_deref(), Some("reviewer"));

        // artifact.revision — reply_to is required; wrapper preserves it.
        let revision = server
            .handle_tools_call(Some(json!({
                "name": "artifact.revision",
                "arguments": {
                    "tenant_id": "focused",
                    "task_id": task_id,
                    "summary": "superseded by revised approach",
                    "reply_to_artifact_id": parent_id,
                    "agent_id": "author"
                }
            })))
            .await
            .unwrap();
        let revision_payload: serde_json::Value =
            serde_json::from_str(revision["content"][0]["text"].as_str().unwrap()).unwrap();
        let revision_artifact = server
            .store
            .get_task_artifact(&tenant, revision_payload["artifact_id"].as_str().unwrap())
            .await
            .unwrap()
            .expect("revision artifact must persist");
        assert_eq!(
            revision_artifact.artifact_kind,
            crate::task_memory::ArtifactKind::Revision
        );
        assert_eq!(
            revision_artifact.reply_to_artifact_id.as_deref(),
            Some(parent_id.as_str())
        );

        // artifact.decision — why_chosen must round-trip.
        let decision = server
            .handle_tools_call(Some(json!({
                "name": "artifact.decision",
                "arguments": {
                    "tenant_id": "focused",
                    "task_id": task_id,
                    "summary": "chose approach B",
                    "why_chosen": "lower latency and simpler code",
                    "agent_id": "author"
                }
            })))
            .await
            .unwrap();
        let decision_payload: serde_json::Value =
            serde_json::from_str(decision["content"][0]["text"].as_str().unwrap()).unwrap();
        let decision_artifact = server
            .store
            .get_task_artifact(&tenant, decision_payload["artifact_id"].as_str().unwrap())
            .await
            .unwrap()
            .expect("decision artifact must persist");
        assert_eq!(
            decision_artifact.artifact_kind,
            crate::task_memory::ArtifactKind::Decision
        );
        assert_eq!(
            decision_artifact.why_chosen.as_deref(),
            Some("lower latency and simpler code")
        );

        // All three wrappers must reject a conflicting `artifact_kind`.
        for wrapper in ["artifact.review", "artifact.revision", "artifact.decision"] {
            let err = server
                .handle_tools_call(Some(json!({
                    "name": wrapper,
                    "arguments": {
                        "tenant_id": "focused",
                        "task_id": task_id,
                        "artifact_kind": "verification",
                        "summary": "attempted override",
                        "reply_to_artifact_id": parent_id
                    }
                })))
                .await
                .err()
                .unwrap_or_else(|| panic!("{} must reject overriding artifact_kind", wrapper));
            let msg = format!("{:?}", err);
            assert!(
                msg.to_lowercase().contains("artifact_kind"),
                "{} rejection must flag the conflicting kind; got {}",
                wrapper,
                msg
            );
        }
    }

    /// Codex Phase 3 coverage gap: a unified matrix test across ALL
    /// four focused artifact wrappers (`artifact.review`,
    /// `artifact.revision`, `artifact.decision`,
    /// `artifact.verification`). Every wrapper must:
    ///   - inject its own `artifact_kind`
    ///   - reject a conflicting caller-supplied `artifact_kind`
    ///   - preserve `agent_id` on the persisted artifact
    ///   - dispatch without needing the legacy `artifact_kind` field
    ///     from the caller
    ///
    /// The three individual wrapper tests remain (they cover
    /// wrapper-specific behavior like `why_chosen` round-trip,
    /// verification countersignature). This one pins the shared
    /// contract so future refactors can't break one wrapper without
    /// the test noticing.
    #[tokio::test]
    async fn all_focused_artifact_wrappers_share_the_same_contract() {
        use crate::task_memory::ArtifactKind;

        let server = test_server();
        let _ = server.handle_initialize(Some(json!({}))).await.unwrap();

        let start = server
            .handle_tools_call(Some(json!({
                "name": "task.start",
                "arguments": {
                    "tenant_id": "matrix",
                    "agent_id": "author",
                    "goal": "matrix test scenario"
                }
            })))
            .await
            .unwrap();
        let start_payload: serde_json::Value =
            serde_json::from_str(start["content"][0]["text"].as_str().unwrap()).unwrap();
        let parent_id = start_payload["artifact_id"].as_str().unwrap().to_string();
        let task_id = start_payload["task_id"].as_str().unwrap().to_string();
        let tenant = crate::types::TenantId::new("matrix").unwrap();

        // (tool_name, expected ArtifactKind, wrapper-specific minimum args)
        let cases: Vec<(&str, ArtifactKind, serde_json::Value)> = vec![
            (
                "artifact.review",
                ArtifactKind::Review,
                json!({"summary": "review matrix"}),
            ),
            (
                "artifact.revision",
                ArtifactKind::Revision,
                json!({"summary": "revision matrix", "reply_to_artifact_id": parent_id}),
            ),
            (
                "artifact.decision",
                ArtifactKind::Decision,
                json!({"summary": "decision matrix", "why_chosen": "matrix preferred"}),
            ),
            (
                "artifact.verification",
                ArtifactKind::Verification,
                json!({
                    "summary": "verification matrix",
                    "reply_to_artifact_id": parent_id,
                    "supports_claim": true
                }),
            ),
        ];

        for (tool, expected_kind, wrapper_args) in &cases {
            // Base args — every call supplies task_id + agent_id +
            // tenant_id + the wrapper-specific fields, but NOT
            // `artifact_kind` (the wrapper is responsible for that).
            let mut args = wrapper_args.clone();
            let obj = args.as_object_mut().unwrap();
            obj.insert("tenant_id".to_string(), json!("matrix"));
            obj.insert("task_id".to_string(), json!(&task_id));
            obj.insert("agent_id".to_string(), json!(format!("reviewer-{}", tool)));

            let response = server
                .handle_tools_call(Some(json!({
                    "name": tool,
                    "arguments": args.clone()
                })))
                .await
                .unwrap_or_else(|err| panic!("{} dispatch failed: {:?}", tool, err));

            let response_payload: serde_json::Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            let artifact_id = response_payload["artifact_id"].as_str().unwrap();
            let persisted = server
                .store
                .get_task_artifact(&tenant, artifact_id)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{} artifact must persist", tool));
            assert_eq!(
                &persisted.artifact_kind, expected_kind,
                "{} must inject artifact_kind {:?}, got {:?}",
                tool, expected_kind, persisted.artifact_kind
            );
            assert_eq!(
                persisted.agent_id.as_deref(),
                Some(format!("reviewer-{}", tool).as_str()),
                "{} must preserve agent_id on the persisted artifact",
                tool
            );

            // Same wrapper called with a conflicting `artifact_kind`
            // must be rejected.
            let mut conflict_args = args.clone();
            conflict_args
                .as_object_mut()
                .unwrap()
                .insert("artifact_kind".to_string(), json!("digest"));
            let err = server
                .handle_tools_call(Some(json!({
                    "name": tool,
                    "arguments": conflict_args
                })))
                .await
                .err()
                .unwrap_or_else(|| panic!("{} must reject overriding artifact_kind", tool));
            let msg = format!("{:?}", err);
            assert!(
                msg.to_lowercase().contains("artifact_kind"),
                "{} rejection must mention artifact_kind; got {}",
                tool,
                msg
            );
        }
    }

    /// Phase 2.3: the focused tool rejects a caller that tries to
    /// override `artifact_kind` — the whole point of the wrapper is
    /// that the kind is tool-driven.
    #[tokio::test]
    async fn artifact_verification_rejects_overriding_artifact_kind() {
        let mut server = test_server();
        let _ = server.handle_initialize(Some(json!({}))).await.unwrap();

        let err = server
            .handle_tools_call(Some(json!({
                "name": "artifact.verification",
                "arguments": {
                    "tenant_id": "ver_tool",
                    "task_id": "fake",
                    "artifact_kind": "review", // mismatched!
                    "reply_to_artifact_id": "fake-parent",
                    "supports_claim": false
                }
            })))
            .await
            .err()
            .expect("override must be rejected");

        let message = format!("{:?}", err);
        assert!(
            message.to_lowercase().contains("artifact_kind"),
            "rejection message must flag the conflicting kind; got: {}",
            message
        );
    }

    #[tokio::test]
    async fn notifications_initialized_alias_is_accepted() {
        let mut server = test_server();
        let response = server
            .handle_jsonrpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;

        // Notifications (no `id`) must produce no response per JSON-RPC 2.0.
        assert!(
            response.is_none(),
            "notifications must not return a response"
        );
    }

    #[tokio::test]
    async fn ping_request_returns_empty_object_with_echoed_id() {
        let mut server = test_server();
        let response = server
            .handle_jsonrpc(r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#)
            .await
            .expect("ping is a request, not a notification — must return a response");

        assert_eq!(response.id, Some(RequestId::Number(42)));
        assert!(response.error.is_none());
        let result = response.result.expect("ping must have a result");
        assert_eq!(
            result,
            Value::Object(serde_json::Map::new()),
            "MCP ping must return an empty object, not null"
        );
    }

    #[tokio::test]
    async fn parse_error_still_returns_response_with_null_id() {
        let mut server = test_server();
        let response = server
            .handle_jsonrpc("not valid json at all")
            .await
            .expect("parse errors must always produce a response");

        assert!(response.id.is_none());
        let error = response.error.expect("parse error must produce an error");
        assert_eq!(error.code, error_codes::PARSE_ERROR);
    }

    #[tokio::test]
    async fn notification_with_invalid_method_suppresses_error_response() {
        let mut server = test_server();
        // Parses successfully (notification), but the method is unknown;
        // handle_request produces an error, which handle_jsonrpc must
        // swallow so we do not emit a response on the wire.
        let response = server
            .handle_jsonrpc(r#"{"jsonrpc":"2.0","method":"notifications/unknown-kind"}"#)
            .await;

        assert!(
            response.is_none(),
            "notifications never receive responses, even when the handler errors"
        );
    }

    #[tokio::test]
    async fn http_transport_supports_initialize_and_memory_search() {
        let (url, handle) = spawn_http_test_server().await;

        let (status, init_body) = http_post_json(
            url.clone(),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "http-test",
                        "version": "1.0.0"
                    }
                }
            })
            .to_string(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);
        let init_payload: Value = serde_json::from_str(&init_body).unwrap();
        assert_eq!(init_payload["result"]["protocolVersion"], "2025-11-25");

        let (status, add_body) = http_post_json(
            url.clone(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "memory.add",
                    "arguments": {
                        "tenant_id": "http_test",
                        "text": "shared marker from http transport",
                        "type": "summary"
                    }
                }
            })
            .to_string(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);
        let add_payload: Value = serde_json::from_str(&add_body).unwrap();
        let add_tool_payload = parse_tool_payload(&add_payload["result"]);
        let chunk_id = add_tool_payload["chunk_id"].as_str().unwrap().to_string();

        let (status, search_body) = http_post_json(
            url.clone(),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "memory.search",
                    "arguments": {
                        "tenant_id": "http_test",
                        "query": "shared marker",
                        "k": 5
                    }
                }
            })
            .to_string(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);
        let search_payload: Value = serde_json::from_str(&search_body).unwrap();
        let search_tool_payload = parse_tool_payload(&search_payload["result"]);
        let results = search_tool_payload["results"].as_array().unwrap();
        assert!(results
            .iter()
            .any(|result| result["chunk_id"].as_str() == Some(chunk_id.as_str())));

        handle.abort();
    }

    #[tokio::test]
    async fn http_transport_get_returns_method_not_allowed() {
        let (url, handle) = spawn_http_test_server().await;

        match http_get(url, None).await {
            Ok(status) => panic!("expected 405 error, got {}", status),
            Err(ureq::Error::Status(status, _)) => assert_eq!(status, 405),
            Err(err) => panic!("unexpected error: {}", err),
        }

        handle.abort();
    }

    #[tokio::test]
    async fn http_transport_rejects_invalid_origin() {
        let (url, handle) = spawn_http_test_server().await;

        match http_post_json(
            url,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "http-test",
                        "version": "1.0.0"
                    }
                }
            })
            .to_string(),
            Some("https://evil.example".to_string()),
        )
        .await
        {
            Ok((status, _)) => panic!("expected 403 error, got {}", status),
            Err(ureq::Error::Status(status, _)) => assert_eq!(status, 403),
            Err(err) => panic!("unexpected error: {}", err),
        }

        handle.abort();
    }

    #[tokio::test]
    async fn handle_tools_list() {
        let server = test_server();
        let result = server.handle_tools_list().await.unwrap();

        assert!(result["tools"].is_array());
    }

    #[tokio::test]
    async fn handle_unknown_method() {
        let mut server = test_server();
        let request = Request {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(1)),
            method: "unknown".to_string(),
            params: None,
        };

        let response = server.handle_request(request).await;
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn handle_tools_call_missing_params() {
        let server = test_server();
        let result = server.handle_tools_call(None).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn handle_tools_call_missing_name() {
        let server = test_server();
        let result = server.handle_tools_call(Some(json!({}))).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn handle_tool_search() {
        let server = test_server();
        let result = server
            .handle_tools_call(Some(json!({
                "name": "memory.search",
                "arguments": {
                    "query": "test",
                    "tenant_id": "test_tenant"
                }
            })))
            .await
            .unwrap();

        assert!(result["content"].is_array());
    }

    #[tokio::test]
    async fn handle_tool_add() {
        let server = test_server_no_tenant_manager();
        let result = server
            .handle_tools_call(Some(json!({
                "name": "memory.add",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "text": "test content",
                    "type": "doc"
                }
            })))
            .await
            .unwrap();

        assert!(result["content"].is_array());

        // Verify the chunk_id is a valid UUID
        let text = result["content"][0]["text"].as_str().unwrap();
        let response: serde_json::Value = serde_json::from_str(text).unwrap();
        let chunk_id = response["chunk_id"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(chunk_id).is_ok());
    }

    #[tokio::test]
    async fn handle_tool_task_start() {
        let server = test_server_no_tenant_manager();
        let result = server
            .handle_tools_call(Some(json!({
                "name": "task.start",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "goal": "Map the perturbation-responsive genes",
                    "motivation": "The pathway response is unresolved",
                    "hypothesis": "RpoS drives the induced genes",
                    "scientific_question": "Which genes increase after the perturbation?",
                    "dataset_refs": [{"name": "rna_seq"}],
                    "expected_outputs": ["differential expression table"]
                }
            })))
            .await
            .unwrap();

        let payload = parse_tool_payload(&result);
        assert!(uuid::Uuid::parse_str(payload["task_id"].as_str().unwrap()).is_ok());
        assert!(uuid::Uuid::parse_str(payload["artifact_id"].as_str().unwrap()).is_ok());
    }

    #[tokio::test]
    async fn handle_tool_task_finish_rejects_invalid_confidence() {
        let server = test_server_no_tenant_manager();
        let result = server
            .handle_tools_call(Some(json!({
                "name": "task.finish",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "task_id": "task-1",
                    "what_worked": [],
                    "what_failed": [],
                    "validation": [],
                    "uncertainty": [],
                    "followups": [],
                    "confidence": 1.2
                }
            })))
            .await;

        assert!(matches!(result, Err(McpError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn handle_tool_stats() {
        let server = test_server();
        let result = server
            .handle_tools_call(Some(json!({
                "name": "memory.stats",
                "arguments": {
                    "tenant_id": "test_tenant"
                }
            })))
            .await
            .unwrap();

        assert!(result["content"].is_array());
    }

    #[tokio::test]
    async fn handle_context_tools_are_dispatched() {
        let server = test_server_no_tenant_manager();
        let tenant_id = "context_dispatch_tenant";

        server
            .handle_tools_call(Some(json!({
                "name": "memory.add",
                "arguments": {
                    "tenant_id": tenant_id,
                    "text": "retrieval architecture hot note",
                    "type": "doc",
                    "tags": [
                        "ctx:doc",
                        "ctx:subsystem:retrieval",
                        "ctx:tier:hot"
                    ]
                }
            })))
            .await
            .expect("memory.add should succeed");

        let list_result = server
            .handle_tools_call(Some(json!({
                "name": "context.list_subsystems",
                "arguments": {
                    "tenant_id": tenant_id
                }
            })))
            .await
            .expect("context.list_subsystems should succeed");
        let list_payload = parse_tool_payload(&list_result);
        let subsystems = list_payload["subsystems"]
            .as_array()
            .expect("list_subsystems should return a subsystems array");
        assert!(subsystems
            .iter()
            .any(|entry| entry["key"].as_str() == Some("retrieval")));

        let hot_result = server
            .handle_tools_call(Some(json!({
                "name": "context.get_hot_context",
                "arguments": {
                    "tenant_id": tenant_id,
                    "k": 5
                }
            })))
            .await
            .expect("context.get_hot_context should succeed");
        let hot_payload = parse_tool_payload(&hot_result);
        let results = hot_payload["results"]
            .as_array()
            .expect("get_hot_context should return results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["source_tier"], "hot");
    }

    #[tokio::test]
    async fn add_then_search() {
        let server = test_server_no_tenant_manager();

        // Add a chunk
        let add_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.add",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "text": "hello world from memd",
                    "type": "doc"
                }
            })))
            .await
            .unwrap();

        let text = add_result["content"][0]["text"].as_str().unwrap();
        let add_response: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(add_response["chunk_id"].is_string());

        // Search for it
        let search_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.search",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "query": "hello"
                }
            })))
            .await
            .unwrap();

        let text = search_result["content"][0]["text"].as_str().unwrap();
        let search_response: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(search_response["results"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn delete_removes_from_search() {
        let server = test_server_no_tenant_manager();

        // Add a chunk
        let add_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.add",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "text": "delete me",
                    "type": "doc"
                }
            })))
            .await
            .unwrap();

        let text = add_result["content"][0]["text"].as_str().unwrap();
        let add_response: serde_json::Value = serde_json::from_str(text).unwrap();
        let chunk_id = add_response["chunk_id"].as_str().unwrap();

        // Delete it
        let delete_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.delete",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "chunk_id": chunk_id
                }
            })))
            .await
            .unwrap();

        let text = delete_result["content"][0]["text"].as_str().unwrap();
        let delete_response: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(delete_response["deleted"].as_bool().unwrap());

        // Search should return empty
        let search_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.search",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "query": "delete"
                }
            })))
            .await
            .unwrap();

        let text = search_result["content"][0]["text"].as_str().unwrap();
        let search_response: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(search_response["results"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn handle_tool_compact_is_dispatched() {
        let server = test_server();
        let result = server
            .handle_tools_call(Some(json!({
                "name": "memory.compact",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "force": false
                }
            })))
            .await
            .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let response: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(response["status"], "skipped");
    }

    #[tokio::test]
    async fn handle_tool_search_accepts_iso8601_time_range() {
        let server = test_server();
        let result = server
            .handle_tools_call(Some(json!({
                "name": "memory.search",
                "arguments": {
                    "tenant_id": "test_tenant",
                    "query": "hello",
                    "filters": {
                        "time_range": {
                            "from": "2026-01-01T00:00:00Z",
                            "to": "2026-01-31T23:59:59Z"
                        }
                    }
                }
            })))
            .await
            .unwrap();

        assert!(result["content"].is_array());
    }

    #[tokio::test]
    async fn handle_tool_metrics_respects_include_recent_flag() {
        let store = Arc::new(MemoryStore::new());
        let metrics = Arc::new(MetricsCollector::default());
        metrics.record_query(QueryMetrics {
            embed_ms: 10,
            dense_search_ms: 5,
            fetch_ms: 2,
            total_ms: 17,
        });
        let server = McpServer::with_metrics(test_config(), store, metrics);

        let result = server
            .handle_tools_call(Some(json!({
                "name": "memory.metrics",
                "arguments": {
                    "include_recent": false
                }
            })))
            .await
            .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let response: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(response["index"].is_object());
        assert!(response["recent_queries"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn handle_tool_metrics_includes_store_index_stats() {
        let store = Arc::new(IndexStatsStore);
        let metrics = Arc::new(MetricsCollector::default());
        let server = McpServer::with_metrics(test_config(), store, metrics);

        let result = server
            .handle_tools_call(Some(json!({
                "name": "memory.metrics",
                "arguments": {}
            })))
            .await
            .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let response: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(response["index"]["test_tenant"]["chunks_indexed"], 3);
    }

    /// Phase 4.4: a tool call that fails validation must bump the
    /// rejection counter, and `memory.metrics` must surface it under
    /// the `rejections` field.
    #[tokio::test]
    async fn failed_tool_call_bumps_rejection_metrics() {
        let store = Arc::new(MemoryStore::new());
        let metrics = Arc::new(MetricsCollector::default());
        let server = McpServer::with_metrics(test_config(), store, Arc::clone(&metrics));

        // Intentionally malformed call: `task.start` requires `goal`
        // (Phase 2.2); an arguments object without it must fail
        // validation and route through the rejection counter.
        let bad = server
            .handle_tools_call(Some(json!({
                "name": "task.start",
                "arguments": {"tenant_id": "reject_probe"}
            })))
            .await;
        assert!(bad.is_err(), "missing `goal` must reject");

        // Nonexistent tool is a separate reason bucket.
        let unknown = server
            .handle_tools_call(Some(json!({
                "name": "nope.nope",
                "arguments": {}
            })))
            .await;
        assert!(
            matches!(unknown, Err(McpError::MethodNotFound(_))),
            "unknown tool must reject as MethodNotFound, got {:?}",
            unknown
        );

        // `memory.metrics` must now include these rejections.
        let metrics_response = server
            .handle_tools_call(Some(json!({
                "name": "memory.metrics",
                "arguments": {}
            })))
            .await
            .expect("memory.metrics itself should succeed");
        let text = metrics_response["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(
            parsed["rejections"]["total"].as_u64().unwrap_or(0) >= 2,
            "rejections.total must be >= 2 after two failed calls; got {}",
            parsed["rejections"]
        );
        assert!(
            parsed["rejections"]["by_tool"]
                .as_object()
                .map(|m| m.contains_key("task.start"))
                .unwrap_or(false),
            "by_tool must include the rejected task.start entry; got {}",
            parsed["rejections"]
        );
    }

    #[tokio::test]
    async fn e2e_memory_tools_with_memory_store() {
        let server = test_server_no_tenant_manager();
        run_memory_tool_flow(&server, "e2e_memory_tenant").await;
    }

    #[tokio::test]
    async fn e2e_memory_add_batch_with_memory_store() {
        let server = test_server_no_tenant_manager();
        run_memory_add_batch_tool_flow(&server, "e2e_memory_batch_tenant").await;
    }

    #[tokio::test]
    async fn e2e_episode_consolidation_with_memory_store() {
        let server = test_server_no_tenant_manager();
        run_episode_consolidation_flow(&server, "e2e_episode_memory_tenant").await;
    }

    #[tokio::test]
    async fn e2e_task_tools_with_memory_store() {
        let server = test_server_no_tenant_manager();
        run_task_tool_flow(&server, "e2e_task_memory_tenant").await;
    }

    #[tokio::test]
    async fn e2e_memory_tools_with_persistent_store() {
        let dir = tempdir().expect("tempdir");
        let store = Arc::new(
            PersistentStore::open(PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                enable_tiered_search: false,
                ..Default::default()
            })
            .expect("persistent store"),
        );
        let server = McpServer::new(test_config_with_data_dir(dir.path().to_path_buf()), store);

        run_memory_tool_flow(&server, "e2e_persistent_tenant").await;
    }

    #[tokio::test]
    async fn e2e_memory_add_batch_with_persistent_store() {
        let dir = tempdir().expect("tempdir");
        let store = Arc::new(
            PersistentStore::open(PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                enable_tiered_search: false,
                ..Default::default()
            })
            .expect("persistent store"),
        );
        let server = McpServer::new(test_config_with_data_dir(dir.path().to_path_buf()), store);

        run_memory_add_batch_tool_flow(&server, "e2e_persistent_batch_tenant").await;
    }

    #[tokio::test]
    async fn e2e_episode_consolidation_with_persistent_store() {
        let dir = tempdir().expect("tempdir");
        let store = Arc::new(
            PersistentStore::open(PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                enable_tiered_search: false,
                ..Default::default()
            })
            .expect("persistent store"),
        );
        let server = McpServer::new(test_config_with_data_dir(dir.path().to_path_buf()), store);

        run_episode_consolidation_flow(&server, "e2e_episode_persistent_tenant").await;
    }

    #[tokio::test]
    async fn e2e_task_tools_with_persistent_store() {
        let dir = tempdir().expect("tempdir");
        let store = Arc::new(
            PersistentStore::open(PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                enable_tiered_search: false,
                ..Default::default()
            })
            .expect("persistent store"),
        );
        let server = McpServer::new(test_config_with_data_dir(dir.path().to_path_buf()), store);

        run_task_tool_flow(&server, "e2e_task_persistent_tenant").await;
    }

    #[tokio::test]
    async fn e2e_memory_compact_force_with_persistent_store() {
        let dir = tempdir().expect("tempdir");
        let store = Arc::new(
            PersistentStore::open(PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                enable_tiered_search: false,
                ..Default::default()
            })
            .expect("persistent store"),
        );
        let server = McpServer::new(test_config_with_data_dir(dir.path().to_path_buf()), store);
        let tenant_id = "e2e_persistent_compact_tenant";

        server
            .handle_tools_call(Some(json!({
                "name": "memory.add",
                "arguments": {
                    "tenant_id": tenant_id,
                    "text": "chunk before forced compaction",
                    "type": "doc"
                }
            })))
            .await
            .expect("memory.add should succeed");

        let compact_result = server
            .handle_tools_call(Some(json!({
                "name": "memory.compact",
                "arguments": {
                    "tenant_id": tenant_id,
                    "force": true
                }
            })))
            .await;

        match compact_result {
            Ok(value) => {
                let payload = parse_tool_payload(&value);
                assert_eq!(payload["status"], "completed");
            }
            Err(McpError::ToolError(msg)) => {
                assert!(!msg.contains("compaction not supported"));
            }
            Err(err) => panic!("unexpected error: {}", err),
        }
    }
}
