//! Metrics collection and reporting
//!
//! Collects latency breakdown and index statistics for observability.
//! Includes tiered metrics for cache/hot/warm tier performance tracking.

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

/// Tiered search metrics (aggregated)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TieredMetrics {
    /// Total cache lookups
    pub cache_lookups: u64,
    /// Cache hits
    pub cache_hits: u64,
    /// Cache misses
    pub cache_misses: u64,
    /// Cache hit rate (0.0-1.0)
    pub cache_hit_rate: f32,
    /// Hot tier searches performed
    pub hot_tier_searches: u64,
    /// Queries satisfied by hot tier alone
    pub hot_tier_hits: u64,
    /// Warm tier searches performed
    pub warm_tier_searches: u64,
    /// Chunks promoted to hot tier
    pub promotions: u64,
    /// Chunks demoted from hot tier
    pub demotions: u64,
    /// Average cache lookup latency (ms)
    pub avg_cache_lookup_ms: f64,
    /// Average hot tier search latency (ms)
    pub avg_hot_tier_ms: f64,
    /// Average warm tier search latency (ms)
    pub avg_warm_tier_ms: f64,
}

impl TieredMetrics {
    /// Calculate cache hit rate from hits and lookups
    pub fn calculate_hit_rate(&mut self) {
        if self.cache_lookups > 0 {
            self.cache_hit_rate = self.cache_hits as f32 / self.cache_lookups as f32;
        } else {
            self.cache_hit_rate = 0.0;
        }
    }
}

/// Per-query tiered metrics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TieredQueryMetrics {
    /// Source tier that provided results ("cache" | "hot" | "warm" | "hybrid")
    pub source_tier: String,
    /// Cache lookup latency (ms)
    pub cache_lookup_ms: u64,
    /// Hot tier search latency (ms)
    pub hot_tier_ms: u64,
    /// Warm tier search latency (ms)
    pub warm_tier_ms: u64,
    /// Whether cache was hit
    pub cache_hit: bool,
    /// Whether hot tier returned sufficient results
    pub hot_tier_hit: bool,
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
    /// Tiered search metrics
    pub tiered: TieredMetrics,
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
    /// Tiered metrics counters
    tiered_lookups: AtomicU64,
    tiered_cache_hits: AtomicU64,
    tiered_hot_hits: AtomicU64,
    tiered_promotions: AtomicU64,
    tiered_demotions: AtomicU64,
    /// Recent tiered query metrics for latency averaging
    tiered_latencies: RwLock<Vec<TieredQueryMetrics>>,
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
            tiered_lookups: AtomicU64::new(self.tiered_lookups.load(Ordering::Relaxed)),
            tiered_cache_hits: AtomicU64::new(self.tiered_cache_hits.load(Ordering::Relaxed)),
            tiered_hot_hits: AtomicU64::new(self.tiered_hot_hits.load(Ordering::Relaxed)),
            tiered_promotions: AtomicU64::new(self.tiered_promotions.load(Ordering::Relaxed)),
            tiered_demotions: AtomicU64::new(self.tiered_demotions.load(Ordering::Relaxed)),
            tiered_latencies: RwLock::new(self.tiered_latencies.read().clone()),
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
            tiered_lookups: AtomicU64::new(0),
            tiered_cache_hits: AtomicU64::new(0),
            tiered_hot_hits: AtomicU64::new(0),
            tiered_promotions: AtomicU64::new(0),
            tiered_demotions: AtomicU64::new(0),
            tiered_latencies: RwLock::new(Vec::with_capacity(max_history)),
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

    /// Record a tiered query's metrics
    pub fn record_tiered_query(&self, metrics: TieredQueryMetrics) {
        // Update counters
        self.tiered_lookups.fetch_add(1, Ordering::Relaxed);
        if metrics.cache_hit {
            self.tiered_cache_hits.fetch_add(1, Ordering::Relaxed);
        }
        if metrics.hot_tier_hit {
            self.tiered_hot_hits.fetch_add(1, Ordering::Relaxed);
        }

        // Add to history (circular buffer)
        let mut tiered_latencies = self.tiered_latencies.write();
        if tiered_latencies.len() >= self.max_history {
            tiered_latencies.remove(0);
        }
        tiered_latencies.push(metrics);
    }

    /// Record a chunk promotion to hot tier
    pub fn record_promotion(&self) {
        self.tiered_promotions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a chunk demotion from hot tier
    pub fn record_demotion(&self) {
        self.tiered_demotions.fetch_add(1, Ordering::Relaxed);
    }

    /// Get tiered search statistics
    pub fn get_tiered_stats(&self) -> TieredMetrics {
        let lookups = self.tiered_lookups.load(Ordering::Relaxed);
        let cache_hits = self.tiered_cache_hits.load(Ordering::Relaxed);
        let hot_hits = self.tiered_hot_hits.load(Ordering::Relaxed);

        let tiered_latencies = self.tiered_latencies.read();

        // Calculate average latencies from recent queries
        let (avg_cache_ms, avg_hot_ms, avg_warm_ms) = if tiered_latencies.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            let count = tiered_latencies.len() as f64;
            let sum_cache: u64 = tiered_latencies.iter().map(|m| m.cache_lookup_ms).sum();
            let sum_hot: u64 = tiered_latencies.iter().map(|m| m.hot_tier_ms).sum();
            let sum_warm: u64 = tiered_latencies.iter().map(|m| m.warm_tier_ms).sum();
            (
                sum_cache as f64 / count,
                sum_hot as f64 / count,
                sum_warm as f64 / count,
            )
        };

        // Count warm tier searches (non-cache-hit queries)
        let warm_tier_searches = lookups.saturating_sub(cache_hits);

        let mut metrics = TieredMetrics {
            cache_lookups: lookups,
            cache_hits,
            cache_misses: lookups.saturating_sub(cache_hits),
            cache_hit_rate: 0.0,                   // Calculated below
            hot_tier_searches: warm_tier_searches, // Hot tier searched on non-cache-hit
            hot_tier_hits: hot_hits,
            warm_tier_searches,
            promotions: self.tiered_promotions.load(Ordering::Relaxed),
            demotions: self.tiered_demotions.load(Ordering::Relaxed),
            avg_cache_lookup_ms: avg_cache_ms,
            avg_hot_tier_ms: avg_hot_ms,
            avg_warm_tier_ms: avg_warm_ms,
        };

        metrics.calculate_hit_rate();
        metrics
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
            tiered: self.get_tiered_stats(),
        }
    }
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
    fn test_tiered_metrics_recording() {
        let collector = MetricsCollector::new(100);

        // Record a cache hit
        collector.record_tiered_query(TieredQueryMetrics {
            source_tier: "cache".to_string(),
            cache_lookup_ms: 1,
            hot_tier_ms: 0,
            warm_tier_ms: 0,
            cache_hit: true,
            hot_tier_hit: false,
        });

        // Record a hot tier hit
        collector.record_tiered_query(TieredQueryMetrics {
            source_tier: "hot".to_string(),
            cache_lookup_ms: 1,
            hot_tier_ms: 5,
            warm_tier_ms: 0,
            cache_hit: false,
            hot_tier_hit: true,
        });

        // Record a warm tier query
        collector.record_tiered_query(TieredQueryMetrics {
            source_tier: "warm".to_string(),
            cache_lookup_ms: 1,
            hot_tier_ms: 3,
            warm_tier_ms: 20,
            cache_hit: false,
            hot_tier_hit: false,
        });

        let stats = collector.get_tiered_stats();

        assert_eq!(stats.cache_lookups, 3);
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.hot_tier_hits, 1);
        assert!((stats.cache_hit_rate - 0.333).abs() < 0.01);
    }

    #[test]
    fn test_tiered_promotions_demotions() {
        let collector = MetricsCollector::new(100);

        collector.record_promotion();
        collector.record_promotion();
        collector.record_demotion();

        let stats = collector.get_tiered_stats();

        assert_eq!(stats.promotions, 2);
        assert_eq!(stats.demotions, 1);
    }

    #[test]
    fn test_tiered_latency_averages() {
        let collector = MetricsCollector::new(100);

        for i in 0..5 {
            collector.record_tiered_query(TieredQueryMetrics {
                source_tier: "warm".to_string(),
                cache_lookup_ms: 2,
                hot_tier_ms: 4,
                warm_tier_ms: (i + 1) * 10, // 10, 20, 30, 40, 50
                cache_hit: false,
                hot_tier_hit: false,
            });
        }

        let stats = collector.get_tiered_stats();

        assert!((stats.avg_cache_lookup_ms - 2.0).abs() < 0.01);
        assert!((stats.avg_hot_tier_ms - 4.0).abs() < 0.01);
        assert!((stats.avg_warm_tier_ms - 30.0).abs() < 0.01); // (10+20+30+40+50)/5
    }

    #[test]
    fn test_snapshot_includes_tiered() {
        let collector = MetricsCollector::new(100);

        collector.record_tiered_query(TieredQueryMetrics {
            source_tier: "cache".to_string(),
            cache_lookup_ms: 1,
            hot_tier_ms: 0,
            warm_tier_ms: 0,
            cache_hit: true,
            hot_tier_hit: false,
        });

        collector.record_promotion();

        let snapshot = collector.snapshot(HashMap::new());

        assert_eq!(snapshot.tiered.cache_lookups, 1);
        assert_eq!(snapshot.tiered.cache_hits, 1);
        assert_eq!(snapshot.tiered.promotions, 1);
    }

    #[test]
    fn test_tiered_metrics_calculate_hit_rate() {
        let mut metrics = TieredMetrics {
            cache_lookups: 100,
            cache_hits: 75,
            ..Default::default()
        };

        metrics.calculate_hit_rate();
        assert!((metrics.cache_hit_rate - 0.75).abs() < 0.001);

        // Edge case: no lookups
        let mut empty_metrics = TieredMetrics::default();
        empty_metrics.calculate_hit_rate();
        assert_eq!(empty_metrics.cache_hit_rate, 0.0);
    }
}
