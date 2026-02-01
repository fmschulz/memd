//! Throttle module for rate-limiting compaction operations
//!
//! Provides configurable throttling to ensure compaction doesn't cause
//! query latency spikes. Supports both sync and async delays.

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
/// Provides delay methods and batched processing helpers to prevent
/// compaction from monopolizing I/O.
#[derive(Debug, Clone)]
pub struct Throttle {
    config: ThrottleConfig,
}

impl Default for Throttle {
    fn default() -> Self {
        Self {
            config: ThrottleConfig::default(),
        }
    }
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

    /// Asynchronous delay between operations
    ///
    /// If throttling is disabled, returns immediately.
    pub async fn delay_async(&self) {
        if !self.config.enabled {
            return;
        }
        tokio::time::sleep(Duration::from_millis(self.config.batch_delay_ms)).await;
    }

    /// Process items in batches with delays between each batch
    ///
    /// Processes items in chunks of `batch_size`, inserting a delay
    /// between each chunk (not before the first).
    pub fn process_batched<T, F, R>(&self, items: Vec<T>, f: F) -> Vec<R>
    where
        F: Fn(&[T]) -> Vec<R>,
    {
        let mut results = Vec::new();
        let mut first = true;

        for chunk in items.chunks(self.config.batch_size) {
            if first {
                first = false;
            } else {
                self.delay_sync();
            }
            results.extend(f(chunk));
        }

        results
    }

    /// Process items in batches with async delays between each batch
    ///
    /// Same as `process_batched` but uses async delays. Takes ownership
    /// of sub-vecs to avoid lifetime issues with async closures.
    pub async fn process_batched_async<T, F, Fut, R>(&self, items: Vec<T>, f: F) -> Vec<R>
    where
        T: Clone,
        F: Fn(Vec<T>) -> Fut,
        Fut: std::future::Future<Output = Vec<R>>,
    {
        let mut results = Vec::new();
        let mut first = true;

        for chunk in items.chunks(self.config.batch_size) {
            if first {
                first = false;
            } else {
                self.delay_async().await;
            }
            results.extend(f(chunk.to_vec()).await);
        }

        results
    }

    /// Get the configured batch size
    pub fn batch_size(&self) -> usize {
        self.config.batch_size
    }

    /// Check if throttling is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
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

        assert!(elapsed.as_millis() < 10, "Disabled throttle should not delay");
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
    fn test_process_batched() {
        let config = ThrottleConfig {
            enabled: true,
            batch_delay_ms: 1, // Minimal delay for test speed
            batch_size: 3,
        };
        let throttle = Throttle::new(config);

        let items: Vec<i32> = (0..10).collect();
        let results = throttle.process_batched(items, |batch| {
            batch.iter().map(|x| x * 2).collect()
        });

        assert_eq!(results, vec![0, 2, 4, 6, 8, 10, 12, 14, 16, 18]);
    }

    #[test]
    fn test_process_batched_empty() {
        let throttle = Throttle::default();
        let items: Vec<i32> = vec![];
        let results = throttle.process_batched(items, |batch| {
            batch.iter().map(|x| x * 2).collect()
        });

        assert!(results.is_empty());
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
        assert!(throttle.is_enabled());
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
        assert!(throttle.is_enabled());
    }
}
