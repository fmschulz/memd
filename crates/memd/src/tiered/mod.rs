//! Tiered storage for hot tier and caching
//!
//! Provides access tracking, promotion scoring, and hot tier management
//! for frequently accessed chunks, plus semantic query caching.

pub mod access_tracker;
pub mod hot_tier;
pub mod semantic_cache;

pub use access_tracker::{AccessEvent, AccessStats, AccessTracker, AccessTrackerConfig, PromotionScore};
pub use hot_tier::{HotTier, HotTierConfig, HotTierStats};
pub use semantic_cache::{
    CacheEntry, CacheHit, CacheStats, CachedResult, SemanticCache, SemanticCacheConfig,
};
