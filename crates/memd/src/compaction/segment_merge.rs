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
/// Forces all currently searchable Tantivy segments into one segment and
/// reports before/after statistics.
pub struct SegmentMerger;

impl SegmentMerger {
    /// Create a new SegmentMerger
    pub fn new() -> Self {
        Self
    }

    /// Force all searchable segments into one merged segment.
    pub fn merge(&self, index: &Bm25Index) -> Result<MergeResult> {
        let start = Instant::now();

        // Get before stats
        let segments_before = index.segment_count()?;
        let docs_before = index.total_docs()?;

        index.merge_all_segments()?;

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
            index.commit().unwrap();
        }

        let result = merger.merge(&index).unwrap();

        assert_eq!(result.docs_before, 5);
        assert_eq!(result.docs_after, 5);
        assert!(result.segments_after <= 1);
        assert_eq!(
            result.segments_merged,
            result.segments_before.saturating_sub(result.segments_after)
        );
        let _duration = result.duration;
    }
}
