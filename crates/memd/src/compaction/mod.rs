//! Compaction module for memd
//!
//! Provides infrastructure for monitoring compaction health and managing
//! the compaction lifecycle. Includes metrics gathering, HNSW rebuild, segment
//! merge, throttling, and compaction runner.

pub mod expiry_sweep;
pub mod history_promotion;
pub mod hnsw_rebuild;
pub mod metrics;
pub mod runner;
pub mod segment_merge;
pub mod throttle;

pub use expiry_sweep::{ExpirySweep, SweepResult};
pub use history_promotion::{HistoryPromotion, PromotionResult};
pub use hnsw_rebuild::{HnswRebuilder, RebuildResult};
pub use metrics::CompactionMetrics;
pub use runner::{CompactionResult, CompactionRunner};
pub use segment_merge::{MergeResult, SegmentMerger};
pub use throttle::{Throttle, ThrottleConfig};

/// Thresholds that trigger compaction
///
/// When any threshold is exceeded, compaction should be considered.
#[derive(Debug, Clone)]
pub struct CompactionThresholds {
    /// Tombstone ratio threshold (0.0 to 1.0, default 0.20 = 20%)
    pub tombstone_ratio_pct: f32,
    /// Maximum segment count before compaction (default 10)
    pub max_segment_count: usize,
    /// HNSW staleness threshold (0.0 to 1.0, default 0.15 = 15%)
    pub hnsw_staleness_pct: f32,
}

impl Default for CompactionThresholds {
    fn default() -> Self {
        Self {
            tombstone_ratio_pct: 0.20,
            max_segment_count: 10,
            hnsw_staleness_pct: 0.15,
        }
    }
}

/// Configuration for the compaction manager
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Thresholds that trigger compaction
    pub thresholds: CompactionThresholds,
    /// Delay between batch operations in milliseconds (default 10)
    pub batch_delay_ms: u64,
    /// Number of chunks to process per batch (default 100)
    pub batch_size: usize,
    /// Whether compaction is enabled (default true)
    pub enabled: bool,
    /// Whether `ExpirySweep` runs as part of each compaction cycle
    /// (Track C3/C5). Default `true`. Operators can disable the sweep
    /// temporarily without unwinding the rest of compaction.
    pub expiry_sweep_enabled: bool,
    /// Whether `HistoryPromotion` runs as part of each compaction
    /// cycle (Track C4/C5). Default `true`.
    pub history_promotion_enabled: bool,
    /// Minimum overlay-idle window before `HistoryPromotion` demotes
    /// a stale Superseded/Expired row to the History tier. Default
    /// 90 days in milliseconds.
    pub history_promotion_age_ms: i64,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            thresholds: CompactionThresholds::default(),
            batch_delay_ms: 10,
            batch_size: 100,
            enabled: true,
            expiry_sweep_enabled: true,
            history_promotion_enabled: true,
            history_promotion_age_ms: 90 * 86_400_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_thresholds() {
        let thresholds = CompactionThresholds::default();
        assert!((thresholds.tombstone_ratio_pct - 0.20).abs() < 0.001);
        assert_eq!(thresholds.max_segment_count, 10);
        assert!((thresholds.hnsw_staleness_pct - 0.15).abs() < 0.001);
    }

    #[test]
    fn default_config() {
        let config = CompactionConfig::default();
        assert_eq!(config.batch_delay_ms, 10);
        assert_eq!(config.batch_size, 100);
        assert!(config.enabled);
    }
}
