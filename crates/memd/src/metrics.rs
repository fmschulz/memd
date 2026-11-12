//! Metrics collection and reporting
//!
//! Collects latency breakdown and index statistics for observability.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Per-query latency breakdown
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryMetrics {
    /// Time spent generating query embedding (ms)
    pub embed_ms: u64,
    /// Time spent in dense HNSW search (ms)
    pub dense_search_ms: u64,
    /// Time spent fetching chunk data (ms)
    pub fetch_ms: u64,
    /// Total query time (ms)
    pub total_ms: u64,
}

impl QueryMetrics {
    /// Create from timing measurements
    pub fn from_timings(
        embed: Duration,
        dense_search: Duration,
        fetch: Duration,
        total: Duration,
    ) -> Self {
        Self {
            embed_ms: embed.as_millis() as u64,
            dense_search_ms: dense_search.as_millis() as u64,
            fetch_ms: fetch.as_millis() as u64,
            total_ms: total.as_millis() as u64,
        }
    }
}

/// Aggregated latency statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyStats {
    /// Number of queries
    pub count: u64,
    /// Average total latency (ms)
    pub avg_total_ms: f64,
    /// Average embed latency (ms)
    pub avg_embed_ms: f64,
    /// Average dense search latency (ms)
    pub avg_dense_search_ms: f64,
    /// P50 total latency (ms)
    pub p50_total_ms: u64,
    /// P90 total latency (ms)
    pub p90_total_ms: u64,
    /// P99 total latency (ms)
    pub p99_total_ms: u64,
}

/// Index statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexStats {
    /// Number of chunks indexed
    pub chunks_indexed: u64,
    /// Number of embeddings stored
    pub embeddings_count: u64,
    /// Embedding dimension
    pub embedding_dimension: usize,
    /// Total index memory estimate (bytes)
    pub index_memory_bytes: u64,
}

/// Per-(tool, reason) rejection counter.
///
/// Phase 4.4: every time a tool call surfaces an `OperationError` (invalid
/// params, method not found, internal error, etc.) we bump the
/// matching counter. Operators can then see at a glance which tool
/// is producing which kind of rejection — historically these errors
/// flew straight back to the client with no aggregate visibility.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RejectionStats {
    /// Total rejections across all tools/reasons.
    pub total: u64,
    /// Per-tool rejection counts, keyed by tool name (e.g. `"memory.search"`).
    pub by_tool: HashMap<String, u64>,
    /// Per-reason rejection counts, keyed by operation-error variant name
    /// (e.g. `"invalid_params"`, `"method_not_found"`).
    pub by_reason: HashMap<String, u64>,
}

/// Per-call operation payload token estimate.
///
/// These counters estimate tokens added by operation request params and
/// handler response payloads that `memd` can observe. They do not include the
/// agent's hidden reasoning, system prompt, repo/tool transcripts outside
/// `memd`, or provider-side cache accounting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolTokenUsage {
    /// Timestamp (Unix ms)
    pub timestamp: i64,
    /// Operation name, e.g. `"memory.search"`.
    pub tool: String,
    /// Serialized operation request params size.
    pub request_bytes: u64,
    /// Serialized operation response or error size.
    pub response_bytes: u64,
    /// Estimated request tokens.
    pub estimated_request_tokens: u64,
    /// Estimated response tokens.
    pub estimated_response_tokens: u64,
    /// Estimated request + response tokens.
    pub estimated_total_tokens: u64,
    /// Whether the tool call returned `Ok`.
    pub success: bool,
}

impl ToolTokenUsage {
    /// Build usage from serialized payload sizes.
    pub fn from_payload_sizes(
        tool: impl Into<String>,
        request_bytes: usize,
        response_bytes: usize,
        success: bool,
    ) -> Self {
        let request_bytes = request_bytes as u64;
        let response_bytes = response_bytes as u64;
        let estimated_request_tokens = estimate_tokens_from_bytes(request_bytes);
        let estimated_response_tokens = estimate_tokens_from_bytes(response_bytes);

        Self {
            timestamp: now_unix_ms(),
            tool: tool.into(),
            request_bytes,
            response_bytes,
            estimated_request_tokens,
            estimated_response_tokens,
            estimated_total_tokens: estimated_request_tokens + estimated_response_tokens,
            success,
        }
    }
}

/// Aggregated token estimate for one operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolTokenAggregate {
    /// Tool calls observed.
    pub calls: u64,
    /// Calls that returned an error.
    pub errors: u64,
    /// Total serialized request bytes.
    pub request_bytes: u64,
    /// Total serialized response/error bytes.
    pub response_bytes: u64,
    /// Total estimated request tokens.
    pub estimated_request_tokens: u64,
    /// Total estimated response tokens.
    pub estimated_response_tokens: u64,
    /// Total estimated request + response tokens.
    pub estimated_total_tokens: u64,
}

impl ToolTokenAggregate {
    fn record(&mut self, usage: &ToolTokenUsage) {
        self.calls += 1;
        if !usage.success {
            self.errors += 1;
        }
        self.request_bytes += usage.request_bytes;
        self.response_bytes += usage.response_bytes;
        self.estimated_request_tokens += usage.estimated_request_tokens;
        self.estimated_response_tokens += usage.estimated_response_tokens;
        self.estimated_total_tokens += usage.estimated_total_tokens;
    }
}

/// Aggregated operation payload token estimates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsageStats {
    /// Estimator used for all token fields.
    pub estimator: String,
    /// Aggregate over all operation calls.
    pub total: ToolTokenAggregate,
    /// Aggregate per operation.
    pub by_tool: HashMap<String, ToolTokenAggregate>,
    /// Recent operation payload estimates.
    pub recent_tool_calls: Vec<ToolTokenUsage>,
}

/// Complete metrics snapshot
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    /// Timestamp (Unix ms)
    pub timestamp: i64,
    /// Per-tenant index stats
    pub index: HashMap<String, IndexStats>,
    /// Latency statistics
    pub latency: LatencyStats,
    /// Recent query metrics (last N queries)
    pub recent_queries: Vec<QueryMetrics>,
    /// Phase 4.4: per-tool / per-reason rejection counts.
    #[serde(default)]
    pub rejections: RejectionStats,
    /// Estimated operation payload token usage by operation.
    #[serde(default)]
    pub token_usage: TokenUsageStats,
}

/// Metrics collector
pub struct MetricsCollector {
    /// Recent query latencies for percentile calculation
    latencies: RwLock<Vec<QueryMetrics>>,
    /// Maximum history size
    max_history: usize,
    /// Total query count
    query_count: AtomicU64,
    /// Cumulative totals for averages
    total_embed_ms: AtomicU64,
    total_search_ms: AtomicU64,
    total_fetch_ms: AtomicU64,
    total_total_ms: AtomicU64,
    /// Phase 4.4: per-tool and per-reason rejection counts.
    rejections_total: AtomicU64,
    rejections_by_tool: RwLock<HashMap<String, u64>>,
    rejections_by_reason: RwLock<HashMap<String, u64>>,
    /// Operation payload token estimates.
    tool_token_calls: AtomicU64,
    tool_token_errors: AtomicU64,
    tool_token_request_bytes: AtomicU64,
    tool_token_response_bytes: AtomicU64,
    tool_token_request_tokens: AtomicU64,
    tool_token_response_tokens: AtomicU64,
    tool_token_by_tool: RwLock<HashMap<String, ToolTokenAggregate>>,
    tool_token_recent: RwLock<Vec<ToolTokenUsage>>,
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new(1000)
    }
}

impl Clone for MetricsCollector {
    fn clone(&self) -> Self {
        Self {
            latencies: RwLock::new(self.latencies.read().clone()),
            max_history: self.max_history,
            query_count: AtomicU64::new(self.query_count.load(Ordering::Relaxed)),
            total_embed_ms: AtomicU64::new(self.total_embed_ms.load(Ordering::Relaxed)),
            total_search_ms: AtomicU64::new(self.total_search_ms.load(Ordering::Relaxed)),
            total_fetch_ms: AtomicU64::new(self.total_fetch_ms.load(Ordering::Relaxed)),
            total_total_ms: AtomicU64::new(self.total_total_ms.load(Ordering::Relaxed)),
            rejections_total: AtomicU64::new(self.rejections_total.load(Ordering::Relaxed)),
            rejections_by_tool: RwLock::new(self.rejections_by_tool.read().clone()),
            rejections_by_reason: RwLock::new(self.rejections_by_reason.read().clone()),
            tool_token_calls: AtomicU64::new(self.tool_token_calls.load(Ordering::Relaxed)),
            tool_token_errors: AtomicU64::new(self.tool_token_errors.load(Ordering::Relaxed)),
            tool_token_request_bytes: AtomicU64::new(
                self.tool_token_request_bytes.load(Ordering::Relaxed),
            ),
            tool_token_response_bytes: AtomicU64::new(
                self.tool_token_response_bytes.load(Ordering::Relaxed),
            ),
            tool_token_request_tokens: AtomicU64::new(
                self.tool_token_request_tokens.load(Ordering::Relaxed),
            ),
            tool_token_response_tokens: AtomicU64::new(
                self.tool_token_response_tokens.load(Ordering::Relaxed),
            ),
            tool_token_by_tool: RwLock::new(self.tool_token_by_tool.read().clone()),
            tool_token_recent: RwLock::new(self.tool_token_recent.read().clone()),
        }
    }
}

impl MetricsCollector {
    /// Create a new metrics collector
    pub fn new(max_history: usize) -> Self {
        Self {
            latencies: RwLock::new(Vec::with_capacity(max_history)),
            max_history,
            query_count: AtomicU64::new(0),
            total_embed_ms: AtomicU64::new(0),
            total_search_ms: AtomicU64::new(0),
            total_fetch_ms: AtomicU64::new(0),
            total_total_ms: AtomicU64::new(0),
            rejections_total: AtomicU64::new(0),
            rejections_by_tool: RwLock::new(HashMap::new()),
            rejections_by_reason: RwLock::new(HashMap::new()),
            tool_token_calls: AtomicU64::new(0),
            tool_token_errors: AtomicU64::new(0),
            tool_token_request_bytes: AtomicU64::new(0),
            tool_token_response_bytes: AtomicU64::new(0),
            tool_token_request_tokens: AtomicU64::new(0),
            tool_token_response_tokens: AtomicU64::new(0),
            tool_token_by_tool: RwLock::new(HashMap::new()),
            tool_token_recent: RwLock::new(Vec::with_capacity(max_history)),
        }
    }

    /// Phase 4.4: record a rejected tool call.
    ///
    /// `tool` is the operation name (e.g. `"memory.search"`). `reason`
    /// is a short kebab-case label like `"invalid-params"` or
    /// `"method-not-found"` — typically derived from the `McpError`
    /// variant. Cheap path: three atomic/lock ops, no allocation
    /// beyond the HashMap entry when a new key is seen.
    pub fn record_rejection(&self, tool: &str, reason: &str) {
        self.rejections_total.fetch_add(1, Ordering::Relaxed);
        *self
            .rejections_by_tool
            .write()
            .entry(tool.to_string())
            .or_insert(0) += 1;
        *self
            .rejections_by_reason
            .write()
            .entry(reason.to_string())
            .or_insert(0) += 1;
    }

    /// Phase 4.4: read the current rejection aggregate.
    pub fn get_rejection_stats(&self) -> RejectionStats {
        RejectionStats {
            total: self.rejections_total.load(Ordering::Relaxed),
            by_tool: self.rejections_by_tool.read().clone(),
            by_reason: self.rejections_by_reason.read().clone(),
        }
    }

    /// Record estimated operation request/response tokens for one call.
    pub fn record_tool_token_usage(&self, usage: ToolTokenUsage) {
        self.tool_token_calls.fetch_add(1, Ordering::Relaxed);
        if !usage.success {
            self.tool_token_errors.fetch_add(1, Ordering::Relaxed);
        }
        self.tool_token_request_bytes
            .fetch_add(usage.request_bytes, Ordering::Relaxed);
        self.tool_token_response_bytes
            .fetch_add(usage.response_bytes, Ordering::Relaxed);
        self.tool_token_request_tokens
            .fetch_add(usage.estimated_request_tokens, Ordering::Relaxed);
        self.tool_token_response_tokens
            .fetch_add(usage.estimated_response_tokens, Ordering::Relaxed);

        self.tool_token_by_tool
            .write()
            .entry(usage.tool.clone())
            .or_default()
            .record(&usage);

        let mut recent = self.tool_token_recent.write();
        if recent.len() >= self.max_history {
            recent.remove(0);
        }
        recent.push(usage);
    }

    /// Get recent operation token estimates.
    pub fn get_recent_tool_token_usage(&self, limit: usize) -> Vec<ToolTokenUsage> {
        let recent = self.tool_token_recent.read();
        recent.iter().rev().take(limit).cloned().collect()
    }

    /// Get aggregated operation payload token estimates.
    pub fn get_token_usage_stats(&self) -> TokenUsageStats {
        let request_tokens = self.tool_token_request_tokens.load(Ordering::Relaxed);
        let response_tokens = self.tool_token_response_tokens.load(Ordering::Relaxed);

        TokenUsageStats {
            estimator: "ceil(serialized_mcp_payload_bytes / 4)".to_string(),
            total: ToolTokenAggregate {
                calls: self.tool_token_calls.load(Ordering::Relaxed),
                errors: self.tool_token_errors.load(Ordering::Relaxed),
                request_bytes: self.tool_token_request_bytes.load(Ordering::Relaxed),
                response_bytes: self.tool_token_response_bytes.load(Ordering::Relaxed),
                estimated_request_tokens: request_tokens,
                estimated_response_tokens: response_tokens,
                estimated_total_tokens: request_tokens + response_tokens,
            },
            by_tool: self.tool_token_by_tool.read().clone(),
            recent_tool_calls: self.get_recent_tool_token_usage(10),
        }
    }

    /// Record a query's latency breakdown
    pub fn record_query(&self, metrics: QueryMetrics) {
        // Update totals
        self.query_count.fetch_add(1, Ordering::Relaxed);
        self.total_embed_ms
            .fetch_add(metrics.embed_ms, Ordering::Relaxed);
        self.total_search_ms
            .fetch_add(metrics.dense_search_ms, Ordering::Relaxed);
        self.total_fetch_ms
            .fetch_add(metrics.fetch_ms, Ordering::Relaxed);
        self.total_total_ms
            .fetch_add(metrics.total_ms, Ordering::Relaxed);

        // Add to history (circular buffer)
        let mut latencies = self.latencies.write();
        if latencies.len() >= self.max_history {
            latencies.remove(0);
        }
        latencies.push(metrics);
    }

    /// Get latency statistics
    pub fn get_latency_stats(&self) -> LatencyStats {
        let count = self.query_count.load(Ordering::Relaxed);
        if count == 0 {
            return LatencyStats::default();
        }

        let latencies = self.latencies.read();

        // Calculate percentiles
        let mut totals: Vec<u64> = latencies.iter().map(|m| m.total_ms).collect();
        totals.sort_unstable();

        let p50 = percentile(&totals, 50);
        let p90 = percentile(&totals, 90);
        let p99 = percentile(&totals, 99);

        LatencyStats {
            count,
            avg_total_ms: self.total_total_ms.load(Ordering::Relaxed) as f64 / count as f64,
            avg_embed_ms: self.total_embed_ms.load(Ordering::Relaxed) as f64 / count as f64,
            avg_dense_search_ms: self.total_search_ms.load(Ordering::Relaxed) as f64 / count as f64,
            p50_total_ms: p50,
            p90_total_ms: p90,
            p99_total_ms: p99,
        }
    }

    /// Get recent query metrics
    pub fn get_recent_queries(&self, limit: usize) -> Vec<QueryMetrics> {
        let latencies = self.latencies.read();
        latencies.iter().rev().take(limit).cloned().collect()
    }

    /// Create a metrics snapshot
    pub fn snapshot(&self, index_stats: HashMap<String, IndexStats>) -> MetricsSnapshot {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        MetricsSnapshot {
            timestamp,
            index: index_stats,
            latency: self.get_latency_stats(),
            recent_queries: self.get_recent_queries(10),
            rejections: self.get_rejection_stats(),
            token_usage: self.get_token_usage_stats(),
        }
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn estimate_tokens_from_bytes(bytes: u64) -> u64 {
    bytes.saturating_add(3) / 4
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    // Use an inclusive rank over n-1 so p50/p90 on small samples behave as expected.
    let idx = (p * (sorted.len() - 1) / 100).min(sorted.len() - 1);
    sorted[idx]
}

/// Timer for measuring durations
pub struct Timer {
    start: Instant,
}

impl Timer {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_stats() {
        let collector = MetricsCollector::new(100);

        // Record some queries
        for i in 1..=10 {
            collector.record_query(QueryMetrics {
                embed_ms: i * 10,
                dense_search_ms: i * 5,
                fetch_ms: i * 2,
                total_ms: i * 17,
            });
        }

        let stats = collector.get_latency_stats();
        assert_eq!(stats.count, 10);
        assert!(stats.avg_total_ms > 0.0);
        assert!(stats.p50_total_ms > 0);
    }

    #[test]
    fn test_percentile() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&data, 50), 5);
        assert_eq!(percentile(&data, 90), 9);
    }

    #[test]
    fn test_timer() {
        let timer = Timer::start();
        std::thread::sleep(Duration::from_millis(10));
        assert!(timer.elapsed_ms() >= 10);
    }

    #[test]
    fn test_query_metrics_from_timings() {
        let metrics = QueryMetrics::from_timings(
            Duration::from_millis(10),
            Duration::from_millis(5),
            Duration::from_millis(2),
            Duration::from_millis(17),
        );

        assert_eq!(metrics.embed_ms, 10);
        assert_eq!(metrics.dense_search_ms, 5);
        assert_eq!(metrics.fetch_ms, 2);
        assert_eq!(metrics.total_ms, 17);
    }

    #[test]
    fn test_snapshot() {
        let collector = MetricsCollector::new(100);

        collector.record_query(QueryMetrics {
            embed_ms: 10,
            dense_search_ms: 5,
            fetch_ms: 2,
            total_ms: 17,
        });

        let mut index_stats = HashMap::new();
        index_stats.insert(
            "test_tenant".to_string(),
            IndexStats {
                chunks_indexed: 100,
                embeddings_count: 100,
                embedding_dimension: 384,
                index_memory_bytes: 1024 * 1024,
            },
        );

        let snapshot = collector.snapshot(index_stats);

        assert!(snapshot.timestamp > 0);
        assert_eq!(snapshot.latency.count, 1);
        assert_eq!(snapshot.recent_queries.len(), 1);
        assert!(snapshot.index.contains_key("test_tenant"));
    }

    #[test]
    fn test_circular_buffer() {
        let collector = MetricsCollector::new(5);

        // Record more than max_history
        for i in 1..=10 {
            collector.record_query(QueryMetrics {
                embed_ms: i,
                dense_search_ms: i,
                fetch_ms: i,
                total_ms: i,
            });
        }

        // Should only keep last 5
        let recent = collector.get_recent_queries(10);
        assert_eq!(recent.len(), 5);

        // Most recent should be 10 (reversed order)
        assert_eq!(recent[0].total_ms, 10);
        assert_eq!(recent[4].total_ms, 6);
    }

    #[test]
    fn test_empty_stats() {
        let collector = MetricsCollector::new(100);
        let stats = collector.get_latency_stats();

        assert_eq!(stats.count, 0);
        assert_eq!(stats.avg_total_ms, 0.0);
        assert_eq!(stats.p50_total_ms, 0);
    }

    #[test]
    fn test_tool_token_usage_recording() {
        let collector = MetricsCollector::new(10);

        collector.record_tool_token_usage(ToolTokenUsage::from_payload_sizes(
            "memory.search",
            15,
            400,
            true,
        ));
        collector.record_tool_token_usage(ToolTokenUsage::from_payload_sizes(
            "memory.search",
            5,
            7,
            false,
        ));

        let stats = collector.get_token_usage_stats();
        assert_eq!(stats.total.calls, 2);
        assert_eq!(stats.total.errors, 1);
        assert_eq!(stats.total.request_bytes, 20);
        assert_eq!(stats.total.response_bytes, 407);
        assert_eq!(stats.total.estimated_request_tokens, 6);
        assert_eq!(stats.total.estimated_response_tokens, 102);
        assert_eq!(stats.total.estimated_total_tokens, 108);

        let search = stats.by_tool.get("memory.search").unwrap();
        assert_eq!(search.calls, 2);
        assert_eq!(search.errors, 1);
        assert_eq!(stats.recent_tool_calls.len(), 2);
        assert!(!stats.recent_tool_calls[0].success);
    }

    #[test]
    fn rejection_stats_record_and_snapshot() {
        let collector = MetricsCollector::new(100);

        collector.record_rejection("memory.search", "invalid-params");
        collector.record_rejection("memory.search", "invalid-params");
        collector.record_rejection("memory.search", "tool-error");
        collector.record_rejection("task.start", "invalid-params");

        let stats = collector.get_rejection_stats();
        assert_eq!(stats.total, 4);
        assert_eq!(stats.by_tool.get("memory.search"), Some(&3));
        assert_eq!(stats.by_tool.get("task.start"), Some(&1));
        assert_eq!(stats.by_reason.get("invalid-params"), Some(&3));
        assert_eq!(stats.by_reason.get("tool-error"), Some(&1));

        // Rejection stats must also flow into the full snapshot.
        let snapshot = collector.snapshot(HashMap::new());
        assert_eq!(snapshot.rejections.total, 4);
        assert_eq!(snapshot.rejections.by_tool.get("memory.search"), Some(&3));
    }
}
