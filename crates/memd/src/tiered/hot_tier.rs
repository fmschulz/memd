//! Hot tier with separate HNSW index for promoted chunks
//!
//! Maintains a smaller, faster HNSW index for frequently accessed chunks.
//! Provides lower latency for hot data at the cost of additional memory.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::index::{HnswConfig, HnswIndex, SearchResult};
use crate::types::{ChunkId, TenantId};

use super::access_tracker::AccessTracker;

/// Configuration for hot tier
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotTierConfig {
    /// Hot tier capacity as percentage of total indexed chunks (default 0.10 = 10%)
    pub capacity_percentage: f32,
    /// Hard cap on hot tier size (default 50_000)
    pub max_chunks: usize,
    /// Minimum promotion score to be eligible (default 0.3)
    pub min_promotion_score: f32,
    /// How often to check for eviction (default 60s)
    pub eviction_check_interval: Duration,
    /// HNSW configuration for hot tier (uses smaller ef_search for speed)
    pub hnsw_config: HnswConfig,
}

impl Default for HotTierConfig {
    fn default() -> Self {
        Self {
            capacity_percentage: 0.10,
            max_chunks: 50_000,
            min_promotion_score: 0.3,
            eviction_check_interval: Duration::from_secs(60),
            hnsw_config: HnswConfig {
                max_connections: 16,
                ef_construction: 200,
                ef_search: 30, // Lower than warm tier (50) for faster queries
                max_elements: 50_000,
                dimension: 384,
            },
        }
    }
}

/// Entry for a chunk in the hot tier
#[derive(Debug, Clone)]
pub struct HotChunkEntry {
    /// The chunk ID
    pub chunk_id: ChunkId,
    /// Cached embedding vector
    pub embedding: Vec<f32>,
    /// When this chunk was promoted (Unix ms)
    pub promoted_at: i64,
    /// Promotion score at time of promotion
    pub promotion_score: f32,
    /// Tenant this chunk belongs to
    pub tenant_id: TenantId,
}

/// Statistics for hot tier
#[derive(Debug, Clone)]
pub struct HotTierStats {
    /// Number of chunks in hot tier
    pub chunk_count: usize,
    /// Capacity used as percentage
    pub capacity_used: f32,
    /// Current version (for cache invalidation)
    pub version: u64,
    /// Average promotion score of entries
    pub avg_promotion_score: f32,
}

/// Hot tier with separate HNSW index for promoted chunks
pub struct HotTier {
    /// The HNSW index for hot tier
    index: HnswIndex,
    /// Chunk entries with metadata
    chunks: RwLock<HashMap<ChunkId, HotChunkEntry>>,
    /// Configuration
    config: HotTierConfig,
    /// Version counter for cache invalidation
    version: AtomicU64,
    /// Access tracker for automatic tracking
    access_tracker: Option<Arc<RwLock<AccessTracker>>>,
}

impl HotTier {
    /// Create a new hot tier with the given configuration
    pub fn new(config: HotTierConfig) -> Self {
        let index = HnswIndex::new(config.hnsw_config.clone());

        Self {
            index,
            chunks: RwLock::new(HashMap::new()),
            config,
            version: AtomicU64::new(0),
            access_tracker: None,
        }
    }

    /// Create hot tier with an access tracker for automatic access recording
    pub fn with_access_tracker(config: HotTierConfig, tracker: Arc<RwLock<AccessTracker>>) -> Self {
        let mut tier = Self::new(config);
        tier.access_tracker = Some(tracker);
        tier
    }

    /// Promote a chunk to the hot tier
    ///
    /// Returns true if the chunk was added, false if already present
    pub fn promote(
        &self,
        chunk_id: ChunkId,
        embedding: Vec<f32>,
        tenant_id: TenantId,
        promotion_score: f32,
    ) -> Result<bool> {
        let mut chunks = self.chunks.write();

        // Check if already in hot tier
        if chunks.contains_key(&chunk_id) {
            return Ok(false);
        }

        // Check capacity
        let current_len = chunks.len();
        if current_len >= self.config.max_chunks {
            // Need to evict first
            return Ok(false);
        }

        // Insert into HNSW index
        self.index.insert(&chunk_id, &embedding)?;

        // Store entry
        let entry = HotChunkEntry {
            chunk_id: chunk_id.clone(),
            embedding,
            promoted_at: current_time_ms(),
            promotion_score,
            tenant_id,
        };
        chunks.insert(chunk_id, entry);

        // Increment version for cache invalidation
        self.version.fetch_add(1, Ordering::SeqCst);

        Ok(true)
    }

    /// Demote a chunk from the hot tier
    ///
    /// Returns true if the chunk was removed, false if not present
    pub fn demote(&self, chunk_id: &ChunkId) -> bool {
        let mut chunks = self.chunks.write();

        if chunks.remove(chunk_id).is_some() {
            // Note: HNSW doesn't support removal, so we just remove from our tracking
            // The entry will be orphaned in the index but won't match any chunk_id
            self.version.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// Check if a chunk is in the hot tier
    pub fn contains(&self, chunk_id: &ChunkId) -> bool {
        self.chunks.read().contains_key(chunk_id)
    }

    /// Search the hot tier
    ///
    /// Records access for any returned chunks if access tracker is configured.
    pub fn search(&self, query_embedding: &[f32], k: usize) -> Result<Vec<SearchResult>> {
        let results = self.index.search(query_embedding, k)?;

        // Filter to only chunks still in our tracking (demoted entries won't match)
        let chunks = self.chunks.read();
        let filtered: Vec<SearchResult> = results
            .into_iter()
            .filter(|r| chunks.contains_key(&r.chunk_id))
            .collect();

        // Record access if tracker configured
        if let Some(ref tracker) = self.access_tracker {
            use super::access_tracker::AccessEvent;
            let tracker = tracker.write();
            for result in &filtered {
                tracker.record_access(AccessEvent::new(result.chunk_id.clone()));
            }
        }

        Ok(filtered)
    }

    /// Search with timing information
    pub fn search_with_timing(
        &self,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<(Vec<SearchResult>, Duration)> {
        let start = Instant::now();
        let results = self.search(query_embedding, k)?;
        let elapsed = start.elapsed();
        Ok((results, elapsed))
    }

    /// Get the number of chunks in the hot tier
    pub fn len(&self) -> usize {
        self.chunks.read().len()
    }

    /// Check if hot tier is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get current version (for cache invalidation)
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Evict low-score chunks if over capacity
    ///
    /// Call periodically or when capacity is reached.
    /// Returns number of chunks evicted.
    pub fn evict_if_needed(&self, total_indexed: usize) -> usize {
        let capacity = self.calculate_capacity(total_indexed);
        let current = self.len();

        if current <= capacity {
            return 0;
        }

        let to_evict = current - capacity;
        self.evict_lowest_score(to_evict)
    }

    /// Evict the N lowest-scoring chunks
    fn evict_lowest_score(&self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }

        let mut chunks = self.chunks.write();

        // Collect and sort by score ascending (lowest first)
        let mut entries: Vec<_> = chunks
            .iter()
            .map(|(id, entry)| (id.clone(), entry.promotion_score))
            .collect();

        entries.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Remove lowest N
        let mut evicted = 0;
        for (chunk_id, _) in entries.into_iter().take(n) {
            if chunks.remove(&chunk_id).is_some() {
                evicted += 1;
            }
        }

        if evicted > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }

        evicted
    }

    /// Calculate capacity based on total indexed chunks
    fn calculate_capacity(&self, total_indexed: usize) -> usize {
        let percentage_capacity = (total_indexed as f32 * self.config.capacity_percentage) as usize;
        percentage_capacity.min(self.config.max_chunks)
    }

    /// Get statistics for the hot tier
    pub fn get_stats(&self) -> HotTierStats {
        let chunks = self.chunks.read();
        let count = chunks.len();

        let avg_score = if count > 0 {
            chunks.values().map(|e| e.promotion_score).sum::<f32>() / count as f32
        } else {
            0.0
        };

        let capacity_used = if self.config.max_chunks > 0 {
            count as f32 / self.config.max_chunks as f32
        } else {
            0.0
        };

        HotTierStats {
            chunk_count: count,
            capacity_used,
            version: self.version(),
            avg_promotion_score: avg_score,
        }
    }

    /// Get the minimum promotion score threshold
    pub fn min_promotion_score(&self) -> f32 {
        self.config.min_promotion_score
    }

    /// Get configuration
    pub fn config(&self) -> &HotTierConfig {
        &self.config
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

    fn normalize(v: &mut [f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
    }

    fn make_config() -> HotTierConfig {
        HotTierConfig {
            capacity_percentage: 0.5,
            max_chunks: 100,
            min_promotion_score: 0.3,
            eviction_check_interval: Duration::from_secs(60),
            hnsw_config: HnswConfig {
                max_connections: 16,
                ef_construction: 200,
                ef_search: 30,
                max_elements: 100,
                dimension: 4,
            },
        }
    }

    #[test]
    fn test_promote_and_search() {
        let config = make_config();
        let tier = HotTier::new(config);

        let chunk_id = ChunkId::new();
        let tenant_id = TenantId::new("test").unwrap();
        let mut embedding = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut embedding);

        // Promote chunk
        let added = tier.promote(chunk_id.clone(), embedding.clone(), tenant_id, 0.5).unwrap();
        assert!(added);
        assert_eq!(tier.len(), 1);
        assert!(tier.contains(&chunk_id));

        // Search should find it
        let results = tier.search(&embedding, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, chunk_id);
        assert!(results[0].score > 0.99); // Near-exact match
    }

    #[test]
    fn test_promote_duplicate() {
        let config = make_config();
        let tier = HotTier::new(config);

        let chunk_id = ChunkId::new();
        let tenant_id = TenantId::new("test").unwrap();
        let mut embedding = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut embedding);

        // First promote succeeds
        let added1 = tier.promote(chunk_id.clone(), embedding.clone(), tenant_id.clone(), 0.5).unwrap();
        assert!(added1);

        // Second promote returns false (already present)
        let added2 = tier.promote(chunk_id.clone(), embedding.clone(), tenant_id, 0.5).unwrap();
        assert!(!added2);
        assert_eq!(tier.len(), 1);
    }

    #[test]
    fn test_demote() {
        let config = make_config();
        let tier = HotTier::new(config);

        let chunk_id = ChunkId::new();
        let tenant_id = TenantId::new("test").unwrap();
        let mut embedding = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut embedding);

        tier.promote(chunk_id.clone(), embedding.clone(), tenant_id, 0.5).unwrap();
        assert_eq!(tier.len(), 1);

        // Demote
        let removed = tier.demote(&chunk_id);
        assert!(removed);
        assert_eq!(tier.len(), 0);
        assert!(!tier.contains(&chunk_id));

        // Search should not find demoted chunk
        let results = tier.search(&embedding, 1).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_version_tracking() {
        let config = make_config();
        let tier = HotTier::new(config);

        let chunk_id = ChunkId::new();
        let tenant_id = TenantId::new("test").unwrap();
        let mut embedding = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut embedding);

        let v0 = tier.version();

        tier.promote(chunk_id.clone(), embedding, tenant_id, 0.5).unwrap();
        let v1 = tier.version();
        assert!(v1 > v0, "Version should increment on promote");

        tier.demote(&chunk_id);
        let v2 = tier.version();
        assert!(v2 > v1, "Version should increment on demote");
    }

    #[test]
    fn test_eviction() {
        let mut config = make_config();
        config.max_chunks = 5;
        let tier = HotTier::new(config);

        let tenant_id = TenantId::new("test").unwrap();

        // Add 5 chunks with different scores
        for i in 0..5 {
            let chunk_id = ChunkId::new();
            let mut embedding = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
            normalize(&mut embedding);
            tier.promote(chunk_id, embedding, tenant_id.clone(), i as f32 * 0.1).unwrap();
        }

        assert_eq!(tier.len(), 5);

        // Evict 2 lowest scoring
        let evicted = tier.evict_lowest_score(2);
        assert_eq!(evicted, 2);
        assert_eq!(tier.len(), 3);
    }

    #[test]
    fn test_stats() {
        let config = make_config();
        let tier = HotTier::new(config);

        let tenant_id = TenantId::new("test").unwrap();

        // Add some chunks
        for i in 0..3 {
            let chunk_id = ChunkId::new();
            let mut embedding = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
            normalize(&mut embedding);
            let score = (i + 1) as f32 * 0.2; // 0.2, 0.4, 0.6
            tier.promote(chunk_id, embedding, tenant_id.clone(), score).unwrap();
        }

        let stats = tier.get_stats();
        assert_eq!(stats.chunk_count, 3);
        assert!(stats.version > 0);
        assert!((stats.avg_promotion_score - 0.4).abs() < 0.01); // (0.2 + 0.4 + 0.6) / 3 = 0.4
        assert!(stats.capacity_used > 0.0);
    }

    #[test]
    fn test_search_with_timing() {
        let config = make_config();
        let tier = HotTier::new(config);

        let chunk_id = ChunkId::new();
        let tenant_id = TenantId::new("test").unwrap();
        let mut embedding = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut embedding);

        tier.promote(chunk_id.clone(), embedding.clone(), tenant_id, 0.5).unwrap();

        let (results, duration) = tier.search_with_timing(&embedding, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert!(duration.as_nanos() > 0); // Should take some time
    }

    #[test]
    fn test_capacity_calculation() {
        let mut config = make_config();
        config.capacity_percentage = 0.10;
        config.max_chunks = 1000;
        let tier = HotTier::new(config);

        // 10% of 500 = 50
        assert_eq!(tier.calculate_capacity(500), 50);

        // 10% of 20000 = 2000, but capped at 1000
        assert_eq!(tier.calculate_capacity(20000), 1000);
    }

    #[test]
    fn test_config_defaults() {
        let config = HotTierConfig::default();
        assert_eq!(config.capacity_percentage, 0.10);
        assert_eq!(config.max_chunks, 50_000);
        assert_eq!(config.min_promotion_score, 0.3);
        assert_eq!(config.hnsw_config.ef_search, 30); // Faster than warm tier
    }
}
