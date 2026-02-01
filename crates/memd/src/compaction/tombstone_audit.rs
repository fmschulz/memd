//! Tombstone filtering audit
//!
//! Verifies that deleted chunks are properly filtered in all retrieval paths.
//! Used to validate tombstone handling correctness before and after compaction.

use crate::error::Result;
use crate::store::metadata::SqliteMetadataStore;
use crate::store::segment::SegmentReader;
use crate::types::{ChunkId, ChunkStatus, TenantId};

/// Result of a tombstone audit
#[derive(Debug, Clone)]
pub struct AuditResult {
    /// Total count of deleted chunks found in metadata
    pub total_deleted: usize,
    /// Chunks that should be filtered but weren't (tombstone leaks)
    pub tombstone_leaks: Vec<ChunkId>,
    /// List of code paths that were audited
    pub paths_audited: Vec<String>,
    /// True if no leaks were found
    pub passed: bool,
}

impl AuditResult {
    /// Create a new passing audit result
    pub fn passed(paths_audited: Vec<String>) -> Self {
        Self {
            total_deleted: 0,
            tombstone_leaks: Vec::new(),
            paths_audited,
            passed: true,
        }
    }

    /// Create a new failed audit result
    pub fn failed(
        total_deleted: usize,
        tombstone_leaks: Vec<ChunkId>,
        paths_audited: Vec<String>,
    ) -> Self {
        Self {
            total_deleted,
            tombstone_leaks,
            paths_audited,
            passed: false,
        }
    }

    /// Combine multiple audit results into one
    pub fn combine(results: Vec<AuditResult>) -> AuditResult {
        let mut combined = AuditResult {
            total_deleted: 0,
            tombstone_leaks: Vec::new(),
            paths_audited: Vec::new(),
            passed: true,
        };

        for result in results {
            combined.total_deleted += result.total_deleted;
            combined.tombstone_leaks.extend(result.tombstone_leaks);
            combined.paths_audited.extend(result.paths_audited);
            if !result.passed {
                combined.passed = false;
            }
        }

        combined
    }
}

/// Tombstone filtering audit utility
///
/// Verifies that tombstone filtering works correctly across different
/// code paths that retrieve chunk data.
pub struct TombstoneAudit;

impl TombstoneAudit {
    /// Create a new TombstoneAudit instance
    pub fn new() -> Self {
        Self
    }

    /// Audit segment reader tombstone filtering
    ///
    /// Checks that deleted chunks (according to metadata) cannot be
    /// read from the segment reader.
    ///
    /// # Arguments
    /// * `reader` - Segment reader to audit
    /// * `metadata` - Metadata store to check for deleted status
    /// * `segment_id` - Segment ID to audit
    ///
    /// # Returns
    /// AuditResult indicating whether tombstone filtering is working
    pub fn audit_segment_reader(
        &self,
        reader: &SegmentReader,
        metadata: &SqliteMetadataStore,
        segment_id: u64,
    ) -> Result<AuditResult> {
        use crate::store::metadata::MetadataStore;

        // Get all metadata for this segment
        let chunks = metadata.get_by_segment(segment_id)?;

        let mut total_deleted = 0;
        let mut tombstone_leaks = Vec::new();

        for chunk_meta in chunks {
            if chunk_meta.status == ChunkStatus::Deleted {
                total_deleted += 1;

                // Try to read the deleted chunk
                match reader.read_chunk(chunk_meta.ordinal) {
                    Ok(Some(_data)) => {
                        // Chunk was readable - this is a leak!
                        tombstone_leaks.push(chunk_meta.chunk_id);
                    }
                    Ok(None) => {
                        // Chunk correctly filtered - this is expected
                    }
                    Err(e) => {
                        // Read error - log but don't count as leak
                        tracing::warn!(
                            "Error reading chunk {} during audit: {}",
                            chunk_meta.chunk_id,
                            e
                        );
                    }
                }
            }
        }

        let paths_audited = vec!["SegmentReader::read_chunk".to_string()];

        if tombstone_leaks.is_empty() {
            Ok(AuditResult {
                total_deleted,
                tombstone_leaks,
                paths_audited,
                passed: true,
            })
        } else {
            Ok(AuditResult::failed(total_deleted, tombstone_leaks, paths_audited))
        }
    }

    /// Audit metadata store list function
    ///
    /// Verifies that list() never returns deleted chunks.
    ///
    /// # Arguments
    /// * `metadata` - Metadata store to audit
    /// * `tenant_id` - Tenant to audit
    ///
    /// # Returns
    /// AuditResult indicating whether list() filters deleted chunks
    pub fn audit_metadata_store(
        &self,
        metadata: &SqliteMetadataStore,
        tenant_id: &TenantId,
    ) -> Result<AuditResult> {
        use crate::store::metadata::MetadataStore;

        // Get list results (should never include deleted)
        let chunks = metadata.list(tenant_id, 10000, 0)?;

        let mut tombstone_leaks = Vec::new();

        for chunk_meta in &chunks {
            if chunk_meta.status == ChunkStatus::Deleted {
                // This is a leak - list() should filter deleted
                tombstone_leaks.push(chunk_meta.chunk_id.clone());
            }
        }

        let paths_audited = vec!["SqliteMetadataStore::list".to_string()];

        if tombstone_leaks.is_empty() {
            Ok(AuditResult::passed(paths_audited))
        } else {
            Ok(AuditResult::failed(tombstone_leaks.len(), tombstone_leaks, paths_audited))
        }
    }
}

impl Default for TombstoneAudit {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_result_passed() {
        let result = AuditResult::passed(vec!["test_path".to_string()]);
        assert!(result.passed);
        assert!(result.tombstone_leaks.is_empty());
        assert_eq!(result.paths_audited.len(), 1);
    }

    #[test]
    fn audit_result_failed() {
        let chunk_id = ChunkId::new();
        let result = AuditResult::failed(
            5,
            vec![chunk_id],
            vec!["test_path".to_string()],
        );
        assert!(!result.passed);
        assert_eq!(result.tombstone_leaks.len(), 1);
        assert_eq!(result.total_deleted, 5);
    }

    #[test]
    fn combine_results() {
        let result1 = AuditResult::passed(vec!["path1".to_string()]);
        let result2 = AuditResult::passed(vec!["path2".to_string()]);

        let combined = AuditResult::combine(vec![result1, result2]);
        assert!(combined.passed);
        assert_eq!(combined.paths_audited.len(), 2);
    }

    #[test]
    fn combine_results_one_failed() {
        let result1 = AuditResult::passed(vec!["path1".to_string()]);
        let chunk_id = ChunkId::new();
        let result2 = AuditResult::failed(1, vec![chunk_id], vec!["path2".to_string()]);

        let combined = AuditResult::combine(vec![result1, result2]);
        assert!(!combined.passed);
        assert_eq!(combined.tombstone_leaks.len(), 1);
    }
}
