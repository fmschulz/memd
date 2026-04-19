//! Compaction runner for orchestrating all compaction operations
//!
//! CompactionRunner coordinates HNSW rebuild, segment merge, and cache
//! invalidation with throttling between major operations.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::compaction::expiry_sweep::ExpirySweep;
use crate::compaction::history_promotion::HistoryPromotion;
use crate::compaction::hnsw_rebuild::RebuildResult;
use crate::compaction::metrics::CompactionMetrics;
use crate::compaction::segment_merge::{MergeResult, SegmentMerger};
use crate::compaction::throttle::{Throttle, ThrottleConfig};
use crate::compaction::CompactionConfig;
use crate::error::Result;
use crate::index::Bm25Index;
use crate::store::dense::DenseSearcher;
use crate::store::hybrid::HybridSearcher;
use crate::store::metadata::{MetadataStore, SqliteMetadataStore};
use crate::tiered::SemanticCache;
use crate::types::{ChunkId, TenantId};

/// Result of a compaction operation
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Number of tombstones (deleted chunks) processed
    pub tombstones_processed: usize,
    /// HNSW rebuild result (if rebuild was triggered)
    pub hnsw_rebuild: Option<RebuildResult>,
    /// Segment merge result (if merge was triggered)
    pub segment_merge: Option<MergeResult>,
    /// Number of cache entries invalidated
    pub cache_entries_invalidated: usize,
    /// Number of rows whose status flipped to `Expired` during this
    /// cycle's `ExpirySweep` (Track C3/C5). 0 when the sweep was
    /// disabled via `CompactionConfig::expiry_sweep_enabled` or when
    /// no rows were past their retention window.
    pub expired_count: usize,
    /// Number of rows whose tier was demoted to `History` during this
    /// cycle's `HistoryPromotion` (Track C4/C5). 0 when the promotion
    /// was disabled or when no rows matched the age threshold.
    pub promoted_count: usize,
    /// Total duration of compaction
    pub duration: Duration,
}

/// Compaction runner that orchestrates all compaction operations
///
/// Coordinates HNSW rebuild, segment merge, and cache invalidation
/// with throttle delays between major operations.
pub struct CompactionRunner {
    config: CompactionConfig,
    throttle: Throttle,
}

impl CompactionRunner {
    /// Create a new CompactionRunner with the given configuration
    pub fn new(config: CompactionConfig) -> Self {
        let throttle_config = ThrottleConfig {
            batch_delay_ms: config.batch_delay_ms,
            batch_size: config.batch_size,
            enabled: config.enabled,
        };

        Self {
            config,
            throttle: Throttle::new(throttle_config),
        }
    }

    /// Run compaction for a tenant
    ///
    /// Coordinates all compaction operations in order:
    /// 1. Gather metrics and deleted chunk IDs
    /// 2. HNSW rebuild (if staleness exceeds threshold)
    /// 3. Segment merge (if segment count exceeds threshold)
    /// 4. Cache invalidation (always, for deleted chunks)
    ///
    /// Inserts throttle delays between major operations.
    pub fn run_compaction(
        &self,
        tenant_id: &TenantId,
        metadata: &SqliteMetadataStore,
        dense_searcher: &DenseSearcher,
        sparse_index: Option<&Bm25Index>,
        semantic_cache: Option<&SemanticCache>,
        hybrid: Option<&HybridSearcher>,
    ) -> Result<CompactionResult> {
        let start = Instant::now();

        tracing::info!(tenant_id = %tenant_id, "compaction started");

        // 0. Track C5 — ExpirySweep and HistoryPromotion run BEFORE
        //    the excluded-ID set is gathered so the sweep/promotion's
        //    status/tier transitions flow into B2's HNSW-rebuild
        //    exclusion via `list_lifecycle_hidden` below. Both sweeps
        //    are invoked with `hybrid=None` — the runner owns the
        //    single centralised cache bump at the end of the cycle
        //    rather than letting each sweep bump independently.
        let expired_count = if self.config.expiry_sweep_enabled {
            match ExpirySweep::new().run(metadata, None, tenant_id) {
                Ok(r) => {
                    tracing::debug!(
                        tenant_id = %tenant_id,
                        expired = r.expired_count,
                        "expiry sweep completed"
                    );
                    r.expired_count
                }
                Err(e) => {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        error = %e,
                        "expiry sweep failed, continuing compaction"
                    );
                    0
                }
            }
        } else {
            0
        };

        let promoted_count = if self.config.history_promotion_enabled {
            match HistoryPromotion::new(self.config.history_promotion_age_ms)
                .run(metadata, None, tenant_id)
            {
                Ok(r) => {
                    tracing::debug!(
                        tenant_id = %tenant_id,
                        promoted = r.promoted_count,
                        "history promotion completed"
                    );
                    r.promoted_count
                }
                Err(e) => {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        error = %e,
                        "history promotion failed, continuing compaction"
                    );
                    0
                }
            }
        } else {
            0
        };

        // 1. Build the HNSW-rebuild excluded set.
        //
        // Previously only tombstones (hard-deleted rows) were excluded.
        // After Track A shipped lifecycle overlay, the retrieval layer
        // also hides rows that are Superseded, Expired (status or past
        // wall-clock), or in the History tier — they must not show up in
        // search results (enforced by the B1 visibility filter at the
        // handler boundary). If we rebuild HNSW with those rows still
        // included, the tiered/dense backend carries the weight
        // (distance, memory) for chunks that are already unreachable to
        // callers, and over time the HNSW drifts toward a graph
        // dominated by hidden history.
        //
        // Unioning lifecycle-hidden ids into the excluded set on every
        // rebuild keeps the HNSW shape aligned with what the handler
        // will actually surface. Metrics stay separate — tombstones
        // (hard deletes) vs lifecycle-hidden (soft) are different
        // accounting categories.
        let deleted_chunk_ids = metadata.get_deleted_chunk_ids(tenant_id)?;
        let lifecycle_hidden_ids = metadata.list_lifecycle_hidden(tenant_id)?;
        let tombstones_processed = deleted_chunk_ids.len();
        let lifecycle_hidden_count = lifecycle_hidden_ids.len();

        let mut excluded_chunk_ids_set: HashSet<ChunkId> =
            HashSet::with_capacity(tombstones_processed + lifecycle_hidden_count);
        excluded_chunk_ids_set.extend(deleted_chunk_ids.iter().cloned());
        excluded_chunk_ids_set.extend(lifecycle_hidden_ids.iter().cloned());
        let deleted_chunk_ids_set = excluded_chunk_ids_set;

        tracing::debug!(
            tenant_id = %tenant_id,
            tombstones = tombstones_processed,
            lifecycle_hidden = lifecycle_hidden_count,
            total_excluded = deleted_chunk_ids_set.len(),
            "gathered exclusion set for compaction"
        );

        // 2. Gather metrics for threshold checks
        let hnsw_stats = dense_searcher.get_rebuild_stats(tenant_id);
        let segment_count = sparse_index
            .map(|idx| idx.segment_count().unwrap_or(0))
            .unwrap_or(0);

        let metrics = CompactionMetrics::gather(metadata, hnsw_stats, segment_count, tenant_id)?;

        // THROTTLE between gathering and rebuild
        self.throttle.delay_sync();

        // 3. HNSW Rebuild (if staleness exceeds threshold)
        let hnsw_rebuild = if metrics
            .exceeds_hnsw_staleness_threshold(self.config.thresholds.hnsw_staleness_pct)
        {
            tracing::info!(
                tenant_id = %tenant_id,
                staleness = metrics.hnsw_staleness,
                threshold = self.config.thresholds.hnsw_staleness_pct,
                "triggering HNSW rebuild"
            );

            match dense_searcher.rebuild_hnsw_for_tenant(tenant_id, &deleted_chunk_ids_set) {
                Ok(result) => {
                    tracing::info!(
                        tenant_id = %tenant_id,
                        processed = result.embeddings_processed,
                        included = result.embeddings_included,
                        excluded = result.embeddings_excluded,
                        duration_ms = result.duration.as_millis(),
                        "HNSW rebuild completed"
                    );
                    Some(result)
                }
                Err(e) => {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        error = %e,
                        "HNSW rebuild failed, continuing compaction"
                    );
                    None
                }
            }
        } else {
            tracing::debug!(
                tenant_id = %tenant_id,
                staleness = metrics.hnsw_staleness,
                threshold = self.config.thresholds.hnsw_staleness_pct,
                "HNSW rebuild not needed"
            );
            None
        };

        // THROTTLE between rebuild and merge
        self.throttle.delay_sync();

        // 4. Segment merge (if segment count exceeds threshold)
        let segment_merge = if let Some(sparse) = sparse_index {
            if metrics.exceeds_segment_threshold(self.config.thresholds.max_segment_count) {
                tracing::info!(
                    tenant_id = %tenant_id,
                    segments = metrics.segment_count,
                    threshold = self.config.thresholds.max_segment_count,
                    "triggering segment merge"
                );

                let merger = SegmentMerger::new();
                match merger.merge(sparse) {
                    Ok(result) => {
                        tracing::info!(
                            tenant_id = %tenant_id,
                            before = result.segments_before,
                            after = result.segments_after,
                            merged = result.segments_merged,
                            duration_ms = result.duration.as_millis(),
                            "segment merge completed"
                        );
                        Some(result)
                    }
                    Err(e) => {
                        tracing::warn!(
                            tenant_id = %tenant_id,
                            error = %e,
                            "segment merge failed, continuing compaction"
                        );
                        None
                    }
                }
            } else {
                tracing::debug!(
                    tenant_id = %tenant_id,
                    segments = metrics.segment_count,
                    threshold = self.config.thresholds.max_segment_count,
                    "segment merge not needed"
                );
                None
            }
        } else {
            None
        };

        // THROTTLE between merge and cache invalidation
        self.throttle.delay_sync();

        // 5. Cache invalidation (for deleted chunks)
        let cache_entries_invalidated = if let Some(cache) = semantic_cache {
            if !deleted_chunk_ids.is_empty() {
                tracing::debug!(
                    tenant_id = %tenant_id,
                    chunks = deleted_chunk_ids.len(),
                    "invalidating cache entries for deleted chunks"
                );

                cache.invalidate_chunks(&deleted_chunk_ids);

                // Return count of chunks we tried to invalidate
                // (actual entries removed tracked in cache stats)
                deleted_chunk_ids.len()
            } else {
                0
            }
        } else {
            0
        };

        // Centralised cache bump for the whole cycle — narrowly
        // scoped to C3/C4 overlay transitions. Only these two phases
        // need a runner-level bump because:
        //   * HNSW rebuild / segment merge have no observable overlay
        //     effect the handler layer would cache differently, and
        //     dense rebuild already cycles its own internal state.
        //   * Cache invalidation for tombstones is handled by
        //     `SemanticCache::invalidate_chunks` above.
        //   * The sweeps are invoked with hybrid=None so they do not
        //     bump independently; the single bump below collapses
        //     them into one tenant_memory_version advance.
        //
        // Importantly, we do NOT use `tombstones_processed` as a
        // "something happened" proxy: that number reflects the
        // current tombstone inventory (the existing deleted-row
        // count from `get_deleted_chunk_ids`), not whether this
        // particular cycle mutated anything.
        if let Some(h) = hybrid {
            if expired_count > 0 || promoted_count > 0 {
                h.bump_tenant_memory_version(tenant_id);
            }
        }

        let duration = start.elapsed();

        tracing::info!(
            tenant_id = %tenant_id,
            tombstones = tombstones_processed,
            expired = expired_count,
            promoted = promoted_count,
            hnsw_rebuilt = hnsw_rebuild.is_some(),
            segments_merged = segment_merge.is_some(),
            cache_invalidated = cache_entries_invalidated,
            duration_ms = duration.as_millis(),
            "compaction complete"
        );

        Ok(CompactionResult {
            tombstones_processed,
            hnsw_rebuild,
            segment_merge,
            cache_entries_invalidated,
            expired_count,
            promoted_count,
            duration,
        })
    }

    /// Check if compaction should run based on metrics
    ///
    /// Returns true if ANY threshold is exceeded.
    pub fn should_run(&self, metrics: &CompactionMetrics) -> bool {
        metrics.exceeds_tombstone_threshold(self.config.thresholds.tombstone_ratio_pct)
            || metrics.exceeds_segment_threshold(self.config.thresholds.max_segment_count)
            || metrics.exceeds_hnsw_staleness_threshold(self.config.thresholds.hnsw_staleness_pct)
    }

    /// Get the current configuration
    pub fn config(&self) -> &CompactionConfig {
        &self.config
    }
}

impl Default for CompactionRunner {
    fn default() -> Self {
        Self::new(CompactionConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runner_creation() {
        let config = CompactionConfig::default();
        let runner = CompactionRunner::new(config);
        assert!(runner.config().enabled);
    }

    #[test]
    fn test_default_runner() {
        let runner = CompactionRunner::default();
        assert_eq!(runner.config().batch_delay_ms, 10);
        assert_eq!(runner.config().batch_size, 100);
    }

    #[test]
    fn test_should_run_below_thresholds() {
        let runner = CompactionRunner::default();
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.10,
            segment_count: 5,
            hnsw_staleness: 0.05,
            ..Default::default()
        };
        assert!(!runner.should_run(&metrics));
    }

    #[test]
    fn test_should_run_tombstone_exceeded() {
        let runner = CompactionRunner::default();
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.25, // Above 0.20 threshold
            segment_count: 5,
            hnsw_staleness: 0.05,
            ..Default::default()
        };
        assert!(runner.should_run(&metrics));
    }

    #[test]
    fn test_should_run_segments_exceeded() {
        let runner = CompactionRunner::default();
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.10,
            segment_count: 15, // Above 10 threshold
            hnsw_staleness: 0.05,
            ..Default::default()
        };
        assert!(runner.should_run(&metrics));
    }

    #[test]
    fn test_should_run_hnsw_exceeded() {
        let runner = CompactionRunner::default();
        let metrics = CompactionMetrics {
            tombstone_ratio: 0.10,
            segment_count: 5,
            hnsw_staleness: 0.20, // Above 0.15 threshold
            ..Default::default()
        };
        assert!(runner.should_run(&metrics));
    }
}
