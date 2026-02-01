//! Compaction module for memd
//!
//! Provides infrastructure for monitoring compaction health and managing
//! the compaction lifecycle. Includes metrics gathering, tombstone auditing,
//! HNSW rebuild, and segment merge operations.

pub mod hnsw_rebuild;
pub mod metrics;
pub mod throttle;
pub mod tombstone_audit;

pub use hnsw_rebuild::{HnswRebuilder, RebuildResult};
pub use metrics::CompactionMetrics;
pub use throttle::{Throttle, ThrottleConfig};
pub use tombstone_audit::{AuditResult, TombstoneAudit};

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
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            thresholds: CompactionThresholds::default(),
            batch_delay_ms: 10,
            batch_size: 100,
            enabled: true,
        }
    }
}

/// Manager for compaction operations
///
/// Coordinates compaction decisions based on metrics and thresholds.
/// Currently a skeleton - full implementation in later plans.
pub struct CompactionManager {
    config: CompactionConfig,
}

impl CompactionManager {
    /// Create a new CompactionManager with the given configuration
    pub fn new(config: CompactionConfig) -> Self {
        Self { config }
    }

    /// Check if any threshold is exceeded
    ///
    /// Returns true if ANY of the following conditions are met:
    /// - Tombstone ratio exceeds configured threshold
    /// - Segment count exceeds configured maximum
    /// - HNSW staleness exceeds configured threshold
    pub fn check_thresholds(&self, metrics: &CompactionMetrics) -> bool {
        metrics.exceeds_tombstone_threshold(self.config.thresholds.tombstone_ratio_pct)
            || metrics.exceeds_segment_threshold(self.config.thresholds.max_segment_count)
            || metrics.exceeds_hnsw_staleness_threshold(self.config.thresholds.hnsw_staleness_pct)
    }

    /// Get the current configuration
    pub fn config(&self) -> &CompactionConfig {
        &self.config
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

    #[test]
    fn manager_creation() {
        let config = CompactionConfig::default();
        let manager = CompactionManager::new(config);
        assert!(manager.config().enabled);
    }

    #[test]
    fn check_thresholds_below() {
        let manager = CompactionManager::new(CompactionConfig::default());
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.10,     // Below 0.20 threshold
            segment_count: 5,          // Below 10 threshold
            hnsw_staleness: 0.05,      // Below 0.15 threshold
            ..Default::default()
        };
        assert!(!manager.check_thresholds(&metrics));
    }

    #[test]
    fn check_thresholds_tombstone_exceeded() {
        let manager = CompactionManager::new(CompactionConfig::default());
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.25,     // Above 0.20 threshold
            segment_count: 5,
            hnsw_staleness: 0.05,
            ..Default::default()
        };
        assert!(manager.check_thresholds(&metrics));
    }

    #[test]
    fn check_thresholds_segment_exceeded() {
        let manager = CompactionManager::new(CompactionConfig::default());
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.10,
            segment_count: 15,         // Above 10 threshold
            hnsw_staleness: 0.05,
            ..Default::default()
        };
        assert!(manager.check_thresholds(&metrics));
    }

    #[test]
    fn check_thresholds_hnsw_exceeded() {
        let manager = CompactionManager::new(CompactionConfig::default());
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.10,
            segment_count: 5,
            hnsw_staleness: 0.20,      // Above 0.15 threshold
            ..Default::default()
        };
        assert!(manager.check_thresholds(&metrics));
    }
}
