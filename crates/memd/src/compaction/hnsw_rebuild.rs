//! HNSW index rebuild for compaction
//!
//! Rebuilds a clean HNSW index from the embedding cache, excluding deleted entries.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anndists::dist::distances::DistCosine;
use hnsw_rs::hnsw::Hnsw;

use crate::error::Result;
use crate::index::hnsw::{HnswConfig, HnswIndex};

/// Result of an HNSW rebuild operation
#[derive(Debug, Clone)]
pub struct RebuildResult {
    /// Total embeddings processed from cache
    pub embeddings_processed: usize,
    /// Embeddings included in new index
    pub embeddings_included: usize,
    /// Embeddings excluded (deleted)
    pub embeddings_excluded: usize,
    /// Time taken for rebuild
    pub duration: Duration,
}

/// Rebuilds a clean HNSW index from embedding cache
///
/// This is a stateless utility that creates a new HNSW graph from the
/// embeddings in the source index's cache, excluding specified deleted IDs.
/// The caller (CompactionManager) is responsible for atomically swapping
/// the old index with a new HnswIndex containing the rebuilt graph.
pub struct HnswRebuilder;

impl HnswRebuilder {
    /// Create a new HnswRebuilder
    pub fn new() -> Self {
        Self
    }

    /// Rebuild a clean HNSW graph from the source index's embedding cache
    ///
    /// # Arguments
    /// * `source_index` - The source HnswIndex to rebuild from
    /// * `deleted_internal_ids` - Set of internal IDs to exclude from rebuild
    /// * `config` - HNSW configuration for the new graph
    ///
    /// # Returns
    /// A tuple of (new Hnsw graph, RebuildResult with statistics)
    ///
    /// # Note
    /// This returns a raw Hnsw, not HnswIndex. The caller should create a new
    /// HnswIndex and swap it atomically. This separation allows the rebuild to
    /// run in the background while the old index serves queries.
    pub fn rebuild_clean(
        &self,
        source_index: &HnswIndex,
        deleted_internal_ids: &HashSet<usize>,
        config: &HnswConfig,
    ) -> Result<(Hnsw<'static, f32, DistCosine>, RebuildResult)> {
        let start = Instant::now();

        // Create new HNSW with same config parameters
        let new_hnsw = Hnsw::new(
            config.max_connections,
            config.max_elements,
            16, // max_layer (same as HnswIndex::new)
            config.ef_construction,
            DistCosine {},
        );

        // Get read access to embedding cache
        let cache = source_index.get_embedding_cache().read();

        let mut embeddings_processed = 0;
        let mut embeddings_included = 0;
        let mut embeddings_excluded = 0;

        // Iterate valid embeddings and filter out deleted ones
        for (internal_id, embedding) in cache.iter_valid() {
            embeddings_processed += 1;

            if deleted_internal_ids.contains(&internal_id) {
                embeddings_excluded += 1;
            } else {
                new_hnsw.insert_slice((embedding, internal_id));
                embeddings_included += 1;
            }
        }

        let duration = start.elapsed();

        let result = RebuildResult {
            embeddings_processed,
            embeddings_included,
            embeddings_excluded,
            duration,
        };

        tracing::info!(
            "HNSW rebuild complete: {} processed, {} included, {} excluded in {:?}",
            embeddings_processed,
            embeddings_included,
            embeddings_excluded,
            duration
        );

        Ok((new_hnsw, result))
    }
}

impl Default for HnswRebuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkId;

    fn normalize(v: &mut [f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
    }

    #[test]
    fn test_rebuild_clean_empty() {
        let config = HnswConfig {
            max_elements: 100,
            dimension: 4,
            ..Default::default()
        };
        let source = HnswIndex::new(config.clone());
        let deleted = HashSet::new();

        let rebuilder = HnswRebuilder::new();
        let (_, result) = rebuilder.rebuild_clean(&source, &deleted, &config).unwrap();

        assert_eq!(result.embeddings_processed, 0);
        assert_eq!(result.embeddings_included, 0);
        assert_eq!(result.embeddings_excluded, 0);
    }

    #[test]
    fn test_rebuild_clean_no_deletions() {
        let config = HnswConfig {
            max_elements: 100,
            dimension: 4,
            ..Default::default()
        };
        let source = HnswIndex::new(config.clone());

        // Insert some embeddings
        for i in 0..5 {
            let chunk_id = ChunkId::new();
            let mut emb = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
            normalize(&mut emb);
            source.insert(&chunk_id, &emb).unwrap();
        }

        let deleted = HashSet::new();
        let rebuilder = HnswRebuilder::new();
        let (_, result) = rebuilder.rebuild_clean(&source, &deleted, &config).unwrap();

        assert_eq!(result.embeddings_processed, 5);
        assert_eq!(result.embeddings_included, 5);
        assert_eq!(result.embeddings_excluded, 0);
    }

    #[test]
    fn test_rebuild_clean_with_deletions() {
        let config = HnswConfig {
            max_elements: 100,
            dimension: 4,
            ..Default::default()
        };
        let source = HnswIndex::new(config.clone());

        // Insert 5 embeddings (internal IDs 0-4)
        for i in 0..5 {
            let chunk_id = ChunkId::new();
            let mut emb = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
            normalize(&mut emb);
            source.insert(&chunk_id, &emb).unwrap();
        }

        // Mark internal IDs 1 and 3 as deleted
        let mut deleted = HashSet::new();
        deleted.insert(1);
        deleted.insert(3);

        let rebuilder = HnswRebuilder::new();
        let (_, result) = rebuilder.rebuild_clean(&source, &deleted, &config).unwrap();

        assert_eq!(result.embeddings_processed, 5);
        assert_eq!(result.embeddings_included, 3); // 0, 2, 4 included
        assert_eq!(result.embeddings_excluded, 2); // 1, 3 excluded
    }

    #[test]
    fn test_rebuild_result_duration() {
        let config = HnswConfig {
            max_elements: 100,
            dimension: 4,
            ..Default::default()
        };
        let source = HnswIndex::new(config.clone());
        let deleted = HashSet::new();

        let rebuilder = HnswRebuilder::new();
        let (_, result) = rebuilder.rebuild_clean(&source, &deleted, &config).unwrap();

        // Duration should be non-negative
        assert!(result.duration.as_nanos() >= 0);
    }
}
