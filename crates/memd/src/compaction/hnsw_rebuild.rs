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

    /// Phase 3.2 regression: `HnswIndex::swap_graph` must actually
    /// replace the live Hnsw graph so a search performed AFTER the
    /// swap reflects the rebuilt set (i.e. excluded points are gone).
    /// Before Phase 3.2, `rebuild_clean` computed a new graph but the
    /// caller discarded it, so the live index continued to serve
    /// deleted points (filtered downstream at the metadata layer).
    #[test]
    fn swap_graph_replaces_live_hnsw() {
        let config = HnswConfig {
            max_elements: 100,
            dimension: 4,
            ..Default::default()
        };
        let source = HnswIndex::new(config.clone());

        // Insert 5 embeddings.
        let mut chunk_ids = Vec::new();
        for i in 0..5 {
            let chunk_id = ChunkId::new();
            let mut emb = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
            normalize(&mut emb);
            source.insert(&chunk_id, &emb).unwrap();
            chunk_ids.push(chunk_id);
        }

        // Baseline: searching with query near embedding 0 returns
        // ChunkId 0 as top hit.
        let mut query = vec![0.0, 1.0, 2.0, 3.0];
        normalize(&mut query);
        let before = source.search(&query, 5).unwrap();
        assert!(
            before.iter().any(|r| r.chunk_id == chunk_ids[0]),
            "sanity: source graph must return chunk 0 before the swap"
        );

        // Mark chunk 0 as deleted and rebuild the graph without it.
        let internal_id_0 = source
            .get_mapping()
            .read()
            .get_internal_id(&chunk_ids[0])
            .unwrap();
        let mut deleted = HashSet::new();
        deleted.insert(internal_id_0);

        let rebuilder = HnswRebuilder::new();
        let (new_hnsw, result) = rebuilder.rebuild_clean(&source, &deleted, &config).unwrap();
        assert_eq!(result.embeddings_excluded, 1);
        assert_eq!(result.embeddings_included, 4);

        // Swap the rebuilt graph in atomically. The same
        // `deleted` set is passed so the embedding cache's valid
        // bits stay in sync with the live graph.
        source.swap_graph(new_hnsw, &deleted);

        // After the swap: the graph itself must not return chunk 0 as
        // a hit for the close query. `search` also filters by the
        // mapping's deleted-set if present; the rebuilt graph simply
        // does not contain the point anymore, so the count of hits
        // should drop by one relative to the pre-swap result.
        let after = source.search(&query, 5).unwrap();
        assert!(
            !after.iter().any(|r| r.chunk_id == chunk_ids[0]),
            "after swap_graph, the rebuilt graph must not surface the excluded chunk; \
             got hits: {:?}",
            after.iter().map(|r| r.chunk_id.clone()).collect::<Vec<_>>()
        );
        assert!(after.len() <= 4, "only four points remain in the graph");
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

        let _duration = result.duration;
    }
}
