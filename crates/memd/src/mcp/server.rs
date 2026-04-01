//! MCP server implementation
//!
//! Handles JSON-RPC communication over stdio and streamable HTTP transports.
//! This is the primary interface for agent integration.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use axum::extract::State;
use axum::http::header::{CONTENT_TYPE, ORIGIN};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, error, info, warn};

use super::error::McpError;
use super::handlers::{
    handle_artifact_create, handle_artifact_find_decisions, handle_artifact_find_evidence,
    handle_artifact_find_failures, handle_artifact_find_highlights, handle_artifact_get,
    handle_artifact_list_thread, handle_artifact_search, handle_context_brief_project,
    handle_context_find_relevant_context, handle_context_get_files_for_subsystem,
    handle_context_get_hot_context, handle_context_list_subsystems,
    handle_context_search_documents, handle_context_suggest_agent, handle_find_callers,
    handle_find_definition, handle_find_errors, handle_find_imports, handle_find_references,
    handle_find_tool_calls, handle_memory_add, handle_memory_add_batch, handle_memory_compact,
    handle_memory_consolidate_episode, handle_memory_delete, handle_memory_feedback,
    handle_memory_get, handle_memory_metrics, handle_memory_search, handle_memory_stats,
    handle_task_add_evidence, handle_task_finish, handle_task_get, handle_task_progress,
    handle_task_resume, handle_task_run_finish, handle_task_run_start, handle_task_search,
    handle_task_start, AddBatchParams, AddParams, ArtifactCreateParams, ArtifactGetParams,
    ArtifactLibraryParams, ArtifactListThreadParams, CompactParams, ConsolidateEpisodeParams,
    ContextFindRelevantContextParams, ContextGetFilesForSubsystemParams,
    ContextGetHotContextParams, ContextListSubsystemsParams, ContextSearchDocumentsParams,
    ContextSuggestAgentParams, DeleteParams, FeedbackParams, FindCallersParams,
    FindDefinitionParams, FindErrorsParams, FindImportsParams, FindReferencesParams,
    FindToolCallsParams, GetParams, MetricsParams, ProjectBriefParams, SearchParams, StatsParams,
    TaskAddEvidenceParams, TaskFinishParams, TaskGetParams, TaskProgressParams, TaskResumeParams,
    TaskRunFinishParams, TaskRunStartParams, TaskSearchParams, TaskStartParams,
};
use super::protocol::{Request, Response, RpcError};
use super::tools::get_all_tools;
use crate::metrics::MetricsCollector;
use crate::store::{Store, TenantManager};
use crate::structural::{SymbolQueryService, TraceQueryService};
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

struct HttpServerState<S: Store> {
    server: Arc<AsyncMutex<McpServer<S>>>,
}

impl<S: Store> Clone for HttpServerState<S> {
    fn clone(&self) -> Self {
        Self {
            server: Arc::clone(&self.server),
        }
    }
}

/// MCP server that handles JSON-RPC requests over stdio
pub struct McpServer<S: Store> {
    config: Config,
    store: Arc<S>,
    tenant_manager: Option<TenantManager>,
    metrics: Arc<MetricsCollector>,
    symbol_query_service: Option<Arc<SymbolQueryService>>,
    trace_query_service: Option<Arc<TraceQueryService>>,
    initialized: bool,
}

impl<S: Store> McpServer<S> {
    /// Create a new MCP server with the given configuration and store
    pub fn new(config: Config, store: Arc<S>) -> Self {
        // Create tenant manager from config data_dir
        let tenant_manager = config.data_dir_expanded().ok().map(TenantManager::new);

        Self {
            config,
            store,
            tenant_manager,
            metrics: Arc::new(MetricsCollector::default()),
            symbol_query_service: None,
            trace_query_service: None,
            initialized: false,
        }
    }

    /// Create a new MCP server with custom metrics collector
    pub fn with_metrics(config: Config, store: Arc<S>, metrics: Arc<MetricsCollector>) -> Self {
        let tenant_manager = config.data_dir_expanded().ok().map(TenantManager::new);

        Self {
            config,
            store,
            tenant_manager,
            metrics,
            symbol_query_service: None,
            trace_query_service: None,
            initialized: false,
        }
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

            // Parse and handle the request
            let response = self.handle_jsonrpc(&line).await;

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

    /// Handle a single line of input (one JSON-RPC request)
    pub async fn handle_jsonrpc(&mut self, line: &str) -> Response {
        // Try to parse the request
        let request = match Request::parse(line) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "failed to parse request");
                return Response::error(None, e.into());
            }
        };

        // Handle the request
        self.handle_request(request).await
    }

    /// Handle a parsed JSON-RPC request
    async fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id.clone();

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params).await,
            "initialized" | "notifications/initialized" | "notifications/cancelled" | "ping" => {
                // Notification that client is ready - no response needed
                // but we return success for notifications that have an id
                if request.is_notification() {
                    return Response::success(None, Value::Null);
                }
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
    async fn handle_initialize(&mut self, params: Option<Value>) -> Result<Value, McpError> {
        if self.initialized {
            warn!("server already initialized");
        }

        self.initialized = true;
        let protocol_version = negotiate_protocol_version(params.as_ref());

        info!(
            protocol_version = protocol_version,
            server_name = SERVER_NAME,
            server_version = SERVER_VERSION,
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

        // Dispatch to tool handlers
        match name {
            "memory.search" => {
                let params: SearchParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid search params: {}", e))
                })?;
                handle_memory_search(&*self.store, params).await
            }
            "memory.add" => {
                let params: AddParams = serde_json::from_value(arguments)
                    .map_err(|e| McpError::InvalidParams(format!("invalid add params: {}", e)))?;
                handle_memory_add(&*self.store, self.tenant_manager.as_ref(), params).await
            }
            "memory.add_batch" => {
                let params: AddBatchParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid add_batch params: {}", e))
                })?;
                handle_memory_add_batch(&*self.store, self.tenant_manager.as_ref(), params).await
            }
            "task.start" => {
                let params: TaskStartParams = serde_json::from_value(arguments).map_err(|e| {
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
                        McpError::InvalidParams(format!("invalid task.run_finish params: {}", e))
                    })?;
                handle_task_run_finish(&*self.store, self.tenant_manager.as_ref(), params).await
            }
            "task.add_evidence" => {
                let params: TaskAddEvidenceParams =
                    serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid task.add_evidence params: {}", e))
                    })?;
                handle_task_add_evidence(&*self.store, self.tenant_manager.as_ref(), params).await
            }
            "task.finish" => {
                let params: TaskFinishParams = serde_json::from_value(arguments).map_err(|e| {
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
                let params: TaskSearchParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid task.search params: {}", e))
                })?;
                handle_task_search(&*self.store, params).await
            }
            "task.resume" => {
                let params: TaskResumeParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid task.resume params: {}", e))
                })?;
                handle_task_resume(&*self.store, params).await
            }
            "artifact.create" => {
                let params: ArtifactCreateParams =
                    serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid artifact.create params: {}", e))
                    })?;
                handle_artifact_create(&*self.store, self.tenant_manager.as_ref(), params).await
            }
            "artifact.get" => {
                let params: ArtifactGetParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid artifact.get params: {}", e))
                })?;
                handle_artifact_get(&*self.store, params).await
            }
            "artifact.search" => {
                let params: TaskSearchParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid artifact.search params: {}", e))
                })?;
                handle_artifact_search(&*self.store, params).await
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
                let params: ArtifactListThreadParams =
                    serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!(
                            "invalid artifact.list_thread params: {}",
                            e
                        ))
                    })?;
                handle_artifact_list_thread(&*self.store, params).await
            }
            "memory.get" => {
                let params: GetParams = serde_json::from_value(arguments)
                    .map_err(|e| McpError::InvalidParams(format!("invalid get params: {}", e)))?;
                handle_memory_get(&*self.store, params).await
            }
            "memory.delete" => {
                let params: DeleteParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid delete params: {}", e))
                })?;
                handle_memory_delete(&*self.store, params).await
            }
            "memory.feedback" => {
                let params: FeedbackParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid feedback params: {}", e))
                })?;
                handle_memory_feedback(&*self.store, params).await
            }
            "memory.stats" => {
                let params: StatsParams = serde_json::from_value(arguments)
                    .map_err(|e| McpError::InvalidParams(format!("invalid stats params: {}", e)))?;
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
            "memory.consolidate_episode" => {
                let params: ConsolidateEpisodeParams =
                    serde_json::from_value(arguments).map_err(|e| {
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
                        McpError::InvalidParams(format!("invalid list_subsystems params: {}", e))
                    })?;
                handle_context_list_subsystems(&*self.store, params).await
            }
            "context.get_files_for_subsystem" => {
                let params: ContextGetFilesForSubsystemParams = serde_json::from_value(arguments)
                    .map_err(|e| {
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
                let params: ContextFindRelevantContextParams = serde_json::from_value(arguments)
                    .map_err(|e| {
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
                let params: ContextSuggestAgentParams =
                    serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid suggest_agent params: {}", e))
                    })?;
                handle_context_suggest_agent(&*self.store, params).await
            }
            "context.get_hot_context" => {
                let params: ContextGetHotContextParams = serde_json::from_value(arguments)
                    .map_err(|e| {
                        McpError::InvalidParams(format!("invalid get_hot_context params: {}", e))
                    })?;
                handle_context_get_hot_context(&*self.store, params).await
            }
            "code.find_definition" => {
                let params: FindDefinitionParams =
                    serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid find_definition params: {}", e))
                    })?;
                let query_service = self.symbol_query_service.as_ref().ok_or_else(|| {
                    McpError::ToolError("Structural index not initialized".to_string())
                })?;
                handle_find_definition(query_service, params)
            }
            "code.find_references" => {
                let params: FindReferencesParams =
                    serde_json::from_value(arguments).map_err(|e| {
                        McpError::InvalidParams(format!("invalid find_references params: {}", e))
                    })?;
                let query_service = self.symbol_query_service.as_ref().ok_or_else(|| {
                    McpError::ToolError("Structural index not initialized".to_string())
                })?;
                handle_find_references(query_service, params)
            }
            "code.find_callers" => {
                let params: FindCallersParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid find_callers params: {}", e))
                })?;
                let query_service = self.symbol_query_service.as_ref().ok_or_else(|| {
                    McpError::ToolError("Structural index not initialized".to_string())
                })?;
                handle_find_callers(query_service, params)
            }
            "code.find_imports" => {
                let params: FindImportsParams = serde_json::from_value(arguments).map_err(|e| {
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
                        McpError::InvalidParams(format!("invalid find_tool_calls params: {}", e))
                    })?;
                let trace_service = self.trace_query_service.as_ref().ok_or_else(|| {
                    McpError::ToolError("Trace index not initialized".to_string())
                })?;
                handle_find_tool_calls(trace_service, params)
            }
            "debug.find_errors" => {
                let params: FindErrorsParams = serde_json::from_value(arguments).map_err(|e| {
                    McpError::InvalidParams(format!("invalid find_errors params: {}", e))
                })?;
                let trace_service = self.trace_query_service.as_ref().ok_or_else(|| {
                    McpError::ToolError("Trace index not initialized".to_string())
                })?;
                handle_find_errors(trace_service, params)
            }
            _ => Err(McpError::InvalidParams(format!("unknown tool '{}'", name))),
        }
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
            )
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
                )
            }
        };

        let is_notification = request.is_notification();
        let mut server = state.server.lock().await;
        let response = server.handle_request(request).await;

        if is_notification {
            if let Some(error) = response.error {
                return json_error_http_response(StatusCode::BAD_REQUEST, error, protocol_version);
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
                )
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

async fn serve_http_server<S: Store + Send + Sync + 'static>(
    listener: TcpListener,
    server: McpServer<S>,
    path: &str,
) -> crate::Result<()> {
    let accept_header_note = "clients should send Accept: application/json, text/event-stream";
    info!(
        bind = %listener.local_addr().map(|addr| addr.to_string()).unwrap_or_else(|_| "<unknown>".to_string()),
        path = path,
        note = accept_header_note,
        "HTTP MCP server starting"
    );

    let state = HttpServerState {
        server: Arc::new(AsyncMutex::new(server)),
    };
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

    use super::super::protocol::RequestId;
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
            symbol_query_service: None,
            trace_query_service: None,
            initialized: false,
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

    #[tokio::test]
    async fn notifications_initialized_alias_is_accepted() {
        let mut server = test_server();
        let response = server
            .handle_jsonrpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;

        assert!(response.error.is_none());
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
