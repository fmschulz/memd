//! Compaction metrics gathering
//!
//! Collects metrics needed to make compaction decisions:
//! - Tombstone ratio (deleted / total chunks)
//! - Segment count
//! - HNSW staleness (orphaned entries ratio)

use crate::error::Result;
use crate::store::metadata::{MetadataStore, SqliteMetadataStore};
use crate::types::TenantId;

/// Metrics for compaction decision making
///
/// Gathered from metadata store and HNSW index statistics.
#[derive(Debug, Clone, Default)]
pub struct CompactionMetrics {
    /// Ratio of deleted to total chunks (0.0 to 1.0)
    pub tombstone_ratio: f32,
    /// Count of active (non-deleted) chunks
    pub active_chunks: usize,
    /// Count of deleted chunks
    pub deleted_chunks: usize,
    /// Number of segments for this tenant
    pub segment_count: usize,
    /// Ratio of orphaned HNSW entries (0.0 to 1.0)
    pub hnsw_staleness: f32,
    /// Number of entries in HNSW cache (active mappings)
    pub hnsw_cache_size: usize,
    /// Total entries in HNSW index (may include orphans)
    pub hnsw_index_size: usize,
}

impl CompactionMetrics {
    /// Gather compaction metrics from stores
    ///
    /// # Arguments
    /// * `metadata` - Metadata store to query for chunk counts
    /// * `hnsw_stats` - Tuple of (cache_size, index_size) from HNSW
    /// * `segment_count` - Number of segments for this tenant
    /// * `tenant_id` - Tenant to gather metrics for
    ///
    /// # Returns
    /// CompactionMetrics with all fields populated
    pub fn gather(
        metadata: &SqliteMetadataStore,
        hnsw_stats: (usize, usize),
        segment_count: usize,
        tenant_id: &TenantId,
    ) -> Result<Self> {
        // Get active and deleted counts from metadata
        let (active, deleted) = metadata.count_by_status(tenant_id)?;

        // Calculate tombstone ratio (handle division by zero)
        let total = active + deleted;
        let tombstone_ratio = if total > 0 {
            deleted as f32 / total as f32
        } else {
            0.0
        };

        // Unpack HNSW stats
        let (hnsw_cache_size, hnsw_index_size) = hnsw_stats;

        // Calculate HNSW staleness (orphaned entries ratio)
        // Staleness = (index_size - cache_size) / index_size
        // Cache size represents valid mappings, index_size is total entries
        let hnsw_staleness = if hnsw_index_size > 0 {
            let orphans = hnsw_index_size.saturating_sub(hnsw_cache_size);
            orphans as f32 / hnsw_index_size as f32
        } else {
            0.0
        };

        Ok(Self {
            tombstone_ratio,
            active_chunks: active,
            deleted_chunks: deleted,
            segment_count,
            hnsw_staleness,
            hnsw_cache_size,
            hnsw_index_size,
        })
    }

    /// Check if tombstone ratio exceeds threshold
    pub fn exceeds_tombstone_threshold(&self, threshold: f32) -> bool {
        self.tombstone_ratio > threshold
    }

    /// Check if segment count exceeds threshold
    pub fn exceeds_segment_threshold(&self, threshold: usize) -> bool {
        self.segment_count > threshold
    }

    /// Check if HNSW staleness exceeds threshold
    pub fn exceeds_hnsw_staleness_threshold(&self, threshold: f32) -> bool {
        self.hnsw_staleness > threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_metrics() {
        let metrics = CompactionMetrics::default();
        assert!((metrics.tombstone_ratio - 0.0).abs() < 0.001);
        assert_eq!(metrics.active_chunks, 0);
        assert_eq!(metrics.deleted_chunks, 0);
        assert_eq!(metrics.segment_count, 0);
        assert!((metrics.hnsw_staleness - 0.0).abs() < 0.001);
    }

    #[test]
    fn exceeds_tombstone_threshold() {
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.25,
            ..Default::default()
        };
        assert!(metrics.exceeds_tombstone_threshold(0.20));
        assert!(!metrics.exceeds_tombstone_threshold(0.30));
    }

    #[test]
    fn exceeds_segment_threshold() {
        let metrics = CompactionMetrics {
            segment_count: 15,
            ..Default::default()
        };
        assert!(metrics.exceeds_segment_threshold(10));
        assert!(!metrics.exceeds_segment_threshold(20));
    }

    #[test]
    fn exceeds_hnsw_staleness_threshold() {
        let metrics = CompactionMetrics {
            hnsw_staleness: 0.20,
            ..Default::default()
        };
        assert!(metrics.exceeds_hnsw_staleness_threshold(0.15));
        assert!(!metrics.exceeds_hnsw_staleness_threshold(0.25));
    }
}
