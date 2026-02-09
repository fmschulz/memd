//! Tiered storage for hot tier and caching
//!
//! Provides access tracking, promotion scoring, and hot tier management
//! for frequently accessed chunks, plus semantic query caching.
//!
//! The TieredSearcher coordinates the cache -> hot -> warm fallback chain
//! and manages automatic promotion/demotion of chunks between tiers.

pub mod access_tracker;
pub mod hot_tier;
pub mod semantic_cache;
pub mod tiered_searcher;

pub use access_tracker::{
    AccessEvent, AccessStats, AccessTracker, AccessTrackerConfig, PromotionScore,
};
pub use hot_tier::{HotTier, HotTierConfig, HotTierStats};
pub use semantic_cache::{
    CacheEntry, CacheHit, CacheStats, CachedResult, SemanticCache, SemanticCacheConfig,
};
pub use tiered_searcher::{
    MaintenanceResult, ScoredChunk, SourceTier, TierAction, TierDecision, TieredSearchResult,
    TieredSearcher, TieredSearcherConfig, TieredTiming, WarmTierSearch,
};
