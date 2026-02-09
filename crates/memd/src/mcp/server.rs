//! MCP server implementation
//!
//! Handles JSON-RPC communication over stdio transport.
//! This is the primary interface for agent integration.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use super::error::McpError;
use super::handlers::{
    handle_find_callers, handle_find_definition, handle_find_errors, handle_find_imports,
    handle_find_references, handle_find_tool_calls, handle_memory_add, handle_memory_add_batch,
    handle_memory_compact, handle_memory_consolidate_episode, handle_memory_delete,
    handle_memory_get, handle_memory_metrics, handle_memory_search, handle_memory_stats,
    AddBatchParams, AddParams, CompactParams, ConsolidateEpisodeParams, DeleteParams,
    FindCallersParams, FindDefinitionParams, FindErrorsParams, FindImportsParams,
    FindReferencesParams, FindToolCallsParams, GetParams, MetricsParams, SearchParams, StatsParams,
};
use super::protocol::{Request, Response};
use super::tools::get_all_tools;
use crate::metrics::MetricsCollector;
use crate::store::{Store, TenantManager};
use crate::structural::{SymbolQueryService, TraceQueryService};
use crate::Config;

/// MCP protocol version supported by this server
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Server name for capability negotiation
const SERVER_NAME: &str = "memd";

/// Server version
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

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
            let response = self.handle_line(&line).await;

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
    async fn handle_line(&mut self, line: &str) -> Response {
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
            "initialized" => {
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
    async fn handle_initialize(&mut self, _params: Option<Value>) -> Result<Value, McpError> {
        if self.initialized {
            warn!("server already initialized");
        }

        self.initialized = true;

        info!(
            protocol_version = PROTOCOL_VERSION,
            server_name = SERVER_NAME,
            server_version = SERVER_VERSION,
            "server initialized"
        );

        Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
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

    use super::super::protocol::RequestId;
    use crate::config::Config;
    use crate::error::Result as MemdResult;
    use crate::metrics::IndexStats;
    use crate::metrics::QueryMetrics;
    use crate::store::{MemoryStore, PersistentStore, PersistentStoreConfig, Store, StoreStats};
    use crate::types::{ChunkId, MemoryChunk, TenantId};
    use async_trait::async_trait;
    use tempfile::tempdir;

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

    #[tokio::test]
    async fn handle_initialize() {
        let mut server = test_server();
        let result = server.handle_initialize(None).await.unwrap();

        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
        assert!(result["capabilities"]["tools"].is_object());
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
