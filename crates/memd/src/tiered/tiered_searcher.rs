//! Tiered search coordinator with cache/hot/warm fallback
//!
//! TieredSearcher routes queries through the tiered architecture:
//! semantic cache (fastest) -> hot tier (fast) -> warm tier (standard).
//! It automatically promotes frequently accessed chunks and demotes stale ones.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;

use crate::error::Result;
use crate::index::SearchResult;
use crate::types::{ChunkId, TenantId};

use super::access_tracker::{AccessEvent, AccessTracker};
use super::hot_tier::HotTier;
use super::semantic_cache::{CachedResult, SemanticCache};

/// Configuration for tiered search
#[derive(Debug, Clone)]
pub struct TieredSearcherConfig {
    /// Enable semantic cache (default true)
    pub enable_cache: bool,
    /// Enable hot tier (default true)
    pub enable_hot_tier: bool,
    /// Minimum score for promotion to hot tier (default 0.4)
    pub promotion_threshold: f32,
    /// Number of queries without access before demotion (default 100)
    pub demotion_queries_threshold: u32,
    /// Auto-promote chunks from matching project (default true)
    pub auto_promote_on_project_match: bool,
    /// Enable debug output for tier decisions (default false)
    pub debug_tier_decisions: bool,
}

impl Default for TieredSearcherConfig {
    fn default() -> Self {
        Self {
            enable_cache: true,
            enable_hot_tier: true,
            promotion_threshold: 0.4,
            demotion_queries_threshold: 100,
            auto_promote_on_project_match: true,
            debug_tier_decisions: false,
        }
    }
}

/// Source tier for a search result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTier {
    /// Result came from semantic cache
    Cache,
    /// Result came from hot tier
    Hot,
    /// Result came from warm tier
    Warm,
}

/// Action for a tier decision
#[derive(Debug, Clone)]
pub enum TierAction {
    /// Promote chunk to hot tier
    Promote {
        /// Source tier being promoted from
        from: SourceTier,
        /// Promotion score
        score: f32,
    },
    /// Demote chunk from hot tier
    Demote {
        /// Reason for demotion
        reason: String,
    },
    /// No action needed
    None,
}

/// A decision about tier placement for a chunk
#[derive(Debug, Clone)]
pub struct TierDecision {
    /// The chunk being evaluated
    pub chunk_id: ChunkId,
    /// The action to take
    pub action: TierAction,
    /// Human-readable reason
    pub reason: String,
    /// Promotion/access score if applicable
    pub score: Option<f32>,
    /// Which tier the chunk is currently in
    pub source_tier: SourceTier,
}

/// A chunk with score and source tier info
#[derive(Debug, Clone)]
pub struct ScoredChunk {
    /// The chunk ID
    pub chunk_id: ChunkId,
    /// Similarity/relevance score
    pub score: f32,
    /// Which tier returned this result
    pub source_tier: SourceTier,
}

/// Timing breakdown for tiered search
#[derive(Debug, Clone, Default)]
pub struct TieredTiming {
    /// Time spent on cache lookup (ms)
    pub cache_lookup_ms: u64,
    /// Time spent on hot tier search (ms)
    pub hot_tier_ms: u64,
    /// Time spent on warm tier search (ms)
    pub warm_tier_ms: u64,
    /// Time spent on promotion checks (ms)
    pub promotion_check_ms: u64,
    /// Total search time (ms)
    pub total_ms: u64,
}

/// Result of a tiered search
#[derive(Debug, Clone)]
pub struct TieredSearchResult {
    /// Search results with scores
    pub results: Vec<ScoredChunk>,
    /// Primary source tier (first non-empty result source)
    pub source_tier: SourceTier,
    /// Whether cache was hit
    pub cache_hit: bool,
    /// Whether hot tier returned results
    pub hot_tier_hit: bool,
    /// Timing breakdown
    pub timing: TieredTiming,
    /// Tier decisions (only if debug enabled)
    pub tier_decisions: Vec<TierDecision>,
}

/// Trait for warm tier search operations
///
/// Abstracts the warm tier (main index) to allow different implementations.
pub trait WarmTierSearch: Send + Sync {
    /// Search the warm tier
    fn search(&self, query_embedding: &[f32], k: usize) -> Result<Vec<SearchResult>>;

    /// Get the embedding for a chunk (for promotion to hot tier)
    fn get_embedding(&self, chunk_id: &ChunkId) -> Option<Vec<f32>>;

    /// Get current version for cache invalidation
    fn get_version(&self) -> u64;

    /// Get the total number of indexed chunks
    fn len(&self) -> usize;

    /// Check if warm tier is empty
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Tiered search coordinator
///
/// Coordinates cache/hot/warm fallback chain and manages promotion/demotion.
pub struct TieredSearcher<W: WarmTierSearch> {
    /// Semantic query cache
    cache: Arc<SemanticCache>,
    /// Hot tier with promoted chunks
    hot_tier: Arc<RwLock<HotTier>>,
    /// Access tracker for promotion scoring
    access_tracker: Arc<RwLock<AccessTracker>>,
    /// Warm tier (main index)
    warm_tier: Arc<W>,
    /// Configuration
    config: TieredSearcherConfig,
    /// Query counter for demotion checks
    query_counter: AtomicU64,
    /// Last demotion check timestamp (Unix ms)
    last_demotion_check: AtomicU64,
}

impl<W: WarmTierSearch> TieredSearcher<W> {
    /// Create a new tiered searcher
    pub fn new(
        cache: Arc<SemanticCache>,
        hot_tier: Arc<RwLock<HotTier>>,
        access_tracker: Arc<RwLock<AccessTracker>>,
        warm_tier: Arc<W>,
        config: TieredSearcherConfig,
    ) -> Self {
        Self {
            cache,
            hot_tier,
            access_tracker,
            warm_tier,
            config,
            query_counter: AtomicU64::new(0),
            last_demotion_check: AtomicU64::new(current_time_ms() as u64),
        }
    }

    /// Get the current configuration
    pub fn config(&self) -> &TieredSearcherConfig {
        &self.config
    }

    /// Get the query counter value
    pub fn query_count(&self) -> u64 {
        self.query_counter.load(Ordering::Relaxed)
    }
}

/// Get current time in milliseconds
fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiered::{AccessTrackerConfig, HotTierConfig, SemanticCacheConfig};

    /// Mock warm tier for testing
    struct MockWarmTier {
        results: RwLock<Vec<SearchResult>>,
        embeddings: RwLock<std::collections::HashMap<ChunkId, Vec<f32>>>,
        version: AtomicU64,
    }

    impl MockWarmTier {
        fn new() -> Self {
            Self {
                results: RwLock::new(Vec::new()),
                embeddings: RwLock::new(std::collections::HashMap::new()),
                version: AtomicU64::new(1),
            }
        }

        fn add_chunk(&self, chunk_id: ChunkId, embedding: Vec<f32>, score: f32) {
            self.results.write().push(SearchResult {
                chunk_id: chunk_id.clone(),
                score,
            });
            self.embeddings.write().insert(chunk_id, embedding);
        }
    }

    impl WarmTierSearch for MockWarmTier {
        fn search(&self, _query_embedding: &[f32], k: usize) -> Result<Vec<SearchResult>> {
            let results = self.results.read();
            Ok(results.iter().take(k).cloned().collect())
        }

        fn get_embedding(&self, chunk_id: &ChunkId) -> Option<Vec<f32>> {
            self.embeddings.read().get(chunk_id).cloned()
        }

        fn get_version(&self) -> u64 {
            self.version.load(Ordering::Relaxed)
        }

        fn len(&self) -> usize {
            self.results.read().len()
        }
    }

    fn make_test_searcher() -> TieredSearcher<MockWarmTier> {
        let cache = Arc::new(SemanticCache::new(SemanticCacheConfig::default()));
        let hot_tier = Arc::new(RwLock::new(HotTier::new(HotTierConfig::default())));
        let access_tracker = Arc::new(RwLock::new(AccessTracker::new(
            AccessTrackerConfig::default(),
        )));
        let warm_tier = Arc::new(MockWarmTier::new());

        TieredSearcher::new(
            cache,
            hot_tier,
            access_tracker,
            warm_tier,
            TieredSearcherConfig::default(),
        )
    }

    #[test]
    fn test_config_defaults() {
        let config = TieredSearcherConfig::default();
        assert!(config.enable_cache);
        assert!(config.enable_hot_tier);
        assert!((config.promotion_threshold - 0.4).abs() < 0.01);
        assert_eq!(config.demotion_queries_threshold, 100);
        assert!(config.auto_promote_on_project_match);
        assert!(!config.debug_tier_decisions);
    }

    #[test]
    fn test_tiered_searcher_creation() {
        let searcher = make_test_searcher();
        assert_eq!(searcher.query_count(), 0);
        assert!(searcher.config().enable_cache);
    }

    #[test]
    fn test_source_tier_equality() {
        assert_eq!(SourceTier::Cache, SourceTier::Cache);
        assert_ne!(SourceTier::Cache, SourceTier::Hot);
        assert_ne!(SourceTier::Hot, SourceTier::Warm);
    }

    #[test]
    fn test_mock_warm_tier() {
        let warm = MockWarmTier::new();
        let chunk_id = ChunkId::new();
        let embedding = vec![1.0, 0.0, 0.0, 0.0];

        warm.add_chunk(chunk_id.clone(), embedding.clone(), 0.95);

        assert_eq!(warm.len(), 1);
        assert!(warm.get_embedding(&chunk_id).is_some());

        let results = warm.search(&embedding, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, chunk_id);
    }
}
