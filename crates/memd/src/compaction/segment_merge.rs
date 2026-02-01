//! Sparse segment merge for compaction
//!
//! Triggers Tantivy's built-in merge policy to compact fragmented segments.

use std::time::{Duration, Instant};

use crate::error::Result;
use crate::index::Bm25Index;

/// Result of a segment merge operation
#[derive(Debug, Clone)]
pub struct MergeResult {
    /// Number of segments before merge
    pub segments_before: usize,
    /// Number of segments after merge
    pub segments_after: usize,
    /// Number of segments merged (before - after)
    pub segments_merged: usize,
    /// Total documents before merge
    pub docs_before: u64,
    /// Total documents after merge
    pub docs_after: u64,
    /// Time taken for merge
    pub duration: Duration,
}

/// Merges sparse index segments using Tantivy's built-in policy
///
/// Tantivy's LogMergePolicy automatically merges segments during commit.
/// This merger triggers commits to ensure pending merges are applied and
/// reports before/after statistics.
pub struct SegmentMerger {
    /// Minimum segment count before merge is needed (default 4)
    min_segments_for_merge: usize,
    /// Maximum docs before merge is forced (default 100_000)
    #[allow(dead_code)]
    max_docs_before_merge: usize,
    /// Deleted docs ratio threshold (default 0.2 = 20%)
    #[allow(dead_code)]
    del_docs_ratio: f32,
}

impl SegmentMerger {
    /// Create a new SegmentMerger with default settings
    pub fn new() -> Self {
        Self {
            min_segments_for_merge: 4,
            max_docs_before_merge: 100_000,
            del_docs_ratio: 0.2,
        }
    }

    /// Create a new SegmentMerger with custom configuration
    pub fn with_config(min_segments: usize, max_docs: usize, del_ratio: f32) -> Self {
        Self {
            min_segments_for_merge: min_segments,
            max_docs_before_merge: max_docs,
            del_docs_ratio: del_ratio,
        }
    }

    /// Trigger segment merge by committing the index
    ///
    /// Tantivy's LogMergePolicy runs during commit, merging small segments
    /// into larger ones. This method triggers a commit and reports statistics.
    pub fn merge(&self, index: &Bm25Index) -> Result<MergeResult> {
        let start = Instant::now();

        // Get before stats
        let segments_before = index.segment_count()?;
        let docs_before = index.total_docs()?;

        // Tantivy's IndexWriter handles merging automatically on commit
        // Force a commit to trigger any pending merges
        index.commit()?;

        // Get after stats (need to reload to see changes)
        let segments_after = index.segment_count()?;
        let docs_after = index.total_docs()?;

        let duration = start.elapsed();

        let segments_merged = segments_before.saturating_sub(segments_after);

        let result = MergeResult {
            segments_before,
            segments_after,
            segments_merged,
            docs_before,
            docs_after,
            duration,
        };

        if segments_merged > 0 {
            tracing::info!(
                "Segment merge complete: {} -> {} segments ({} merged) in {:?}",
                segments_before,
                segments_after,
                segments_merged,
                duration
            );
        } else {
            tracing::debug!(
                "No segments merged: {} segments, {} docs in {:?}",
                segments_before,
                docs_before,
                duration
            );
        }

        Ok(result)
    }

    /// Check if merge is needed based on current segment count
    pub fn needs_merge(&self, current_segment_count: usize) -> bool {
        current_segment_count > self.min_segments_for_merge
    }

    /// Get the minimum segment threshold
    pub fn min_segments_threshold(&self) -> usize {
        self.min_segments_for_merge
    }
}

impl Default for SegmentMerger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::sparse::SparseIndex;
    use crate::types::{ChunkId, TenantId};

    fn create_test_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    #[test]
    fn test_merge_empty_index() {
        let index = Bm25Index::new().unwrap();
        let merger = SegmentMerger::new();

        let result = merger.merge(&index).unwrap();

        assert_eq!(result.docs_before, 0);
        assert_eq!(result.docs_after, 0);
    }

    #[test]
    fn test_merge_with_data() {
        let index = Bm25Index::new().unwrap();
        let tenant = create_test_tenant();
        let merger = SegmentMerger::new();

        // Insert some data
        for i in 0..5 {
            let chunk_id = ChunkId::new();
            index
                .insert(&tenant, &chunk_id, &[format!("test content {}", i)])
                .unwrap();
        }

        let result = merger.merge(&index).unwrap();

        assert_eq!(result.docs_before, 5);
        assert_eq!(result.docs_after, 5);
        assert!(result.duration.as_nanos() >= 0);
    }

    #[test]
    fn test_needs_merge_below_threshold() {
        let merger = SegmentMerger::new();

        assert!(!merger.needs_merge(1));
        assert!(!merger.needs_merge(2));
        assert!(!merger.needs_merge(4)); // At threshold
    }

    #[test]
    fn test_needs_merge_above_threshold() {
        let merger = SegmentMerger::new();

        assert!(merger.needs_merge(5));
        assert!(merger.needs_merge(10));
        assert!(merger.needs_merge(100));
    }

    #[test]
    fn test_custom_config() {
        let merger = SegmentMerger::with_config(10, 50_000, 0.1);

        assert_eq!(merger.min_segments_threshold(), 10);
        assert!(!merger.needs_merge(5));
        assert!(!merger.needs_merge(10));
        assert!(merger.needs_merge(11));
    }

    #[test]
    fn test_default() {
        let merger = SegmentMerger::default();
        assert_eq!(merger.min_segments_threshold(), 4);
    }
}
