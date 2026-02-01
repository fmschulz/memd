//! Multi-signal access tracking for hot tier promotion
//!
//! Tracks chunk access patterns using frequency, recency, and project context
//! to compute promotion scores for the hot tier.

use std::collections::HashMap;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::types::ChunkId;

/// An access event for a chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessEvent {
    /// The chunk that was accessed
    pub chunk_id: ChunkId,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    /// Optional project context for locality boost
    pub project_id: Option<String>,
    /// Optional query ID for deduplication
    pub query_id: Option<String>,
}

impl AccessEvent {
    /// Create a new access event with current timestamp
    pub fn new(chunk_id: ChunkId) -> Self {
        Self {
            chunk_id,
            timestamp: current_time_ms(),
            project_id: None,
            query_id: None,
        }
    }

    /// Create access event with project context
    pub fn with_project(chunk_id: ChunkId, project_id: String) -> Self {
        Self {
            chunk_id,
            timestamp: current_time_ms(),
            project_id: Some(project_id),
            query_id: None,
        }
    }
}

/// Access statistics for a single chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessStats {
    /// Total number of accesses
    pub access_count: u32,
    /// Unix timestamp of last access (ms)
    pub last_access: i64,
    /// Unix timestamp of first access (ms)
    pub first_access: i64,
    /// Access count per project
    pub project_hits: HashMap<String, u32>,
    /// Last computed decay score (cached)
    decay_score: f32,
}

impl AccessStats {
    fn new(timestamp: i64) -> Self {
        Self {
            access_count: 1,
            last_access: timestamp,
            first_access: timestamp,
            project_hits: HashMap::new(),
            decay_score: 1.0,
        }
    }

    fn record_access(&mut self, timestamp: i64, project_id: Option<&str>) {
        self.access_count = self.access_count.saturating_add(1);
        self.last_access = timestamp;

        if let Some(project) = project_id {
            *self.project_hits.entry(project.to_string()).or_insert(0) += 1;
        }
    }
}

/// Promotion score for a chunk
#[derive(Debug, Clone)]
pub struct PromotionScore {
    /// The chunk being scored
    pub chunk_id: ChunkId,
    /// Overall score (0.0-1.0 normalized)
    pub score: f32,
    /// Frequency component contribution
    pub frequency_component: f32,
    /// Recency component contribution
    pub recency_component: f32,
    /// Project context component contribution
    pub project_component: f32,
    /// Whether chunk is eligible for promotion
    pub eligible: bool,
}

impl PromotionScore {
    fn new(chunk_id: ChunkId) -> Self {
        Self {
            chunk_id,
            score: 0.0,
            frequency_component: 0.0,
            recency_component: 0.0,
            project_component: 0.0,
            eligible: false,
        }
    }
}

/// Configuration for access tracking and promotion scoring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessTrackerConfig {
    /// Weight for frequency signal (default 0.4)
    pub frequency_weight: f32,
    /// Weight for recency signal (default 0.4)
    pub recency_weight: f32,
    /// Weight for project context signal (default 0.2)
    pub project_weight: f32,
    /// Half-life for time decay in hours (default 24)
    pub decay_half_life_hours: u32,
    /// Minimum accesses required for promotion (default 2)
    pub min_accesses_for_promotion: u32,
}

impl Default for AccessTrackerConfig {
    fn default() -> Self {
        Self {
            frequency_weight: 0.4,
            recency_weight: 0.4,
            project_weight: 0.2,
            decay_half_life_hours: 24,
            min_accesses_for_promotion: 2,
        }
    }
}

/// Multi-signal access tracker for hot tier promotion decisions
pub struct AccessTracker {
    /// Per-chunk access statistics
    stats: RwLock<HashMap<ChunkId, AccessStats>>,
    /// Configuration
    config: AccessTrackerConfig,
    /// Maximum observed log2(access_count) for normalization
    max_log_count: RwLock<f32>,
}

impl AccessTracker {
    /// Create a new access tracker with given configuration
    pub fn new(config: AccessTrackerConfig) -> Self {
        Self {
            stats: RwLock::new(HashMap::new()),
            config,
            max_log_count: RwLock::new(1.0),
        }
    }

    /// Record an access event for a chunk
    pub fn record_access(&self, event: AccessEvent) {
        let mut stats = self.stats.write();

        match stats.get_mut(&event.chunk_id) {
            Some(existing) => {
                existing.record_access(event.timestamp, event.project_id.as_deref());

                // Update max log count if needed
                let log_count = (existing.access_count as f32 + 1.0).log2();
                let mut max_log = self.max_log_count.write();
                if log_count > *max_log {
                    *max_log = log_count;
                }
            }
            None => {
                let mut new_stats = AccessStats::new(event.timestamp);
                if let Some(ref project) = event.project_id {
                    new_stats.project_hits.insert(project.clone(), 1);
                }
                stats.insert(event.chunk_id, new_stats);
            }
        }
    }

    /// Get the promotion score for a specific chunk
    pub fn get_promotion_score(
        &self,
        chunk_id: &ChunkId,
        current_project: Option<&str>,
    ) -> PromotionScore {
        let stats = self.stats.read();

        match stats.get(chunk_id) {
            Some(access_stats) => self.compute_score(chunk_id.clone(), access_stats, current_project),
            None => PromotionScore::new(chunk_id.clone()),
        }
    }

    /// Get top promotion candidates sorted by score
    pub fn get_top_candidates(
        &self,
        k: usize,
        current_project: Option<&str>,
    ) -> Vec<PromotionScore> {
        let stats = self.stats.read();

        let mut scores: Vec<PromotionScore> = stats
            .iter()
            .map(|(chunk_id, access_stats)| {
                self.compute_score(chunk_id.clone(), access_stats, current_project)
            })
            .filter(|s| s.eligible)
            .collect();

        // Sort by score descending
        scores.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(k);
        scores
    }

    /// Apply time decay to all entries (call periodically)
    pub fn decay_all(&self) {
        let now = current_time_ms();
        let decay_ms = (self.config.decay_half_life_hours as i64) * 3600 * 1000;

        let mut stats = self.stats.write();
        for access_stats in stats.values_mut() {
            let hours_since_last = (now - access_stats.last_access) as f32 / (3600.0 * 1000.0);
            let half_life = self.config.decay_half_life_hours as f32;
            access_stats.decay_score = (-hours_since_last / half_life).exp();
        }

        // Remove entries with very low decay scores to prevent unbounded growth
        let min_decay = 0.01; // Below 1% contribution
        stats.retain(|_, s| s.decay_score >= min_decay);
    }

    /// Remove a chunk from tracking (e.g., on deletion)
    pub fn remove_chunk(&self, chunk_id: &ChunkId) -> bool {
        self.stats.write().remove(chunk_id).is_some()
    }

    /// Get the number of tracked chunks
    pub fn len(&self) -> usize {
        self.stats.read().len()
    }

    /// Check if tracker is empty
    pub fn is_empty(&self) -> bool {
        self.stats.read().is_empty()
    }

    /// Compute promotion score for a chunk
    fn compute_score(
        &self,
        chunk_id: ChunkId,
        stats: &AccessStats,
        current_project: Option<&str>,
    ) -> PromotionScore {
        let now = current_time_ms();
        let max_log = *self.max_log_count.read();

        // Frequency component: log2(access_count + 1) normalized by max
        let log_count = (stats.access_count as f32 + 1.0).log2();
        let frequency_component = if max_log > 0.0 {
            log_count / max_log
        } else {
            0.0
        };

        // Recency component: exponential decay based on time since last access
        let hours_since_last = (now - stats.last_access) as f32 / (3600.0 * 1000.0);
        let half_life = self.config.decay_half_life_hours as f32;
        let recency_component = (-hours_since_last / half_life).exp();

        // Project component: 1.0 if accessed from current project, 0.0 otherwise
        let project_component = match current_project {
            Some(project) => {
                if stats.project_hits.contains_key(project) {
                    1.0
                } else {
                    0.0
                }
            }
            None => 0.0,
        };

        // Weighted combination
        let score = frequency_component * self.config.frequency_weight
            + recency_component * self.config.recency_weight
            + project_component * self.config.project_weight;

        // Eligibility check
        let eligible = stats.access_count >= self.config.min_accesses_for_promotion;

        PromotionScore {
            chunk_id,
            score,
            frequency_component,
            recency_component,
            project_component,
            eligible,
        }
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

    #[test]
    fn test_record_access() {
        let tracker = AccessTracker::new(AccessTrackerConfig::default());
        let chunk_id = ChunkId::new();

        tracker.record_access(AccessEvent::new(chunk_id.clone()));
        tracker.record_access(AccessEvent::new(chunk_id.clone()));

        let score = tracker.get_promotion_score(&chunk_id, None);
        assert_eq!(score.chunk_id, chunk_id);
        assert!(score.score > 0.0);
        assert!(score.eligible); // 2 accesses meets default threshold
    }

    #[test]
    fn test_project_context() {
        let tracker = AccessTracker::new(AccessTrackerConfig::default());
        let chunk_id = ChunkId::new();

        // Access from project A
        tracker.record_access(AccessEvent::with_project(chunk_id.clone(), "project_a".into()));
        tracker.record_access(AccessEvent::with_project(chunk_id.clone(), "project_a".into()));

        // Score should be higher when querying from project A
        let score_a = tracker.get_promotion_score(&chunk_id, Some("project_a"));
        let score_b = tracker.get_promotion_score(&chunk_id, Some("project_b"));

        assert!(
            score_a.score > score_b.score,
            "Project context should boost score: {} vs {}",
            score_a.score,
            score_b.score
        );
        assert!(score_a.project_component > 0.0);
        assert_eq!(score_b.project_component, 0.0);
    }

    #[test]
    fn test_frequency_normalization() {
        let tracker = AccessTracker::new(AccessTrackerConfig::default());
        let chunk1 = ChunkId::new();
        let chunk2 = ChunkId::new();

        // chunk1: 10 accesses
        for _ in 0..10 {
            tracker.record_access(AccessEvent::new(chunk1.clone()));
        }

        // chunk2: 2 accesses
        tracker.record_access(AccessEvent::new(chunk2.clone()));
        tracker.record_access(AccessEvent::new(chunk2.clone()));

        let score1 = tracker.get_promotion_score(&chunk1, None);
        let score2 = tracker.get_promotion_score(&chunk2, None);

        assert!(
            score1.frequency_component > score2.frequency_component,
            "More accesses should have higher frequency: {} vs {}",
            score1.frequency_component,
            score2.frequency_component
        );
    }

    #[test]
    fn test_get_top_candidates() {
        let config = AccessTrackerConfig {
            min_accesses_for_promotion: 1, // Lower threshold for test
            ..Default::default()
        };
        let tracker = AccessTracker::new(config);

        let chunks: Vec<ChunkId> = (0..5).map(|_| ChunkId::new()).collect();

        // Give different access counts
        for (i, chunk) in chunks.iter().enumerate() {
            for _ in 0..=i {
                tracker.record_access(AccessEvent::new(chunk.clone()));
            }
        }

        let top = tracker.get_top_candidates(3, None);
        assert_eq!(top.len(), 3);

        // Should be sorted by score descending
        assert!(top[0].score >= top[1].score);
        assert!(top[1].score >= top[2].score);
    }

    #[test]
    fn test_eligibility() {
        let config = AccessTrackerConfig {
            min_accesses_for_promotion: 3,
            ..Default::default()
        };
        let tracker = AccessTracker::new(config);
        let chunk_id = ChunkId::new();

        // Only 2 accesses - not eligible
        tracker.record_access(AccessEvent::new(chunk_id.clone()));
        tracker.record_access(AccessEvent::new(chunk_id.clone()));

        let score = tracker.get_promotion_score(&chunk_id, None);
        assert!(!score.eligible);

        // 3rd access - now eligible
        tracker.record_access(AccessEvent::new(chunk_id.clone()));
        let score = tracker.get_promotion_score(&chunk_id, None);
        assert!(score.eligible);
    }

    #[test]
    fn test_remove_chunk() {
        let tracker = AccessTracker::new(AccessTrackerConfig::default());
        let chunk_id = ChunkId::new();

        tracker.record_access(AccessEvent::new(chunk_id.clone()));
        assert_eq!(tracker.len(), 1);

        let removed = tracker.remove_chunk(&chunk_id);
        assert!(removed);
        assert_eq!(tracker.len(), 0);

        // Removing again should return false
        let removed_again = tracker.remove_chunk(&chunk_id);
        assert!(!removed_again);
    }

    #[test]
    fn test_config_weights() {
        let config = AccessTrackerConfig {
            frequency_weight: 0.5,
            recency_weight: 0.3,
            project_weight: 0.2,
            ..Default::default()
        };

        assert!((config.frequency_weight + config.recency_weight + config.project_weight - 1.0).abs() < 0.001);
    }
}
