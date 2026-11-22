//! Throttle module for rate-limiting compaction operations
//!
//! Provides configurable throttling to ensure compaction doesn't cause
//! query latency spikes.

use std::time::Duration;

/// Configuration for throttling behavior
#[derive(Debug, Clone)]
pub struct ThrottleConfig {
    /// Delay between batch operations in milliseconds (default 10)
    pub batch_delay_ms: u64,
    /// Number of items to process per batch (default 100)
    pub batch_size: usize,
    /// Whether throttling is enabled (default true)
    pub enabled: bool,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self {
            batch_delay_ms: 10,
            batch_size: 100,
            enabled: true,
        }
    }
}

/// Throttle for rate-limiting compaction work
///
/// Provides a delay method to prevent compaction from monopolizing I/O.
#[derive(Debug, Clone, Default)]
pub struct Throttle {
    config: ThrottleConfig,
}

impl Throttle {
    /// Create a new Throttle with the given configuration
    pub fn new(config: ThrottleConfig) -> Self {
        Self { config }
    }

    /// Synchronous delay between operations
    ///
    /// If throttling is disabled, returns immediately.
    pub fn delay_sync(&self) {
        if !self.config.enabled {
            return;
        }
        std::thread::sleep(Duration::from_millis(self.config.batch_delay_ms));
    }

    /// Get the configured batch size
    pub fn batch_size(&self) -> usize {
        self.config.batch_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_throttle_disabled() {
        let config = ThrottleConfig {
            enabled: false,
            batch_delay_ms: 1000, // Would be slow if actually used
            batch_size: 10,
        };
        let throttle = Throttle::new(config);

        // Should return immediately even with large delay
        let start = std::time::Instant::now();
        throttle.delay_sync();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() < 10,
            "Disabled throttle should not delay"
        );
    }

    #[test]
    fn test_throttle_enabled() {
        let config = ThrottleConfig {
            enabled: true,
            batch_delay_ms: 50,
            batch_size: 10,
        };
        let throttle = Throttle::new(config);

        let start = std::time::Instant::now();
        throttle.delay_sync();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() >= 50,
            "Enabled throttle should delay at least batch_delay_ms"
        );
    }

    #[test]
    fn test_getters() {
        let config = ThrottleConfig {
            enabled: true,
            batch_delay_ms: 25,
            batch_size: 50,
        };
        let throttle = Throttle::new(config);

        assert_eq!(throttle.batch_size(), 50);
    }

    #[test]
    fn test_default_config() {
        let config = ThrottleConfig::default();
        assert_eq!(config.batch_delay_ms, 10);
        assert_eq!(config.batch_size, 100);
        assert!(config.enabled);
    }

    #[test]
    fn test_default_throttle() {
        let throttle = Throttle::default();
        assert_eq!(throttle.batch_size(), 100);
    }
}
