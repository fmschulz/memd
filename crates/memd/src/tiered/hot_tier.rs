//! Hot tier with separate HNSW index for promoted chunks
//!
//! Placeholder - implementation in Task 3.

use serde::{Deserialize, Serialize};

/// Configuration for hot tier
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotTierConfig {
    /// Placeholder
    pub capacity_percentage: f32,
}

impl Default for HotTierConfig {
    fn default() -> Self {
        Self {
            capacity_percentage: 0.10,
        }
    }
}

/// Statistics for hot tier
#[derive(Debug, Clone)]
pub struct HotTierStats {
    /// Placeholder
    pub chunk_count: usize,
}

/// Hot tier with separate HNSW index
pub struct HotTier {
    _config: HotTierConfig,
}

impl HotTier {
    /// Placeholder constructor
    pub fn new(config: HotTierConfig) -> Self {
        Self { _config: config }
    }
}
