//! Tool call handlers for MCP
//!
//! Bridges MCP tool calls to store operations.
//! Each handler validates parameters, calls the store, and formats the response.

use std::collections::{HashMap, HashSet};

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
    pub episode_id: Option<String>,
    #[serde(default)]
    pub time_range: Option<TimeRange>,
}

/// Time range filter
#[derive(Debug, Deserialize)]
pub struct TimeRange {
    pub from: Option<String>,
    pub to: Option<String>,
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
    pub episode_id: Option<String>,
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
    pub episode_id: Option<String>,
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

/// Parameters for memory.compact
#[derive(Debug, Deserialize)]
pub struct CompactParams {
    pub tenant_id: String,
    #[serde(default)]
    pub force: bool,
}

/// Parameters for memory.consolidate_episode
#[derive(Debug, Deserialize)]
pub struct ConsolidateEpisodeParams {
    pub tenant_id: String,
    pub episode_id: String,
    #[serde(default = "default_episode_limit")]
    pub max_chunks: usize,
    #[serde(default = "default_true")]
    pub retain_source_chunks: bool,
}

fn default_episode_limit() -> usize {
    50
}

fn default_true() -> bool {
    true
}

fn default_depth() -> u32 {
    1
}

/// Parameters for code.find_definition
#[derive(Debug, Deserialize)]
pub struct FindDefinitionParams {
    pub tenant_id: String,
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Parameters for code.find_references
#[derive(Debug, Deserialize)]
pub struct FindReferencesParams {
    pub tenant_id: String,
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Parameters for code.find_callers
#[derive(Debug, Deserialize)]
pub struct FindCallersParams {
    pub tenant_id: String,
    pub name: String,
    #[serde(default = "default_depth")]
    pub depth: u32,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Parameters for code.find_imports
#[derive(Debug, Deserialize)]
pub struct FindImportsParams {
    pub tenant_id: String,
    pub module: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

fn default_limit() -> usize {
    50
}

/// Parameters for debug.find_tool_calls
#[derive(Debug, Deserialize)]
pub struct FindToolCallsParams {
    pub tenant_id: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub time_from: Option<String>,
    #[serde(default)]
    pub time_to: Option<String>,
    #[serde(default)]
    pub errors_only: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// Parameters for debug.find_errors
#[derive(Debug, Deserialize)]
pub struct FindErrorsParams {
    pub tenant_id: String,
    #[serde(default)]
    pub error_signature: Option<String>,
    #[serde(default)]
    pub function_name: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub time_from: Option<String>,
    #[serde(default)]
    pub time_to: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_true")]
    pub include_frames: bool,
}

// ---------- Response Types ----------

/// Result of a search operation
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub results: Vec<ChunkResult>,
    /// Tier debug info (only present when debug_tiers=true)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tier_info: Option<TierDebugInfo>,
    /// Repair-loop diagnostics when a fallback query rewrite was attempted
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repair_info: Option<RepairInfo>,
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

/// Diagnostics for query repair behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairInfo {
    pub attempted: bool,
    pub repaired: bool,
    pub original_query: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repaired_query: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub episode_id: Option<String>,
    /// Provenance-first citation details for this result
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub citation: Option<CitationResult>,
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

/// Citation metadata for a search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationResult {
    pub citation_id: String,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chunk_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub total_chunks: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub char_start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub char_end: Option<usize>,
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

/// Result of memory.consolidate_episode
#[derive(Debug, Serialize, Deserialize)]
pub struct ConsolidateEpisodeResult {
    pub summary_chunk_id: String,
    pub source_chunk_count: usize,
    pub retained_source_chunks: bool,
}

/// Result of a stats operation
#[derive(Debug, Serialize, Deserialize)]
pub struct StatsResult {
    pub total_chunks: usize,
    pub deleted_chunks: usize,
    pub chunk_types: HashMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub disk_stats: Option<DiskStatsResult>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub compaction: Option<CompactionStatsResult>,
}

/// Disk statistics in stats result
#[derive(Debug, Serialize, Deserialize)]
pub struct DiskStatsResult {
    pub total_bytes: u64,
    pub segment_count: usize,
}

/// Compaction statistics in stats result
#[derive(Debug, Serialize, Deserialize)]
pub struct CompactionStatsResult {
    /// Ratio of deleted to total chunks (0.0 to 1.0)
    pub tombstone_ratio: f32,
    /// Number of active (non-deleted) chunks
    pub active_chunks: usize,
    /// Number of deleted chunks
    pub deleted_chunks: usize,
    /// Number of sparse index segments
    pub segment_count: usize,
    /// HNSW index staleness (0.0 to 1.0)
    pub hnsw_staleness: f32,
    /// Number of embeddings in HNSW cache
    pub hnsw_cache_size: usize,
    /// Number of embeddings in HNSW index
    pub hnsw_index_size: usize,
    /// Whether compaction is needed based on default thresholds
    pub needs_compaction: bool,
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

/// Result of code.find_definition
#[derive(Debug, Serialize, Deserialize)]
pub struct FindDefinitionResult {
    pub definitions: Vec<SymbolLocationResult>,
}

/// A symbol location in the codebase
#[derive(Debug, Serialize, Deserialize)]
pub struct SymbolLocationResult {
    pub file_path: String,
    pub name: String,
    pub kind: String,
    pub line_start: u32,
    pub line_end: u32,
    pub col_start: u32,
    pub col_end: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    pub language: String,
}

/// Result of code.find_references
#[derive(Debug, Serialize, Deserialize)]
pub struct FindReferencesResult {
    pub references: Vec<SymbolLocationResult>,
}

/// Result of code.find_callers
#[derive(Debug, Serialize, Deserialize)]
pub struct FindCallersResult {
    pub callers: Vec<CallerInfoResult>,
}

/// Information about a caller
#[derive(Debug, Serialize, Deserialize)]
pub struct CallerInfoResult {
    pub caller_name: String,
    pub caller_file: String,
    pub call_line: u32,
    pub call_col: u32,
    pub caller_kind: String,
    pub depth: u32,
}

/// Result of code.find_imports
#[derive(Debug, Serialize, Deserialize)]
pub struct FindImportsResult {
    pub imports: Vec<ImportInfoResult>,
}

/// Information about an import
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportInfoResult {
    pub importing_file: String,
    pub import_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

// ---------- Helper Functions ----------

fn validate_search_k(k: usize) -> Result<(), McpError> {
    if (1..=100).contains(&k) {
        return Ok(());
    }

    Err(McpError::InvalidParams(
        "invalid 'k': must be between 1 and 100".to_string(),
    ))
}

fn validate_search_time_range(
    filters: Option<&SearchFilters>,
) -> Result<(Option<i64>, Option<i64>), McpError> {
    let Some(time_range) = filters.and_then(|f| f.time_range.as_ref()) else {
        return Ok((None, None));
    };

    let from_ms = time_range
        .from
        .as_deref()
        .map(|s| {
            crate::structural::parse_iso_datetime(s).map_err(|e| {
                McpError::InvalidParams(format!("invalid filters.time_range.from: {}", e))
            })
        })
        .transpose()?;

    let to_ms = time_range
        .to
        .as_deref()
        .map(|s| {
            crate::structural::parse_iso_datetime(s).map_err(|e| {
                McpError::InvalidParams(format!("invalid filters.time_range.to: {}", e))
            })
        })
        .transpose()?;

    if let (Some(from_ms), Some(to_ms)) = (from_ms, to_ms) {
        if from_ms > to_ms {
            return Err(McpError::InvalidParams(
                "invalid filters.time_range: 'from' must be <= 'to'".to_string(),
            ));
        }
    }

    Ok((from_ms, to_ms))
}

#[derive(Debug, Default)]
struct ParsedSearchFilters {
    chunk_types: Option<HashSet<ChunkType>>,
    episode_id: Option<String>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
}

fn parse_search_filters(filters: Option<&SearchFilters>) -> Result<ParsedSearchFilters, McpError> {
    let (from_ms, to_ms) = validate_search_time_range(filters)?;

    let chunk_types = filters
        .and_then(|f| f.types.as_ref())
        .map(|types| {
            types
                .iter()
                .map(|t| parse_chunk_type(t))
                .collect::<Result<HashSet<_>, _>>()
        })
        .transpose()?;

    Ok(ParsedSearchFilters {
        chunk_types,
        episode_id: filters.and_then(|f| f.episode_id.clone()),
        from_ms,
        to_ms,
    })
}

fn apply_search_filters(
    scored_chunks: Vec<(MemoryChunk, f32)>,
    project_id: Option<&str>,
    filters: &ParsedSearchFilters,
    k: usize,
) -> Vec<(MemoryChunk, f32)> {
    scored_chunks
        .into_iter()
        .filter(|(chunk, _)| {
            if let Some(project_id) = project_id {
                if chunk.project_id.as_option() != Some(project_id) {
                    return false;
                }
            }

            if let Some(types) = filters.chunk_types.as_ref() {
                if !types.contains(&chunk.chunk_type) {
                    return false;
                }
            }

            if let Some(episode_id) = filters.episode_id.as_deref() {
                let expected_tag = format!("episode:{}", episode_id);
                if !chunk.tags.iter().any(|tag| tag == &expected_tag) {
                    return false;
                }
            }

            if let Some(from_ms) = filters.from_ms {
                if chunk.timestamp_created < from_ms {
                    return false;
                }
            }

            if let Some(to_ms) = filters.to_ms {
                if chunk.timestamp_created > to_ms {
                    return false;
                }
            }

            true
        })
        .take(k)
        .collect()
}

fn parse_tag_usize(tags: &[String], prefix: &str) -> Option<usize> {
    tags.iter().find_map(|tag| {
        tag.strip_prefix(prefix)
            .and_then(|value| value.parse().ok())
    })
}

fn extract_episode_id(tags: &[String]) -> Option<String> {
    tags.iter()
        .find_map(|tag| tag.strip_prefix("episode:").map(|value| value.to_string()))
}

fn make_episode_tag(episode_id: &str) -> String {
    format!("episode:{}", episode_id)
}

fn validate_episode_id(episode_id: &str) -> Result<(), McpError> {
    if episode_id.is_empty() {
        return Err(McpError::InvalidParams(
            "episode_id must not be empty".to_string(),
        ));
    }

    if episode_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(());
    }

    Err(McpError::InvalidParams(
        "episode_id must contain only letters, digits, '_' or '-'".to_string(),
    ))
}

fn build_citation(chunk: &MemoryChunk) -> CitationResult {
    let hash_prefix = chunk.hash.get(..12).unwrap_or(&chunk.hash);
    CitationResult {
        citation_id: format!("{}:{}", chunk.chunk_id, hash_prefix),
        content_hash: chunk.hash.clone(),
        source_uri: chunk.source.uri.clone(),
        source_repo: chunk.source.repo.clone(),
        source_commit: chunk.source.commit.clone(),
        source_path: chunk.source.path.clone(),
        source_tool_name: chunk.source.tool_name.clone(),
        source_tool_call_id: chunk.source.tool_call_id.clone(),
        chunk_index: parse_tag_usize(&chunk.tags, "chunk_index:"),
        total_chunks: parse_tag_usize(&chunk.tags, "total_chunks:"),
        char_start: parse_tag_usize(&chunk.tags, "char_start:"),
        char_end: parse_tag_usize(&chunk.tags, "char_end:"),
    }
}

fn has_active_search_filters(project_id: Option<&str>, filters: &ParsedSearchFilters) -> bool {
    project_id.is_some()
        || filters.chunk_types.is_some()
        || filters.episode_id.is_some()
        || filters.from_ms.is_some()
        || filters.to_ms.is_some()
}

fn adaptive_fetch_k(k: usize, query: &str, has_filters: bool) -> usize {
    if has_filters {
        return 100;
    }

    let token_count = query.split_whitespace().count();
    let is_complex = token_count >= 6 || query.len() >= 80;
    if is_complex {
        return (k.saturating_mul(2)).clamp(1, 100);
    }

    k
}

fn normalize_query_for_repair(query: &str) -> Option<String> {
    let normalized = query
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    if normalized.is_empty() {
        return None;
    }

    let original = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized == original.to_lowercase() {
        None
    } else {
        Some(normalized)
    }
}

fn build_episode_summary_text(episode_id: &str, chunks: &[MemoryChunk]) -> String {
    let mut lines = Vec::with_capacity(chunks.len() + 1);
    lines.push(format!(
        "Episode {} summary ({} chunks)",
        episode_id,
        chunks.len()
    ));

    for chunk in chunks {
        let snippet = chunk
            .text
            .replace('\n', " ")
            .chars()
            .take(180)
            .collect::<String>();
        lines.push(format!("- [{}] {}", chunk.chunk_type, snippet));
    }

    lines.join("\n")
}

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
    let json_str = serde_json::to_string(result)
        .map_err(|e| McpError::ToolError(format!("failed to serialize response: {}", e)))?;

    Ok(json!({
        "content": [{
            "type": "text",
            "text": json_str
        }]
    }))
}

async fn collect_episode_chunks<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    episode_id: &str,
    max_chunks: usize,
) -> Result<Vec<MemoryChunk>, McpError> {
    let page_size = 200usize;
    let mut offset = 0usize;
    let mut episode_chunks = Vec::new();

    loop {
        let page = store
            .list_chunks(tenant_id, page_size, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if page.is_empty() {
            break;
        }

        for chunk in page {
            if extract_episode_id(&chunk.tags).as_deref() == Some(episode_id) {
                episode_chunks.push(chunk);
                if episode_chunks.len() >= max_chunks {
                    return Ok(episode_chunks);
                }
            }
        }

        offset = offset.saturating_add(page_size);
    }

    Ok(episode_chunks)
}

// ---------- Handler Functions ----------

/// Handle memory.search tool call
pub async fn handle_memory_search<S: Store>(
    store: &S,
    params: SearchParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    let parsed_filters = parse_search_filters(params.filters.as_ref())?;
    let debug_tiers = params.debug_tiers.unwrap_or(false);
    let project_id_filter = params.project_id.as_deref();
    let has_filters = has_active_search_filters(project_id_filter, &parsed_filters);
    let fetch_k = adaptive_fetch_k(params.k, &params.query, has_filters);

    info!(
        tenant_id = %tenant_id,
        query = %params.query,
        k = params.k,
        fetch_k = fetch_k,
        debug_tiers = debug_tiers,
        "memory.search"
    );

    // Use search_with_tier_info if debug_tiers is requested
    if debug_tiers {
        let (scored_chunks, timing) = store
            .search_with_tier_info(&tenant_id, &params.query, fetch_k)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;

        let mut scored_chunks =
            apply_search_filters(scored_chunks, project_id_filter, &parsed_filters, params.k);
        let mut timing = timing;
        let mut repair_info = None;

        if scored_chunks.is_empty() && !params.query.is_empty() {
            if let Some(repaired_query) = normalize_query_for_repair(&params.query) {
                let (repair_scored, repair_timing) = store
                    .search_with_tier_info(&tenant_id, &repaired_query, fetch_k)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?;
                let repaired_filtered = apply_search_filters(
                    repair_scored,
                    project_id_filter,
                    &parsed_filters,
                    params.k,
                );
                let repaired = !repaired_filtered.is_empty();
                if repaired {
                    scored_chunks = repaired_filtered;
                    timing = repair_timing;
                }
                repair_info = Some(RepairInfo {
                    attempted: true,
                    repaired,
                    original_query: params.query.clone(),
                    repaired_query: Some(repaired_query),
                });
            }
        }

        debug!(
            results_count = scored_chunks.len(),
            "search completed with tier info"
        );

        // Build tier debug info if timing is available
        let tier_info = timing.map(|t| {
            let source_tier = if t.cache_lookup_ms > 0 && t.hot_tier_ms == 0 && t.warm_tier_ms == 0
            {
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
                episode_id: extract_episode_id(&chunk.tags),
                citation: Some(build_citation(chunk)),
                source_tier: default_tier.clone(),
            })
            .collect();

        return format_mcp_response(&SearchResult {
            results,
            tier_info,
            repair_info,
        });
    }

    // Standard path without tier info
    let scored_chunks = store
        .search_with_scores(&tenant_id, &params.query, fetch_k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let mut scored_chunks =
        apply_search_filters(scored_chunks, project_id_filter, &parsed_filters, params.k);
    let mut repair_info = None;

    if scored_chunks.is_empty() && !params.query.is_empty() {
        if let Some(repaired_query) = normalize_query_for_repair(&params.query) {
            let repair_scored = store
                .search_with_scores(&tenant_id, &repaired_query, fetch_k)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
            let repaired_filtered =
                apply_search_filters(repair_scored, project_id_filter, &parsed_filters, params.k);
            let repaired = !repaired_filtered.is_empty();
            if repaired {
                scored_chunks = repaired_filtered;
            }
            repair_info = Some(RepairInfo {
                attempted: true,
                repaired,
                original_query: params.query.clone(),
                repaired_query: Some(repaired_query),
            });
        }
    }

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
            episode_id: extract_episode_id(&chunk.tags),
            citation: Some(build_citation(chunk)),
            source_tier: None,
        })
        .collect();

    format_mcp_response(&SearchResult {
        results,
        tier_info: None,
        repair_info,
    })
}

/// Handle memory.add tool call
pub async fn handle_memory_add<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: AddParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let chunk_type = parse_chunk_type(&params.chunk_type)?;

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

    if let Some(episode_id) = &params.episode_id {
        validate_episode_id(episode_id)?;
        let mut tags = chunk.tags.clone();
        tags.push(make_episode_tag(episode_id));
        chunk = chunk.with_tags(tags);
    }

    chunk = chunk.with_source(params_to_source(params.source));

    if !params.tags.is_empty() {
        let mut tags = chunk.tags.clone();
        tags.extend(params.tags);
        chunk = chunk.with_tags(tags);
    }

    let chunk_id = store
        .add(chunk)
        .await
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

        if let Some(episode_id) = &chunk_params.episode_id {
            validate_episode_id(episode_id)?;
            let mut tags = chunk.tags.clone();
            tags.push(make_episode_tag(episode_id));
            chunk = chunk.with_tags(tags);
        }

        chunk = chunk.with_source(params_to_source(chunk_params.source));

        if !chunk_params.tags.is_empty() {
            let mut tags = chunk.tags.clone();
            tags.extend(chunk_params.tags);
            chunk = chunk.with_tags(tags);
        }

        chunks.push(chunk);
    }

    let chunk_ids = store
        .add_batch(chunks)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    info!(count = chunk_ids.len(), "batch add completed");

    format_mcp_response(&AddBatchResult {
        chunk_ids: chunk_ids.iter().map(|id| id.to_string()).collect(),
    })
}

/// Handle memory.get tool call
pub async fn handle_memory_get<S: Store>(store: &S, params: GetParams) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;

    debug!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        "memory.get"
    );

    let chunk = store
        .get(&tenant_id, &chunk_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    let json_str = if let Some(c) = chunk {
        info!(chunk_id = %chunk_id, "chunk found");
        serde_json::to_string(&c)
            .map_err(|e| McpError::ToolError(format!("failed to serialize chunk: {}", e)))?
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

    let deleted = store
        .delete(&tenant_id, &chunk_id)
        .await
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

    let store_stats: StoreStats = store
        .stats(&tenant_id)
        .await
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

    // Get compaction metrics if available
    let compaction = store
        .get_compaction_metrics(&tenant_id)
        .ok()
        .map(|m| CompactionStatsResult {
            tombstone_ratio: m.tombstone_ratio,
            active_chunks: m.active_chunks,
            deleted_chunks: m.deleted_chunks,
            segment_count: m.segment_count,
            hnsw_staleness: m.hnsw_staleness,
            hnsw_cache_size: m.hnsw_cache_size,
            hnsw_index_size: m.hnsw_index_size,
            needs_compaction: m.tombstone_ratio > 0.20
                || m.segment_count > 10
                || m.hnsw_staleness > 0.15,
        });

    format_mcp_response(&StatsResult {
        total_chunks: store_stats.total_chunks,
        deleted_chunks: store_stats.deleted_chunks,
        chunk_types: store_stats.chunk_types,
        disk_stats,
        compaction,
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

/// Handle memory.compact tool call
pub async fn handle_memory_compact<S: Store>(
    store: &S,
    params: CompactParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        force = params.force,
        "memory.compact"
    );

    if params.force {
        // Force compaction regardless of thresholds
        let result = store
            .run_compaction(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;

        info!(
            tenant_id = %tenant_id,
            tombstones = result.tombstones_processed,
            hnsw_rebuilt = result.hnsw_rebuild.is_some(),
            segments_merged = result.segment_merge.is_some(),
            cache_invalidated = result.cache_entries_invalidated,
            duration_ms = result.duration.as_millis(),
            "compaction completed (forced)"
        );

        return format_mcp_response(&json!({
            "status": "completed",
            "tombstones_processed": result.tombstones_processed,
            "hnsw_rebuild": result.hnsw_rebuild.map(|r| json!({
                "embeddings_processed": r.embeddings_processed,
                "embeddings_included": r.embeddings_included,
                "embeddings_excluded": r.embeddings_excluded,
                "duration_ms": r.duration.as_millis()
            })),
            "segment_merge": result.segment_merge.map(|r| json!({
                "segments_before": r.segments_before,
                "segments_after": r.segments_after,
                "segments_merged": r.segments_merged,
                "duration_ms": r.duration.as_millis()
            })),
            "cache_entries_invalidated": result.cache_entries_invalidated,
            "duration_ms": result.duration.as_millis()
        }));
    }

    // Check thresholds first
    match store.run_compaction_if_needed(&tenant_id) {
        Ok(Some(result)) => {
            info!(
                tenant_id = %tenant_id,
                tombstones = result.tombstones_processed,
                hnsw_rebuilt = result.hnsw_rebuild.is_some(),
                segments_merged = result.segment_merge.is_some(),
                cache_invalidated = result.cache_entries_invalidated,
                duration_ms = result.duration.as_millis(),
                "compaction completed"
            );

            format_mcp_response(&json!({
                "status": "completed",
                "tombstones_processed": result.tombstones_processed,
                "hnsw_rebuild": result.hnsw_rebuild.map(|r| json!({
                    "embeddings_processed": r.embeddings_processed,
                    "embeddings_included": r.embeddings_included,
                    "embeddings_excluded": r.embeddings_excluded,
                    "duration_ms": r.duration.as_millis()
                })),
                "segment_merge": result.segment_merge.map(|r| json!({
                    "segments_before": r.segments_before,
                    "segments_after": r.segments_after,
                    "segments_merged": r.segments_merged,
                    "duration_ms": r.duration.as_millis()
                })),
                "cache_entries_invalidated": result.cache_entries_invalidated,
                "duration_ms": result.duration.as_millis()
            }))
        }
        Ok(None) => {
            debug!(tenant_id = %tenant_id, "compaction skipped - thresholds not exceeded");

            format_mcp_response(&json!({
                "status": "skipped",
                "reason": "No compaction needed - all thresholds below limits"
            }))
        }
        Err(e) => Err(McpError::ToolError(e.to_string())),
    }
}

/// Handle memory.consolidate_episode tool call
pub async fn handle_memory_consolidate_episode<S: Store>(
    store: &S,
    params: ConsolidateEpisodeParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_episode_id(&params.episode_id)?;

    if params.max_chunks == 0 {
        return Err(McpError::InvalidParams(
            "max_chunks must be greater than 0".to_string(),
        ));
    }

    let mut episode_chunks =
        collect_episode_chunks(store, &tenant_id, &params.episode_id, params.max_chunks).await?;
    if episode_chunks.is_empty() {
        return Err(McpError::ToolError(format!(
            "no chunks found for episode '{}'",
            params.episode_id
        )));
    }

    episode_chunks.sort_by_key(|chunk| chunk.timestamp_created);
    let summary_text = build_episode_summary_text(&params.episode_id, &episode_chunks);
    let tags = vec![
        make_episode_tag(&params.episode_id),
        "episode_summary:true".to_string(),
        format!("episode_source_chunks:{}", episode_chunks.len()),
    ];

    let summary_chunk = MemoryChunk::new(tenant_id.clone(), summary_text, ChunkType::Summary)
        .with_tags(tags)
        .with_source(Source::from_tool(
            "memory.consolidate_episode",
            Option::<String>::None,
        ));
    let summary_chunk_id = store
        .add(summary_chunk)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    if !params.retain_source_chunks {
        for chunk in &episode_chunks {
            let _ = store
                .delete(&tenant_id, &chunk.chunk_id)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;
        }
    }

    format_mcp_response(&ConsolidateEpisodeResult {
        summary_chunk_id: summary_chunk_id.to_string(),
        source_chunk_count: episode_chunks.len(),
        retained_source_chunks: params.retain_source_chunks,
    })
}

// ---------- Structural Query Handlers ----------

use crate::structural::{CallerInfo, ImportInfo, SymbolLocation, SymbolQueryService};

/// Handle code.find_definition tool call
pub fn handle_find_definition(
    query_service: &SymbolQueryService,
    params: FindDefinitionParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        "code.find_definition"
    );

    let locations = query_service
        .find_symbol_definition(&tenant_id, &params.name, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = locations.len(), "find_definition completed");

    let definitions: Vec<SymbolLocationResult> = locations
        .into_iter()
        .map(symbol_location_to_result)
        .collect();

    format_mcp_response(&FindDefinitionResult { definitions })
}

/// Handle code.find_references tool call
pub fn handle_find_references(
    query_service: &SymbolQueryService,
    params: FindReferencesParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        "code.find_references"
    );

    let locations = query_service
        .find_references(&tenant_id, &params.name, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = locations.len(), "find_references completed");

    let references: Vec<SymbolLocationResult> = locations
        .into_iter()
        .map(symbol_location_to_result)
        .collect();

    format_mcp_response(&FindReferencesResult { references })
}

/// Handle code.find_callers tool call
pub fn handle_find_callers(
    query_service: &SymbolQueryService,
    params: FindCallersParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;

    // Clamp depth to 1-3
    let depth = params.depth.clamp(1, 3);

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        depth = depth,
        "code.find_callers"
    );

    let caller_infos = query_service
        .find_callers(
            &tenant_id,
            &params.name,
            depth,
            params.project_id.as_deref(),
        )
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = caller_infos.len(), "find_callers completed");

    let callers: Vec<CallerInfoResult> = caller_infos
        .into_iter()
        .map(caller_info_to_result)
        .collect();

    format_mcp_response(&FindCallersResult { callers })
}

/// Handle code.find_imports tool call
pub fn handle_find_imports(
    query_service: &SymbolQueryService,
    params: FindImportsParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        module = %params.module,
        "code.find_imports"
    );

    let import_infos = query_service
        .find_imports(&tenant_id, &params.module, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = import_infos.len(), "find_imports completed");

    let imports: Vec<ImportInfoResult> = import_infos
        .into_iter()
        .map(import_info_to_result)
        .collect();

    format_mcp_response(&FindImportsResult { imports })
}

/// Convert SymbolLocation to result type
fn symbol_location_to_result(loc: SymbolLocation) -> SymbolLocationResult {
    SymbolLocationResult {
        file_path: loc.file_path,
        name: loc.name,
        kind: loc.kind.as_str().to_string(),
        line_start: loc.line_start,
        line_end: loc.line_end,
        col_start: loc.col_start,
        col_end: loc.col_end,
        signature: loc.signature,
        docstring: loc.docstring,
        visibility: loc.visibility,
        language: loc.language,
    }
}

/// Convert CallerInfo to result type
fn caller_info_to_result(info: CallerInfo) -> CallerInfoResult {
    CallerInfoResult {
        caller_name: info.caller_name,
        caller_file: info.caller_file,
        call_line: info.call_line,
        call_col: info.call_col,
        caller_kind: info.caller_kind.as_str().to_string(),
        depth: info.depth,
    }
}

/// Convert ImportInfo to result type
fn import_info_to_result(info: ImportInfo) -> ImportInfoResult {
    ImportInfoResult {
        importing_file: info.importing_file,
        import_line: info.import_line,
        alias: info.alias,
    }
}

// ---------- Trace Query Handlers ----------

use crate::structural::{
    parse_iso_datetime, ErrorResult, FrameInfo, TimeRange as StructuralTimeRange, ToolCallResult,
    TraceQueryService,
};

/// Result type for debug.find_tool_calls
#[derive(Debug, Serialize, Deserialize)]
pub struct FindToolCallsResult {
    pub tool_calls: Vec<ToolCallResult>,
    pub total_count: usize,
}

/// Result type for debug.find_errors
#[derive(Debug, Serialize, Deserialize)]
pub struct FindErrorsResult {
    pub errors: Vec<ErrorResultResponse>,
    pub total_count: usize,
}

/// Error result with optional frames
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResultResponse {
    pub trace_id: i64,
    pub error_signature: String,
    pub error_message: String,
    pub timestamp_ms: i64,
    pub timestamp_formatted: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<FrameInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Convert ErrorResult to response, optionally including frames
fn error_to_response(error: ErrorResult, include_frames: bool) -> ErrorResultResponse {
    ErrorResultResponse {
        trace_id: error.trace_id,
        error_signature: error.error_signature,
        error_message: error.error_message,
        timestamp_ms: error.timestamp_ms,
        timestamp_formatted: error.timestamp_formatted,
        frames: if include_frames {
            Some(error.frames)
        } else {
            None
        },
        session_id: error.session_id,
    }
}

/// Parse time range from optional ISO 8601 strings
fn parse_trace_time_range(
    time_from: Option<&str>,
    time_to: Option<&str>,
) -> Result<Option<StructuralTimeRange>, McpError> {
    let from_ms = match time_from {
        Some(s) => Some(parse_iso_datetime(s).map_err(|e| McpError::InvalidParams(e.to_string()))?),
        None => None,
    };
    let to_ms = match time_to {
        Some(s) => Some(parse_iso_datetime(s).map_err(|e| McpError::InvalidParams(e.to_string()))?),
        None => None,
    };

    if from_ms.is_none() && to_ms.is_none() {
        Ok(None)
    } else {
        Ok(Some(StructuralTimeRange { from_ms, to_ms }))
    }
}

/// Handle debug.find_tool_calls tool call
pub fn handle_find_tool_calls(
    trace_service: &TraceQueryService,
    params: FindToolCallsParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(100);

    // Parse time range
    let time_range =
        parse_trace_time_range(params.time_from.as_deref(), params.time_to.as_deref())?;

    info!(
        tenant_id = %tenant_id,
        tool_name = ?params.tool_name,
        session_id = ?params.session_id,
        errors_only = params.errors_only,
        limit = limit,
        "debug.find_tool_calls"
    );

    let tool_calls = if params.errors_only {
        trace_service
            .find_tool_calls_with_errors(&tenant_id, time_range)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        trace_service
            .find_tool_calls(
                &tenant_id,
                params.tool_name.as_deref(),
                time_range,
                params.session_id.as_deref(),
                limit,
            )
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    debug!(
        results_count = tool_calls.len(),
        "find_tool_calls completed"
    );

    let total_count = tool_calls.len();
    format_mcp_response(&FindToolCallsResult {
        tool_calls,
        total_count,
    })
}

/// Handle debug.find_errors tool call
pub fn handle_find_errors(
    trace_service: &TraceQueryService,
    params: FindErrorsParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(100);

    // Parse time range
    let time_range =
        parse_trace_time_range(params.time_from.as_deref(), params.time_to.as_deref())?;

    info!(
        tenant_id = %tenant_id,
        error_signature = ?params.error_signature,
        function_name = ?params.function_name,
        file_path = ?params.file_path,
        limit = limit,
        "debug.find_errors"
    );

    let error_results = trace_service
        .find_errors(
            &tenant_id,
            params.error_signature.as_deref(),
            params.function_name.as_deref(),
            params.file_path.as_deref(),
            time_range,
            limit,
        )
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = error_results.len(), "find_errors completed");

    let total_count = error_results.len();
    let errors: Vec<ErrorResultResponse> = error_results
        .into_iter()
        .map(|e| error_to_response(e, params.include_frames))
        .collect();

    format_mcp_response(&FindErrorsResult {
        errors,
        total_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use proptest::prelude::*;

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
    async fn search_rejects_k_zero() {
        let store = make_store();
        let params = SearchParams {
            tenant_id: "test".to_string(),
            query: "hello".to_string(),
            project_id: None,
            k: 0,
            filters: None,
            debug_tiers: None,
        };

        let result = handle_memory_search(&store, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn search_rejects_k_above_max() {
        let store = make_store();
        let params = SearchParams {
            tenant_id: "test".to_string(),
            query: "hello".to_string(),
            project_id: None,
            k: 101,
            filters: None,
            debug_tiers: None,
        };

        let result = handle_memory_search(&store, params).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
    }

    proptest! {
        #[test]
        fn validate_search_k_property(k in 0usize..=200usize) {
            let result = validate_search_k(k);
            if (1..=100).contains(&k) {
                prop_assert!(result.is_ok());
            } else {
                prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
            }
        }
    }

    #[test]
    fn adaptive_fetch_k_expands_for_complex_queries() {
        let query = "this is a very long and complex search query with many tokens";
        assert_eq!(adaptive_fetch_k(10, query, false), 20);
        assert_eq!(adaptive_fetch_k(10, query, true), 100);
        assert_eq!(adaptive_fetch_k(10, "short query", false), 10);
    }

    #[test]
    fn normalize_query_for_repair_rewrites_noise() {
        let repaired = normalize_query_for_repair("Alpha!unique?marker").unwrap();
        assert_eq!(repaired, "alpha unique marker");
        assert!(normalize_query_for_repair("clean query").is_none());
    }

    proptest! {
        #[test]
        fn validate_search_time_range_order_property(day_a in 1u8..=28, day_b in 1u8..=28) {
            let filters = SearchFilters {
                types: None,
                episode_id: None,
                time_range: Some(TimeRange {
                    from: Some(format!("2026-01-{day_a:02}T00:00:00Z")),
                    to: Some(format!("2026-01-{day_b:02}T23:59:59Z")),
                }),
            };

            let result = validate_search_time_range(Some(&filters));
            if day_a <= day_b {
                prop_assert!(result.is_ok());
            } else {
                prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
            }
        }
    }

    proptest! {
        #[test]
        fn validate_search_time_range_rejects_invalid_iso(invalid in "[A-Za-z]{1,16}") {
            let filters = SearchFilters {
                types: None,
                episode_id: None,
                time_range: Some(TimeRange {
                    from: Some(invalid),
                    to: Some("2026-01-01T00:00:00Z".to_string()),
                }),
            };

            let result = validate_search_time_range(Some(&filters));
            prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
        }
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
            episode_id: None,
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
    async fn search_filters_by_project_id() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "project a chunk".to_string(),
                chunk_type: "doc".to_string(),
                project_id: Some("project_a".to_string()),
                episode_id: None,
                source: None,
                tags: vec![],
            },
        )
        .await
        .unwrap();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "project b chunk".to_string(),
                chunk_type: "doc".to_string(),
                project_id: Some("project_b".to_string()),
                episode_id: None,
                source: None,
                tags: vec![],
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "chunk".to_string(),
                project_id: Some("project_a".to_string()),
                k: 10,
                filters: None,
                debug_tiers: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(search_response.results[0].text, "project a chunk");
    }

    #[tokio::test]
    async fn search_filters_by_types() {
        let store = make_store();

        for (text, chunk_type) in [
            ("doc chunk", "doc"),
            ("code chunk", "code"),
            ("trace chunk", "trace"),
        ] {
            handle_memory_add(
                &store,
                None,
                AddParams {
                    tenant_id: "test".to_string(),
                    text: text.to_string(),
                    chunk_type: chunk_type.to_string(),
                    project_id: None,
                    episode_id: None,
                    source: None,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        }

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "chunk".to_string(),
                project_id: None,
                k: 10,
                filters: Some(SearchFilters {
                    types: Some(vec!["code".to_string(), "doc".to_string()]),
                    episode_id: None,
                    time_range: None,
                }),
                debug_tiers: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 2);
        assert!(search_response
            .results
            .iter()
            .all(|r| matches!(r.chunk_type.as_str(), "doc" | "code")));
    }

    #[tokio::test]
    async fn search_filters_by_time_range() {
        let store = make_store();
        let tenant_id = TenantId::new("test").unwrap();

        let mut old_chunk = MemoryChunk::new(tenant_id.clone(), "old chunk", ChunkType::Doc);
        old_chunk.timestamp_created =
            crate::structural::parse_iso_datetime("2026-01-01T00:00:00Z").unwrap();
        store.add(old_chunk).await.unwrap();

        let mut middle_chunk = MemoryChunk::new(tenant_id.clone(), "middle chunk", ChunkType::Doc);
        middle_chunk.timestamp_created =
            crate::structural::parse_iso_datetime("2026-01-15T12:00:00Z").unwrap();
        store.add(middle_chunk).await.unwrap();

        let mut new_chunk = MemoryChunk::new(tenant_id, "new chunk", ChunkType::Doc);
        new_chunk.timestamp_created =
            crate::structural::parse_iso_datetime("2026-02-01T00:00:00Z").unwrap();
        store.add(new_chunk).await.unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "chunk".to_string(),
                project_id: None,
                k: 10,
                filters: Some(SearchFilters {
                    types: None,
                    episode_id: None,
                    time_range: Some(TimeRange {
                        from: Some("2026-01-10T00:00:00Z".to_string()),
                        to: Some("2026-01-20T23:59:59Z".to_string()),
                    }),
                }),
                debug_tiers: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(search_response.results[0].text, "middle chunk");
    }

    #[tokio::test]
    async fn search_filters_by_episode_id() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "episode alpha".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: Some("ep1".to_string()),
                source: None,
                tags: vec![],
            },
        )
        .await
        .unwrap();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "episode beta".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: Some("ep2".to_string()),
                source: None,
                tags: vec![],
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "episode".to_string(),
                project_id: None,
                k: 10,
                filters: Some(SearchFilters {
                    types: None,
                    episode_id: Some("ep1".to_string()),
                    time_range: None,
                }),
                debug_tiers: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(
            search_response.results[0].episode_id.as_deref(),
            Some("ep1")
        );
    }

    #[tokio::test]
    async fn search_returns_citation_with_provenance_and_offsets() {
        let store = make_store();

        let long_text = format!(
            "alpha_unique_marker {}",
            "lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(80)
        );

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: long_text,
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    uri: Some("file:///tmp/test_doc.md".to_string()),
                    repo: Some("acme/repo".to_string()),
                    commit: Some("abc123".to_string()),
                    path: Some("docs/test_doc.md".to_string()),
                    tool_name: Some("ingest".to_string()),
                    tool_call_id: Some("call-1".to_string()),
                }),
                tags: vec![],
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "alpha_unique_marker".to_string(),
                project_id: None,
                k: 10,
                filters: None,
                debug_tiers: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert!(!search_response.results.is_empty());

        let citation = search_response.results[0]
            .citation
            .as_ref()
            .expect("citation should be present");

        assert!(!citation.citation_id.is_empty());
        assert!(!citation.content_hash.is_empty());
        assert_eq!(citation.source_path.as_deref(), Some("docs/test_doc.md"));
        assert_eq!(citation.source_tool_name.as_deref(), Some("ingest"));
        assert!(citation.chunk_index.is_some());
        assert!(citation.total_chunks.is_some());
        assert!(citation.char_start.is_some());
        assert!(citation.char_end.is_some());
    }

    #[tokio::test]
    async fn search_repair_loop_recovers_result() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "alpha unique marker".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![],
            },
        )
        .await
        .unwrap();

        let result = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "alpha!unique?marker".to_string(),
                project_id: None,
                k: 5,
                filters: None,
                debug_tiers: None,
            },
        )
        .await
        .unwrap();

        let text = result["content"][0]["text"].as_str().unwrap();
        let search_response: SearchResult = serde_json::from_str(text).unwrap();
        assert_eq!(search_response.results.len(), 1);
        assert_eq!(search_response.results[0].text, "alpha unique marker");

        let repair_info = search_response
            .repair_info
            .as_ref()
            .expect("repair_info should be present");
        assert!(repair_info.attempted);
        assert!(repair_info.repaired);
        assert_eq!(
            repair_info.repaired_query.as_deref(),
            Some("alpha unique marker")
        );
    }

    #[tokio::test]
    async fn add_with_all_fields() {
        let store = make_store();

        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: "function hello() {}".to_string(),
            chunk_type: "code".to_string(),
            project_id: Some("my_project".to_string()),
            episode_id: None,
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
                    episode_id: None,
                    source: None,
                    tags: vec![],
                },
                BatchChunkParams {
                    text: "chunk 2".to_string(),
                    chunk_type: "code".to_string(),
                    project_id: None,
                    episode_id: None,
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
            episode_id: None,
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
                episode_id: None,
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
            episode_id: None,
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
            episode_id: None,
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
            episode_id: None,
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
