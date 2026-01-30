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
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new(1000)
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
        }
    }
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (p * sorted.len() / 100).min(sorted.len() - 1);
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
}
