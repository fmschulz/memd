//! Tool call handlers for MCP
//!
//! Bridges MCP tool calls to store operations.
//! Each handler validates parameters, calls the store, and formats the response.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use super::error::McpError;
use crate::metrics::{IndexStats, MetricsCollector};
use crate::store::{FeedbackEntry, RelevanceLabel, Store, StoreStats, TenantManager};
use crate::task_memory::{
    build_task_projections, ArtifactKind, DatasetRef, EntityRef, TaskArtifact, TaskProvenance,
    TaskSearchFilters,
};
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

/// Dataset reference supplied to task tools.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskDatasetRefParams {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Entity reference supplied to task tools.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskEntityRefParams {
    pub name: String,
    pub entity_type: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// Provenance supplied to task tools.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TaskProvenanceParams {
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_version: Option<String>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// Parameters for task.start
#[derive(Debug, Deserialize)]
pub struct TaskStartParams {
    pub tenant_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub goal: String,
    pub motivation: String,
    pub hypothesis: String,
    pub scientific_question: String,
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    pub expected_outputs: Vec<String>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.finish
#[derive(Debug, Deserialize)]
pub struct TaskFinishParams {
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub scientific_question: Option<String>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    pub what_worked: Vec<String>,
    pub what_failed: Vec<String>,
    pub validation: Vec<String>,
    pub uncertainty: Vec<String>,
    pub followups: Vec<String>,
    pub confidence: f32,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.progress
#[derive(Debug, Deserialize)]
pub struct TaskProgressParams {
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub failed_attempts: Vec<String>,
    pub next_step: String,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.run_start
#[derive(Debug, Deserialize)]
pub struct TaskRunStartParams {
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub tool_name: String,
    #[serde(default)]
    pub tool_version: Option<String>,
    pub command: String,
    pub why_chosen: String,
    pub parameters: Value,
    pub inputs: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.run_finish
#[derive(Debug, Deserialize)]
pub struct TaskRunFinishParams {
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_version: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    pub outputs: Vec<String>,
    #[serde(default)]
    pub metrics: Option<Value>,
    pub notes: String,
    #[serde(default)]
    pub validation: Vec<String>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.add_evidence
#[derive(Debug, Deserialize)]
pub struct TaskAddEvidenceParams {
    pub tenant_id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub summary: String,
    pub evidence_kind: String,
    pub supports_claim: bool,
    #[serde(default)]
    pub metric_name: Option<String>,
    #[serde(default)]
    pub metric_value: Option<Value>,
    #[serde(default)]
    pub metrics: Option<Value>,
    #[serde(default)]
    pub dataset_refs: Vec<TaskDatasetRefParams>,
    #[serde(default)]
    pub entity_refs: Vec<TaskEntityRefParams>,
    #[serde(default)]
    pub provenance: Option<TaskProvenanceParams>,
}

/// Parameters for task.get
#[derive(Debug, Deserialize)]
pub struct TaskGetParams {
    pub tenant_id: String,
    pub task_id: String,
}

/// Exact task-aware filters for task.search.
#[derive(Debug, Deserialize, Default)]
pub struct TaskSearchFiltersParams {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub artifact_kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub dataset_name: Option<String>,
    #[serde(default)]
    pub dataset_version: Option<String>,
    #[serde(default)]
    pub entity_name: Option<String>,
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Parameters for task.search
#[derive(Debug, Deserialize)]
pub struct TaskSearchParams {
    pub tenant_id: String,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default)]
    pub filters: Option<TaskSearchFiltersParams>,
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

/// Parameters for memory.feedback
#[derive(Debug, Deserialize)]
pub struct FeedbackParams {
    pub tenant_id: String,
    pub query: String,
    pub chunk_id: String,
    pub relevance: String,
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

/// Parameters for context.list_subsystems
#[derive(Debug, Deserialize)]
pub struct ContextListSubsystemsParams {
    pub tenant_id: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// Parameters for context.get_files_for_subsystem
#[derive(Debug, Deserialize)]
pub struct ContextGetFilesForSubsystemParams {
    pub tenant_id: String,
    pub subsystem_key: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// Parameters for context.search_context_documents
#[derive(Debug, Deserialize)]
pub struct ContextSearchDocumentsParams {
    pub tenant_id: String,
    pub query: String,
    #[serde(default = "default_context_limit")]
    pub k: usize,
    #[serde(default)]
    pub subsystem_key: Option<String>,
    /// Optional tier filter: "hot" | "cold"
    #[serde(default)]
    pub tier: Option<String>,
}

/// Parameters for context.find_relevant_context
#[derive(Debug, Deserialize)]
pub struct ContextFindRelevantContextParams {
    pub tenant_id: String,
    pub task: String,
    #[serde(default = "default_context_limit")]
    pub k: usize,
    #[serde(default)]
    pub subsystem_keys: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub include_hot: bool,
}

/// Parameters for context.suggest_agent
#[derive(Debug, Deserialize)]
pub struct ContextSuggestAgentParams {
    pub tenant_id: String,
    pub task: String,
    #[serde(default)]
    pub changed_files: Option<Vec<String>>,
    #[serde(default = "default_context_agent_limit")]
    pub k: usize,
}

/// Parameters for context.get_hot_context
#[derive(Debug, Deserialize)]
pub struct ContextGetHotContextParams {
    pub tenant_id: String,
    #[serde(default = "default_context_limit")]
    pub k: usize,
}

fn default_episode_limit() -> usize {
    50
}

fn default_context_limit() -> usize {
    20
}

fn default_context_agent_limit() -> usize {
    3
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

/// Result of a task artifact write operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskArtifactResult {
    pub task_id: String,
    pub artifact_id: String,
    pub projection_chunk_ids: Vec<String>,
}

/// Result of task.get.
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskGetResult {
    pub task_id: String,
    pub artifacts: Vec<TaskArtifact>,
}

/// Result of a delete operation
#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteResult {
    pub deleted: bool,
}

/// Result of a feedback operation
#[derive(Debug, Serialize, Deserialize)]
pub struct FeedbackResult {
    pub stored: bool,
}

/// Result of memory.consolidate_episode
#[derive(Debug, Serialize, Deserialize)]
pub struct ConsolidateEpisodeResult {
    pub summary_chunk_id: String,
    pub source_chunk_count: usize,
    pub retained_source_chunks: bool,
}

/// Result of context.list_subsystems
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextListSubsystemsResult {
    pub subsystems: Vec<SubsystemSummary>,
}

/// Subsystem summary
#[derive(Debug, Serialize, Deserialize)]
pub struct SubsystemSummary {
    pub key: String,
    pub chunk_count: usize,
    pub file_count: usize,
}

/// Result of context.get_files_for_subsystem
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextGetFilesForSubsystemResult {
    pub subsystem_key: String,
    pub files: Vec<String>,
}

/// Result of context.search_context_documents
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextSearchDocumentsResult {
    pub results: Vec<ChunkResult>,
}

/// Result of context.find_relevant_context
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextFindRelevantContextResult {
    pub results: Vec<ChunkResult>,
    pub hot_included: bool,
}

/// Agent recommendation entry
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentSuggestion {
    pub agent_name: String,
    pub score: f32,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub matched_triggers: Vec<String>,
}

/// Result of context.suggest_agent
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextSuggestAgentResult {
    pub recommendations: Vec<AgentSuggestion>,
    pub considered_agents: usize,
}

/// Result of context.get_hot_context
#[derive(Debug, Serialize, Deserialize)]
pub struct ContextGetHotContextResult {
    pub results: Vec<ChunkResult>,
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

fn parse_relevance_label(value: &str) -> Result<RelevanceLabel, McpError> {
    match value.to_ascii_lowercase().as_str() {
        "relevant" | "positive" => Ok(RelevanceLabel::Relevant),
        "irrelevant" | "negative" => Ok(RelevanceLabel::Irrelevant),
        _ => Err(McpError::InvalidParams(
            "invalid relevance: must be one of [relevant, irrelevant]".to_string(),
        )),
    }
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

const TAG_CTX_TIER_HOT: &str = "ctx:tier:hot";
const TAG_CTX_TIER_COLD: &str = "ctx:tier:cold";
const TAG_CTX_DOC: &str = "ctx:doc";
const TAG_CTX_SUBSYSTEM_PREFIX: &str = "ctx:subsystem:";
const TAG_CTX_FILE_PREFIX: &str = "ctx:file:";
const TAG_CTX_TRIGGER_PREFIX: &str = "ctx:trigger:";
const TAG_CTX_AGENT_PREFIX: &str = "ctx:agent:";

fn has_exact_tag(tags: &[String], expected: &str) -> bool {
    tags.iter().any(|tag| tag == expected)
}

fn tag_values(tags: &[String], prefix: &str) -> Vec<String> {
    tags.iter()
        .filter_map(|tag| tag.strip_prefix(prefix).map(str::to_string))
        .collect()
}

fn chunk_matches_subsystem(chunk: &MemoryChunk, subsystem_key: &str) -> bool {
    tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX)
        .iter()
        .any(|value| value == subsystem_key)
}

fn chunk_matches_any_subsystem(chunk: &MemoryChunk, subsystem_keys: &[String]) -> bool {
    if subsystem_keys.is_empty() {
        return true;
    }
    subsystem_keys
        .iter()
        .any(|key| chunk_matches_subsystem(chunk, key))
}

fn chunk_matches_tier(chunk: &MemoryChunk, tier: Option<&str>) -> bool {
    match tier {
        Some("hot") => has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT),
        Some("cold") => has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD),
        Some(_) => false,
        None => true,
    }
}

fn is_context_chunk(chunk: &MemoryChunk) -> bool {
    if has_exact_tag(&chunk.tags, TAG_CTX_DOC)
        || has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT)
        || has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD)
        || !tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX).is_empty()
    {
        return true;
    }

    matches!(
        chunk.chunk_type,
        ChunkType::Doc
            | ChunkType::Research
            | ChunkType::Decision
            | ChunkType::Plan
            | ChunkType::Summary
    )
}

fn chunk_to_result(chunk: &MemoryChunk, score: f32, source_tier: Option<String>) -> ChunkResult {
    ChunkResult {
        chunk_id: chunk.chunk_id.to_string(),
        text: chunk.text.clone(),
        score,
        chunk_type: chunk.chunk_type.to_string(),
        source: SourceResult::from(&chunk.source),
        timestamp_created: chunk.timestamp_created,
        tags: chunk.tags.clone(),
        episode_id: extract_episode_id(&chunk.tags),
        citation: Some(build_citation(chunk)),
        source_tier,
    }
}

async fn collect_all_chunks<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    max_chunks: usize,
) -> Result<Vec<MemoryChunk>, McpError> {
    if max_chunks == 0 {
        return Ok(Vec::new());
    }

    let page_size = 200usize.min(max_chunks.max(1));
    let mut offset = 0usize;
    let mut chunks = Vec::new();

    loop {
        let page = store
            .list_chunks(tenant_id, page_size, offset)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if page.is_empty() {
            break;
        }

        for chunk in page {
            chunks.push(chunk);
            if chunks.len() >= max_chunks {
                return Ok(chunks);
            }
        }

        offset = offset.saturating_add(page_size);
    }

    Ok(chunks)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();

    if pattern == "*" {
        return true;
    }

    if !pattern.contains('*') {
        return text.contains(&pattern);
    }

    let parts: Vec<&str> = pattern.split('*').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return true;
    }

    let mut cursor = 0usize;
    for (idx, part) in parts.iter().enumerate() {
        let slice = &text[cursor..];
        let Some(found) = slice.find(part) else {
            return false;
        };

        if idx == 0 && !pattern.starts_with('*') && found != 0 {
            return false;
        }

        cursor += found + part.len();
    }

    if !pattern.ends_with('*') {
        if let Some(last) = parts.last() {
            return text.ends_with(last);
        }
    }

    true
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

fn validate_identifier(name: &str, value: &str) -> Result<(), McpError> {
    if value.trim().is_empty() {
        return Err(McpError::InvalidParams(format!(
            "{} must not be empty",
            name
        )));
    }
    Ok(())
}

fn validate_confidence(confidence: f32) -> Result<(), McpError> {
    if !(0.0..=1.0).contains(&confidence) {
        return Err(McpError::InvalidParams(
            "confidence must be between 0.0 and 1.0".to_string(),
        ));
    }
    Ok(())
}

fn dataset_params_to_refs(params: Vec<TaskDatasetRefParams>) -> Result<Vec<DatasetRef>, McpError> {
    let mut refs = Vec::with_capacity(params.len());
    for dataset in params {
        validate_identifier("dataset_refs[].name", &dataset.name)?;
        refs.push(DatasetRef {
            name: dataset.name,
            version: dataset.version,
            description: dataset.description,
        });
    }
    Ok(refs)
}

fn entity_params_to_refs(params: Vec<TaskEntityRefParams>) -> Result<Vec<EntityRef>, McpError> {
    let mut refs = Vec::with_capacity(params.len());
    for entity in params {
        validate_identifier("entity_refs[].name", &entity.name)?;
        validate_identifier("entity_refs[].entity_type", &entity.entity_type)?;
        refs.push(EntityRef {
            name: entity.name,
            entity_type: entity.entity_type,
            role: entity.role,
        });
    }
    Ok(refs)
}

fn params_to_task_provenance(params: Option<TaskProvenanceParams>) -> TaskProvenance {
    params
        .map(|p| TaskProvenance {
            uri: p.uri,
            repo: p.repo,
            commit: p.commit,
            path: p.path,
            tool_name: p.tool_name,
            tool_version: p.tool_version,
            tool_call_id: p.tool_call_id,
        })
        .unwrap_or_default()
}

fn parse_task_search_filters(
    filters: Option<&TaskSearchFiltersParams>,
) -> Result<TaskSearchFilters, McpError> {
    let Some(filters) = filters else {
        return Ok(TaskSearchFilters::default());
    };

    let artifact_kind = filters
        .artifact_kind
        .as_deref()
        .map(ArtifactKind::from_str)
        .transpose()
        .map_err(McpError::InvalidParams)?;

    Ok(TaskSearchFilters {
        task_id: filters.task_id.clone(),
        artifact_kind,
        status: filters.status.clone(),
        dataset_name: filters.dataset_name.clone(),
        dataset_version: filters.dataset_version.clone(),
        entity_name: filters.entity_name.clone(),
        entity_type: filters.entity_type.clone(),
        tool_name: filters.tool_name.clone(),
        project_id: filters.project_id.clone(),
        agent_id: filters.agent_id.clone(),
        session_id: filters.session_id.clone(),
    })
}

fn has_active_task_filters(filters: &TaskSearchFilters) -> bool {
    filters.task_id.is_some()
        || filters.artifact_kind.is_some()
        || filters.status.is_some()
        || filters.dataset_name.is_some()
        || filters.dataset_version.is_some()
        || filters.entity_name.is_some()
        || filters.entity_type.is_some()
        || filters.tool_name.is_some()
        || filters.project_id.is_some()
        || filters.agent_id.is_some()
        || filters.session_id.is_some()
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

/// Handle task.start tool call.
pub async fn handle_task_start<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskStartParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_identifier("goal", &params.goal)?;
    validate_identifier("motivation", &params.motivation)?;
    validate_identifier("hypothesis", &params.hypothesis)?;
    validate_identifier("scientific_question", &params.scientific_question)?;
    if let Some(parent_task_id) = params.parent_task_id.as_deref() {
        validate_identifier("parent_task_id", parent_task_id)?;
    }

    info!(
        tenant_id = %tenant_id,
        goal = %params.goal,
        "task.start"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_start(tenant_id);
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.parent_task_id = params.parent_task_id;
    artifact.agent_id = params.agent_id;
    artifact.session_id = params.session_id;
    artifact.goal = Some(params.goal);
    artifact.motivation = Some(params.motivation);
    artifact.hypothesis = Some(params.hypothesis);
    artifact.scientific_question = Some(params.scientific_question);
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.expected_outputs = params.expected_outputs;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    let projections = build_task_projections(&artifact);
    let result = store
        .add_task_artifact(artifact, projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.finish tool call.
pub async fn handle_task_finish<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskFinishParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_confidence(params.confidence)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        "task.finish"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_finish(tenant_id, params.task_id);
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = params.agent_id;
    artifact.session_id = params.session_id;
    artifact.status = Some(params.status.unwrap_or_else(|| "completed".to_string()));
    artifact.goal = params.goal;
    artifact.scientific_question = params.scientific_question;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.what_worked = params.what_worked;
    artifact.what_failed = params.what_failed;
    artifact.validation = params.validation;
    artifact.uncertainty = params.uncertainty;
    artifact.followups = params.followups;
    artifact.confidence = Some(params.confidence);
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    let projections = build_task_projections(&artifact);
    let result = store
        .add_task_artifact(artifact, projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.progress tool call.
pub async fn handle_task_progress<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskProgressParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("summary", &params.summary)?;
    validate_identifier("next_step", &params.next_step)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        "task.progress"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_progress(tenant_id, params.task_id);
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = params.agent_id;
    artifact.session_id = params.session_id;
    artifact.summary = Some(params.summary);
    artifact.blockers = params.blockers;
    artifact.what_failed = params.failed_attempts;
    artifact.followups = vec![params.next_step];
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    let result = store
        .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.run_start tool call.
pub async fn handle_task_run_start<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskRunStartParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("tool_name", &params.tool_name)?;
    validate_identifier("command", &params.command)?;
    validate_identifier("why_chosen", &params.why_chosen)?;
    if params.inputs.is_empty() {
        return Err(McpError::InvalidParams(
            "inputs must not be empty".to_string(),
        ));
    }

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        tool_name = %params.tool_name,
        "task.run_start"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_run_start(tenant_id, params.task_id);
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = params.agent_id;
    artifact.session_id = params.session_id;
    artifact.summary = params.summary;
    artifact.tool_name = Some(params.tool_name);
    artifact.tool_version = params.tool_version;
    artifact.command = Some(params.command);
    artifact.why_chosen = Some(params.why_chosen);
    artifact.parameters = Some(params.parameters);
    artifact.inputs = params.inputs;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    if artifact.provenance.tool_name.is_none() {
        artifact.provenance.tool_name = artifact.tool_name.clone();
    }
    if artifact.provenance.tool_version.is_none() {
        artifact.provenance.tool_version = artifact.tool_version.clone();
    }

    let result = store
        .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.run_finish tool call.
pub async fn handle_task_run_finish<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskRunFinishParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("status", &params.status)?;
    validate_identifier("notes", &params.notes)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        status = %params.status,
        "task.run_finish"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_run_finish(tenant_id, params.task_id);
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = params.agent_id;
    artifact.session_id = params.session_id;
    artifact.status = Some(params.status);
    artifact.tool_name = params.tool_name;
    artifact.tool_version = params.tool_version;
    artifact.command = params.command;
    artifact.outputs = params.outputs;
    artifact.metrics = params.metrics;
    artifact.summary = Some(params.notes);
    artifact.validation = params.validation;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    if artifact.provenance.tool_name.is_none() {
        artifact.provenance.tool_name = artifact.tool_name.clone();
    }
    if artifact.provenance.tool_version.is_none() {
        artifact.provenance.tool_version = artifact.tool_version.clone();
    }

    let result = store
        .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.add_evidence tool call.
pub async fn handle_task_add_evidence<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskAddEvidenceParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("summary", &params.summary)?;
    validate_identifier("evidence_kind", &params.evidence_kind)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        evidence_kind = %params.evidence_kind,
        "task.add_evidence"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_evidence(tenant_id, params.task_id);
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = params.agent_id;
    artifact.session_id = params.session_id;
    artifact.summary = Some(params.summary);
    artifact.evidence_kind = Some(params.evidence_kind);
    artifact.supports_claim = Some(params.supports_claim);
    artifact.metrics = match (params.metric_name, params.metric_value, params.metrics) {
        (_, _, Some(metrics)) => Some(metrics),
        (Some(metric_name), Some(metric_value), None) => Some(json!({
            "metric_name": metric_name,
            "metric_value": metric_value,
        })),
        _ => None,
    };
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    let result = store
        .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.get tool call.
pub async fn handle_task_get<S: Store>(
    store: &S,
    params: TaskGetParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;

    let artifacts = store
        .list_task_artifacts(&tenant_id, &params.task_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskGetResult {
        task_id: params.task_id,
        artifacts,
    })
}

/// Handle task.search tool call.
pub async fn handle_task_search<S: Store>(
    store: &S,
    params: TaskSearchParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    let filters = parse_task_search_filters(params.filters.as_ref())?;
    let has_filters = has_active_task_filters(&filters);
    let candidate_limit = if has_filters {
        params.k.saturating_mul(20).clamp(50, 1000)
    } else {
        params.k.saturating_mul(25).clamp(100, 1000)
    };

    let chunk_ids = store
        .search_task_projection_chunk_ids(&tenant_id, &filters, candidate_limit)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let ranked = store
        .rerank_chunks_for_query(&tenant_id, &params.query, &chunk_ids, params.k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let results = ranked
        .iter()
        .map(|(chunk, score)| chunk_to_result(chunk, *score, None))
        .collect::<Vec<_>>();

    format_mcp_response(&SearchResult {
        results,
        tier_info: None,
        repair_info: None,
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

/// Handle memory.feedback tool call
pub async fn handle_memory_feedback<S: Store>(
    store: &S,
    params: FeedbackParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;
    let query = params.query.trim();
    if query.is_empty() {
        return Err(McpError::InvalidParams(
            "query must not be empty".to_string(),
        ));
    }
    let relevance = parse_relevance_label(&params.relevance)?;

    let chunk = store
        .get(&tenant_id, &chunk_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    if chunk.is_none() {
        return Err(McpError::InvalidParams(
            "chunk_id not found for tenant".to_string(),
        ));
    }

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let feedback = FeedbackEntry::new(
        tenant_id,
        query.to_string(),
        chunk_id,
        relevance,
        timestamp_ms,
    );
    store
        .add_feedback(feedback)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&FeedbackResult { stored: true })
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

/// Handle context.list_subsystems tool call
pub async fn handle_context_list_subsystems<S: Store>(
    store: &S,
    params: ContextListSubsystemsParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(500);
    let prefix = params
        .prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    info!(tenant_id = %tenant_id, prefix = ?prefix, limit = limit, "context.list_subsystems");

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut summaries: HashMap<String, (usize, HashSet<String>)> = HashMap::new();

    for chunk in chunks {
        let subsystems = tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX);
        for subsystem in subsystems {
            if let Some(prefix) = prefix {
                if !subsystem.starts_with(prefix) {
                    continue;
                }
            }

            let entry = summaries.entry(subsystem).or_insert((0, HashSet::new()));
            entry.0 += 1;

            if let Some(path) = chunk.source.path.as_deref() {
                entry.1.insert(path.to_string());
            }
            for file_tag in tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX) {
                entry.1.insert(file_tag);
            }
        }
    }

    let mut subsystem_summaries: Vec<SubsystemSummary> = summaries
        .into_iter()
        .map(|(key, (chunk_count, files))| SubsystemSummary {
            key,
            chunk_count,
            file_count: files.len(),
        })
        .collect();
    subsystem_summaries.sort_by(|a, b| a.key.cmp(&b.key));
    subsystem_summaries.truncate(limit);

    format_mcp_response(&ContextListSubsystemsResult {
        subsystems: subsystem_summaries,
    })
}

/// Handle context.get_files_for_subsystem tool call
pub async fn handle_context_get_files_for_subsystem<S: Store>(
    store: &S,
    params: ContextGetFilesForSubsystemParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    let subsystem_key = params.subsystem_key.trim();
    if subsystem_key.is_empty() {
        return Err(McpError::InvalidParams(
            "subsystem_key must not be empty".to_string(),
        ));
    }
    let limit = params.limit.min(2_000);

    info!(tenant_id = %tenant_id, subsystem_key = subsystem_key, limit = limit, "context.get_files_for_subsystem");

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut files = HashSet::new();

    for chunk in chunks {
        if !chunk_matches_subsystem(&chunk, subsystem_key) {
            continue;
        }

        if let Some(path) = chunk.source.path.as_deref() {
            files.insert(path.to_string());
        }
        for file_tag in tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX) {
            files.insert(file_tag);
        }
    }

    let mut files: Vec<String> = files.into_iter().collect();
    files.sort();
    files.truncate(limit);

    format_mcp_response(&ContextGetFilesForSubsystemResult {
        subsystem_key: subsystem_key.to_string(),
        files,
    })
}

/// Handle context.search_context_documents tool call
pub async fn handle_context_search_documents<S: Store>(
    store: &S,
    params: ContextSearchDocumentsParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    let tier = params
        .tier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(tier) = tier {
        if tier != "hot" && tier != "cold" {
            return Err(McpError::InvalidParams(
                "tier must be one of: hot, cold".to_string(),
            ));
        }
    }

    let subsystem_key = params
        .subsystem_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_filters = subsystem_key.is_some() || tier.is_some();
    let fetch_k = adaptive_fetch_k(params.k, &params.query, has_filters);

    info!(
        tenant_id = %tenant_id,
        query = %params.query,
        k = params.k,
        fetch_k = fetch_k,
        subsystem_key = ?subsystem_key,
        tier = ?tier,
        "context.search_context_documents"
    );

    let scored_chunks = store
        .search_with_scores(&tenant_id, &params.query, fetch_k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    let mut filtered = Vec::new();
    for (chunk, score) in scored_chunks {
        if !is_context_chunk(&chunk) {
            continue;
        }
        if let Some(subsystem_key) = subsystem_key {
            if !chunk_matches_subsystem(&chunk, subsystem_key) {
                continue;
            }
        }
        if !chunk_matches_tier(&chunk, tier) {
            continue;
        }

        let source_tier = if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            Some("hot".to_string())
        } else if has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD) {
            Some("cold".to_string())
        } else {
            None
        };
        filtered.push(chunk_to_result(&chunk, score, source_tier));
        if filtered.len() >= params.k {
            break;
        }
    }

    format_mcp_response(&ContextSearchDocumentsResult { results: filtered })
}

/// Handle context.find_relevant_context tool call
pub async fn handle_context_find_relevant_context<S: Store>(
    store: &S,
    params: ContextFindRelevantContextParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    let subsystem_keys: Vec<String> = params
        .subsystem_keys
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let has_filters = !subsystem_keys.is_empty();
    let fetch_k = adaptive_fetch_k(params.k, &params.task, has_filters);

    info!(
        tenant_id = %tenant_id,
        task = %params.task,
        k = params.k,
        include_hot = params.include_hot,
        subsystem_keys = subsystem_keys.len(),
        fetch_k = fetch_k,
        "context.find_relevant_context"
    );

    let mut dedupe = HashSet::new();
    let mut results = Vec::new();
    let mut hot_included = false;

    if params.include_hot {
        let mut hot_chunks = collect_all_chunks(store, &tenant_id, 20_000).await?;
        hot_chunks.retain(|chunk| {
            has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT)
                && chunk_matches_any_subsystem(chunk, &subsystem_keys)
        });
        hot_chunks.sort_by_key(|chunk| std::cmp::Reverse(chunk.timestamp_created));

        for chunk in hot_chunks.into_iter().take(params.k.min(5)) {
            let id = chunk.chunk_id.to_string();
            if dedupe.insert(id) {
                hot_included = true;
                results.push(chunk_to_result(&chunk, 1.0, Some("hot".to_string())));
            }
        }
    }

    let scored_chunks = store
        .search_with_scores(&tenant_id, &params.task, fetch_k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    for (chunk, score) in scored_chunks {
        if !is_context_chunk(&chunk) {
            continue;
        }
        if !params.include_hot && has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            continue;
        }
        if !chunk_matches_any_subsystem(&chunk, &subsystem_keys) {
            continue;
        }

        let id = chunk.chunk_id.to_string();
        if !dedupe.insert(id) {
            continue;
        }

        let source_tier = if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            Some("hot".to_string())
        } else if has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD) {
            Some("cold".to_string())
        } else {
            None
        };
        results.push(chunk_to_result(&chunk, score, source_tier));
        if results.len() >= params.k {
            break;
        }
    }

    format_mcp_response(&ContextFindRelevantContextResult {
        results,
        hot_included,
    })
}

/// Handle context.suggest_agent tool call
pub async fn handle_context_suggest_agent<S: Store>(
    store: &S,
    params: ContextSuggestAgentParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    let changed_files: Vec<String> = params
        .changed_files
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();

    info!(
        tenant_id = %tenant_id,
        task = %params.task,
        changed_files = changed_files.len(),
        k = params.k,
        "context.suggest_agent"
    );

    #[derive(Default)]
    struct AgentScore {
        score: f32,
        reasons: HashSet<String>,
        matched_triggers: HashSet<String>,
    }

    let task_lower = params.task.to_ascii_lowercase();
    let task_tokens: Vec<String> = params
        .task
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() >= 3)
        .map(|token| token.to_ascii_lowercase())
        .collect();

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut scores: HashMap<String, AgentScore> = HashMap::new();

    for chunk in chunks {
        let agent_names = tag_values(&chunk.tags, TAG_CTX_AGENT_PREFIX);
        if agent_names.is_empty() {
            continue;
        }

        let chunk_text = chunk.text.to_ascii_lowercase();
        let triggers = tag_values(&chunk.tags, TAG_CTX_TRIGGER_PREFIX);
        let subsystem_tags = tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX);
        let file_tags = tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX);

        for agent_name in agent_names {
            let mut score = 0.1f32;
            let mut reasons = HashSet::new();
            let mut matched_triggers = HashSet::new();

            let lexical_hits = task_tokens
                .iter()
                .filter(|token| chunk_text.contains(token.as_str()))
                .count();
            if lexical_hits > 0 {
                score += lexical_hits as f32 * 0.03;
                reasons.insert(format!("keyword_overlap:{}", lexical_hits));
            }

            if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
                score += 0.05;
                reasons.insert("hot_tier_profile".to_string());
            }

            for subsystem in &subsystem_tags {
                if task_lower.contains(&subsystem.to_ascii_lowercase()) {
                    score += 0.15;
                    reasons.insert(format!("subsystem_match:{}", subsystem));
                }
            }

            for trigger in &triggers {
                for changed_file in &changed_files {
                    if wildcard_match(trigger, changed_file) {
                        score += 0.6;
                        matched_triggers.insert(format!("{} -> {}", trigger, changed_file));
                    }
                }
            }

            for file_tag in &file_tags {
                for changed_file in &changed_files {
                    if wildcard_match(file_tag, changed_file)
                        || changed_file.contains(file_tag)
                        || file_tag.contains(changed_file)
                    {
                        score += 0.2;
                        reasons.insert(format!("file_match:{}", file_tag));
                    }
                }
            }

            let entry = scores.entry(agent_name).or_default();
            if score > entry.score {
                entry.score = score;
            }
            entry.reasons.extend(reasons);
            entry.matched_triggers.extend(matched_triggers);
        }
    }

    let considered_agents = scores.len();
    let mut recommendations: Vec<AgentSuggestion> = scores
        .into_iter()
        .map(|(agent_name, score)| {
            let mut reasons: Vec<String> = score.reasons.into_iter().collect();
            reasons.sort();
            let mut matched_triggers: Vec<String> = score.matched_triggers.into_iter().collect();
            matched_triggers.sort();

            AgentSuggestion {
                agent_name,
                score: score.score,
                reasons,
                matched_triggers,
            }
        })
        .collect();

    recommendations.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.agent_name.cmp(&b.agent_name))
    });
    recommendations.truncate(params.k);

    format_mcp_response(&ContextSuggestAgentResult {
        recommendations,
        considered_agents,
    })
}

/// Handle context.get_hot_context tool call
pub async fn handle_context_get_hot_context<S: Store>(
    store: &S,
    params: ContextGetHotContextParams,
) -> Result<Value, McpError> {
    let tenant_id = validate_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    info!(tenant_id = %tenant_id, k = params.k, "context.get_hot_context");

    let mut chunks = collect_all_chunks(store, &tenant_id, 20_000).await?;
    chunks.retain(|chunk| has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT));
    chunks.sort_by_key(|chunk| std::cmp::Reverse(chunk.timestamp_created));

    let results: Vec<ChunkResult> = chunks
        .iter()
        .take(params.k)
        .map(|chunk| chunk_to_result(chunk, 1.0, Some("hot".to_string())))
        .collect();

    format_mcp_response(&ContextGetHotContextResult { results })
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
    use crate::store::{MemoryStore, Store};
    use proptest::prelude::*;
    use serde::de::DeserializeOwned;

    fn make_store() -> MemoryStore {
        MemoryStore::new()
    }

    fn parse_tool_payload<T: DeserializeOwned>(result: &Value) -> T {
        let text = result["content"][0]["text"]
            .as_str()
            .expect("tool response should include JSON text");
        serde_json::from_str(text).expect("tool response text should parse as JSON payload")
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
    async fn feedback_records_relevance_event() {
        let store = make_store();

        let add_result = handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "feedback target chunk".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![],
            },
        )
        .await
        .unwrap();
        let add_text = add_result["content"][0]["text"].as_str().unwrap();
        let add_payload: AddResult = serde_json::from_str(add_text).unwrap();

        let feedback_result = handle_memory_feedback(
            &store,
            FeedbackParams {
                tenant_id: "test".to_string(),
                query: "feedback target".to_string(),
                chunk_id: add_payload.chunk_id,
                relevance: "relevant".to_string(),
            },
        )
        .await
        .unwrap();

        let text = feedback_result["content"][0]["text"].as_str().unwrap();
        let payload: FeedbackResult = serde_json::from_str(text).unwrap();
        assert!(payload.stored);
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

    #[tokio::test]
    async fn context_list_subsystems_groups_by_subsystem_tag() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "retrieval planning doc".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    path: Some("src/retrieval/mod.rs".to_string()),
                    ..Default::default()
                }),
                tags: vec![
                    "ctx:doc".to_string(),
                    "ctx:subsystem:retrieval".to_string(),
                    "ctx:file:src/retrieval/mod.rs".to_string(),
                ],
            },
        )
        .await
        .unwrap();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "retrieval indexing notes".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    path: Some("src/retrieval/index.rs".to_string()),
                    ..Default::default()
                }),
                tags: vec![
                    "ctx:doc".to_string(),
                    "ctx:subsystem:retrieval".to_string(),
                    "ctx:file:src/retrieval/index.rs".to_string(),
                ],
            },
        )
        .await
        .unwrap();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "planner decision".to_string(),
                chunk_type: "decision".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    path: Some("src/planner/mod.rs".to_string()),
                    ..Default::default()
                }),
                tags: vec![
                    "ctx:doc".to_string(),
                    "ctx:subsystem:planner".to_string(),
                    "ctx:file:src/planner/mod.rs".to_string(),
                ],
            },
        )
        .await
        .unwrap();

        let result = handle_context_list_subsystems(
            &store,
            ContextListSubsystemsParams {
                tenant_id: "test".to_string(),
                prefix: None,
                limit: 50,
            },
        )
        .await
        .unwrap();

        let payload: ContextListSubsystemsResult = parse_tool_payload(&result);
        assert_eq!(payload.subsystems.len(), 2);

        let retrieval = payload
            .subsystems
            .iter()
            .find(|entry| entry.key == "retrieval")
            .expect("retrieval subsystem should exist");
        assert_eq!(retrieval.chunk_count, 2);
        assert_eq!(retrieval.file_count, 2);
    }

    #[tokio::test]
    async fn context_get_files_for_subsystem_returns_tag_and_source_paths() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "storage architecture".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: Some(SourceParams {
                    path: Some("crates/memd/src/store/mod.rs".to_string()),
                    ..Default::default()
                }),
                tags: vec![
                    "ctx:subsystem:storage".to_string(),
                    "ctx:file:crates/memd/src/store/hybrid.rs".to_string(),
                ],
            },
        )
        .await
        .unwrap();

        let result = handle_context_get_files_for_subsystem(
            &store,
            ContextGetFilesForSubsystemParams {
                tenant_id: "test".to_string(),
                subsystem_key: "storage".to_string(),
                limit: 10,
            },
        )
        .await
        .unwrap();

        let payload: ContextGetFilesForSubsystemResult = parse_tool_payload(&result);
        assert_eq!(payload.subsystem_key, "storage");
        assert_eq!(payload.files.len(), 2);
        assert!(payload
            .files
            .contains(&"crates/memd/src/store/mod.rs".to_string()));
        assert!(payload
            .files
            .contains(&"crates/memd/src/store/hybrid.rs".to_string()));
    }

    #[tokio::test]
    async fn context_search_documents_filters_by_tier_and_subsystem() {
        let store = make_store();

        for (text, tier_tag) in [
            ("hot retrieval context", "ctx:tier:hot"),
            ("cold retrieval context", "ctx:tier:cold"),
        ] {
            handle_memory_add(
                &store,
                None,
                AddParams {
                    tenant_id: "test".to_string(),
                    text: text.to_string(),
                    chunk_type: "doc".to_string(),
                    project_id: None,
                    episode_id: None,
                    source: None,
                    tags: vec![
                        "ctx:doc".to_string(),
                        "ctx:subsystem:retrieval".to_string(),
                        tier_tag.to_string(),
                    ],
                },
            )
            .await
            .unwrap();
        }

        let result = handle_context_search_documents(
            &store,
            ContextSearchDocumentsParams {
                tenant_id: "test".to_string(),
                query: "retrieval".to_string(),
                k: 10,
                subsystem_key: Some("retrieval".to_string()),
                tier: Some("hot".to_string()),
            },
        )
        .await
        .unwrap();

        let payload: ContextSearchDocumentsResult = parse_tool_payload(&result);
        assert_eq!(payload.results.len(), 1);
        assert_eq!(payload.results[0].text, "hot retrieval context");
        assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
    }

    #[tokio::test]
    async fn context_find_relevant_context_can_prepend_hot_chunks() {
        let store = make_store();
        let tenant = TenantId::new("test").unwrap();

        let mut hot = MemoryChunk::new(tenant.clone(), "incident runbook", ChunkType::Doc);
        hot.tags = vec!["ctx:tier:hot".to_string(), "ctx:subsystem:ops".to_string()];
        hot.timestamp_created = 10;
        store.add(hot).await.unwrap();

        let mut relevant = MemoryChunk::new(
            tenant,
            "database migration checklist for ops",
            ChunkType::Doc,
        );
        relevant.tags = vec![
            "ctx:doc".to_string(),
            "ctx:subsystem:ops".to_string(),
            "ctx:tier:cold".to_string(),
        ];
        relevant.timestamp_created = 5;
        store.add(relevant).await.unwrap();

        let result = handle_context_find_relevant_context(
            &store,
            ContextFindRelevantContextParams {
                tenant_id: "test".to_string(),
                task: "database migration".to_string(),
                k: 5,
                subsystem_keys: Some(vec!["ops".to_string()]),
                include_hot: true,
            },
        )
        .await
        .unwrap();

        let payload: ContextFindRelevantContextResult = parse_tool_payload(&result);
        assert!(payload.hot_included);
        assert!(!payload.results.is_empty());
        assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
    }

    #[tokio::test]
    async fn context_suggest_agent_uses_trigger_and_file_matches() {
        let store = make_store();

        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: "storage compaction and WAL tuning playbook".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![
                    "ctx:agent:storage-specialist".to_string(),
                    "ctx:trigger:crates/memd/src/store/*".to_string(),
                    "ctx:subsystem:storage".to_string(),
                    "ctx:file:crates/memd/src/store/hybrid.rs".to_string(),
                    "ctx:tier:hot".to_string(),
                ],
            },
        )
        .await
        .unwrap();

        let result = handle_context_suggest_agent(
            &store,
            ContextSuggestAgentParams {
                tenant_id: "test".to_string(),
                task: "Improve storage compaction behavior".to_string(),
                changed_files: Some(vec!["crates/memd/src/store/hybrid.rs".to_string()]),
                k: 3,
            },
        )
        .await
        .unwrap();

        let payload: ContextSuggestAgentResult = parse_tool_payload(&result);
        assert!(!payload.recommendations.is_empty());
        assert_eq!(
            payload.recommendations[0].agent_name,
            "storage-specialist".to_string()
        );
        assert!(!payload.recommendations[0].matched_triggers.is_empty());
    }

    #[tokio::test]
    async fn context_get_hot_context_returns_most_recent_chunks() {
        let store = make_store();
        let tenant = TenantId::new("test").unwrap();

        let mut older = MemoryChunk::new(tenant.clone(), "older hot context", ChunkType::Doc);
        older.tags = vec!["ctx:tier:hot".to_string()];
        older.timestamp_created = 1;
        store.add(older).await.unwrap();

        let mut newest = MemoryChunk::new(tenant, "newest hot context", ChunkType::Doc);
        newest.tags = vec!["ctx:tier:hot".to_string()];
        newest.timestamp_created = 2;
        store.add(newest).await.unwrap();

        let result = handle_context_get_hot_context(
            &store,
            ContextGetHotContextParams {
                tenant_id: "test".to_string(),
                k: 1,
            },
        )
        .await
        .unwrap();

        let payload: ContextGetHotContextResult = parse_tool_payload(&result);
        assert_eq!(payload.results.len(), 1);
        assert_eq!(payload.results[0].text, "newest hot context");
        assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
    }

    #[tokio::test]
    async fn task_get_returns_full_artifact_history() {
        let store = make_store();

        let start: TaskArtifactResult = parse_tool_payload(
            &handle_task_start(
                &store,
                None,
                TaskStartParams {
                    tenant_id: "test".to_string(),
                    project_id: Some("proj_alpha".to_string()),
                    parent_task_id: None,
                    agent_id: Some("agent-1".to_string()),
                    session_id: Some("session-7".to_string()),
                    goal: "Quantify the stress-response regulon".to_string(),
                    motivation: "The regulator mechanism is unresolved".to_string(),
                    hypothesis: "Sigma factor S drives the induced genes".to_string(),
                    scientific_question: "Which genes increase after the perturbation?".to_string(),
                    dataset_refs: vec![TaskDatasetRefParams {
                        name: "rna_seq".to_string(),
                        version: Some("v1".to_string()),
                        description: None,
                    }],
                    expected_outputs: vec!["differential expression table".to_string()],
                    entity_refs: vec![],
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        handle_task_progress(
            &store,
            None,
            TaskProgressParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                summary: "Initial QC exposed one low-depth replicate".to_string(),
                blockers: vec!["One replicate is borderline".to_string()],
                failed_attempts: vec!["Default trimming removed too much signal".to_string()],
                next_step: "Re-run with stricter QC but lighter trimming".to_string(),
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        handle_task_run_start(
            &store,
            None,
            TaskRunStartParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                tool_name: "mmseqs".to_string(),
                tool_version: Some("15".to_string()),
                command: "mmseqs search db query out tmp".to_string(),
                why_chosen: "Fast enough for iterative parameter sweeps".to_string(),
                parameters: json!({"sensitivity": 7.5}),
                inputs: vec!["query.faa".to_string()],
                summary: Some("Homology search for candidate regulators".to_string()),
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        handle_task_run_finish(
            &store,
            None,
            TaskRunFinishParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                status: "completed".to_string(),
                tool_name: Some("mmseqs".to_string()),
                tool_version: Some("15".to_string()),
                command: Some("mmseqs search db query out tmp".to_string()),
                outputs: vec!["hits.tsv".to_string()],
                metrics: Some(json!({"top_hit_bitscore": 310.5})),
                notes: "Recovered a strong candidate regulator".to_string(),
                validation: vec!["Top hit was stable across reruns".to_string()],
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        handle_task_add_evidence(
            &store,
            None,
            TaskAddEvidenceParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                summary: "Top hit exceeded the curated threshold".to_string(),
                evidence_kind: "metric".to_string(),
                supports_claim: true,
                metric_name: Some("top_hit_bitscore".to_string()),
                metric_value: Some(json!(310.5)),
                metrics: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_task_get(
            &store,
            TaskGetParams {
                tenant_id: "test".to_string(),
                task_id: start.task_id,
            },
        )
        .await
        .unwrap();

        let payload: TaskGetResult = parse_tool_payload(&result);
        assert_eq!(payload.artifacts.len(), 5);
        assert!(payload
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_kind == ArtifactKind::TaskStart));
        assert!(payload
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_kind == ArtifactKind::Evidence));
    }

    #[tokio::test]
    async fn task_search_filters_exactly_by_tool_and_dataset() {
        let store = make_store();

        let task_a: TaskArtifactResult = parse_tool_payload(
            &handle_task_start(
                &store,
                None,
                TaskStartParams {
                    tenant_id: "test".to_string(),
                    project_id: Some("proj_alpha".to_string()),
                    parent_task_id: None,
                    agent_id: None,
                    session_id: None,
                    goal: "Task A goal".to_string(),
                    motivation: "Task A motivation".to_string(),
                    hypothesis: "Task A hypothesis".to_string(),
                    scientific_question: "Task A question".to_string(),
                    dataset_refs: vec![TaskDatasetRefParams {
                        name: "rna_seq".to_string(),
                        version: Some("v1".to_string()),
                        description: None,
                    }],
                    expected_outputs: vec!["table".to_string()],
                    entity_refs: vec![],
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        handle_task_run_start(
            &store,
            None,
            TaskRunStartParams {
                tenant_id: "test".to_string(),
                task_id: task_a.task_id.clone(),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                tool_name: "mmseqs".to_string(),
                tool_version: None,
                command: "mmseqs search db query out tmp".to_string(),
                why_chosen: "Fast iterative search".to_string(),
                parameters: json!({"sensitivity": 7.5}),
                inputs: vec!["query.faa".to_string()],
                summary: Some("Candidate search".to_string()),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "rna_seq".to_string(),
                    version: Some("v1".to_string()),
                    description: None,
                }],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        let task_b: TaskArtifactResult = parse_tool_payload(
            &handle_task_start(
                &store,
                None,
                TaskStartParams {
                    tenant_id: "test".to_string(),
                    project_id: Some("proj_beta".to_string()),
                    parent_task_id: None,
                    agent_id: None,
                    session_id: None,
                    goal: "Task B goal".to_string(),
                    motivation: "Task B motivation".to_string(),
                    hypothesis: "Task B hypothesis".to_string(),
                    scientific_question: "Task B question".to_string(),
                    dataset_refs: vec![TaskDatasetRefParams {
                        name: "proteomics".to_string(),
                        version: Some("v2".to_string()),
                        description: None,
                    }],
                    expected_outputs: vec!["summary".to_string()],
                    entity_refs: vec![],
                    provenance: None,
                },
            )
            .await
            .unwrap(),
        );

        handle_task_run_start(
            &store,
            None,
            TaskRunStartParams {
                tenant_id: "test".to_string(),
                task_id: task_b.task_id,
                project_id: Some("proj_beta".to_string()),
                agent_id: None,
                session_id: None,
                tool_name: "blast".to_string(),
                tool_version: None,
                command: "blastp -query q -db db".to_string(),
                why_chosen: "Reference comparison".to_string(),
                parameters: json!({"evalue": 1e-5}),
                inputs: vec!["query.faa".to_string()],
                summary: Some("Candidate search".to_string()),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "proteomics".to_string(),
                    version: Some("v2".to_string()),
                    description: None,
                }],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap();

        let result = handle_task_search(
            &store,
            TaskSearchParams {
                tenant_id: "test".to_string(),
                query: "parameter sweeps".to_string(),
                k: 10,
                filters: Some(TaskSearchFiltersParams {
                    task_id: Some(task_a.task_id),
                    artifact_kind: Some("run_start".to_string()),
                    status: Some("started".to_string()),
                    dataset_name: Some("rna_seq".to_string()),
                    dataset_version: Some("v1".to_string()),
                    entity_name: None,
                    entity_type: None,
                    tool_name: Some("mmseqs".to_string()),
                    project_id: Some("proj_alpha".to_string()),
                    agent_id: None,
                    session_id: None,
                }),
            },
        )
        .await
        .unwrap();

        let payload: SearchResult = parse_tool_payload(&result);
        assert_eq!(payload.results.len(), 1);
        assert!(payload.results[0]
            .tags
            .iter()
            .any(|tag| tag.starts_with("task:kind:run_start")));
    }

    #[tokio::test]
    async fn task_start_stores_canonical_artifact_and_projection_chunks() {
        let store = make_store();

        let result = handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "test".to_string(),
                project_id: Some("proj_alpha".to_string()),
                parent_task_id: None,
                agent_id: Some("agent-1".to_string()),
                session_id: Some("session-7".to_string()),
                goal: "Quantify the stress-response regulon".to_string(),
                motivation: "The regulator mechanism is unresolved".to_string(),
                hypothesis: "Sigma factor S drives the induced genes".to_string(),
                scientific_question: "Which genes increase after the perturbation?".to_string(),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "rna_seq".to_string(),
                    version: Some("v1".to_string()),
                    description: None,
                }],
                expected_outputs: vec!["differential expression table".to_string()],
                entity_refs: vec![TaskEntityRefParams {
                    name: "RpoS".to_string(),
                    entity_type: "protein".to_string(),
                    role: Some("candidate regulator".to_string()),
                }],
                provenance: Some(TaskProvenanceParams {
                    tool_name: Some("codex".to_string()),
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();

        let payload: TaskArtifactResult = parse_tool_payload(&result);
        assert!(!payload.artifact_id.is_empty());
        assert!(!payload.task_id.is_empty());
        assert!(!payload.projection_chunk_ids.is_empty());

        let tenant = TenantId::new("test").unwrap();
        let stored = store
            .get_task_artifact(&tenant, &payload.artifact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.goal.as_deref(),
            Some("Quantify the stress-response regulon")
        );
        assert_eq!(stored.dataset_refs.len(), 1);

        let search = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "stress-response regulon".to_string(),
                project_id: Some("proj_alpha".to_string()),
                k: 10,
                filters: None,
                debug_tiers: None,
            },
        )
        .await
        .unwrap();
        let search_payload: SearchResult = parse_tool_payload(&search);
        assert!(!search_payload.results.is_empty());
        assert!(search_payload.results.iter().any(|result| result
            .tags
            .iter()
            .any(|tag| tag.starts_with("task:kind:task_start"))));
    }

    #[tokio::test]
    async fn task_finish_stores_failed_and_validation_projections() {
        let store = make_store();

        let result =
            handle_task_finish(
                &store,
                None,
                TaskFinishParams {
                    tenant_id: "test".to_string(),
                    task_id: "task-123".to_string(),
                    project_id: Some("proj_alpha".to_string()),
                    agent_id: Some("agent-1".to_string()),
                    session_id: Some("session-7".to_string()),
                    status: Some("completed".to_string()),
                    goal: Some("Quantify the stress-response regulon".to_string()),
                    scientific_question: None,
                    dataset_refs: vec![],
                    entity_refs: vec![],
                    what_worked: vec![
                        "Re-running with stricter QC stabilized the hit list".to_string()
                    ],
                    what_failed: vec!["The first alignment preset over-trimmed reads".to_string()],
                    validation: vec!["Independent replicate confirmed the top genes".to_string()],
                    uncertainty: vec!["One replicate remains borderline".to_string()],
                    followups: vec!["Collect an additional replicate".to_string()],
                    confidence: 0.78,
                    provenance: None,
                },
            )
            .await
            .unwrap();

        let payload: TaskArtifactResult = parse_tool_payload(&result);
        let tenant = TenantId::new("test").unwrap();
        let stored = store
            .get_task_artifact(&tenant, &payload.artifact_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.task_id, "task-123");
        assert_eq!(stored.confidence, Some(0.78));

        let search = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "test".to_string(),
                query: "over-trimmed reads".to_string(),
                project_id: Some("proj_alpha".to_string()),
                k: 10,
                filters: None,
                debug_tiers: None,
            },
        )
        .await
        .unwrap();
        let search_payload: SearchResult = parse_tool_payload(&search);
        assert!(search_payload.results.iter().any(|result| result
            .tags
            .iter()
            .any(|tag| tag.starts_with("task:projection:failed"))));
    }

    #[tokio::test]
    async fn task_finish_rejects_out_of_range_confidence() {
        let store = make_store();

        let result = handle_task_finish(
            &store,
            None,
            TaskFinishParams {
                tenant_id: "test".to_string(),
                task_id: "task-123".to_string(),
                project_id: None,
                agent_id: None,
                session_id: None,
                status: None,
                goal: None,
                scientific_question: None,
                dataset_refs: vec![],
                entity_refs: vec![],
                what_worked: vec![],
                what_failed: vec![],
                validation: vec![],
                uncertainty: vec![],
                followups: vec![],
                confidence: 1.1,
                provenance: None,
            },
        )
        .await;

        assert!(matches!(result, Err(McpError::InvalidParams(_))));
    }
}
