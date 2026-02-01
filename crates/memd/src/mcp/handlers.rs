//! Tool call handlers for MCP
//!
//! Bridges MCP tool calls to store operations.
//! Each handler validates parameters, calls the store, and formats the response.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use super::error::McpError;
use crate::metrics::{IndexStats, MetricsCollector};
use crate::store::{Store, StoreStats, TenantManager};
use crate::types::{ChunkId, ChunkType, MemoryChunk, ProjectId, Source, TenantId};

// ---------- Request Types ----------

/// Parameters for memory.search
#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub tenant_id: String,
    pub query: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default)]
    pub filters: Option<SearchFilters>,
    /// Enable debug output showing tier source for each result
    #[serde(default)]
    pub debug_tiers: Option<bool>,
}

fn default_k() -> usize {
    20
}

/// Optional filters for search
#[derive(Debug, Deserialize, Default)]
pub struct SearchFilters {
    #[serde(default)]
    pub types: Option<Vec<String>>,
    #[serde(default)]
    pub time_range: Option<TimeRange>,
}

/// Time range filter
#[derive(Debug, Deserialize)]
pub struct TimeRange {
    pub from: Option<i64>,
    pub to: Option<i64>,
}

/// Parameters for memory.add
#[derive(Debug, Deserialize)]
pub struct AddParams {
    pub tenant_id: String,
    pub text: String,
    #[serde(rename = "type")]
    pub chunk_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub source: Option<SourceParams>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Source information for a chunk
#[derive(Debug, Deserialize, Default)]
pub struct SourceParams {
    pub uri: Option<String>,
    pub repo: Option<String>,
    pub commit: Option<String>,
    pub path: Option<String>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
}

/// Single chunk for batch add
#[derive(Debug, Deserialize)]
pub struct BatchChunkParams {
    pub text: String,
    #[serde(rename = "type")]
    pub chunk_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub source: Option<SourceParams>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Parameters for memory.add_batch
#[derive(Debug, Deserialize)]
pub struct AddBatchParams {
    pub tenant_id: String,
    pub chunks: Vec<BatchChunkParams>,
}

/// Parameters for memory.get
#[derive(Debug, Deserialize)]
pub struct GetParams {
    pub tenant_id: String,
    pub chunk_id: String,
}

/// Parameters for memory.delete
#[derive(Debug, Deserialize)]
pub struct DeleteParams {
    pub tenant_id: String,
    pub chunk_id: String,
}

/// Parameters for memory.stats
#[derive(Debug, Deserialize)]
pub struct StatsParams {
    pub tenant_id: String,
}

/// Parameters for memory.metrics
#[derive(Debug, Deserialize, Default)]
pub struct MetricsParams {
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default = "default_true")]
    pub include_recent: bool,
    /// Include tiered stats (cache, hot tier, promotions) - default true
    #[serde(default = "default_true")]
    pub include_tiered: bool,
}

fn default_true() -> bool {
    true
}

// ---------- Response Types ----------

/// Result of a search operation
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub results: Vec<ChunkResult>,
    /// Tier debug info (only present when debug_tiers=true)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tier_info: Option<TierDebugInfo>,
}

/// Debug information about tier performance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierDebugInfo {
    /// Primary source tier ("cache" | "hot" | "warm" | "hybrid")
    pub source_tier: String,
    /// Whether cache was hit
    pub cache_hit: bool,
    /// Whether hot tier returned results
    pub hot_tier_hit: bool,
    /// Cache lookup latency (ms)
    pub cache_lookup_ms: u64,
    /// Hot tier search latency (ms)
    pub hot_tier_ms: u64,
    /// Warm tier search latency (ms)
    pub warm_tier_ms: u64,
}

/// Single chunk in search results
#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkResult {
    pub chunk_id: String,
    pub text: String,
    pub score: f32, // Stub: 1.0 for all results
    pub chunk_type: String,
    pub source: SourceResult,
    pub timestamp_created: i64,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    /// Which tier this result came from (only present when debug_tiers=true)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_tier: Option<String>,
}

/// Source information in results
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct SourceResult {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

impl From<&Source> for SourceResult {
    fn from(s: &Source) -> Self {
        Self {
            uri: s.uri.clone(),
            repo: s.repo.clone(),
            commit: s.commit.clone(),
            path: s.path.clone(),
            tool_name: s.tool_name.clone(),
            tool_call_id: s.tool_call_id.clone(),
        }
    }
}

/// Result of an add operation
#[derive(Debug, Serialize, Deserialize)]
pub struct AddResult {
    pub chunk_id: String,
}

/// Result of a batch add operation
#[derive(Debug, Serialize, Deserialize)]
pub struct AddBatchResult {
    pub chunk_ids: Vec<String>,
}

/// Result of a delete operation
#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteResult {
    pub deleted: bool,
}

/// Result of a stats operation
#[derive(Debug, Serialize, Deserialize)]
pub struct StatsResult {
    pub total_chunks: usize,
    pub deleted_chunks: usize,
    pub chunk_types: HashMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub disk_stats: Option<DiskStatsResult>,
}

/// Disk statistics in stats result
#[derive(Debug, Serialize, Deserialize)]
pub struct DiskStatsResult {
    pub total_bytes: u64,
    pub segment_count: usize,
}

/// Combined tiered search statistics result
#[derive(Debug, Serialize, Deserialize)]
pub struct TieredStatsResult {
    /// Semantic cache statistics
    pub cache_stats: CacheStatsResult,
    /// Hot tier statistics
    pub hot_tier_stats: HotTierStatsResult,
    /// Tiered performance metrics
    pub metrics: TieredMetricsResult,
}

/// Cache statistics
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheStatsResult {
    /// Total cache lookups
    pub total_lookups: u64,
    /// Cache hits
    pub hits: u64,
    /// Cache misses
    pub misses: u64,
    /// Hit rate (0.0-1.0)
    pub hit_rate: f32,
    /// Number of entries in cache
    pub entry_count: usize,
    /// Average confidence of cached entries
    pub avg_confidence: f32,
}

/// Hot tier statistics
#[derive(Debug, Serialize, Deserialize)]
pub struct HotTierStatsResult {
    /// Number of chunks in hot tier
    pub chunk_count: usize,
    /// Capacity used (0.0-1.0)
    pub capacity_used: f32,
    /// Hot tier version
    pub version: u64,
    /// Average promotion score of chunks in hot tier
    pub avg_promotion_score: f32,
}

/// Tiered performance metrics
#[derive(Debug, Serialize, Deserialize)]
pub struct TieredMetricsResult {
    /// Total promotions
    pub promotions: u64,
    /// Total demotions
    pub demotions: u64,
    /// Average cache lookup latency (ms)
    pub avg_cache_ms: f64,
    /// Average hot tier search latency (ms)
    pub avg_hot_tier_ms: f64,
    /// Average warm tier search latency (ms)
    pub avg_warm_tier_ms: f64,
}

// ---------- Helper Functions ----------

/// Parse a chunk type string into ChunkType enum
fn parse_chunk_type(s: &str) -> Result<ChunkType, McpError> {
    match s.to_lowercase().as_str() {
        "code" => Ok(ChunkType::Code),
        "doc" | "scientific" => Ok(ChunkType::Doc),  // Map scientific documents to Doc type
        "trace" => Ok(ChunkType::Trace),
        "decision" => Ok(ChunkType::Decision),
        "plan" => Ok(ChunkType::Plan),
        "research" => Ok(ChunkType::Research),
        "message" => Ok(ChunkType::Message),
        "summary" => Ok(ChunkType::Summary),
        "general" | "other" => Ok(ChunkType::Other),
        _ => Err(McpError::InvalidParams(format!(
            "invalid chunk type '{}', must be one of: code, doc, scientific, trace, decision, plan, research, message, summary, general, other",
            s
        ))),
    }
}

/// Validate tenant_id and return TenantId
fn validate_tenant_id(tenant_id: &str) -> Result<TenantId, McpError> {
    TenantId::new(tenant_id).map_err(|e| McpError::InvalidParams(e.to_string()))
}

/// Validate chunk_id and return ChunkId
fn validate_chunk_id(chunk_id: &str) -> Result<ChunkId, McpError> {
    ChunkId::parse(chunk_id).map_err(|e| McpError::InvalidParams(e.to_string()))
}

/// Convert SourceParams to Source
fn params_to_source(params: Option<SourceParams>) -> Source {
    params
        .map(|p| Source {
            uri: p.uri,
            repo: p.repo,
            commit: p.commit,
            path: p.path,
            tool_name: p.tool_name,
            tool_call_id: p.tool_call_id,
        })
        .unwrap_or_default()
}

/// Format result as MCP content response
fn format_mcp_response<T: Serialize>(result: &T) -> Result<Value, McpError> {
    let json_str = serde_json::to_string(result).map_err(|e| {
        McpError::ToolError(format!("failed to serialize response: {}", e))
    })?;

    Ok(json!({
        "content": [{
            "type": "text",
            "text": json_str
        }]
    }))
}

// ---------- Handler Functions ----------

/// Handle memory.search tool call
pub async fn handle_memory_search<S: Store>(
    store: &S,
    params: SearchParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let debug_tiers = params.debug_tiers.unwrap_or(false);

    info!(
        tenant_id = %tenant_id,
        query = %params.query,
        k = params.k,
        debug_tiers = debug_tiers,
        "memory.search"
    );

    // Use search_with_tier_info if debug_tiers is requested
    if debug_tiers {
        let (scored_chunks, timing) = store
            .search_with_tier_info(&tenant_id, &params.query, params.k)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;

        debug!(results_count = scored_chunks.len(), "search completed with tier info");

        // Build tier debug info if timing is available
        let tier_info = timing.map(|t| {
            let source_tier = if t.cache_lookup_ms > 0 && t.hot_tier_ms == 0 && t.warm_tier_ms == 0 {
                "cache".to_string()
            } else if t.hot_tier_ms > 0 && t.warm_tier_ms == 0 {
                "hot".to_string()
            } else if t.warm_tier_ms > 0 {
                "warm".to_string()
            } else {
                "hybrid".to_string()
            };

            let cache_hit = t.cache_lookup_ms > 0 && t.hot_tier_ms == 0 && t.warm_tier_ms == 0;
            let hot_tier_hit = t.hot_tier_ms > 0 && t.warm_tier_ms == 0;

            TierDebugInfo {
                source_tier,
                cache_hit,
                hot_tier_hit,
                cache_lookup_ms: t.cache_lookup_ms,
                hot_tier_ms: t.hot_tier_ms,
                warm_tier_ms: t.warm_tier_ms,
            }
        });

        // Determine source tier per result based on scoring heuristics
        // If we have tier_info, derive per-result tier from overall timing
        let default_tier = tier_info.as_ref().map(|t| t.source_tier.clone());

        let results: Vec<ChunkResult> = scored_chunks
            .iter()
            .map(|(chunk, score)| ChunkResult {
                chunk_id: chunk.chunk_id.to_string(),
                text: chunk.text.clone(),
                score: *score,
                chunk_type: chunk.chunk_type.to_string(),
                source: SourceResult::from(&chunk.source),
                timestamp_created: chunk.timestamp_created,
                tags: chunk.tags.clone(),
                source_tier: default_tier.clone(),
            })
            .collect();

        return format_mcp_response(&SearchResult { results, tier_info });
    }

    // Standard path without tier info
    let scored_chunks = store
        .search_with_scores(&tenant_id, &params.query, params.k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = scored_chunks.len(), "search completed");

    let results: Vec<ChunkResult> = scored_chunks
        .iter()
        .map(|(chunk, score)| ChunkResult {
            chunk_id: chunk.chunk_id.to_string(),
            text: chunk.text.clone(),
            score: *score,
            chunk_type: chunk.chunk_type.to_string(),
            source: SourceResult::from(&chunk.source),
            timestamp_created: chunk.timestamp_created,
            tags: chunk.tags.clone(),
            source_tier: None,
        })
        .collect();

    format_mcp_response(&SearchResult { results, tier_info: None })
}

/// Handle memory.add tool call
pub async fn handle_memory_add<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: AddParams,
) -> Result<Value, McpError> {
    warn!("🔍 [DEBUG] handle_memory_add CALLED - tenant_id={}, text_len={}",
          params.tenant_id, params.text.len());
    let tenant_id = match validate_tenant_id(&params.tenant_id) {
        Ok(tid) => {
            warn!("🔍 [DEBUG] tenant_id validation succeeded: {}", tid);
            tid
        }
        Err(e) => {
            warn!("❌ [DEBUG] tenant_id validation FAILED: {}", e);
            return Err(e);
        }
    };
    let chunk_type = match parse_chunk_type(&params.chunk_type) {
        Ok(ct) => {
            warn!("🔍 [DEBUG] chunk_type validation succeeded: {:?}", ct);
            ct
        }
        Err(e) => {
            warn!("❌ [DEBUG] chunk_type validation FAILED: {}", e);
            return Err(e);
        }
    };

    info!(
        tenant_id = %tenant_id,
        chunk_type = %chunk_type,
        text_len = params.text.len(),
        "memory.add"
    );

    // Ensure tenant directory exists if tenant_manager is available
    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut chunk = MemoryChunk::new(tenant_id, &params.text, chunk_type);

    // Apply optional fields
    if let Some(project_id) = &params.project_id {
        chunk = chunk.with_project(ProjectId::new(Some(project_id.clone())));
    }

    chunk = chunk.with_source(params_to_source(params.source));

    if !params.tags.is_empty() {
        chunk = chunk.with_tags(params.tags);
    }

    let chunk_id = store.add(chunk).await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    info!(chunk_id = %chunk_id, "chunk added");

    format_mcp_response(&AddResult {
        chunk_id: chunk_id.to_string(),
    })
}

/// Handle memory.add_batch tool call
pub async fn handle_memory_add_batch<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: AddBatchParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        count = params.chunks.len(),
        "memory.add_batch"
    );

    // Ensure tenant directory exists if tenant_manager is available
    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut chunks = Vec::with_capacity(params.chunks.len());

    for chunk_params in params.chunks {
        let chunk_type = parse_chunk_type(&chunk_params.chunk_type)?;
        let mut chunk = MemoryChunk::new(tenant_id.clone(), &chunk_params.text, chunk_type);

        if let Some(project_id) = &chunk_params.project_id {
            chunk = chunk.with_project(ProjectId::new(Some(project_id.clone())));
        }

        chunk = chunk.with_source(params_to_source(chunk_params.source));

        if !chunk_params.tags.is_empty() {
            chunk = chunk.with_tags(chunk_params.tags);
        }

        chunks.push(chunk);
    }

    let chunk_ids = store.add_batch(chunks).await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    info!(count = chunk_ids.len(), "batch add completed");

    format_mcp_response(&AddBatchResult {
        chunk_ids: chunk_ids.iter().map(|id| id.to_string()).collect(),
    })
}

/// Handle memory.get tool call
pub async fn handle_memory_get<S: Store>(
    store: &S,
    params: GetParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;

    debug!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        "memory.get"
    );

    let chunk = store.get(&tenant_id, &chunk_id).await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    let json_str = if let Some(c) = chunk {
        info!(chunk_id = %chunk_id, "chunk found");
        serde_json::to_string(&c).map_err(|e| {
            McpError::ToolError(format!("failed to serialize chunk: {}", e))
        })?
    } else {
        debug!(chunk_id = %chunk_id, "chunk not found");
        "null".to_string()
    };

    Ok(json!({
        "content": [{
            "type": "text",
            "text": json_str
        }]
    }))
}

/// Handle memory.delete tool call
pub async fn handle_memory_delete<S: Store>(
    store: &S,
    params: DeleteParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;

    info!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        "memory.delete"
    );

    let deleted = store.delete(&tenant_id, &chunk_id).await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    if deleted {
        info!(chunk_id = %chunk_id, "chunk deleted");
    } else {
        warn!(chunk_id = %chunk_id, "chunk not found for deletion");
    }

    format_mcp_response(&DeleteResult { deleted })
}

/// Handle memory.stats tool call
pub async fn handle_memory_stats<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: StatsParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;

    info!(tenant_id = %tenant_id, "memory.stats");

    let store_stats: StoreStats = store.stats(&tenant_id).await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Get disk stats if tenant_manager is available
    let disk_stats = tenant_manager
        .map(|tm| {
            tm.tenant_disk_stats(&tenant_id)
                .ok()
                .map(|ds| DiskStatsResult {
                    total_bytes: ds.total_bytes,
                    segment_count: ds.segment_count,
                })
        })
        .flatten();

    format_mcp_response(&StatsResult {
        total_chunks: store_stats.total_chunks,
        deleted_chunks: store_stats.deleted_chunks,
        chunk_types: store_stats.chunk_types,
        disk_stats,
    })
}

/// Handle memory.metrics tool call
pub fn handle_memory_metrics(
    metrics: &MetricsCollector,
    index_stats: HashMap<String, IndexStats>,
    params: MetricsParams,
) -> Result<Value, McpError> {
    info!(
        tenant_id = ?params.tenant_id,
        include_recent = params.include_recent,
        include_tiered = params.include_tiered,
        "memory.metrics"
    );

    // Filter index stats by tenant if specified
    let filtered_stats = if let Some(ref tenant_id_str) = params.tenant_id {
        let tenant_id = validate_tenant_id(tenant_id_str)?;
        index_stats
            .into_iter()
            .filter(|(k, _)| k == tenant_id.as_str())
            .collect()
    } else {
        index_stats
    };

    let mut snapshot = metrics.snapshot(filtered_stats);

    if !params.include_recent {
        snapshot.recent_queries.clear();
    }

    // Clear tiered stats if not requested
    if !params.include_tiered {
        snapshot.tiered = Default::default();
    }

    format_mcp_response(&snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn make_store() -> MemoryStore {
        MemoryStore::new()
    }

    #[tokio::test]
    async fn search_empty_store() {
        let store = make_store();
        let params = SearchParams {
            tenant_id: "test".to_string(),
            query: "hello".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
        };

        let result = handle_memory_search(&store, params).await.unwrap();
        assert!(result["content"].is_array());

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_result: SearchResult = serde_json::from_str(text).unwrap();
        assert!(search_result.results.is_empty());
    }

    #[tokio::test]
    async fn add_and_search() {
        let store = make_store();

        // Add a chunk
        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "hello world".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            source: None,
            tags: vec![],
        };

        let add_result = handle_memory_add(&store, None, add_params).await.unwrap();
        let text = add_result["content"][0]["text"].as_str().unwrap();
        let add_response: AddResult = serde_json::from_str(text).unwrap();
        assert!(!add_response.chunk_id.is_empty());

        // Search for it
        let search_params = SearchParams {
            tenant_id: "test".to_string(),
            query: "hello".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
        };

        let search_result = handle_memory_search(&store, search_params).await.unwrap();
        let text = search_result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(search_response.results[0].text, "hello world");
    }

    #[tokio::test]
    async fn add_with_all_fields() {
        let store = make_store();

        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "function hello() {}".to_string(),
            chunk_type: "code".to_string(),
            project_id: Some("my_project".to_string()),
            source: Some(SourceParams {
                path: Some("src/main.rs".to_string()),
                repo: Some("my-repo".to_string()),
                ..Default::default()
            }),
            tags: vec!["rust".to_string(), "function".to_string()],
        };

        let result = handle_memory_add(&store, None, add_params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let response: AddResult = serde_json::from_str(text).unwrap();

        // Verify the chunk was stored correctly
        let get_params = GetParams {
            tenant_id: "test".to_string(),
            chunk_id: response.chunk_id.clone(),
        };

        let get_result = handle_memory_get(&store, get_params).await.unwrap();
        let text = get_result["content"][0]["text"].as_str().unwrap();
        let chunk: MemoryChunk = serde_json::from_str(text).unwrap();

        assert_eq!(chunk.text, "function hello() {}");
        assert_eq!(chunk.chunk_type, ChunkType::Code);
        assert_eq!(chunk.source.path, Some("src/main.rs".to_string()));
        assert_eq!(chunk.tags, vec!["rust", "function"]);
    }

    #[tokio::test]
    async fn add_batch() {
        let store = make_store();

        let params = AddBatchParams {
            tenant_id: "test".to_string(),
            chunks: vec![
                BatchChunkParams {
                    text: "chunk 1".to_string(),
                    chunk_type: "doc".to_string(),
                    project_id: None,
                    source: None,
                    tags: vec![],
                },
                BatchChunkParams {
                    text: "chunk 2".to_string(),
                    chunk_type: "code".to_string(),
                    project_id: None,
                    source: None,
                    tags: vec![],
                },
            ],
        };

        let result = handle_memory_add_batch(&store, None, params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let response: AddBatchResult = serde_json::from_str(text).unwrap();
        assert_eq!(response.chunk_ids.len(), 2);
    }

    #[tokio::test]
    async fn delete_chunk() {
        let store = make_store();

        // Add a chunk
        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "to be deleted".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            source: None,
            tags: vec![],
        };

        let add_result = handle_memory_add(&store, None, add_params).await.unwrap();
        let text = add_result["content"][0]["text"].as_str().unwrap();
        let add_response: AddResult = serde_json::from_str(text).unwrap();

        // Delete it
        let delete_params = DeleteParams {
            tenant_id: "test".to_string(),
            chunk_id: add_response.chunk_id.clone(),
        };

        let delete_result = handle_memory_delete(&store, delete_params).await.unwrap();
        let text = delete_result["content"][0]["text"].as_str().unwrap();
        let delete_response: DeleteResult = serde_json::from_str(text).unwrap();
        assert!(delete_response.deleted);

        // Verify it's no longer retrievable
        let get_params = GetParams {
            tenant_id: "test".to_string(),
            chunk_id: add_response.chunk_id,
        };

        let get_result = handle_memory_get(&store, get_params).await.unwrap();
        let text = get_result["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "null");
    }

    #[tokio::test]
    async fn stats() {
        let store = make_store();

        // Add some chunks
        for i in 0..3 {
            let add_params = AddParams {
                tenant_id: "test".to_string(),
                text: format!("doc {}", i),
                chunk_type: "doc".to_string(),
                project_id: None,
                source: None,
                tags: vec![],
            };
            handle_memory_add(&store, None, add_params).await.unwrap();
        }

        let params = StatsParams {
            tenant_id: "test".to_string(),
        };

        let result = handle_memory_stats(&store, None, params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let stats: StatsResult = serde_json::from_str(text).unwrap();

        assert_eq!(stats.total_chunks, 3);
        assert_eq!(stats.deleted_chunks, 0);
        assert_eq!(stats.chunk_types.get("doc"), Some(&3));
    }

    #[tokio::test]
    async fn invalid_tenant_id() {
        let store = make_store();

        let params = SearchParams {
            tenant_id: "invalid-tenant".to_string(), // hyphens not allowed
            query: "test".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
        };

        let result = handle_memory_search(&store, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn invalid_chunk_type() {
        let store = make_store();

        let params = AddParams {
            tenant_id: "test".to_string(),
            text: "hello".to_string(),
            chunk_type: "invalid_type".to_string(),
            project_id: None,
            source: None,
            tags: vec![],
        };

        let result = handle_memory_add(&store, None, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn invalid_chunk_id() {
        let store = make_store();

        let params = GetParams {
            tenant_id: "test".to_string(),
            chunk_id: "not-a-uuid".to_string(),
        };

        let result = handle_memory_get(&store, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn tenant_isolation() {
        let store = make_store();

        // Add chunk as tenant A
        let add_params = AddParams {
            tenant_id: "tenant_a".to_string(),
            text: "secret data".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            source: None,
            tags: vec![],
        };

        handle_memory_add(&store, None, add_params).await.unwrap();

        // Search as tenant B - should return empty
        let search_params = SearchParams {
            tenant_id: "tenant_b".to_string(),
            query: "secret".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
        };

        let result = handle_memory_search(&store, search_params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert!(search_response.results.is_empty());
    }

    #[tokio::test]
    async fn search_with_debug_tiers() {
        let store = make_store();

        // Add a chunk
        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "debug tier test".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            source: None,
            tags: vec![],
        };

        handle_memory_add(&store, None, add_params).await.unwrap();

        // Search with debug_tiers enabled
        let search_params = SearchParams {
            tenant_id: "test".to_string(),
            query: "debug".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: Some(true),
        };

        let result = handle_memory_search(&store, search_params).await.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();

        // MemoryStore doesn't have tiered support, so tier_info should be None
        // and source_tier on results should be None (since timing is None)
        assert_eq!(search_response.results.len(), 1);
        assert!(search_response.tier_info.is_none());
    }
}
