//! Persistent store implementation
//!
//! Integrates segments, WAL, SQLite metadata, and tombstones.
//! Implements crash recovery via WAL replay on startup.
//! Uses hybrid search (dense + sparse) for retrieval.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::dense::DenseSearcher;
use super::hybrid::{ChunkMetaForRerank, HybridConfig, HybridSearchResult, HybridSearcher};
use super::metadata::{ChunkMetadata, MetadataStore, SqliteMetadataStore};
use crate::compaction::{CompactionConfig, CompactionMetrics, CompactionResult, CompactionRunner};
use crate::metrics::TieredMetrics;
use crate::store::{apply_feedback_scores, FeedbackConfig, FeedbackEntry};
use crate::task_memory::{
    TaskArtifact, TaskArtifactWriteResult, TaskProjection, TaskRecord, TaskSearchFilters,
};
use crate::tiered::{CacheStats, HotTierStats, TierDecision, TieredTiming};

/// Combined tiered search statistics
#[derive(Debug, Clone)]
pub struct TieredStats {
    /// Semantic cache statistics
    pub cache: Option<CacheStats>,
    /// Hot tier statistics (if available)
    pub hot_tier: Option<HotTierStats>,
    /// Number of entries in access tracker
    pub access_tracker_entries: usize,
    /// Aggregated tiered metrics from MetricsCollector
    pub tiered_metrics: TieredMetrics,
}
use super::segment::{SegmentReader, SegmentWriter};
use super::wal::{TaskArtifactWalPayload, WalReader, WalRecord, WalRecordType, WalWriter};
use super::{rank_candidate_chunks, score_candidate_chunk, Store, StoreStats};
use crate::embeddings::EmbeddingModel;
use crate::error::{MemdError, Result};
use crate::index::{Bm25Index, SparseIndex};
use crate::metrics::{IndexStats, MetricsCollector, QueryMetrics, TieredQueryMetrics};
use crate::retrieval::RerankerMode;
use crate::types::lifecycle::{LifecycleDelta, ResolvedChunk};
use crate::types::{ChunkId, ChunkStatus, MemoryChunk, TenantId};

/// Configuration for persistent store
#[derive(Debug, Clone)]
pub struct PersistentStoreConfig {
    /// Base data directory
    pub data_dir: PathBuf,
    /// Maximum chunks per segment before rotation
    pub segment_max_chunks: u32,
    /// WAL checkpoint interval (chunks)
    pub wal_checkpoint_interval: u32,
    /// Enable dense vector search
    pub enable_dense_search: bool,
    /// Enable hybrid search (dense + sparse)
    pub enable_hybrid_search: bool,
    /// Enable tiered search (cache/hot/warm fallback)
    pub enable_tiered_search: bool,
    /// Hybrid search configuration
    pub hybrid_config: Option<HybridConfig>,
    /// Embedding model to use for dense search
    pub embedding_model: EmbeddingModel,
    /// Enable async/background indexing of newly added chunks
    pub enable_async_indexing: bool,
    /// Max pending chunks processed per async indexer tick
    pub async_index_batch_size: usize,
    /// Poll interval for async indexer in milliseconds
    pub async_index_poll_ms: u64,
    /// On startup, backfill the HNSW index for tenants whose in-memory
    /// dense state is colder than their metadata (observed when the
    /// previous daemon crashed or was killed before `save_all()` ran).
    /// When enabled, `open()` schedules a best-effort background task on
    /// the ambient Tokio runtime that re-indexes stranded chunks. Reads
    /// still work during the backfill (they go through metadata +
    /// segment files); only semantic search is degraded until the task
    /// completes.
    pub backfill_hnsw_on_startup: bool,
    /// On startup, populate `canonical_text` for any chunk row whose
    /// value is NULL — pre-D2 production rows were inserted with
    /// `canonical_text: None`, so Track D's `idx_chunks_canonical`
    /// partial index never sees them and `memory.find_near_duplicates`
    /// / exact-mode `supersede_near_duplicates` would silently miss
    /// them. The backfill reads each row's text from its segment,
    /// canonicalises it via `canonicalize_for_type(text, chunk_type)`,
    /// and writes the result back. Best-effort, single-pass.
    pub backfill_canonical_text_on_startup: bool,
}

impl Default for PersistentStoreConfig {
    fn default() -> Self {
        let enable_async_indexing = std::env::var("MEMD_ASYNC_INDEXING")
            .ok()
            .map(|v| {
                let normalized = v.trim().to_ascii_lowercase();
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false);
        let async_index_batch_size = std::env::var("MEMD_ASYNC_INDEX_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(128);
        let async_index_poll_ms = std::env::var("MEMD_ASYNC_INDEX_POLL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(250);
        let backfill_hnsw_on_startup = std::env::var("MEMD_BACKFILL_HNSW_ON_STARTUP")
            .ok()
            .map(|v| {
                let normalized = v.trim().to_ascii_lowercase();
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true);
        let backfill_canonical_text_on_startup =
            std::env::var("MEMD_BACKFILL_CANONICAL_TEXT_ON_STARTUP")
                .ok()
                .map(|v| {
                    let normalized = v.trim().to_ascii_lowercase();
                    matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
                })
                .unwrap_or(true);

        Self {
            data_dir: PathBuf::from("data"),
            segment_max_chunks: 10_000,
            // Safety valve (v0.3.1): the WAL-checkpoint-then-truncate
            // path has a known soundness gap — a checkpoint can be
            // appended after metadata/index updates, and recovery only
            // replays records AFTER the last checkpoint. That can
            // strand committed metadata if the referenced active
            // segment was never finalized. Until we have a
            // checkpoint-before-truncate flow with tests, disable
            // periodic checkpointing and always replay the full WAL on
            // recovery. Tests that exercise the checkpoint path opt in
            // explicitly by setting this field.
            wal_checkpoint_interval: 0,
            enable_dense_search: true,
            enable_hybrid_search: true,
            enable_tiered_search: true,
            hybrid_config: None,
            embedding_model: EmbeddingModel::default(),
            enable_async_indexing,
            async_index_batch_size,
            async_index_poll_ms,
            backfill_hnsw_on_startup,
            backfill_canonical_text_on_startup,
        }
    }
}

/// Persistent store with crash recovery
pub struct PersistentStore {
    config: PersistentStoreConfig,
    /// Per-tenant state
    tenants: Arc<RwLock<HashMap<String, Arc<TenantStore>>>>,
    /// Global metadata store
    metadata: Arc<SqliteMetadataStore>,
    /// Dense vector search (optional)
    dense_searcher: Option<Arc<DenseSearcher>>,
    /// Sparse index (shared with hybrid_searcher)
    sparse_index: Option<Arc<Bm25Index>>,
    /// Hybrid searcher (replaces dense_searcher usage in search)
    hybrid_searcher: Option<Arc<HybridSearcher>>,
    /// Metrics collector for query latency
    metrics: Arc<MetricsCollector>,
    /// Compaction runner (None if compaction disabled)
    compaction_runner: Option<CompactionRunner>,
    /// Optional async index worker handle
    async_indexer: Option<AsyncIndexerHandle>,
}

/// Per-tenant storage state
struct TenantStore {
    tenant_id: String,
    base_dir: PathBuf,
    /// Current active segment writer (None if read-only)
    active_segment: Mutex<Option<ActiveSegment>>,
    /// Loaded segment readers
    segments: RwLock<HashMap<u64, SegmentReader>>,
    /// WAL writer
    wal: Mutex<WalWriter>,
    /// Counter for WAL checkpoint
    writes_since_checkpoint: Mutex<u32>,
    /// Max chunks per segment (for rotation)
    segment_max_chunks: u32,
}

struct ActiveSegment {
    writer: SegmentWriter,
    chunk_count: u32,
}

/// Result of an HNSW backfill pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BackfillStats {
    /// Number of tenants whose HNSW index received at least one new chunk.
    pub tenants_backfilled: usize,
    /// Total chunks re-indexed into HNSW.
    pub chunks_indexed: usize,
    /// Chunks encountered but not indexed (missing text, load error, or
    /// a per-batch index error).
    pub chunks_skipped: usize,
}

/// Result of a canonical_text backfill pass over legacy NULL rows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalBackfillStats {
    /// Rows whose canonical_text was populated by this pass.
    pub rows_backfilled: usize,
    /// Rows visited but not updated (missing text, load error, write
    /// error). The next pass will retry.
    pub rows_skipped: usize,
}

struct PendingChunkAdd {
    chunk: MemoryChunk,
    chunk_id: ChunkId,
    payload: Vec<u8>,
}

struct AsyncIndexerHandle {
    shutdown_tx: watch::Sender<bool>,
    job_tx: mpsc::UnboundedSender<IndexJob>,
    task: JoinHandle<()>,
}

struct IndexJob {
    tenant_id: TenantId,
    chunk_ids: Vec<ChunkId>,
    index_rows: Vec<(ChunkId, String)>,
}

impl PersistentStore {
    /// Borrow the hybrid searcher when hybrid retrieval is enabled.
    pub fn hybrid(&self) -> Option<&HybridSearcher> {
        self.hybrid_searcher.as_deref()
    }

    /// Borrow the metadata store.
    pub fn metadata(&self) -> &SqliteMetadataStore {
        self.metadata.as_ref()
    }

    /// Open or create persistent store
    pub fn open(config: PersistentStoreConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;

        // Open global metadata database
        let metadata_path = config.data_dir.join("metadata.db");
        let metadata = Arc::new(SqliteMetadataStore::open(&metadata_path)?);

        // Initialize dense searcher if enabled
        let dense_searcher = if config.enable_dense_search {
            use super::dense::DenseSearchConfig;

            let dense_config = DenseSearchConfig::default();
            match DenseSearcher::new(dense_config) {
                Ok(searcher) => {
                    let searcher = searcher.with_base_path(config.data_dir.clone());
                    Some(Arc::new(searcher))
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        "failed to initialize dense searcher, falling back to text search"
                    );
                    eprintln!(
                        "WARNING: Dense searcher initialization failed - embeddings will NOT work!"
                    );
                    eprintln!("ERROR: {}", e);
                    eprintln!("This will cause 0.000 Recall on semantic queries!");
                    eprintln!("Check that Candle and model files are available.");
                    None
                }
            }
        } else {
            None
        };

        // Initialize sparse index if hybrid search enabled
        let sparse_index = if config.enable_hybrid_search {
            let sparse_path = config.data_dir.join("sparse_index");
            match Bm25Index::with_path(Some(sparse_path)) {
                Ok(index) => Some(Arc::new(index)),
                Err(e) => {
                    warn!(
                        error = %e,
                        "failed to initialize sparse index, hybrid search disabled"
                    );
                    None
                }
            }
        } else {
            None
        };

        // Initialize hybrid searcher if both dense and sparse available (or just dense)
        let hybrid_searcher = if config.enable_hybrid_search && dense_searcher.is_some() {
            let mut hybrid_config = config.hybrid_config.clone().unwrap_or_default();
            // Apply tiered search configuration
            hybrid_config.enable_tiered = config.enable_tiered_search;
            let hybrid = HybridSearcher::new(
                Arc::clone(dense_searcher.as_ref().unwrap()),
                sparse_index.clone(),
                hybrid_config,
            );
            Some(Arc::new(hybrid))
        } else {
            None
        };

        // Initialize compaction runner
        let compaction_runner = Some(CompactionRunner::new(CompactionConfig::default()));

        let store = Self {
            config,
            tenants: Arc::new(RwLock::new(HashMap::new())),
            metadata,
            dense_searcher,
            sparse_index,
            hybrid_searcher,
            metrics: Arc::new(MetricsCollector::default()),
            compaction_runner,
            async_indexer: None,
        };

        // Recover existing tenants
        store.discover_and_recover_tenants()?;

        let async_indexer = store.start_async_indexer_if_enabled();
        let mut store = store;
        store.async_indexer = async_indexer;

        if store.config.backfill_hnsw_on_startup {
            store.spawn_startup_hnsw_backfill();
        }
        if store.config.backfill_canonical_text_on_startup {
            store.spawn_startup_canonical_backfill();
        }

        Ok(store)
    }

    /// Schedule a one-shot background task that re-indexes any tenants
    /// whose HNSW state is colder than their metadata. No-op when no
    /// Tokio runtime is available (e.g., sync test contexts) — callers
    /// can invoke `backfill_hnsw_for_cold_tenants` explicitly in that case.
    fn spawn_startup_hnsw_backfill(&self) {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => {
                // Test / sync context. The caller who wants backfill can
                // call `backfill_hnsw_for_cold_tenants` directly.
                return;
            }
        };

        let dense_searcher = self.dense_searcher.clone();
        let hybrid_searcher = self.hybrid_searcher.clone();
        let metadata = Arc::clone(&self.metadata);
        let tenants = Arc::clone(&self.tenants);

        handle.spawn(async move {
            match run_hnsw_backfill(
                dense_searcher.as_ref(),
                hybrid_searcher.as_ref(),
                metadata.as_ref(),
                tenants.as_ref(),
            )
            .await
            {
                Ok(stats) => {
                    if stats.tenants_backfilled > 0 || stats.chunks_indexed > 0 {
                        info!(
                            tenants = stats.tenants_backfilled,
                            chunks = stats.chunks_indexed,
                            skipped = stats.chunks_skipped,
                            "startup HNSW backfill completed"
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, "startup HNSW backfill failed");
                }
            }
        });
    }

    pub fn async_indexing_enabled(&self) -> bool {
        self.async_indexer.is_some()
    }

    /// Backfill the per-tenant HNSW index for tenants whose metadata has
    /// chunks but whose in-memory HNSW is empty or significantly stale.
    ///
    /// Why this exists: `DenseSearcher` holds HNSW indices in memory and
    /// only persists them on a graceful shutdown path (Drop / shutdown()).
    /// If the daemon crashes, is killed, or restarts before `save_all()`
    /// runs, on next boot every tenant's HNSW starts empty while
    /// `load_segments()` happily rehydrates segment readers from disk.
    /// Reads still work (via metadata + segment files) but semantic search
    /// returns nothing — pre-crash data is invisible.
    ///
    /// This method snapshots the tenant's active metadata rows once
    /// (avoiding the LIMIT/OFFSET race with concurrent writes), filters
    /// via per-chunk HNSW membership (`DenseSearcher::contains_chunk`),
    /// and re-indexes only the missing chunks in batches of 64. Count
    /// comparisons are not used: HNSW's internal id counter never
    /// decrements on delete, so count-based heuristics can silently
    /// miss stale tenants after delete/re-add cycles.
    ///
    /// Intended to be called once at startup, either synchronously
    /// before serving traffic or spawned as a background task. The
    /// returned `BackfillStats` are informational; a non-zero
    /// `chunks_skipped` indicates coverage may be partial (typically
    /// from a corrupt segment or a transient index error).
    pub async fn backfill_hnsw_for_cold_tenants(&self) -> Result<BackfillStats> {
        run_hnsw_backfill(
            self.dense_searcher.as_ref(),
            self.hybrid_searcher.as_ref(),
            self.metadata.as_ref(),
            self.tenants.as_ref(),
        )
        .await
    }

    /// Populate `canonical_text` for any chunk row whose value is NULL.
    ///
    /// Pre-D2 production rows were inserted with `canonical_text: None`
    /// and the `idx_chunks_canonical` partial index never sees them.
    /// This pass restores Track D's exact-mode dedup contract for those
    /// rows without requiring a destructive migration. Best-effort: a
    /// single-row failure (deserialization, missing segment) is logged
    /// and counted, not fatal — subsequent runs reattempt the same row.
    pub fn backfill_canonical_text_for_legacy_chunks(&self) -> CanonicalBackfillStats {
        run_canonical_text_backfill(self.metadata.as_ref(), self.tenants.as_ref())
    }

    /// Schedule a one-shot background task that populates canonical_text
    /// for legacy NULL rows. Mirrors the HNSW backfill structure: no-op
    /// when no Tokio runtime is available (sync test contexts) — call
    /// `backfill_canonical_text_for_legacy_chunks` directly in that
    /// case.
    fn spawn_startup_canonical_backfill(&self) {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => return,
        };
        let metadata = Arc::clone(&self.metadata);
        let tenants = Arc::clone(&self.tenants);
        handle.spawn(async move {
            let stats = run_canonical_text_backfill(metadata.as_ref(), tenants.as_ref());
            if stats.rows_backfilled > 0 || stats.rows_skipped > 0 {
                info!(
                    backfilled = stats.rows_backfilled,
                    skipped = stats.rows_skipped,
                    "startup canonical_text backfill completed"
                );
            }
        });
    }

    /// Get reference to metrics collector
    pub fn metrics(&self) -> &MetricsCollector {
        &self.metrics
    }

    /// Get shared metrics collector
    pub fn metrics_arc(&self) -> Arc<MetricsCollector> {
        Arc::clone(&self.metrics)
    }

    fn start_async_indexer_if_enabled(&self) -> Option<AsyncIndexerHandle> {
        if !self.config.enable_async_indexing {
            return None;
        }

        let handle = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle,
            Err(e) => {
                warn!(
                    error = %e,
                    "async indexing requested but no Tokio runtime found; falling back to sync indexing"
                );
                return None;
            }
        };

        let poll_ms = self.config.async_index_poll_ms.max(1);
        let batch_size = self.config.async_index_batch_size.max(1);
        let metadata = Arc::clone(&self.metadata);
        let hybrid_searcher = self.hybrid_searcher.clone();
        let dense_searcher = self.dense_searcher.clone();
        let tenants = Arc::clone(&self.tenants);

        let (job_tx, mut job_rx) = mpsc::unbounded_channel::<IndexJob>();
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let task = handle.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(poll_ms));
            loop {
                tokio::select! {
                    maybe_job = job_rx.recv() => {
                        let Some(job) = maybe_job else {
                            break;
                        };
                        run_async_index_job(
                            metadata.as_ref(),
                            hybrid_searcher.as_ref(),
                            dense_searcher.as_ref(),
                            batch_size,
                            job,
                        )
                        .await;
                    }
                    _ = interval.tick() => {
                        sweep_pending_index_jobs(
                            metadata.as_ref(),
                            tenants.as_ref(),
                            hybrid_searcher.as_ref(),
                            dense_searcher.as_ref(),
                            batch_size,
                        )
                        .await;
                    }
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        Some(AsyncIndexerHandle {
            shutdown_tx,
            job_tx,
            task,
        })
    }

    /// Get index statistics per tenant
    pub fn get_index_stats(&self, tenant_id: Option<&TenantId>) -> HashMap<String, IndexStats> {
        if let Some(ref searcher) = self.dense_searcher {
            let all_stats = searcher.get_stats();
            if let Some(tid) = tenant_id {
                let tid_str = tid.to_string();
                all_stats
                    .into_iter()
                    .filter(|(k, _)| k == &tid_str)
                    .collect()
            } else {
                all_stats
            }
        } else {
            HashMap::new()
        }
    }

    /// Get tiered search statistics
    ///
    /// Returns combined stats from cache, hot tier, access tracker, and tiered metrics.
    /// Returns None if tiered search is not enabled.
    pub fn get_tiered_stats(&self) -> Option<TieredStats> {
        let hybrid = self.hybrid_searcher.as_ref()?;
        if !hybrid.tiered_enabled() {
            return None;
        }

        let cache_stats = hybrid.get_cache_stats();
        let tiered_metrics = self.metrics.get_tiered_stats();

        Some(TieredStats {
            cache: cache_stats,
            hot_tier: None,            // Hot tier stats would need per-tenant access
            access_tracker_entries: 0, // Access tracker is per-tenant
            tiered_metrics,
        })
    }

    /// Run tiered maintenance for a tenant
    ///
    /// This should be called periodically (e.g., every 60 seconds) to:
    /// - Promote frequently accessed chunks to hot tier
    /// - Demote stale chunks from hot tier
    /// - Evict if hot tier is over capacity
    /// - Prune expired cache entries
    pub fn run_maintenance(
        &self,
        tenant_id: &TenantId,
    ) -> Option<crate::tiered::MaintenanceResult> {
        let hybrid = self.hybrid_searcher.as_ref()?;
        let result = hybrid.run_tiered_maintenance(tenant_id)?;

        // Record promotions and demotions in metrics
        for _ in 0..result.promotions_count {
            self.metrics.record_promotion();
        }
        for _ in 0..result.demotions_count {
            self.metrics.record_demotion();
        }

        Some(result)
    }

    /// Invalidate a chunk from cache and hot tier
    ///
    /// Called when a chunk is deleted to ensure tier consistency.
    pub fn invalidate_chunk(&self, chunk_id: &ChunkId) {
        if let Some(ref hybrid) = self.hybrid_searcher {
            hybrid.invalidate_chunk_in_cache(chunk_id);
        }
    }

    /// Run compaction for a tenant regardless of thresholds
    ///
    /// Forces compaction to run even if no thresholds are exceeded.
    pub fn run_compaction(&self, tenant_id: &TenantId) -> Result<CompactionResult> {
        let runner = self
            .compaction_runner
            .as_ref()
            .ok_or_else(|| MemdError::StorageError("compaction disabled".into()))?;

        let semantic_cache = self
            .hybrid_searcher
            .as_ref()
            .and_then(|h| h.get_semantic_cache());

        runner.run_compaction(
            tenant_id,
            &self.metadata,
            self.dense_searcher
                .as_ref()
                .ok_or_else(|| MemdError::StorageError("dense searcher not available".into()))?,
            self.sparse_index.as_deref(),
            semantic_cache,
            self.hybrid_searcher.as_deref(),
        )
    }

    /// Run compaction for a tenant if thresholds are exceeded
    ///
    /// Returns None if no compaction needed (all thresholds below limits).
    /// Returns Some(CompactionResult) if compaction was performed.
    pub fn run_compaction_if_needed(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Option<CompactionResult>> {
        let runner = match &self.compaction_runner {
            Some(r) => r,
            None => return Ok(None),
        };

        // Gather metrics
        let hnsw_stats = self
            .dense_searcher
            .as_ref()
            .map(|s| s.get_rebuild_stats(tenant_id))
            .unwrap_or((0, 0));

        let segment_count = self
            .sparse_index
            .as_ref()
            .map(|s| s.segment_count().unwrap_or(0))
            .unwrap_or(0);

        let metrics =
            CompactionMetrics::gather(&self.metadata, hnsw_stats, segment_count, tenant_id)?;

        if !runner.should_run(&metrics) {
            return Ok(None);
        }

        self.run_compaction(tenant_id).map(Some)
    }

    /// Get compaction metrics for a tenant
    ///
    /// Returns metrics about tombstone ratio, segment count, HNSW staleness.
    pub fn get_compaction_metrics(&self, tenant_id: &TenantId) -> Result<CompactionMetrics> {
        let hnsw_stats = self
            .dense_searcher
            .as_ref()
            .map(|s| s.get_rebuild_stats(tenant_id))
            .unwrap_or((0, 0));

        let segment_count = self
            .sparse_index
            .as_ref()
            .map(|s| s.segment_count().unwrap_or(0))
            .unwrap_or(0);

        CompactionMetrics::gather(&self.metadata, hnsw_stats, segment_count, tenant_id)
    }

    /// Search with tier information for debugging
    ///
    /// Returns results along with timing breakdown and tier decisions.
    /// Useful for MCP handlers that want debug info.
    pub async fn search_with_tier_info(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<(
        Vec<(MemoryChunk, f32)>,
        Option<TieredTiming>,
        Option<Vec<TierDecision>>,
    )> {
        let total_start = Instant::now();

        // Use hybrid search if available
        if let Some(ref hybrid) = self.hybrid_searcher {
            let (hybrid_results, timing) =
                hybrid.search_with_timing(tenant_id, query, k, None).await?;

            let mut results = Vec::with_capacity(hybrid_results.len());
            for result in hybrid_results {
                if let Some(chunk) = self
                    .get_chunk_for_retrieval(tenant_id, &result.chunk_id, "search_with_tier_info")
                    .await?
                {
                    results.push((chunk, result.final_score));
                }
            }
            let feedback = self.list_feedback(tenant_id, query, 512).await?;
            let results = apply_feedback_scores(
                results,
                query,
                &feedback,
                current_time_ms(),
                &FeedbackConfig::default(),
            );

            // Extract tiered timing and decisions
            let tiered_timing = timing.tiered.clone();

            // Note: Tier decisions would require changes to HybridSearcher to expose
            // the TieredSearchResult directly. For now, return None.
            let tier_decisions = None;

            // Record metrics
            self.metrics.record_query(QueryMetrics::from_timings(
                timing.dense_time,
                timing.sparse_time + timing.fusion_time,
                total_start.elapsed() - timing.total_time,
                total_start.elapsed(),
            ));

            return Ok((results, tiered_timing, tier_decisions));
        }

        // Fallback
        let results = self.search_with_scores(tenant_id, query, k).await?;
        Ok((results, None, None))
    }

    fn discover_and_recover_tenants(&self) -> Result<()> {
        let tenants_dir = self.config.data_dir.join("tenants");
        if !tenants_dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(&tenants_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                match name.to_str() {
                    // Skip names that fail tenant-id validation instead of
                    // joining them back into a storage path. Prior to this
                    // guard, a stray directory like `../leak` or one with
                    // a trailing slash could be fed back into
                    // `get_or_create_tenant` and `data_dir.join(tenant_id)`,
                    // yielding unintended paths.
                    Some(id) if TenantId::validate(id).is_ok() => {
                        info!(tenant_id = id, "recovering tenant");
                        let _ = self.get_or_create_tenant(id)?;
                    }
                    Some(id) => {
                        warn!(
                            tenant_id = id,
                            "skipping tenant directory with invalid name"
                        );
                    }
                    None => {
                        warn!(
                            path = %entry.path().display(),
                            "skipping tenant directory with non-UTF-8 name"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn get_or_create_tenant(&self, tenant_id: &str) -> Result<Arc<TenantStore>> {
        // Defensive validation at the storage boundary. Every upstream
        // caller already validates via `validate_tenant_id`, but this is
        // where the value is joined with `data_dir` to become a filesystem
        // path — it must not be possible to escape or confuse the layout
        // even if a new code path forgets to validate earlier.
        TenantId::validate(tenant_id)?;
        // Fast path: read lock
        {
            let tenants = self.tenants.read();
            if let Some(tenant) = tenants.get(tenant_id) {
                return Ok(Arc::clone(tenant));
            }
        }

        // Slow path: write lock + create
        let mut tenants = self.tenants.write();

        // Double-check after acquiring write lock
        if let Some(tenant) = tenants.get(tenant_id) {
            return Ok(Arc::clone(tenant));
        }

        let tenant = TenantStore::open(
            tenant_id.to_string(),
            self.config.data_dir.join("tenants").join(tenant_id),
            &self.metadata,
            self.config.segment_max_chunks,
        )?;

        let tenant = Arc::new(tenant);
        tenants.insert(tenant_id.to_string(), Arc::clone(&tenant));

        Ok(tenant)
    }

    /// Graceful shutdown - finalizes all active segments
    pub fn shutdown(&self) -> Result<()> {
        info!("PersistentStore shutting down");

        // Save dense indices
        if let Some(ref searcher) = self.dense_searcher {
            if let Err(e) = searcher.save_all() {
                warn!(error = %e, "failed to save dense indices on shutdown");
            }
        }

        // Commit sparse index
        if let Some(ref sparse) = self.sparse_index {
            if let Err(e) = sparse.commit() {
                warn!(error = %e, "failed to commit sparse index on shutdown");
            }
        }

        let tenants = self.tenants.read();
        for (tenant_id, tenant) in tenants.iter() {
            if let Err(e) = tenant.finalize_active_segment() {
                warn!(tenant_id, error = %e, "failed to finalize segment on shutdown");
            }
        }
        Ok(())
    }
}

impl Drop for PersistentStore {
    fn drop(&mut self) {
        if let Some(indexer) = self.async_indexer.take() {
            let _ = indexer.shutdown_tx.send(true);
            indexer.task.abort();
        }

        // Best-effort finalization on drop
        if let Err(e) = self.shutdown() {
            warn!(error = %e, "error during PersistentStore drop");
        }
    }
}

impl TenantStore {
    fn open(
        tenant_id: String,
        base_dir: PathBuf,
        metadata: &SqliteMetadataStore,
        segment_max_chunks: u32,
    ) -> Result<Self> {
        std::fs::create_dir_all(&base_dir)?;
        std::fs::create_dir_all(base_dir.join("segments"))?;

        // Open WAL (use open_or_create for seamless startup)
        let wal_path = base_dir.join("wal.log");
        let wal_reader = WalReader::open(&wal_path)?;
        let wal_writer = WalWriter::open_or_create(&wal_path)?;

        let store = Self {
            tenant_id: tenant_id.clone(),
            base_dir,
            active_segment: Mutex::new(None),
            segments: RwLock::new(HashMap::new()),
            wal: Mutex::new(wal_writer),
            writes_since_checkpoint: Mutex::new(0),
            segment_max_chunks,
        };

        // Load existing segments
        store.load_segments()?;

        // Recover from WAL - FULL IMPLEMENTATION
        store.recover_from_wal(&wal_reader, metadata)?;

        Ok(store)
    }

    fn load_segments(&self) -> Result<()> {
        let segments_dir = self.base_dir.join("segments");
        if !segments_dir.exists() {
            return Ok(());
        }

        let mut segments = self.segments.write();
        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let dir_name = entry.file_name();
                if let Some(name) = dir_name.to_str() {
                    if name.starts_with("seg_") && entry.path().join("meta").exists() {
                        // Only load finalized segments (have meta file)
                        match SegmentReader::open(entry.path()) {
                            Ok(reader) => {
                                info!(segment_id = reader.id, "loaded segment");
                                segments.insert(reader.id, reader);
                            }
                            Err(e) => {
                                warn!(path = ?entry.path(), error = %e, "failed to load segment");
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Full WAL recovery implementation
    ///
    /// Replays Add and Delete records from WAL to restore uncommitted state.
    /// Idempotent: skips records for chunks that already exist in metadata.
    fn recover_from_wal(
        &self,
        wal_reader: &WalReader,
        metadata: &SqliteMetadataStore,
    ) -> Result<()> {
        if wal_reader.is_empty() {
            return Ok(());
        }

        let records = wal_reader.records_for_recovery()?;
        if records.is_empty() {
            return Ok(());
        }

        info!(
            records = records.len(),
            tenant = %self.tenant_id,
            "WAL recovery starting"
        );

        let mut adds = 0;
        let mut deletes = 0;
        let mut task_artifacts = 0;
        let mut skipped = 0;

        for record in &records {
            match record.record_type {
                WalRecordType::Add => {
                    // Check if chunk already exists and is readable
                    let tenant_id = TenantId::new(&record.tenant_id).map_err(|e| {
                        MemdError::StorageError(format!("invalid tenant_id in WAL: {}", e))
                    })?;
                    let chunk_id = ChunkId::parse(&record.chunk_id).map_err(|e| {
                        MemdError::StorageError(format!("invalid chunk_id in WAL: {}", e))
                    })?;

                    // If metadata exists, check if segment data is readable
                    if let Some(existing_meta) = metadata.get(&tenant_id, &chunk_id)? {
                        // Try to read from segment to verify data is intact
                        let segments = self.segments.read();
                        if let Some(reader) = segments.get(&existing_meta.segment_id) {
                            if reader
                                .read_chunk(existing_meta.ordinal)
                                .ok()
                                .flatten()
                                .is_some()
                            {
                                // Data exists and is readable, skip
                                skipped += 1;
                                continue;
                            }
                        }
                        // Metadata exists but segment data is missing or unreadable
                        // This is a crash recovery case - re-write the chunk
                        debug!(
                            chunk_id = %chunk_id,
                            "recovering orphan metadata - segment data missing"
                        );
                    }

                    // Deserialize chunk from payload
                    let chunk: MemoryChunk =
                        serde_json::from_slice(&record.payload).map_err(|e| {
                            MemdError::StorageError(format!("deserialize WAL chunk: {}", e))
                        })?;

                    // Write to active segment
                    self.get_or_create_active_segment(self.segment_max_chunks)?;
                    let (segment_id, ordinal) = {
                        let mut active = self.active_segment.lock();
                        let seg = active.as_mut().ok_or_else(|| {
                            MemdError::StorageError("no active segment during recovery".into())
                        })?;
                        let ordinal = seg.writer.append_chunk(&record.payload)?;
                        seg.chunk_count += 1;
                        (seg.writer.id(), ordinal)
                    };

                    // Write to metadata
                    let chunk_meta = ChunkMetadata {
                        chunk_id: chunk.chunk_id.clone(),
                        tenant_id: chunk.tenant_id.clone(),
                        project_id: chunk.project_id.as_option().map(|s| s.to_string()),
                        segment_id,
                        ordinal,
                        chunk_type: chunk.chunk_type,
                        status: chunk.status,
                        timestamp_created: chunk.timestamp_created,
                        hash: chunk.hash.clone(),
                        source_uri: chunk.source.uri.clone(),
                        // A8: writer will populate lifecycle overlay directly once update_lifecycle is wired in.
                        lifecycle: crate::types::LifecycleMetadata::default(),
                        // D2: rebuild canonical_text on WAL recovery so the
                        // dedup index survives restart for chunks that were
                        // written before this code shipped.
                        canonical_text: Some(crate::store::supersession::canonicalize_for_type(
                            &chunk.text,
                            chunk.chunk_type,
                        )),
                    };
                    metadata.insert(&chunk_meta)?;

                    adds += 1;
                }
                WalRecordType::Delete => {
                    // Apply delete: mark in metadata and tombstone
                    let tenant_id = TenantId::new(&record.tenant_id).map_err(|e| {
                        MemdError::StorageError(format!("invalid tenant_id in WAL: {}", e))
                    })?;
                    let chunk_id = ChunkId::parse(&record.chunk_id).map_err(|e| {
                        MemdError::StorageError(format!("invalid chunk_id in WAL: {}", e))
                    })?;

                    // Get metadata to find segment/ordinal
                    if let Some(meta) = metadata.get(&tenant_id, &chunk_id)? {
                        if meta.status != ChunkStatus::Deleted {
                            // Mark in metadata
                            metadata.mark_deleted(&tenant_id, &chunk_id)?;

                            // Mark tombstone in segment. See the same
                            // pattern in `delete_chunk` above — the
                            // per-segment `Arc<RwLock<TombstoneSet>>`
                            // lets us do this under a read lock on the
                            // enclosing map.
                            let segments = self.segments.read();
                            if let Some(reader) = segments.get(&meta.segment_id) {
                                reader.mark_deleted(meta.ordinal)?;
                            }

                            deletes += 1;
                        } else {
                            skipped += 1;
                        }
                    } else {
                        skipped += 1;
                    }
                }
                WalRecordType::TaskArtifact => {
                    let payload: TaskArtifactWalPayload = serde_json::from_slice(&record.payload)
                        .map_err(|e| {
                        MemdError::StorageError(format!(
                            "deserialize WAL task artifact payload: {}",
                            e
                        ))
                    })?;
                    metadata.insert_task_artifact_bundle(
                        &payload.artifact,
                        &payload.projection_chunk_ids,
                        &payload.projection_kinds,
                    )?;
                    task_artifacts += 1;
                }
                WalRecordType::Checkpoint => {
                    // Checkpoint records are filtered out by records_for_recovery()
                    // but handle gracefully if encountered
                }
            }
        }

        info!(
            adds,
            deletes,
            task_artifacts,
            skipped,
            tenant = %self.tenant_id,
            "WAL recovery complete"
        );

        // Durability barrier before WAL truncation.
        //
        // Recovery above called `append_chunk` on a fresh active
        // segment, wrote metadata rows pointing at `(segment_id,
        // ordinal)`, and the original WAL still holds the source of
        // truth for those chunks. If we truncate now without first
        // finalizing the active segment, a second crash before the
        // next rotation leaves metadata pointing at an unfinalized
        // segment directory (no `meta` file, so startup skips loading
        // it) while the WAL is already empty — the chunks are lost.
        //
        // Finalize the active segment first so the recovered chunks
        // land in a real, meta-backed finalized segment. Only then is
        // WAL truncation safe: everything the WAL described is now
        // durable on disk.
        if adds > 0 {
            self.finalize_active_segment()?;
        }

        // After durable recovery, truncate WAL to start fresh.
        {
            let mut wal = self.wal.lock();
            wal.truncate()?;
        }

        Ok(())
    }

    fn next_segment_id(&self) -> u64 {
        // The previous implementation consulted only segments that had
        // been loaded into memory (finalized dirs with a `meta` file).
        // An unfinalized `seg_N/` left behind by a mid-write crash was
        // invisible, so the next rotation reused id N and `create_dir_all`
        // + `truncate(true)` silently overwrote the prior partial
        // segment — invalidating any metadata rows that still referenced
        // it.
        //
        // Scan the filesystem for all `seg_*` directories (finalized or
        // not) and return one past the maximum id found.
        let from_loaded = {
            let segments = self.segments.read();
            segments.keys().copied().max()
        };

        let mut max_on_disk: Option<u64> = from_loaded;
        let segments_dir = self.base_dir.join("segments");
        if let Ok(entries) = std::fs::read_dir(&segments_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                // Expected format: `seg_NNNNNN` with numeric suffix.
                let Some(rest) = name.strip_prefix("seg_") else {
                    continue;
                };
                let Ok(id) = rest.parse::<u64>() else {
                    continue;
                };
                max_on_disk = Some(max_on_disk.map_or(id, |prev| prev.max(id)));
            }
        }

        max_on_disk.map(|id| id + 1).unwrap_or(1)
    }

    fn get_or_create_active_segment(&self, max_chunks: u32) -> Result<()> {
        let mut active = self.active_segment.lock();

        if active.is_some() {
            let seg = active.as_ref().unwrap();
            if seg.chunk_count < max_chunks {
                return Ok(());
            }
            // Need to rotate - finalize current segment
            let seg = active.take().unwrap();
            let meta = seg.writer.finalize()?;
            info!(
                segment_id = meta.id,
                chunks = meta.chunk_count,
                "segment finalized"
            );

            // Load as reader
            let segments_dir = self.base_dir.join("segments");
            let seg_dir = segments_dir.join(format!("seg_{:06}", meta.id));
            let reader = SegmentReader::open(seg_dir)?;
            self.segments.write().insert(meta.id, reader);
        }

        // Create new segment
        let segment_id = self.next_segment_id();
        let segments_dir = self.base_dir.join("segments");
        let writer = SegmentWriter::create(&segments_dir, segment_id)?;

        *active = Some(ActiveSegment {
            writer,
            chunk_count: 0,
        });

        Ok(())
    }

    /// Flush and fsync the active segment's `payload.bin` without
    /// finalizing the segment.
    ///
    /// Called from the chunk/artifact write paths between
    /// `append_chunk` and the SQLite `insert_many` so that, on crash,
    /// no metadata row survives that references bytes only ever present
    /// in the in-memory `BufWriter`.
    fn flush_active_segment_payload(&self) -> Result<()> {
        let mut active = self.active_segment.lock();
        if let Some(seg) = active.as_mut() {
            seg.writer.flush_payload()?;
        }
        Ok(())
    }

    /// Finalize active segment for graceful shutdown
    fn finalize_active_segment(&self) -> Result<()> {
        let mut active = self.active_segment.lock();
        if let Some(seg) = active.take() {
            if seg.chunk_count > 0 {
                let meta = seg.writer.finalize()?;
                info!(
                    segment_id = meta.id,
                    chunks = meta.chunk_count,
                    tenant = %self.tenant_id,
                    "segment finalized on shutdown"
                );

                // Load as reader
                let segments_dir = self.base_dir.join("segments");
                let seg_dir = segments_dir.join(format!("seg_{:06}", meta.id));
                let reader = SegmentReader::open(seg_dir)?;
                self.segments.write().insert(meta.id, reader);
            }
        }
        Ok(())
    }

    /// Read chunk from active segment by ordinal
    fn read_from_active_segment(&self, segment_id: u64, ordinal: u32) -> Result<Option<Vec<u8>>> {
        let mut active = self.active_segment.lock();
        if let Some(seg) = active.as_mut() {
            if seg.writer.id() == segment_id {
                return seg.writer.read_chunk(ordinal);
            }
        }
        Ok(None)
    }
}

impl Drop for TenantStore {
    fn drop(&mut self) {
        // Best-effort finalization on drop
        if let Err(e) = self.finalize_active_segment() {
            warn!(
                tenant = %self.tenant_id,
                error = %e,
                "failed to finalize segment on TenantStore drop"
            );
        }
    }
}

#[async_trait::async_trait]
impl Store for PersistentStore {
    async fn add(&self, chunk: MemoryChunk) -> Result<ChunkId> {
        self.add_chunks_internal(vec![chunk])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| MemdError::StorageError("no chunk id produced".into()))
    }

    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>> {
        self.add_chunks_internal(chunks).await
    }

    async fn add_feedback(&self, feedback: FeedbackEntry) -> Result<()> {
        self.metadata.insert_feedback(&feedback)
    }

    async fn add_task_artifact(
        &self,
        artifact: TaskArtifact,
        projections: Vec<TaskProjection>,
    ) -> Result<TaskArtifactWriteResult> {
        let projection_kinds = projections
            .iter()
            .map(|projection| projection.kind.as_str().to_string())
            .collect::<Vec<_>>();

        if projections.is_empty() {
            return Err(MemdError::StorageError(
                "task artifact requires at least one projection".into(),
            ));
        }

        let tenant_id = artifact.tenant_id.clone();
        let tenant_id_str = tenant_id.to_string();
        let projection_chunks = projections
            .into_iter()
            .map(|projection| projection.chunk)
            .collect::<Vec<_>>();
        if projection_chunks
            .iter()
            .any(|chunk| chunk.tenant_id != tenant_id)
        {
            return Err(MemdError::StorageError(
                "task projections must belong to the same tenant as the artifact".into(),
            ));
        }

        let (expanded_chunks, primary_positions) = self.expand_chunks_for_add(projection_chunks)?;
        let pending = self.prepare_pending_chunks(expanded_chunks)?;
        let tenant = self.get_or_create_tenant(&tenant_id_str)?;

        let expanded_ids: Vec<ChunkId> = pending.iter().map(|row| row.chunk_id.clone()).collect();
        let mut projection_chunk_ids = Vec::with_capacity(primary_positions.len());
        for pos in &primary_positions {
            let chunk_id = expanded_ids.get(*pos).ok_or_else(|| {
                MemdError::StorageError("missing primary projection chunk id".into())
            })?;
            projection_chunk_ids.push(chunk_id.to_string());
        }

        let task_wal_payload = serde_json::to_vec(&TaskArtifactWalPayload {
            artifact: artifact.clone(),
            projection_chunk_ids: projection_chunk_ids.clone(),
            projection_kinds: projection_kinds.clone(),
        })
        .map_err(|e| {
            MemdError::StorageError(format!("serialize task artifact WAL payload: {}", e))
        })?;

        let mut wal_records = pending
            .iter()
            .map(|row| {
                WalRecord::add(
                    tenant_id_str.clone(),
                    row.chunk_id.to_string(),
                    row.chunk.timestamp_created,
                    row.payload.clone(),
                )
            })
            .collect::<Vec<_>>();
        wal_records.push(WalRecord::task_artifact(
            tenant_id_str.clone(),
            artifact.artifact_id.clone(),
            artifact.timestamp_created,
            task_wal_payload,
        ));
        {
            let mut wal = tenant.wal.lock();
            wal.append_batch(&wal_records)?;
        }

        let mut metadata_rows = Vec::with_capacity(pending.len());
        let mut index_rows = Vec::with_capacity(pending.len());
        for row in &pending {
            tenant.get_or_create_active_segment(self.config.segment_max_chunks)?;
            let (segment_id, ordinal) = {
                let mut active = tenant.active_segment.lock();
                let seg = active
                    .as_mut()
                    .ok_or_else(|| MemdError::StorageError("no active segment".into()))?;
                let ordinal = seg.writer.append_chunk(&row.payload)?;
                seg.chunk_count += 1;
                (seg.writer.id(), ordinal)
            };

            metadata_rows.push(ChunkMetadata {
                chunk_id: row.chunk_id.clone(),
                tenant_id: row.chunk.tenant_id.clone(),
                project_id: row.chunk.project_id.as_option().map(|s| s.to_string()),
                segment_id,
                ordinal,
                chunk_type: row.chunk.chunk_type,
                status: row.chunk.status,
                timestamp_created: row.chunk.timestamp_created,
                hash: row.chunk.hash.clone(),
                source_uri: row.chunk.source.uri.clone(),
                // A8: writer will populate lifecycle overlay directly once update_lifecycle is wired in.
                lifecycle: crate::types::LifecycleMetadata::default(),
                // D2 round-2: task artifacts are still chunks; populating
                // canonical_text here keeps `idx_chunks_canonical`
                // coverage repo-wide so new task artifacts written after
                // startup are not silently absent from the dedup index.
                // (Codex round-2 D2 MEDIUM finding.)
                canonical_text: Some(crate::store::supersession::canonicalize_for_type(
                    &row.chunk.text,
                    row.chunk.chunk_type,
                )),
            });
            index_rows.push((row.chunk_id.clone(), row.chunk.text.clone()));
        }
        // Persist the active segment's payload bytes before the SQLite
        // commit. Without this, a crash between `insert_many` and a
        // later `finalize_active_segment()` leaves metadata rows
        // pointing at `(segment_id, ordinal)` tuples whose bytes are
        // still sitting in the unflushed `BufWriter` and are lost.
        tenant.flush_active_segment_payload()?;
        self.metadata.insert_many(&metadata_rows)?;
        let chunk_ids_for_state: Vec<ChunkId> = metadata_rows
            .iter()
            .map(|row| row.chunk_id.clone())
            .collect();
        self.metadata
            .mark_index_pending(&tenant_id, &chunk_ids_for_state, current_time_ms())?;
        self.metadata.insert_task_artifact_bundle(
            &artifact,
            &projection_chunk_ids,
            &projection_kinds,
        )?;

        if self.async_indexing_enabled() {
            if let Some(indexer) = self.async_indexer.as_ref() {
                let job = IndexJob {
                    tenant_id: tenant_id.clone(),
                    chunk_ids: chunk_ids_for_state.clone(),
                    index_rows,
                };
                if indexer.job_tx.send(job).is_err() {
                    let error_message = "async indexer queue is closed";
                    warn!(tenant_id = %tenant_id, error = error_message, "failed to enqueue async index job");
                    mark_index_failed_many(
                        self.metadata.as_ref(),
                        &tenant_id,
                        &chunk_ids_for_state,
                        error_message,
                    );
                }
            } else {
                let error_message = "async indexing enabled but worker unavailable";
                warn!(tenant_id = %tenant_id, error = error_message, "cannot enqueue async index job");
                mark_index_failed_many(
                    self.metadata.as_ref(),
                    &tenant_id,
                    &chunk_ids_for_state,
                    error_message,
                );
            }
        } else {
            let index_result = if let Some(ref hybrid) = self.hybrid_searcher {
                hybrid.index_batch(&tenant_id, &index_rows).await
            } else if let Some(ref searcher) = self.dense_searcher {
                searcher.index_batch(&tenant_id, &index_rows).await
            } else {
                Ok(())
            };

            match index_result {
                Ok(()) => {
                    self.metadata.mark_indexed(
                        &tenant_id,
                        &chunk_ids_for_state,
                        current_time_ms(),
                    )?;
                }
                Err(e) => {
                    warn!(tenant_id = %tenant_id, error = %e, "sync index batch failed");
                    mark_index_failed_many(
                        self.metadata.as_ref(),
                        &tenant_id,
                        &chunk_ids_for_state,
                        &e.to_string(),
                    );
                }
            }
        }

        self.checkpoint_after_batch(&tenant, &tenant_id_str, (pending.len() + 1) as u32)?;

        Ok(TaskArtifactWriteResult {
            task_id: artifact.task_id.clone(),
            artifact_id: artifact.artifact_id.clone(),
            projection_chunk_ids,
        })
    }

    async fn get_task_artifact(
        &self,
        tenant_id: &TenantId,
        artifact_id: &str,
    ) -> Result<Option<TaskArtifact>> {
        self.metadata.get_task_artifact(tenant_id, artifact_id)
    }

    async fn list_task_artifacts(
        &self,
        tenant_id: &TenantId,
        task_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        self.metadata.list_task_artifacts(tenant_id, task_id)
    }

    async fn list_thread_artifacts(
        &self,
        tenant_id: &TenantId,
        thread_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        self.metadata.list_thread_artifacts(tenant_id, thread_id)
    }

    async fn list_tasks(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        self.metadata.list_tasks(tenant_id, project_id, limit)
    }

    async fn list_tenants(&self) -> Result<Vec<TenantId>> {
        let tenants = self.tenants.read();
        Ok(tenants
            .keys()
            .filter_map(|name| TenantId::new(name.clone()).ok())
            .collect())
    }

    async fn search_task_projection_chunk_ids(
        &self,
        tenant_id: &TenantId,
        filters: &TaskSearchFilters,
        limit: usize,
    ) -> Result<Vec<ChunkId>> {
        self.metadata
            .search_task_projection_chunk_ids(tenant_id, filters, limit)
    }

    async fn rerank_chunks_for_query(
        &self,
        tenant_id: &TenantId,
        query: &str,
        chunk_ids: &[ChunkId],
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        if chunk_ids.is_empty() || k == 0 {
            return Ok(Vec::new());
        }

        let mut chunks = Vec::with_capacity(chunk_ids.len());
        for chunk_id in chunk_ids {
            if let Some(chunk) = self
                .get_chunk_for_retrieval(tenant_id, chunk_id, "rerank_chunks_for_query")
                .await?
            {
                chunks.push(chunk);
            }
        }

        let Some(hybrid) = self.hybrid_searcher.as_ref() else {
            return Ok(rank_candidate_chunks(chunks, query, k));
        };

        let use_cross_encoder = hybrid.reranker_mode() == RerankerMode::CrossEncoder;
        let mut base_results = Vec::with_capacity(chunks.len());
        let mut rerank_meta = Vec::with_capacity(chunks.len());
        let mut chunk_by_id = HashMap::with_capacity(chunks.len());

        for chunk in chunks {
            let base_score = score_candidate_chunk(query, &chunk);
            if !query.trim().is_empty() && !use_cross_encoder && base_score <= 0.0 {
                continue;
            }

            base_results.push(HybridSearchResult {
                chunk_id: chunk.chunk_id.clone(),
                final_score: base_score,
                dense_rank: None,
                sparse_rank: None,
            });
            rerank_meta.push(ChunkMetaForRerank {
                chunk_id: chunk.chunk_id.clone(),
                rrf_score: base_score,
                timestamp_created: chunk.timestamp_created,
                project_id: chunk.project_id.as_option().map(str::to_string),
                chunk_type: chunk.chunk_type,
                text: Some(chunk.text.clone()),
            });
            chunk_by_id.insert(chunk.chunk_id.clone(), chunk);
        }

        if base_results.is_empty() {
            return Ok(Vec::new());
        }

        let reranked =
            hybrid.rerank_with_metadata_for_query(query, base_results, rerank_meta, None);
        let mut results = reranked
            .into_iter()
            .filter_map(|result| {
                chunk_by_id
                    .get(&result.chunk_id)
                    .cloned()
                    .map(|chunk| (chunk, result.final_score))
            })
            .collect::<Vec<_>>();
        results.truncate(k);
        Ok(results)
    }

    async fn resolve_artifacts_for_chunks(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
    ) -> Result<HashMap<String, TaskArtifact>> {
        self.metadata
            .resolve_artifacts_for_chunks(tenant_id, chunk_ids)
    }

    async fn list_feedback(
        &self,
        tenant_id: &TenantId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FeedbackEntry>> {
        self.metadata
            .list_feedback_for_query(tenant_id, query, limit)
    }

    async fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<MemoryChunk>> {
        self.get_chunk(tenant_id, chunk_id).await
    }

    async fn get_with_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
    ) -> Result<Option<ResolvedChunk>> {
        let meta = match self.metadata.get(tenant_id, chunk_id)? {
            Some(m) if m.status != ChunkStatus::Deleted => m,
            _ => return Ok(None),
        };
        let chunk = match self.get_chunk(tenant_id, chunk_id).await? {
            Some(c) => c,
            None => return Ok(None),
        };
        Ok(Some(ResolvedChunk {
            chunk,
            status: meta.status,
            lifecycle: meta.lifecycle,
        }))
    }

    fn as_persistent(&self) -> Option<&PersistentStore> {
        Some(self)
    }

    async fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<MemoryChunk>> {
        let scored = self.search_with_scores(tenant_id, query, k).await?;
        Ok(scored.into_iter().map(|(chunk, _score)| chunk).collect())
    }

    async fn search_with_scores(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        let scored = self.hybrid_search(tenant_id, query, k).await?;
        let feedback = self.list_feedback(tenant_id, query, 512).await?;
        Ok(apply_feedback_scores(
            scored,
            query,
            &feedback,
            current_time_ms(),
            &FeedbackConfig::default(),
        ))
    }

    async fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        self.delete_chunk(tenant_id, chunk_id).await
    }

    async fn stats(&self, tenant_id: &TenantId) -> Result<StoreStats> {
        self.get_stats(tenant_id).await
    }

    async fn list_chunks(
        &self,
        tenant_id: &TenantId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MemoryChunk>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let metadata_rows = self.metadata.list(tenant_id, limit, offset)?;
        let mut chunks = Vec::with_capacity(metadata_rows.len());
        for meta in metadata_rows {
            if let Some(chunk) = self.get_chunk(tenant_id, &meta.chunk_id).await? {
                chunks.push(chunk);
            }
        }
        Ok(chunks)
    }

    async fn search_with_tier_info(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<(Vec<(MemoryChunk, f32)>, Option<TieredTiming>)> {
        // Delegate to the specific method that returns timing info
        let (results, timing, _) =
            PersistentStore::search_with_tier_info(self, tenant_id, query, k).await?;
        Ok((results, timing))
    }

    fn get_tiered_stats(&self) -> Option<TieredStats> {
        PersistentStore::get_tiered_stats(self)
    }

    fn get_index_stats(&self, tenant_id: Option<&TenantId>) -> HashMap<String, IndexStats> {
        PersistentStore::get_index_stats(self, tenant_id)
    }

    fn run_compaction(&self, tenant_id: &TenantId) -> Result<CompactionResult> {
        PersistentStore::run_compaction(self, tenant_id)
    }

    fn run_compaction_if_needed(&self, tenant_id: &TenantId) -> Result<Option<CompactionResult>> {
        PersistentStore::run_compaction_if_needed(self, tenant_id)
    }

    fn get_compaction_metrics(&self, tenant_id: &TenantId) -> Result<CompactionMetrics> {
        PersistentStore::get_compaction_metrics(self, tenant_id)
    }
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn mark_index_failed_many(
    metadata: &SqliteMetadataStore,
    tenant_id: &TenantId,
    chunk_ids: &[ChunkId],
    error_message: &str,
) {
    for chunk_id in chunk_ids {
        if let Err(mark_err) =
            metadata.mark_index_failed(tenant_id, chunk_id, error_message, current_time_ms())
        {
            warn!(
                tenant_id = %tenant_id,
                chunk_id = %chunk_id,
                error = %mark_err,
                "failed to record index failure state"
            );
        }
    }
}

async fn run_async_index_job(
    metadata: &SqliteMetadataStore,
    hybrid_searcher: Option<&Arc<HybridSearcher>>,
    dense_searcher: Option<&Arc<DenseSearcher>>,
    batch_size: usize,
    job: IndexJob,
) {
    let mut index_error: Option<String> = None;
    for rows in job.index_rows.chunks(batch_size.max(1)) {
        let result = if let Some(hybrid) = hybrid_searcher {
            hybrid.index_batch(&job.tenant_id, rows).await
        } else if let Some(searcher) = dense_searcher {
            searcher.index_batch(&job.tenant_id, rows).await
        } else {
            Ok(())
        };

        if let Err(e) = result {
            index_error = Some(e.to_string());
            break;
        }
    }

    if let Some(error_message) = index_error {
        warn!(
            tenant_id = %job.tenant_id,
            error = %error_message,
            "async index job failed"
        );
        mark_index_failed_many(metadata, &job.tenant_id, &job.chunk_ids, &error_message);
        return;
    }

    if let Err(e) = metadata.mark_indexed(&job.tenant_id, &job.chunk_ids, current_time_ms()) {
        warn!(
            tenant_id = %job.tenant_id,
            error = %e,
            "failed to mark chunks indexed"
        );
    }
}

fn load_chunk_text_for_index(
    tenants: &RwLock<HashMap<String, Arc<TenantStore>>>,
    metadata: &SqliteMetadataStore,
    tenant_id: &TenantId,
    chunk_id: &ChunkId,
) -> Result<Option<String>> {
    let meta = metadata.get(tenant_id, chunk_id)?;
    let meta = match meta {
        Some(m) if m.status != ChunkStatus::Deleted => m,
        _ => return Ok(None),
    };

    let tenant_str = tenant_id.to_string();
    let tenant = match tenants.read().get(&tenant_str) {
        Some(t) => Arc::clone(t),
        None => return Ok(None),
    };

    if let Some(bytes) = tenant.read_from_active_segment(meta.segment_id, meta.ordinal)? {
        let chunk: MemoryChunk = serde_json::from_slice(&bytes)
            .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
        return Ok(Some(chunk.text));
    }

    let segments = tenant.segments.read();
    let Some(reader) = segments.get(&meta.segment_id) else {
        return Ok(None);
    };

    let payload = reader.read_chunk(meta.ordinal)?;
    let Some(bytes) = payload else {
        return Ok(None);
    };

    let chunk: MemoryChunk = serde_json::from_slice(&bytes)
        .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
    Ok(Some(chunk.text))
}

async fn run_hnsw_backfill(
    dense_searcher: Option<&Arc<DenseSearcher>>,
    hybrid_searcher: Option<&Arc<HybridSearcher>>,
    metadata: &SqliteMetadataStore,
    tenants: &RwLock<HashMap<String, Arc<TenantStore>>>,
) -> Result<BackfillStats> {
    let mut stats = BackfillStats::default();
    let Some(dense) = dense_searcher else {
        // Dense search disabled — nothing to do.
        return Ok(stats);
    };

    let tenant_strs: Vec<String> = tenants.read().keys().cloned().collect();
    for tenant_str in tenant_strs {
        let tenant_id = match TenantId::new(&tenant_str) {
            Ok(id) => id,
            Err(e) => {
                warn!(
                    tenant_id = %tenant_str,
                    error = %e,
                    "skipping tenant with invalid id during HNSW backfill"
                );
                continue;
            }
        };

        // Snapshot the tenant's active chunk metadata into memory in one
        // shot. Paging with OFFSET would race with concurrent writes that
        // shift rows between pages; a single `list` call binds the result
        // set to the SQLite view at one point in time and avoids that.
        //
        // `MetadataStore::list` already filters out soft-deleted rows
        // (see sqlite.rs: `WHERE status != 'deleted'`), so the snapshot
        // is the authoritative "active" set at this moment. Any new
        // writes arriving after this point will be indexed by their
        // write path; any chunks indexed between snapshot and the
        // per-chunk membership check will be skipped by `contains_chunk`.
        let metas = metadata.list(&tenant_id, usize::MAX, 0)?;
        if metas.is_empty() {
            continue;
        }

        // Per-chunk membership is the authoritative cold signal.
        // Count-only heuristics fail when HNSW's `next_id` has grown past
        // the active count due to deletes (dense deletes never decrement
        // the counter).
        let missing: Vec<_> = metas
            .into_iter()
            .filter(|m| !dense.contains_chunk(&tenant_id, &m.chunk_id))
            .collect();

        if missing.is_empty() {
            continue;
        }

        info!(
            tenant_id = %tenant_id,
            missing_count = missing.len(),
            "HNSW cold for tenant — backfilling"
        );

        let batch_size: usize = 64;
        let mut indexed_this_tenant = 0usize;
        let mut tenant_had_batch_failure = false;
        for chunk_batch in missing.chunks(batch_size) {
            let mut index_rows: Vec<(ChunkId, String)> = Vec::with_capacity(chunk_batch.len());
            for m in chunk_batch {
                match load_chunk_text_for_index(tenants, metadata, &tenant_id, &m.chunk_id) {
                    Ok(Some(text)) => index_rows.push((m.chunk_id.clone(), text)),
                    Ok(None) => stats.chunks_skipped += 1,
                    Err(e) => {
                        warn!(
                            tenant_id = %tenant_id,
                            chunk_id = %m.chunk_id,
                            error = %e,
                            "HNSW backfill: failed to load chunk, skipping"
                        );
                        stats.chunks_skipped += 1;
                    }
                }
            }

            if index_rows.is_empty() {
                continue;
            }

            let batch_len = index_rows.len();
            let result = if let Some(hybrid) = hybrid_searcher {
                hybrid.index_batch(&tenant_id, &index_rows).await
            } else {
                dense.index_batch(&tenant_id, &index_rows).await
            };
            match result {
                Ok(()) => {
                    indexed_this_tenant += batch_len;
                    stats.chunks_indexed += batch_len;
                }
                Err(e) => {
                    // Don't abandon the rest of the tenant; a single bad
                    // batch shouldn't block the other 99%. But do record
                    // the failure so callers know coverage may be
                    // incomplete and a follow-up pass is warranted.
                    warn!(
                        tenant_id = %tenant_id,
                        batch_len = batch_len,
                        error = %e,
                        "HNSW backfill batch failed; continuing with next batch"
                    );
                    stats.chunks_skipped += batch_len;
                    tenant_had_batch_failure = true;
                }
            }
        }

        if indexed_this_tenant > 0 {
            stats.tenants_backfilled += 1;
            info!(
                tenant_id = %tenant_id,
                chunks_indexed = indexed_this_tenant,
                had_batch_failure = tenant_had_batch_failure,
                "HNSW backfill complete for tenant"
            );
        }
    }

    Ok(stats)
}

/// Backfill `canonical_text` for any chunk row whose value is NULL.
///
/// Iterates each tenant's snapshot of active rows once, filters those
/// missing a canonical, loads the row's payload from the segment, and
/// writes back `canonicalize_for_type(text, chunk_type)` via the
/// existing `set_canonical_text` API. Errors are logged and counted as
/// skipped rather than aborting the pass — a partial backfill is more
/// useful than no backfill, and skipped rows will be revisited next
/// time the pass runs.
fn run_canonical_text_backfill(
    metadata: &SqliteMetadataStore,
    tenants: &RwLock<HashMap<String, Arc<TenantStore>>>,
) -> CanonicalBackfillStats {
    let mut stats = CanonicalBackfillStats::default();
    let tenant_strs: Vec<String> = tenants.read().keys().cloned().collect();

    for tenant_str in tenant_strs {
        let tenant_id = match TenantId::new(&tenant_str) {
            Ok(id) => id,
            Err(e) => {
                warn!(
                    tenant_id = %tenant_str,
                    error = %e,
                    "skipping tenant with invalid id during canonical_text backfill"
                );
                continue;
            }
        };

        // Snapshot once (same pattern as HNSW backfill — paging with
        // OFFSET races with concurrent writes).
        let metas = match metadata.list(&tenant_id, usize::MAX, 0) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    tenant_id = %tenant_id,
                    error = %e,
                    "canonical_text backfill: list failed, skipping tenant"
                );
                continue;
            }
        };
        let need: Vec<_> = metas
            .into_iter()
            .filter(|m| m.canonical_text.is_none())
            .collect();
        if need.is_empty() {
            continue;
        }

        for meta in &need {
            match load_chunk_text_for_index(tenants, metadata, &tenant_id, &meta.chunk_id) {
                Ok(Some(text)) => {
                    let canonical =
                        crate::store::supersession::canonicalize_for_type(&text, meta.chunk_type);
                    match metadata.set_canonical_text(&tenant_id, &meta.chunk_id, &canonical) {
                        Ok(()) => stats.rows_backfilled += 1,
                        Err(e) => {
                            warn!(
                                tenant_id = %tenant_id,
                                chunk_id = %meta.chunk_id,
                                error = %e,
                                "canonical_text backfill: write failed"
                            );
                            stats.rows_skipped += 1;
                        }
                    }
                }
                Ok(None) => stats.rows_skipped += 1,
                Err(e) => {
                    warn!(
                        tenant_id = %tenant_id,
                        chunk_id = %meta.chunk_id,
                        error = %e,
                        "canonical_text backfill: load text failed"
                    );
                    stats.rows_skipped += 1;
                }
            }
        }
    }

    stats
}

async fn sweep_pending_index_jobs(
    metadata: &SqliteMetadataStore,
    tenants: &RwLock<HashMap<String, Arc<TenantStore>>>,
    hybrid_searcher: Option<&Arc<HybridSearcher>>,
    dense_searcher: Option<&Arc<DenseSearcher>>,
    batch_size: usize,
) {
    let tenant_ids: Vec<String> = tenants.read().keys().cloned().collect();
    for tenant_id_str in tenant_ids {
        let tenant_id = match TenantId::new(&tenant_id_str) {
            Ok(id) => id,
            Err(e) => {
                warn!(tenant_id = %tenant_id_str, error = %e, "invalid tenant id during pending-index sweep");
                continue;
            }
        };

        let pending_ids = match metadata.list_pending_index_chunk_ids(&tenant_id, batch_size) {
            Ok(ids) => ids,
            Err(e) => {
                warn!(tenant_id = %tenant_id, error = %e, "failed to list pending index chunks");
                continue;
            }
        };
        if pending_ids.is_empty() {
            continue;
        }

        let mut chunk_ids = Vec::with_capacity(pending_ids.len());
        let mut index_rows = Vec::with_capacity(pending_ids.len());
        for chunk_id in pending_ids {
            match load_chunk_text_for_index(tenants, metadata, &tenant_id, &chunk_id) {
                Ok(Some(text)) => {
                    chunk_ids.push(chunk_id.clone());
                    index_rows.push((chunk_id, text));
                }
                Ok(None) => {
                    mark_index_failed_many(
                        metadata,
                        &tenant_id,
                        std::slice::from_ref(&chunk_id),
                        "pending chunk not found during index sweep",
                    );
                }
                Err(e) => {
                    mark_index_failed_many(
                        metadata,
                        &tenant_id,
                        std::slice::from_ref(&chunk_id),
                        &e.to_string(),
                    );
                }
            }
        }

        if !chunk_ids.is_empty() {
            run_async_index_job(
                metadata,
                hybrid_searcher,
                dense_searcher,
                batch_size,
                IndexJob {
                    tenant_id: tenant_id.clone(),
                    chunk_ids,
                    index_rows,
                },
            )
            .await;
        }
    }
}

impl PersistentStore {
    fn expand_chunks_for_add(
        &self,
        chunks: Vec<MemoryChunk>,
    ) -> Result<(Vec<MemoryChunk>, Vec<usize>)> {
        if chunks.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let mut expanded = Vec::new();
        let mut primary_positions = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let parts = super::split_for_add(chunk);
            if parts.is_empty() {
                return Err(MemdError::StorageError(
                    "split_for_add produced no chunks".into(),
                ));
            }
            primary_positions.push(expanded.len());
            expanded.extend(parts);
        }
        Ok((expanded, primary_positions))
    }

    fn prepare_pending_chunks(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<PendingChunkAdd>> {
        let mut pending = Vec::with_capacity(chunks.len());
        for mut chunk in chunks {
            let chunk_id = ChunkId::new();
            chunk.chunk_id = chunk_id.clone();
            chunk.hash = {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(chunk.text.as_bytes());
                format!("{:x}", hasher.finalize())
            };
            let payload = serde_json::to_vec(&chunk)
                .map_err(|e| MemdError::StorageError(format!("serialize chunk: {}", e)))?;
            pending.push(PendingChunkAdd {
                chunk,
                chunk_id,
                payload,
            });
        }
        Ok(pending)
    }

    fn checkpoint_after_batch(
        &self,
        tenant: &TenantStore,
        tenant_id: &str,
        writes: u32,
    ) -> Result<()> {
        let interval = self.config.wal_checkpoint_interval;
        if interval == 0 || writes == 0 {
            return Ok(());
        }

        let checkpoints = {
            let mut count = tenant.writes_since_checkpoint.lock();
            *count += writes;
            let checkpoints = *count / interval;
            *count %= interval;
            checkpoints
        };
        if checkpoints == 0 {
            return Ok(());
        }

        let timestamp = current_time_ms();
        let mut wal = tenant.wal.lock();
        for _ in 0..checkpoints {
            wal.append_checkpoint(tenant_id, timestamp)?;
        }
        Ok(())
    }

    async fn add_chunks_internal(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        let (expanded_chunks, primary_positions) = self.expand_chunks_for_add(chunks)?;
        let pending = self.prepare_pending_chunks(expanded_chunks)?;

        let mut tenant_groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, row) in pending.iter().enumerate() {
            tenant_groups
                .entry(row.chunk.tenant_id.to_string())
                .or_default()
                .push(idx);
        }

        for (tenant_id_str, indices) in tenant_groups {
            let tenant = self.get_or_create_tenant(&tenant_id_str)?;
            let tenant_id = pending[indices[0]].chunk.tenant_id.clone();

            let wal_rows: Vec<(String, i64, Vec<u8>)> = indices
                .iter()
                .map(|&idx| {
                    (
                        pending[idx].chunk_id.to_string(),
                        pending[idx].chunk.timestamp_created,
                        pending[idx].payload.clone(),
                    )
                })
                .collect();
            {
                let mut wal = tenant.wal.lock();
                wal.append_add_batch(&tenant_id_str, &wal_rows)?;
            }

            let mut metadata_rows = Vec::with_capacity(indices.len());
            let mut index_rows = Vec::with_capacity(indices.len());
            for idx in &indices {
                let row = &pending[*idx];
                tenant.get_or_create_active_segment(self.config.segment_max_chunks)?;
                let (segment_id, ordinal) = {
                    let mut active = tenant.active_segment.lock();
                    let seg = active
                        .as_mut()
                        .ok_or_else(|| MemdError::StorageError("no active segment".into()))?;
                    let ordinal = seg.writer.append_chunk(&row.payload)?;
                    seg.chunk_count += 1;
                    (seg.writer.id(), ordinal)
                };

                metadata_rows.push(ChunkMetadata {
                    chunk_id: row.chunk_id.clone(),
                    tenant_id: row.chunk.tenant_id.clone(),
                    project_id: row.chunk.project_id.as_option().map(|s| s.to_string()),
                    segment_id,
                    ordinal,
                    chunk_type: row.chunk.chunk_type,
                    status: row.chunk.status,
                    timestamp_created: row.chunk.timestamp_created,
                    hash: row.chunk.hash.clone(),
                    source_uri: row.chunk.source.uri.clone(),
                    // A8: writer will populate lifecycle overlay directly once update_lifecycle is wired in.
                    lifecycle: crate::types::LifecycleMetadata::default(),
                    // D2: populate canonical_text at INSERT time so the
                    // `idx_chunks_canonical` index covers every memory.add
                    // / memory.add_batch write — not just lifecycle-bearing
                    // ones routed through `add_chunk_with_lifecycle`.
                    canonical_text: Some(crate::store::supersession::canonicalize_for_type(
                        &row.chunk.text,
                        row.chunk.chunk_type,
                    )),
                });
                index_rows.push((row.chunk_id.clone(), row.chunk.text.clone()));
            }
            // Durability ordering: flush + fsync payload bytes before
            // the metadata commit. See the sibling call in
            // `add_task_artifact` for rationale.
            tenant.flush_active_segment_payload()?;
            self.metadata.insert_many(&metadata_rows)?;
            let chunk_ids_for_state: Vec<ChunkId> = metadata_rows
                .iter()
                .map(|row| row.chunk_id.clone())
                .collect();
            self.metadata.mark_index_pending(
                &tenant_id,
                &chunk_ids_for_state,
                current_time_ms(),
            )?;

            if self.async_indexing_enabled() {
                if let Some(indexer) = self.async_indexer.as_ref() {
                    let job = IndexJob {
                        tenant_id: tenant_id.clone(),
                        chunk_ids: chunk_ids_for_state.clone(),
                        index_rows,
                    };
                    if indexer.job_tx.send(job).is_err() {
                        let error_message = "async indexer queue is closed";
                        warn!(tenant_id = %tenant_id, error = error_message, "failed to enqueue async index job");
                        mark_index_failed_many(
                            self.metadata.as_ref(),
                            &tenant_id,
                            &chunk_ids_for_state,
                            error_message,
                        );
                    }
                } else {
                    let error_message = "async indexing enabled but worker unavailable";
                    warn!(tenant_id = %tenant_id, error = error_message, "cannot enqueue async index job");
                    mark_index_failed_many(
                        self.metadata.as_ref(),
                        &tenant_id,
                        &chunk_ids_for_state,
                        error_message,
                    );
                }
            } else {
                let index_result = if let Some(ref hybrid) = self.hybrid_searcher {
                    hybrid.index_batch(&tenant_id, &index_rows).await
                } else if let Some(ref searcher) = self.dense_searcher {
                    searcher.index_batch(&tenant_id, &index_rows).await
                } else {
                    Ok(())
                };

                match index_result {
                    Ok(()) => {
                        self.metadata.mark_indexed(
                            &tenant_id,
                            &chunk_ids_for_state,
                            current_time_ms(),
                        )?;
                    }
                    Err(e) => {
                        warn!(tenant_id = %tenant_id, error = %e, "sync index batch failed");
                        mark_index_failed_many(
                            self.metadata.as_ref(),
                            &tenant_id,
                            &chunk_ids_for_state,
                            &e.to_string(),
                        );
                    }
                }
            }

            self.checkpoint_after_batch(&tenant, &tenant_id_str, indices.len() as u32)?;
        }

        let expanded_ids: Vec<ChunkId> = pending.iter().map(|row| row.chunk_id.clone()).collect();
        let mut primary_ids = Vec::with_capacity(primary_positions.len());
        for pos in primary_positions {
            let chunk_id = expanded_ids
                .get(pos)
                .ok_or_else(|| MemdError::StorageError("missing primary chunk id".into()))?;
            primary_ids.push(chunk_id.clone());
        }
        Ok(primary_ids)
    }
}

impl PersistentStore {
    /// Apply a lifecycle delta through the metadata overlay and bump the tenant
    /// cache version so any tiered searcher invalidates entries that predate
    /// the lifecycle change.
    ///
    /// Intentionally `async` even though the current body has no `.await`:
    /// callers introduced by A6+ (e.g. `supersede_chunk`) and C6 live in
    /// async contexts, so keeping the signature async now avoids a
    /// breaking-change churn later.
    #[allow(clippy::unused_async)]
    pub async fn update_lifecycle(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        delta: &LifecycleDelta,
    ) -> Result<()> {
        self.metadata.update_lifecycle(tenant_id, chunk_id, delta)?;
        if let Some(h) = self.hybrid() {
            h.bump_tenant_memory_version(tenant_id);
        }
        Ok(())
    }

    /// Apply a lifecycle delta and report whether the row existed.
    ///
    /// Returns `Ok(true)` when exactly one row was updated, `Ok(false)`
    /// when the UPDATE matched zero rows (non-existent chunk_id OR
    /// cross-tenant access). The cache-version bump only fires on a
    /// successful update — a failed match is a no-op end-to-end.
    ///
    /// Used by `memory.set_expiry` (Track C6) to make the tool's
    /// `{"updated": true}` payload a load-bearing claim rather than a
    /// silent success.
    #[allow(clippy::unused_async)]
    pub async fn update_lifecycle_if_exists(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        delta: &LifecycleDelta,
    ) -> Result<bool> {
        let rows = self
            .metadata
            .update_lifecycle_counted(tenant_id, chunk_id, delta)?;
        if rows > 0 {
            if let Some(h) = self.hybrid() {
                h.bump_tenant_memory_version(tenant_id);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Write a chunk plus its initial lifecycle overlay in one logical step.
    ///
    /// Flow:
    /// 1. Write the payload through the normal `Store::add` path
    ///    (WAL + segment + metadata + async index), which already bumps the
    ///    tenant memory version when hybrid/tiered is enabled.
    /// 2. Persist the canonical text used by writer-driven digest /
    ///    supersession-by-content-identity flows (Track D).
    /// 3. If the initial delta has any non-default field, apply it through
    ///    the overlay UPDATE and bump the tenant cache version a second
    ///    time so consumers observing a snapshot between (1) and (3)
    ///    invalidate it.
    ///
    /// Structural indexing is intentionally NOT performed here — it lives
    /// at the MCP/server layer via post-write hooks so the store stays
    /// agnostic about language-aware extraction.
    pub async fn add_chunk_with_lifecycle(
        &self,
        chunk: MemoryChunk,
        initial: LifecycleDelta,
    ) -> Result<ChunkId> {
        let tenant_id = chunk.tenant_id.clone();

        // Step 1: write payload via existing add path (WAL + segment +
        // SQLite + async index). `add_chunks_internal` already populates
        // `canonical_text` on every inserted row from each row's own
        // `chunk.text` (D2). The previous design did a follow-up
        // `set_canonical_text` here using the WHOLE original document's
        // canonical form, which silently overwrote the primary row's
        // per-row canonical_text whenever `split_for_add` produced
        // multiple rows — Codex round-1 D2 review HIGH finding. The
        // INSERT-side write is now the single source of truth.
        let chunk_id = <Self as Store>::add(self, chunk).await?;

        // Step 2: apply the initial lifecycle delta only if non-empty so
        // we skip a no-op UPDATE on the common "no overlay yet" call.
        if !initial.is_empty() {
            let now = current_time_ms();
            let mut delta = initial;
            if delta.lifecycle_updated_at_ms.is_none() {
                delta.lifecycle_updated_at_ms = Some(now);
            }
            self.metadata
                .update_lifecycle(&tenant_id, &chunk_id, &delta)?;
            // Bump again to invalidate any snapshot captured between
            // `add()` and this overlay UPDATE.
            if let Some(h) = self.hybrid() {
                h.bump_tenant_memory_version(&tenant_id);
            }
        }

        Ok(chunk_id)
    }

    /// Atomically supersede `old_id` with a newly written `new_chunk`.
    ///
    /// Flow:
    /// 0. Reject `tenant_id` / `new_chunk.tenant_id` mismatches — an
    ///    easy-to-make caller mistake that would otherwise persist
    ///    `new_chunk` under a different tenant from the supersession
    ///    edge.
    /// 1. Confirm `old_id` exists in `tenant_id` and is not already
    ///    `Deleted`. Doing this BEFORE writing `new_chunk` is what
    ///    makes the operation safe at the store layer: if we deferred
    ///    the check to `atomic_supersede` (step 4), a missing /
    ///    cross-tenant `old_id` would surface as an error AFTER
    ///    `new_chunk` has already been committed to WAL + segment +
    ///    metadata, leaving an orphan row behind.
    /// 2. Walk the `superseded_by` chain from `old_id` for up to 64
    ///    hops to detect pre-existing cycles before we touch disk.
    /// 3. Write `new_chunk` through `add_chunk_with_lifecycle`, which
    ///    runs the full WAL + segment + metadata + canonical-text path
    ///    and bumps the tenant cache version.
    /// 4. Link old ↔ new in one SQLite transaction via
    ///    `MetadataStore::atomic_supersede`. The pair of UPDATEs is
    ///    all-or-nothing on SQLite's side, so a mid-call crash cannot
    ///    leave a half-linked edge.
    /// 5. Best-effort drop of `old_id` from the BM25 sparse index
    ///    (immediate, when hybrid+sparse is enabled) and bump the
    ///    tenant cache version so tiered/in-memory snapshots taken
    ///    between (3) and (4) invalidate. HNSW exclusion happens at
    ///    next compaction rebuild. Authoritative invisibility of
    ///    superseded rows in retrieval is the visibility filter at
    ///    the handler boundary (Track B), not anything in this layer.
    ///
    /// Structural indexing is intentionally NOT performed here — it
    /// happens at the MCP/server layer via post-write hooks after the
    /// caller's dispatch arm invokes this method.
    pub async fn supersede_chunk(
        &self,
        tenant_id: &TenantId,
        old_id: &ChunkId,
        new_chunk: MemoryChunk,
    ) -> Result<ChunkId> {
        // Step 0: refuse tenant mismatch immediately. Without this guard
        // the new chunk would be written under `new_chunk.tenant_id`
        // while `atomic_supersede` looks for `old_id` under
        // `tenant_id` — the second would fail and orphan the first.
        if new_chunk.tenant_id != *tenant_id {
            return Err(MemdError::ValidationError(format!(
                "supersede_chunk: new_chunk.tenant_id {} does not match tenant_id {}",
                new_chunk.tenant_id, tenant_id
            )));
        }

        // Step 1: confirm `old_id` exists in `tenant_id` and is not
        // Deleted before we commit any new state. Linking a Deleted
        // row would produce an unreachable supersession edge. The
        // head check is deferred to step 2a so that a pre-existing
        // cycle in a forged / corrupted chain is reported as a cycle
        // rather than masked as a generic not-current-head error —
        // the cycle is a structural bug in the overlay, the not-head
        // case is a normal caller error, and both have distinct
        // remediation paths.
        let old_meta = match self.metadata.get(tenant_id, old_id)? {
            Some(m) if m.status == ChunkStatus::Deleted => {
                return Err(MemdError::ValidationError(format!(
                    "supersede_chunk: old chunk {old_id} is deleted in tenant {tenant_id}"
                )));
            }
            Some(m) => m,
            None => {
                return Err(MemdError::ValidationError(format!(
                    "supersede_chunk: old chunk {old_id} not found in tenant {tenant_id}"
                )));
            }
        };

        // Step 2: cycle detection — guards against a pre-existing loop
        // in the `superseded_by` chain that would make the new edge
        // nonsensical. Walks the visited-set from `old_id` and fails
        // on any revisit (not only return-to-start), no length bound.
        self.detect_supersession_cycle(tenant_id, old_id)?;

        // Step 2a: require `old_id` to be the current head (no
        // existing successor) — cycle detection already passed, so if
        // `superseded_by.is_some()` we're in the plain double-supersede
        // case, not a cycle. The SQL layer also enforces head-only
        // semantics via a `superseded_by IS NULL` WHERE clause in
        // `atomic_supersede`, but that fires only after the new chunk
        // has been persisted — this preflight keeps the common
        // caller-error path off the orphan path.
        if let Some(existing_head) = old_meta.lifecycle.superseded_by.as_ref() {
            return Err(MemdError::ValidationError(format!(
                "supersede_chunk: old chunk {old_id} is not current head \
                 (already superseded by {existing_head}) in tenant {tenant_id}"
            )));
        }

        // Step 3: write the new chunk through the normal add path. Passing
        // a default lifecycle delta keeps this light — atomic_supersede
        // populates `supersedes` / `superseded_by` in step 4. Steps 0–2
        // have ruled out every preflight reason `atomic_supersede` would
        // reject the link; the remaining failure modes (SQLite I/O,
        // concurrent supersede racing the head check) are caught in
        // step 4 and compensated in step 4a so the orphan new chunk
        // doesn't remain visible.
        let new_id = self
            .add_chunk_with_lifecycle(new_chunk, LifecycleDelta::default())
            .await?;

        // Step 4: atomically link old ↔ new in a single SQLite transaction.
        // The UPDATE filters on `superseded_by IS NULL` so a concurrent
        // supersede that raced the preflight will fail here rather than
        // forking the graph.
        let now = current_time_ms();
        if let Err(link_err) = self
            .metadata
            .atomic_supersede(tenant_id, old_id, &new_id, now)
        {
            // Step 4a: compensating DURABLE delete on link failure. A
            // metadata-only mark_deleted is not enough — without a WAL
            // delete record, recover_from_wal would replay the original
            // Add after restart and resurrect the orphan as Final; and
            // the hybrid/sparse/dense/tiered indexes would still carry
            // the chunk until the next compaction. Routing through
            // `Store::delete_chunk` hits WAL + metadata + segment
            // tombstone + hybrid delete + cache invalidation, so after
            // this call the orphan is gone from every surface.
            let delete_res = self.delete_chunk(tenant_id, &new_id).await;
            if let Err(del_err) = delete_res {
                warn!(
                    tenant_id = %tenant_id,
                    new_id = %new_id,
                    link_err = %link_err,
                    del_err = %del_err,
                    "supersede_chunk: atomic_supersede failed AND compensating delete_chunk failed; \
                     new chunk is an orphan — investigate manually"
                );
            } else {
                info!(
                    tenant_id = %tenant_id,
                    new_id = %new_id,
                    link_err = %link_err,
                    "supersede_chunk: atomic_supersede failed; orphan new chunk deleted via full delete path"
                );
            }
            return Err(link_err);
        }

        // Step 5: drop old from the sparse index (immediate when
        // hybrid+sparse enabled) and bump the tenant memory version
        // for tiered/in-memory caches. HNSW exclusion lands at next
        // compaction rebuild; the handler-boundary visibility filter
        // (Track B) is the authoritative invisibility guarantee.
        if let Some(h) = self.hybrid() {
            if let Some(sparse) = h.sparse_index() {
                // Best-effort: a sparse-delete failure does not invalidate
                // the supersession edge — the BM25 entry will linger
                // until the next compaction.
                let _ = sparse.delete(tenant_id, old_id);
            }
            h.bump_tenant_memory_version(tenant_id);
        }

        Ok(new_id)
    }

    /// Walk the `superseded_by` chain starting at `start`. Returns
    /// `Err(ValidationError)` if any cycle is detected — either the
    /// chain returns to `start` or revisits a previously-seen node
    /// mid-walk. Returns `Ok(())` on termination at an empty
    /// `superseded_by`. Arbitrary-length acyclic chains are permitted;
    /// the visited-set check is what makes detection robust regardless
    /// of chain length or where the cycle enters.
    fn detect_supersession_cycle(&self, tenant: &TenantId, start: &ChunkId) -> Result<()> {
        use std::collections::HashSet;
        let mut visited: HashSet<ChunkId> = HashSet::new();
        visited.insert(start.clone());
        let mut current = start.clone();
        loop {
            let meta = self.metadata.get(tenant, &current)?;
            match meta.and_then(|m| m.lifecycle.superseded_by) {
                None => return Ok(()),
                Some(next) => {
                    if !visited.insert(next.clone()) {
                        return Err(MemdError::ValidationError(format!(
                            "supersession cycle detected: revisited {next} while walking from {start}"
                        )));
                    }
                    current = next;
                }
            }
        }
    }

    async fn get_chunk(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
    ) -> Result<Option<MemoryChunk>> {
        // Query metadata first
        let meta = self.metadata.get(tenant_id, chunk_id)?;
        let meta = match meta {
            Some(m) if m.status != ChunkStatus::Deleted => m,
            _ => return Ok(None),
        };

        // Load from segment
        let tenant_str = tenant_id.to_string();
        let tenant = match self.tenants.read().get(&tenant_str) {
            Some(t) => Arc::clone(t),
            None => return Ok(None),
        };

        // First check active segment (for chunks not yet in finalized segments)
        if let Some(bytes) = tenant.read_from_active_segment(meta.segment_id, meta.ordinal)? {
            let chunk: MemoryChunk = serde_json::from_slice(&bytes)
                .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
            return Ok(Some(chunk));
        }

        // Check finalized segments — fast path via the in-memory cache.
        {
            let segments = tenant.segments.read();
            if let Some(reader) = segments.get(&meta.segment_id) {
                let payload = reader.read_chunk(meta.ordinal)?;
                return Ok(match payload {
                    Some(bytes) => {
                        let chunk: MemoryChunk = serde_json::from_slice(&bytes).map_err(|e| {
                            MemdError::StorageError(format!("deserialize chunk: {}", e))
                        })?;
                        Some(chunk)
                    }
                    None => None, // Tombstoned
                });
            }
        }

        // Cache miss. Metadata says the chunk exists at (segment_id, ordinal)
        // but the segment reader is not in `tenant.segments`. This should not
        // happen — startup `load_segments()` and every rollover insert into
        // the map. The previous implementation returned `Ok(None)` here,
        // which surfaces as a silent "chunk not found" and masks the real
        // inconsistency. Instead, try to open the segment on demand so the
        // read still succeeds, and log loudly so the drift is observable.
        //
        // On the explicit `memory.get` path the returned error is surfaced to
        // the caller (handlers.rs swallows only in `get_chunk_for_retrieval`),
        // so a truly missing/corrupt segment produces a real error instead of
        // a false "not found".
        let seg_dir = tenant
            .base_dir
            .join("segments")
            .join(format!("seg_{:06}", meta.segment_id));
        let reader = SegmentReader::open(seg_dir).map_err(|e| {
            warn!(
                tenant_id = %tenant_id,
                chunk_id = %chunk_id,
                segment_id = meta.segment_id,
                ordinal = meta.ordinal,
                error = %e,
                "segment reader missing from cache and on-demand open failed"
            );
            MemdError::StorageError(format!(
                "segment {} missing from cache; on-demand open failed: {}",
                meta.segment_id, e
            ))
        })?;

        warn!(
            tenant_id = %tenant_id,
            chunk_id = %chunk_id,
            segment_id = meta.segment_id,
            ordinal = meta.ordinal,
            "segment reader missing from cache; opened on demand (cache drift)"
        );
        let payload = reader.read_chunk(meta.ordinal)?;

        // Repopulate the cache so subsequent reads take the fast path. Use
        // `entry(...).or_insert(...)` instead of unconditional `insert` so we
        // don't overwrite a fresher reader that a concurrent thread (e.g. a
        // rollover or delete that materialized tombstone state) may have
        // installed between our read lock and this write lock.
        tenant
            .segments
            .write()
            .entry(meta.segment_id)
            .or_insert(reader);

        Ok(match payload {
            Some(bytes) => {
                let chunk: MemoryChunk = serde_json::from_slice(&bytes)
                    .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
                Some(chunk)
            }
            None => None,
        })
    }

    async fn get_chunk_for_retrieval(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        operation: &'static str,
    ) -> Result<Option<MemoryChunk>> {
        match self.get_chunk(tenant_id, chunk_id).await {
            Ok(chunk) => Ok(chunk),
            Err(MemdError::StorageError(error)) => {
                warn!(
                    tenant_id = %tenant_id,
                    chunk_id = %chunk_id,
                    operation,
                    error = %error,
                    "skipping unreadable chunk during retrieval"
                );
                Ok(None)
            }
            Err(MemdError::IoError(error)) => {
                warn!(
                    tenant_id = %tenant_id,
                    chunk_id = %chunk_id,
                    operation,
                    error = %error,
                    "skipping unreadable chunk during retrieval"
                );
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    async fn hybrid_search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        warn!(
            tenant_id = %tenant_id,
            query = &query[..query.len().min(50)],
            k = k,
            hybrid = self.hybrid_searcher.is_some(),
            dense = self.dense_searcher.is_some(),
            "hybrid_search called"
        );

        // Use real hybrid search if available, otherwise fallback
        if self.hybrid_searcher.is_some() || self.dense_searcher.is_some() {
            warn!("taking search_with_scores_real path");
            return self.search_with_scores_real(tenant_id, query, k).await;
        }
        // Final fallback: simple text search
        warn!("WARNING: Taking text-only fallback path - no embeddings!");
        return self.search_with_scores_impl(tenant_id, query, k).await;
    }

    async fn search_with_scores_impl(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        // OLD FALLBACK CODE (keep for now but will be removed):
        // For now, simple implementation: list + filter
        // Real search comes in Phase 3 with embeddings
        let metadata_list = self.metadata.list(tenant_id, k * 2, 0)?;

        let mut results = Vec::new();

        for meta in metadata_list {
            if meta.status == ChunkStatus::Deleted {
                continue;
            }

            let Some(chunk) = self
                .get_chunk_for_retrieval(tenant_id, &meta.chunk_id, "text_fallback_search")
                .await?
            else {
                continue;
            };

            if query.is_empty() || chunk.text.to_lowercase().contains(&query.to_lowercase()) {
                results.push(chunk);
                if results.len() >= k {
                    break;
                }
            }
        }

        // Fallback returns results with score 1.0
        Ok(results.into_iter().map(|c| (c, 1.0)).collect())
    }

    /// Replacement for old search_with_scores - now does real hybrid search
    async fn search_with_scores_real(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        warn!(
            tenant_id = %tenant_id,
            hybrid = self.hybrid_searcher.is_some(),
            dense = self.dense_searcher.is_some(),
            "search_with_scores_real called"
        );

        let total_start = Instant::now();

        // Use hybrid search if available (combines dense + sparse)
        if let Some(ref hybrid) = self.hybrid_searcher {
            warn!("using HYBRID search path");
            let (hybrid_results, timing) =
                hybrid.search_with_timing(tenant_id, query, k, None).await?;

            let fetch_start = Instant::now();
            let mut chunk_by_id: HashMap<ChunkId, MemoryChunk> =
                HashMap::with_capacity(hybrid_results.len());
            let mut rerank_meta = Vec::with_capacity(hybrid_results.len());
            let mut base_results: Vec<HybridSearchResult> =
                Vec::with_capacity(hybrid_results.len());

            for result in hybrid_results {
                if let Some(chunk) = self
                    .get_chunk_for_retrieval(tenant_id, &result.chunk_id, "hybrid_search")
                    .await?
                {
                    rerank_meta.push(ChunkMetaForRerank {
                        chunk_id: result.chunk_id.clone(),
                        rrf_score: result.final_score,
                        timestamp_created: chunk.timestamp_created,
                        project_id: chunk.project_id.as_option().map(str::to_string),
                        chunk_type: chunk.chunk_type,
                        text: Some(chunk.text.clone()),
                    });
                    chunk_by_id.insert(result.chunk_id.clone(), chunk);
                    base_results.push(result);
                }
            }

            let reranked =
                hybrid.rerank_with_metadata_for_query(query, base_results, rerank_meta, None);
            let results: Vec<(MemoryChunk, f32)> = reranked
                .into_iter()
                .filter_map(|result| {
                    chunk_by_id
                        .get(&result.chunk_id)
                        .cloned()
                        .map(|chunk| (chunk, result.final_score))
                })
                .collect();
            let fetch_time = fetch_start.elapsed();

            // Record query metrics (use dense time as embed time, sparse time as search time)
            self.metrics.record_query(QueryMetrics::from_timings(
                timing.dense_time,
                timing.sparse_time + timing.fusion_time,
                fetch_time,
                total_start.elapsed(),
            ));

            // Record tiered metrics if tiered search was used
            if timing.tiered.is_some() {
                let tiered_timing = timing.tiered.as_ref().unwrap();
                self.metrics.record_tiered_query(TieredQueryMetrics {
                    source_tier: if tiered_timing.cache_lookup_ms > 0
                        && tiered_timing.hot_tier_ms == 0
                        && tiered_timing.warm_tier_ms == 0
                    {
                        "cache".to_string()
                    } else if tiered_timing.hot_tier_ms > 0 {
                        "hot".to_string()
                    } else {
                        "warm".to_string()
                    },
                    cache_lookup_ms: tiered_timing.cache_lookup_ms,
                    hot_tier_ms: tiered_timing.hot_tier_ms,
                    warm_tier_ms: tiered_timing.warm_tier_ms,
                    cache_hit: tiered_timing.warm_tier_ms == 0 && tiered_timing.hot_tier_ms == 0,
                    hot_tier_hit: tiered_timing.hot_tier_ms > 0 && tiered_timing.warm_tier_ms == 0,
                });
            }

            return Ok(results);
        }

        // Fallback to dense-only if hybrid not available
        if let Some(ref searcher) = self.dense_searcher {
            warn!("using DENSE-ONLY search path");
            let (dense_results, embed_time, search_time) =
                searcher.search_with_timing(tenant_id, query, k).await?;

            warn!(
                dense_count = dense_results.len(),
                "dense search returned results"
            );

            let fetch_start = Instant::now();
            let mut results = Vec::with_capacity(dense_results.len());
            for result in dense_results {
                if let Some(chunk) = self
                    .get_chunk_for_retrieval(tenant_id, &result.chunk_id, "dense_search")
                    .await?
                {
                    results.push((chunk, result.score));
                } else {
                    warn!(chunk_id = %result.chunk_id, "FAILED to fetch chunk - get() returned None");
                }
            }
            warn!(final_count = results.len(), "chunks fetched successfully");
            let fetch_time = fetch_start.elapsed();

            // Record metrics
            self.metrics.record_query(QueryMetrics::from_timings(
                embed_time,
                search_time,
                fetch_time,
                total_start.elapsed(),
            ));

            return Ok(results);
        }

        // Fall back to text search with score 1.0
        warn!("using TEXT-ONLY fallback search (no embeddings available)");
        let chunks = self.search(tenant_id, query, k).await?;
        Ok(chunks.into_iter().map(|c| (c, 1.0)).collect())
    }

    async fn delete_chunk(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        // Get metadata to find segment/ordinal
        let meta = self.metadata.get(tenant_id, chunk_id)?;
        let meta = match meta {
            Some(m) if m.status != ChunkStatus::Deleted => m,
            _ => return Ok(false),
        };

        let tenant_str = tenant_id.to_string();

        // Write to WAL
        let tenant = self.get_or_create_tenant(&tenant_str)?;
        {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);

            let mut wal = tenant.wal.lock();
            wal.append_delete(&tenant_str, &chunk_id.to_string(), timestamp)?;
        }

        // Update metadata status
        self.metadata.mark_deleted(tenant_id, chunk_id)?;

        // Update tombstone in segment. Phase 3.6: `mark_deleted`
        // now takes `&self` because `SegmentReader.tombstones` is a
        // `Arc<RwLock<TombstoneSet>>`. A read lock on the segments
        // map is enough — concurrent reads on other segments and
        // other active readers on this segment no longer block.
        {
            let segments = tenant.segments.read();
            if let Some(reader) = segments.get(&meta.segment_id) {
                reader.mark_deleted(meta.ordinal)?;
            }
        }

        // Remove from hybrid/sparse index and invalidate cache/hot tier
        if let Some(ref hybrid) = self.hybrid_searcher {
            if let Err(e) = hybrid.delete_chunk(tenant_id, chunk_id) {
                warn!(
                    chunk_id = %chunk_id,
                    error = %e,
                    "failed to delete chunk from hybrid searcher"
                );
            }
        }

        // Explicit cache/tier invalidation (hybrid.delete_chunk also does this)
        self.invalidate_chunk(chunk_id);

        info!(tenant_id = %tenant_str, chunk_id = %chunk_id, "chunk deleted");
        Ok(true)
    }

    async fn get_stats(&self, tenant_id: &TenantId) -> Result<StoreStats> {
        let (active, deleted) = self.metadata.count_by_status(tenant_id)?;

        // Get chunk types from metadata
        let chunks = self.metadata.list(tenant_id, 10000, 0)?;
        let mut chunk_types = HashMap::new();
        for meta in &chunks {
            *chunk_types.entry(meta.chunk_type.to_string()).or_insert(0) += 1;
        }

        Ok(StoreStats {
            total_chunks: active + deleted,
            deleted_chunks: deleted,
            chunk_types,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::MockEmbedder;
    use crate::retrieval::{RerankerConfig, RerankerMode};
    use crate::store::dense::{DenseSearchConfig, DenseSearcher};
    use crate::store::hybrid::{HybridConfig, HybridSearcher};
    use crate::store::metadata::MetadataStore;
    use crate::store::Store;
    use crate::task_memory::{build_task_projections, TaskArtifact, TaskSearchFilters};
    use crate::types::{ChunkType, ProjectId};
    use rusqlite::Connection;
    use std::fs;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn make_test_store() -> (PersistentStore, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 10,
            enable_dense_search: false, // Disable for unit tests
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        (store, dir)
    }

    fn make_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    fn make_chunk(tenant: &TenantId, text: &str) -> MemoryChunk {
        MemoryChunk::new(tenant.clone(), text, ChunkType::Doc)
    }

    fn make_long_document() -> String {
        let sentence =
            "This is a long test sentence that should trigger document chunking behavior. ";
        sentence.repeat(40)
    }

    fn segment_payload_path(
        base_dir: &std::path::Path,
        tenant: &TenantId,
        segment_id: u64,
    ) -> std::path::PathBuf {
        base_dir
            .join("tenants")
            .join(tenant.as_str())
            .join("segments")
            .join(format!("seg_{:06}", segment_id))
            .join("payload.bin")
    }

    fn corrupt_segment_payload(base_dir: &std::path::Path, tenant: &TenantId, segment_id: u64) {
        let payload_path = segment_payload_path(base_dir, tenant, segment_id);
        let mut bytes = fs::read(&payload_path).unwrap();
        assert!(!bytes.is_empty(), "payload file must not be empty");
        bytes[0] ^= 0xFF;
        fs::write(payload_path, bytes).unwrap();
    }

    #[test]
    fn default_config_has_valid_async_indexing_settings() {
        let config = PersistentStoreConfig::default();
        assert!(config.async_index_batch_size > 0);
        assert!(config.async_index_poll_ms > 0);
    }

    #[tokio::test]
    async fn async_indexer_scaffold_is_created_when_enabled() {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            enable_dense_search: false,
            enable_hybrid_search: false,
            enable_async_indexing: true,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        assert!(store.async_indexing_enabled());
    }

    #[tokio::test]
    async fn add_and_get() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();
        let chunk = make_chunk(&tenant, "hello persistent");

        let chunk_id = store.add(chunk).await.unwrap();
        let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().text, "hello persistent");
    }

    #[tokio::test]
    async fn add_marks_indexed_when_async_indexing_disabled() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();

        store
            .add(make_chunk(&tenant, "indexed state check"))
            .await
            .unwrap();

        let (pending, indexed, failed) = store.metadata.count_by_index_state(&tenant).unwrap();
        assert_eq!(pending, 0);
        assert_eq!(indexed, 1);
        assert_eq!(failed, 0);
    }

    #[tokio::test]
    async fn add_async_eventually_marks_indexed() {
        let dir = tempdir().unwrap();
        let tenant = make_tenant();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 10,
            enable_dense_search: false,
            enable_hybrid_search: false,
            enable_async_indexing: true,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();

        store
            .add(make_chunk(&tenant, "pending state check"))
            .await
            .unwrap();

        // Async worker runs out-of-band; allow a short settle window.
        let mut saw_pending = false;
        let mut saw_indexed = false;
        for _ in 0..20 {
            let (pending, indexed, failed) = store.metadata.count_by_index_state(&tenant).unwrap();
            assert_eq!(failed, 0);
            if pending > 0 {
                saw_pending = true;
            }
            if indexed > 0 {
                saw_indexed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            saw_pending || saw_indexed,
            "chunk should appear in pending or indexed states"
        );
        assert!(
            saw_indexed,
            "async worker should eventually mark chunk indexed"
        );
    }

    #[tokio::test]
    async fn pending_chunks_are_recovered_by_worker_sweep() {
        let dir = tempdir().unwrap();
        let tenant = make_tenant();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 10,
            enable_dense_search: false,
            enable_hybrid_search: false,
            enable_async_indexing: true,
            async_index_poll_ms: 25,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        let chunk_id = store
            .add(make_chunk(&tenant, "sweep pending recovery"))
            .await
            .unwrap();

        // Wait until initial async indexing completes.
        for _ in 0..20 {
            let (_, indexed, failed) = store.metadata.count_by_index_state(&tenant).unwrap();
            assert_eq!(failed, 0);
            if indexed > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        store
            .metadata
            .mark_index_pending(&tenant, std::slice::from_ref(&chunk_id), current_time_ms())
            .unwrap();

        let mut recovered = false;
        for _ in 0..25 {
            let (pending, indexed, failed) = store.metadata.count_by_index_state(&tenant).unwrap();
            assert_eq!(failed, 0);
            if pending == 0 && indexed > 0 {
                recovered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(recovered, "worker sweep should re-index pending chunks");
    }

    #[tokio::test]
    async fn tenant_isolation() {
        let (store, _dir) = make_test_store();
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();

        let chunk = make_chunk(&tenant_a, "secret");
        let chunk_id = store.add(chunk).await.unwrap();

        // Tenant B cannot see tenant A's chunk
        let result = store.get(&tenant_b, &chunk_id).await.unwrap();
        assert!(result.is_none());

        // Search isolation
        let results = store.search(&tenant_b, "secret", 10).await.unwrap();
        assert!(results.is_empty());
    }

    /// Bug B defense-in-depth: if `tenant.segments` drops a finalized entry
    /// for any reason (observed in prod, unreliable to reproduce from a
    /// rollover race), `get_chunk` must still serve the read by opening the
    /// segment on demand AND it must repopulate the cache for next time.
    #[tokio::test]
    async fn get_chunk_recovers_when_segments_cache_loses_entry() {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 1, // force rollover so the first chunk finalizes
            wal_checkpoint_interval: 10,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        let tenant = make_tenant();

        // With segment_max_chunks=1, the second add triggers rollover, which
        // finalizes the first chunk's segment and registers its reader in
        // `tenant.segments`.
        let finalized_id = store
            .add(make_chunk(&tenant, "finalized bytes that must survive cache drift"))
            .await
            .unwrap();
        let _ = store
            .add(make_chunk(&tenant, "subsequent chunk forces rollover"))
            .await
            .unwrap();

        // Find the tenant store and the segment id that holds the first chunk.
        let meta = store
            .metadata
            .get(&tenant, &finalized_id)
            .unwrap()
            .expect("metadata row for finalized chunk");
        let tenant_store = store
            .tenants
            .read()
            .get(tenant.as_str())
            .cloned()
            .expect("tenant store must exist after an add");

        // Simulate the observed production failure: the reader for this
        // segment disappears from the cache.
        {
            let mut segments = tenant_store.segments.write();
            let removed = segments.remove(&meta.segment_id);
            assert!(
                removed.is_some(),
                "expected finalized reader for segment {} to be cached before removal",
                meta.segment_id
            );
        }

        // The read must still succeed — on-demand open from disk.
        let recovered = store
            .get(&tenant, &finalized_id)
            .await
            .expect("get_chunk must succeed via on-demand open");
        assert!(
            recovered.is_some(),
            "get_chunk returned None despite segment files existing on disk"
        );

        // And the cache must be repopulated so subsequent reads skip the
        // on-demand open.
        assert!(
            tenant_store.segments.read().contains_key(&meta.segment_id),
            "on-demand open must repopulate tenant.segments"
        );
    }

    #[tokio::test]
    async fn soft_delete() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();
        let chunk = make_chunk(&tenant, "to delete");

        let chunk_id = store.add(chunk).await.unwrap();
        let deleted = store.delete(&tenant, &chunk_id).await.unwrap();
        assert!(deleted);

        // Chunk no longer retrievable
        let result = store.get(&tenant, &chunk_id).await.unwrap();
        assert!(result.is_none());

        // Not in search results
        let results = store.search(&tenant, "delete", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn text_search_skips_crc_corrupted_active_chunk() {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 1,
            wal_checkpoint_interval: 10,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        let tenant = make_tenant();

        let healthy_id = store
            .add(make_chunk(&tenant, "healthy finalized chunk"))
            .await
            .unwrap();
        let corrupted_id = store
            .add(make_chunk(&tenant, "corrupted active chunk"))
            .await
            .unwrap();

        let warmup = store.get(&tenant, &corrupted_id).await.unwrap();
        assert!(warmup.is_some());

        let corrupt_meta = store
            .metadata
            .get(&tenant, &corrupted_id)
            .unwrap()
            .expect("corrupted chunk metadata");
        corrupt_segment_payload(dir.path(), &tenant, corrupt_meta.segment_id);

        let results = store.search(&tenant, "chunk", 10).await.unwrap();
        let result_ids = results
            .into_iter()
            .map(|chunk| chunk.chunk_id)
            .collect::<Vec<_>>();

        assert!(result_ids.contains(&healthy_id));
        assert!(!result_ids.contains(&corrupted_id));
    }

    #[tokio::test]
    async fn text_search_skips_crc_corrupted_finalized_chunk() {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 1,
            wal_checkpoint_interval: 10,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let tenant = make_tenant();
        let corrupted_id;
        let healthy_id;
        let corrupted_segment_id;
        let store = PersistentStore::open(config).unwrap();
        corrupted_id = store
            .add(make_chunk(&tenant, "corrupted finalized chunk"))
            .await
            .unwrap();
        healthy_id = store
            .add(make_chunk(&tenant, "healthy finalized chunk"))
            .await
            .unwrap();
        corrupted_segment_id = store
            .metadata
            .get(&tenant, &corrupted_id)
            .unwrap()
            .expect("corrupted chunk metadata")
            .segment_id;

        corrupt_segment_payload(dir.path(), &tenant, corrupted_segment_id);
        let results = store.search(&tenant, "chunk", 10).await.unwrap();
        let result_ids = results
            .into_iter()
            .map(|chunk| chunk.chunk_id)
            .collect::<Vec<_>>();

        assert!(result_ids.contains(&healthy_id));
        assert!(!result_ids.contains(&corrupted_id));
    }

    #[tokio::test]
    async fn hybrid_search_skips_crc_corrupted_active_chunk() {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 1,
            wal_checkpoint_interval: 10,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let mut store = PersistentStore::open(config).unwrap();
        let tenant = make_tenant();

        let embedder = Arc::new(MockEmbedder::new());
        let dense = Arc::new(DenseSearcher::with_embedder(
            embedder,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let hybrid = HybridSearcher::new(
            dense,
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: false,
                reranker: RerankerConfig {
                    mode: RerankerMode::Feature,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        store.hybrid_searcher = Some(Arc::new(hybrid));

        let healthy_id = store
            .add(make_chunk(&tenant, "healthy hybrid retrieval chunk"))
            .await
            .unwrap();
        let corrupted_id = store
            .add(make_chunk(&tenant, "corrupted hybrid retrieval chunk"))
            .await
            .unwrap();

        let warmup = store.get(&tenant, &corrupted_id).await.unwrap();
        assert!(warmup.is_some());

        let corrupt_meta = store
            .metadata
            .get(&tenant, &corrupted_id)
            .unwrap()
            .expect("corrupted chunk metadata");
        corrupt_segment_payload(dir.path(), &tenant, corrupt_meta.segment_id);

        let results = store.search(&tenant, "retrieval", 10).await.unwrap();
        let result_ids = results
            .into_iter()
            .map(|chunk| chunk.chunk_id)
            .collect::<Vec<_>>();

        assert!(result_ids.contains(&healthy_id));
        assert!(!result_ids.contains(&corrupted_id));
    }

    #[tokio::test]
    async fn persistence_across_restarts() {
        let dir = tempdir().unwrap();
        let tenant = make_tenant();
        let chunk_id;

        // First session: add chunk
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                ..Default::default()
            };
            let store = PersistentStore::open(config).unwrap();
            let chunk = make_chunk(&tenant, "persistent data");
            chunk_id = store.add(chunk).await.unwrap();

            // Drop triggers finalization
            drop(store);
        }

        // Second session: retrieve chunk
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                ..Default::default()
            };
            let store = PersistentStore::open(config).unwrap();
            let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

            // Chunk survives restart
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().text, "persistent data");
        }
    }

    #[tokio::test]
    async fn wal_recovery_after_crash() {
        let dir = tempdir().unwrap();
        let tenant = make_tenant();
        let chunk_id;

        // First session: add chunk but simulate crash (no finalization)
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                ..Default::default()
            };
            let store = PersistentStore::open(config).unwrap();
            let chunk = make_chunk(&tenant, "crash test data");
            chunk_id = store.add(chunk).await.unwrap();

            // Simulate crash: forget without drop (leak the store)
            std::mem::forget(store);
        }

        // Second session: should recover from WAL
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                ..Default::default()
            };
            let store = PersistentStore::open(config).unwrap();
            let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

            // Chunk recovered from WAL
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().text, "crash test data");
        }
    }

    #[tokio::test]
    async fn wal_recovery_rebuilds_task_side_tables() {
        let dir = tempdir().unwrap();
        let tenant = make_tenant();
        let metadata_path = dir.path().join("metadata.db");
        let task_id;
        let artifact_id;

        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                ..Default::default()
            };
            let store = PersistentStore::open(config).unwrap();

            let mut artifact = TaskArtifact::new_task_start(tenant.clone());
            artifact.project_id = ProjectId::new(Some("project_alpha".to_string()));
            artifact.goal = Some("Map the perturbation-responsive genes".to_string());
            artifact.dataset_refs = vec![crate::task_memory::DatasetRef {
                name: "rna_seq".to_string(),
                version: Some("v1".to_string()),
                description: None,
            }];
            task_id = artifact.task_id.clone();
            artifact_id = artifact.artifact_id.clone();

            store
                .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
                .await
                .unwrap();

            let conn = Connection::open(&metadata_path).unwrap();
            conn.execute("DELETE FROM artifact_links", []).unwrap();
            conn.execute("DELETE FROM task_datasets", []).unwrap();
            conn.execute("DELETE FROM task_entities", []).unwrap();
            conn.execute("DELETE FROM task_events", []).unwrap();
            conn.execute("DELETE FROM tasks", []).unwrap();
            conn.execute("DELETE FROM task_artifacts", []).unwrap();
            drop(conn);

            std::mem::forget(store);
        }

        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
                enable_dense_search: false,
                enable_hybrid_search: false,
                ..Default::default()
            };
            let store = PersistentStore::open(config).unwrap();

            let recovered = store
                .get_task_artifact(&tenant, &artifact_id)
                .await
                .unwrap();
            assert!(recovered.is_some());

            let artifacts = store.list_task_artifacts(&tenant, &task_id).await.unwrap();
            assert_eq!(artifacts.len(), 1);

            let chunk_ids = store
                .search_task_projection_chunk_ids(
                    &tenant,
                    &TaskSearchFilters {
                        task_id: Some(task_id.clone()),
                        ..Default::default()
                    },
                    10,
                )
                .await
                .unwrap();
            assert!(!chunk_ids.is_empty());
        }
    }

    #[tokio::test]
    async fn rerank_chunks_for_query_uses_hybrid_reranker_for_candidate_set() {
        let (mut store, _dir) = make_test_store();
        let tenant = make_tenant();
        let now_ms = current_time_ms();

        let mut older = make_chunk(&tenant, "alpha beta exact lexical match");
        older.timestamp_created = now_ms - 30 * 24 * 60 * 60 * 1000;
        let older_id = store.add(older).await.unwrap();

        let mut newer = make_chunk(&tenant, "alpha parameter note");
        newer.timestamp_created = now_ms;
        let newer_id = store.add(newer).await.unwrap();

        let embedder = Arc::new(MockEmbedder::new());
        let dense = Arc::new(DenseSearcher::with_embedder(
            embedder,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let hybrid = HybridSearcher::new(
            dense,
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: false,
                reranker: RerankerConfig {
                    mode: RerankerMode::Feature,
                    rrf_weight: 0.0,
                    recency_weight: 1.0,
                    recency_half_life_days: 7.0,
                    project_weight: 0.0,
                    type_weight: 0.0,
                    cross_encoder_weight: 0.0,
                },
                ..Default::default()
            },
        );
        store.hybrid_searcher = Some(Arc::new(hybrid));

        let ranked = store
            .rerank_chunks_for_query(&tenant, "alpha beta", &[older_id, newer_id.clone()], 2)
            .await
            .unwrap();

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0.chunk_id, newer_id);
    }

    #[tokio::test]
    async fn stats() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();

        store.add(make_chunk(&tenant, "doc 1")).await.unwrap();
        store.add(make_chunk(&tenant, "doc 2")).await.unwrap();
        let to_delete = store.add(make_chunk(&tenant, "doc 3")).await.unwrap();

        store.delete(&tenant, &to_delete).await.unwrap();

        let stats = store.stats(&tenant).await.unwrap();
        assert_eq!(stats.total_chunks, 3);
        assert_eq!(stats.deleted_chunks, 1);
    }

    #[tokio::test]
    async fn add_long_document_splits_into_multiple_chunks() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();
        let long_text = make_long_document();

        let _chunk_id = store.add(make_chunk(&tenant, &long_text)).await.unwrap();

        let stats = store.stats(&tenant).await.unwrap();
        assert!(stats.total_chunks > 1);
    }

    #[tokio::test]
    async fn feedback_adjusts_scores_in_persistent_store() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();

        let older = store
            .add(make_chunk(&tenant, "alpha retrieval note"))
            .await
            .unwrap();
        let newer = store
            .add(make_chunk(&tenant, "beta retrieval note"))
            .await
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        store
            .add_feedback(FeedbackEntry::new(
                tenant.clone(),
                "retrieval note",
                older.clone(),
                crate::store::RelevanceLabel::Relevant,
                now_ms,
            ))
            .await
            .unwrap();
        store
            .add_feedback(FeedbackEntry::new(
                tenant.clone(),
                "retrieval note",
                older.clone(),
                crate::store::RelevanceLabel::Relevant,
                now_ms,
            ))
            .await
            .unwrap();
        store
            .add_feedback(FeedbackEntry::new(
                tenant.clone(),
                "retrieval note",
                newer.clone(),
                crate::store::RelevanceLabel::Irrelevant,
                now_ms,
            ))
            .await
            .unwrap();
        store
            .add_feedback(FeedbackEntry::new(
                tenant.clone(),
                "retrieval note",
                newer.clone(),
                crate::store::RelevanceLabel::Irrelevant,
                now_ms,
            ))
            .await
            .unwrap();

        let ranked = store
            .search_with_scores(&tenant, "retrieval note", 10)
            .await
            .unwrap();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0.chunk_id, older);
    }

    /// Bug A: HNSW backfill. Simulates the production cold-start condition
    /// where metadata has chunks but the in-memory HNSW is empty (because
    /// the previous daemon never saved it). After calling
    /// `backfill_hnsw_for_cold_tenants`, the search must find chunks whose
    /// embeddings were previously missing.
    #[tokio::test]
    async fn backfill_hnsw_for_cold_tenants_reindexes_stranded_chunks() {
        use crate::embeddings::MockEmbedder;
        use crate::store::dense::{DenseSearchConfig, DenseSearcher};
        use crate::store::hybrid::{HybridConfig, HybridSearcher};

        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: true,
            enable_hybrid_search: true,
            enable_tiered_search: false, // keep the test simple
            ..Default::default()
        };
        let mut store = PersistentStore::open(config).unwrap();
        let embedder = Arc::new(MockEmbedder::new());
        let dense_searcher = Arc::new(DenseSearcher::with_embedder(
            Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let hybrid = HybridSearcher::new(
            Arc::clone(&dense_searcher),
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: false,
                ..Default::default()
            },
        );
        store.dense_searcher = Some(Arc::clone(&dense_searcher));
        store.hybrid_searcher = Some(Arc::new(hybrid));

        let tenant = make_tenant();
        let texts = [
            "alpha lifecycle overlay prototype",
            "bravo wal recovery idempotent replay",
            "charlie tiered cache invalidation",
        ];
        for text in &texts {
            Store::add(&store, make_chunk(&tenant, text))
                .await
                .unwrap();
        }
        // Sanity: search works while HNSW is warm.
        assert_eq!(dense_searcher.index_len(&tenant), texts.len());

        // Simulate the cold-start condition: swap in a brand new empty
        // dense searcher + hybrid. Metadata is unchanged; segments are on
        // disk; but HNSW has no entries — exactly the state we observed
        // on the production daemon after restart.
        let cold_dense = Arc::new(DenseSearcher::with_embedder(
            Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let cold_hybrid = HybridSearcher::new(
            Arc::clone(&cold_dense),
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: false,
                ..Default::default()
            },
        );
        store.dense_searcher = Some(Arc::clone(&cold_dense));
        store.hybrid_searcher = Some(Arc::new(cold_hybrid));

        // Before backfill: searching the cold HNSW returns nothing.
        assert_eq!(
            cold_dense.index_len(&tenant),
            0,
            "fresh dense searcher must start empty"
        );

        // Act.
        let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();

        // After backfill: HNSW has all three chunks, search finds them.
        assert!(
            stats.chunks_indexed >= texts.len(),
            "backfill must reindex all stranded chunks, got stats {:?}",
            stats
        );
        assert_eq!(stats.tenants_backfilled, 1);
        assert_eq!(cold_dense.index_len(&tenant), texts.len());

        let scored = store
            .search_with_scores(&tenant, "lifecycle", 10)
            .await
            .unwrap();
        assert!(
            !scored.is_empty(),
            "semantic search must return results after backfill"
        );
    }

    #[tokio::test]
    async fn backfill_hnsw_is_noop_when_dense_disabled() {
        let (store, _dir) = make_test_store();
        // make_test_store disables dense search; backfill must return
        // cleanly with zero work done.
        let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();
        assert_eq!(stats.tenants_backfilled, 0);
        assert_eq!(stats.chunks_indexed, 0);
    }

    /// Codex-reviewed regression: count-only heuristics (`index_len >=
    /// active_count`) silently skip stale tenants because HNSW's
    /// `next_id` counter never decrements on delete. Simulate:
    /// add 3 chunks, delete 2 (HNSW count still 3, active metadata 1),
    /// then add 2 new chunks while HNSW is empty (simulates a
    /// cold-restart mid-lifecycle). The stale tenant must still be
    /// backfilled.
    #[tokio::test]
    async fn backfill_hnsw_detects_staleness_via_per_chunk_membership_not_counts() {
        use crate::embeddings::MockEmbedder;
        use crate::store::dense::{DenseSearchConfig, DenseSearcher};
        use crate::store::hybrid::{HybridConfig, HybridSearcher};

        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: true,
            enable_hybrid_search: true,
            enable_tiered_search: false,
            ..Default::default()
        };
        let mut store = PersistentStore::open(config).unwrap();
        let embedder = Arc::new(MockEmbedder::new());
        let dense = Arc::new(DenseSearcher::with_embedder(
            Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let hybrid = HybridSearcher::new(
            Arc::clone(&dense),
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: false,
                ..Default::default()
            },
        );
        store.dense_searcher = Some(Arc::clone(&dense));
        store.hybrid_searcher = Some(Arc::new(hybrid));

        let tenant = make_tenant();
        let id1 = Store::add(&store, make_chunk(&tenant, "first old chunk"))
            .await
            .unwrap();
        let id2 = Store::add(&store, make_chunk(&tenant, "second old chunk"))
            .await
            .unwrap();
        let _id3 = Store::add(&store, make_chunk(&tenant, "third old chunk"))
            .await
            .unwrap();

        // Soft-delete two of the chunks. HNSW's mapping.next_id stays at
        // 3, but metadata only has one active row.
        assert!(store.delete(&tenant, &id1).await.unwrap());
        assert!(store.delete(&tenant, &id2).await.unwrap());

        // Now simulate a cold restart: swap in a fresh empty HNSW state.
        let cold_dense = Arc::new(DenseSearcher::with_embedder(
            Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let cold_hybrid = HybridSearcher::new(
            Arc::clone(&cold_dense),
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: false,
                ..Default::default()
            },
        );
        store.dense_searcher = Some(Arc::clone(&cold_dense));
        store.hybrid_searcher = Some(Arc::new(cold_hybrid));

        // Add one more chunk post-"restart" — this one lands in HNSW
        // normally. With the naive count heuristic, `hnsw_count = 1` and
        // `active_count = 2` so backfill WOULD run, but only because
        // the skew is in our favor. Before per-chunk membership the
        // heuristic could also have failed the other way.
        let id4 = Store::add(&store, make_chunk(&tenant, "new post-restart chunk"))
            .await
            .unwrap();
        assert_eq!(cold_dense.index_len(&tenant), 1);
        assert!(cold_dense.contains_chunk(&tenant, &id4));

        // The surviving-from-old-era chunk is id3. It must be missing
        // from HNSW currently.
        let _ = _id3; // only referenced for assertion symmetry

        // Act.
        let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();

        // Exactly the one surviving pre-restart chunk should have been
        // re-indexed; id4 is already there and gets skipped by the
        // per-chunk membership test.
        assert_eq!(
            stats.chunks_indexed, 1,
            "backfill should re-index only the one missing chunk, got {:?}",
            stats
        );
        assert_eq!(stats.tenants_backfilled, 1);
    }

    #[tokio::test]
    async fn backfill_hnsw_skips_tenants_whose_index_is_already_warm() {
        use crate::embeddings::MockEmbedder;
        use crate::store::dense::{DenseSearchConfig, DenseSearcher};
        use crate::store::hybrid::{HybridConfig, HybridSearcher};

        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: true,
            enable_hybrid_search: true,
            enable_tiered_search: false,
            ..Default::default()
        };
        let mut store = PersistentStore::open(config).unwrap();
        let embedder = Arc::new(MockEmbedder::new());
        let dense = Arc::new(DenseSearcher::with_embedder(
            Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let hybrid = HybridSearcher::new(
            Arc::clone(&dense),
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: false,
                ..Default::default()
            },
        );
        store.dense_searcher = Some(Arc::clone(&dense));
        store.hybrid_searcher = Some(Arc::new(hybrid));

        let tenant = make_tenant();
        Store::add(&store, make_chunk(&tenant, "already indexed"))
            .await
            .unwrap();

        let before = dense.index_len(&tenant);
        let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();
        let after = dense.index_len(&tenant);

        assert_eq!(before, after, "warm tenant must not be re-indexed");
        assert_eq!(stats.tenants_backfilled, 0);
        assert_eq!(stats.chunks_indexed, 0);
    }

    /// Regression test for the `next_segment_id` scan.
    ///
    /// Before the fix, `next_segment_id` only consulted loaded
    /// finalized segments, so a crash that left behind an unfinalized
    /// `seg_N/` directory (no `meta` file) was invisible to the next
    /// rotation. The segment writer would then call `create_dir_all` +
    /// `truncate(true)` on the same id and silently destroy the crashed
    /// segment's payload bytes.
    #[tokio::test]
    async fn next_segment_id_skips_over_orphan_directories() {
        let (store, temp) = make_test_store();
        let tenant = make_tenant();

        // Force creation of a real segment via a normal write. Go
        // through the `Store` trait explicitly so type inference picks
        // the right method.
        Store::add(&store, make_chunk(&tenant, "seed chunk"))
            .await
            .unwrap();
        let tenant_arc = store.get_or_create_tenant(tenant.as_str()).unwrap();

        let initial_id = tenant_arc.next_segment_id();

        // Manually create an orphan segment directory without a `meta`
        // file — this is exactly the state a mid-write crash leaves.
        let orphan_id = initial_id + 5;
        let segments_dir = temp
            .path()
            .join("tenants")
            .join(tenant.as_str())
            .join("segments");
        let orphan_dir = segments_dir.join(format!("seg_{:06}", orphan_id));
        fs::create_dir_all(&orphan_dir).unwrap();
        fs::write(orphan_dir.join("payload.bin"), b"crashed mid-write").unwrap();

        // The next id must be strictly greater than the orphan's id so
        // the next rotation cannot reuse (and overwrite) it.
        let next_id = tenant_arc.next_segment_id();
        assert!(
            next_id > orphan_id,
            "next_segment_id must skip over orphan dirs: got {} but orphan is {}",
            next_id,
            orphan_id
        );
    }

    /// Codex Phase 3 coverage gap: verify that a task/artifact write
    /// bumps the per-tenant warm-tier `memory_version`. The public
    /// write path is `Store::add_task_artifact`, which threads through
    /// the same `hybrid.index_batch` site that `Phase 3.5` hooked with
    /// `bump_tenant_memory_version`. If a future refactor accidentally
    /// takes artifact writes off the hybrid indexing path, the cache
    /// invalidation invariant silently breaks — this test pins it.
    #[tokio::test]
    async fn add_task_artifact_bumps_tenant_memory_version() {
        use crate::embeddings::MockEmbedder;
        use crate::retrieval::{RerankerConfig, RerankerMode};
        use crate::store::dense::{DenseSearchConfig, DenseSearcher};
        use crate::store::hybrid::{HybridConfig, HybridSearcher};
        use crate::task_memory::{build_task_projections, TaskArtifact};

        let (mut store, _dir) = make_test_store_hybrid_tiered();
        let hybrid = store.hybrid_searcher.as_ref().unwrap().clone();
        let tenant = make_tenant();

        // Seed the tiered searcher by issuing a search — this builds
        // the per-tenant warm tier lazily, which is the version
        // counter we want to observe.
        store
            .search_with_scores(&tenant, "seed probe", 1)
            .await
            .unwrap();

        let before = hybrid
            .tenant_memory_version(&tenant)
            .expect("tiered searcher must exist after a search call");

        // Drive a real task artifact write through Store::add_task_artifact.
        let mut artifact = TaskArtifact::new_task_start(tenant.clone());
        artifact.goal = Some("pin version-bump invariant for task writes".to_string());
        let projections = build_task_projections(&artifact);
        <PersistentStore as Store>::add_task_artifact(&store, artifact.clone(), projections)
            .await
            .unwrap();

        let after = hybrid
            .tenant_memory_version(&tenant)
            .expect("tiered searcher must still exist");

        assert!(
            after > before,
            "add_task_artifact must bump per-tenant memory_version: \
             before={} after={}",
            before,
            after
        );
        // Suppress unused-assignment warning.
        let _ = hybrid;

        // Avoid dead-code warning on the `store` shadow below.
        drop(store);

        fn make_test_store_hybrid_tiered() -> (PersistentStore, tempfile::TempDir) {
            let dir = tempdir().unwrap();
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 0,
                enable_dense_search: true,
                enable_hybrid_search: true,
                enable_tiered_search: true,
                ..Default::default()
            };
            let mut store = PersistentStore::open(config).unwrap();

            let embedder = Arc::new(MockEmbedder::new());
            let dense = Arc::new(DenseSearcher::with_embedder(
                embedder,
                DenseSearchConfig {
                    persist: false,
                    ..Default::default()
                },
            ));
            let hybrid = HybridSearcher::new(
                dense,
                None,
                HybridConfig {
                    enable_sparse: false,
                    enable_tiered: true,
                    reranker: RerankerConfig {
                        mode: RerankerMode::Feature,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            store.hybrid_searcher = Some(Arc::new(hybrid));
            (store, dir)
        }
    }

    /// Track C6: `PersistentStore::update_lifecycle_if_exists` must
    /// bump `tenant_memory_version` when the row was found AND hybrid
    /// is enabled, and MUST NOT bump when the row didn't exist. Pins
    /// the cache-invalidation contract that `memory.set_expiry`
    /// depends on so a later refactor can't silently take it off the
    /// bump path.
    #[tokio::test]
    async fn update_lifecycle_if_exists_bumps_only_on_match() {
        use crate::embeddings::MockEmbedder;
        use crate::retrieval::{RerankerConfig, RerankerMode};
        use crate::store::dense::{DenseSearchConfig, DenseSearcher};
        use crate::store::hybrid::{HybridConfig, HybridSearcher};
        use crate::types::LifecycleDelta;

        fn hybrid_store() -> (PersistentStore, tempfile::TempDir) {
            let dir = tempdir().unwrap();
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 0,
                enable_dense_search: true,
                enable_hybrid_search: true,
                enable_tiered_search: true,
                ..Default::default()
            };
            let mut store = PersistentStore::open(config).unwrap();
            let embedder = Arc::new(MockEmbedder::new());
            let dense = Arc::new(DenseSearcher::with_embedder(
                embedder,
                DenseSearchConfig {
                    persist: false,
                    ..Default::default()
                },
            ));
            let hybrid = HybridSearcher::new(
                dense,
                None,
                HybridConfig {
                    enable_sparse: false,
                    enable_tiered: true,
                    reranker: RerankerConfig {
                        mode: RerankerMode::Feature,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            store.hybrid_searcher = Some(Arc::new(hybrid));
            (store, dir)
        }

        let (store, _dir) = hybrid_store();
        let hybrid = store.hybrid_searcher.as_ref().unwrap().clone();
        let tenant = make_tenant();

        // Seed the tiered warm tier so tenant_memory_version is live.
        store
            .search_with_scores(&tenant, "seed probe", 1)
            .await
            .unwrap();

        // Add a chunk we can update.
        let id = <PersistentStore as Store>::add(
            &store,
            MemoryChunk::new(tenant.clone(), "target", ChunkType::Doc),
        )
        .await
        .unwrap();

        let v_after_add = hybrid
            .tenant_memory_version(&tenant)
            .expect("tiered searcher must exist after an add");

        // Matched update must return true AND bump the version.
        let updated = store
            .update_lifecycle_if_exists(
                &tenant,
                &id,
                &LifecycleDelta {
                    expires_at_ms: Some(Some(1_i64)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(updated, "existing row must report updated=true");
        let v_after_update = hybrid.tenant_memory_version(&tenant).unwrap_or(0);
        assert!(
            v_after_update > v_after_add,
            "matched update must bump tenant_memory_version: \
             before={v_after_add} after={v_after_update}"
        );

        // Unmatched update must return false AND leave the version alone.
        let bogus = ChunkId::new();
        let updated = store
            .update_lifecycle_if_exists(
                &tenant,
                &bogus,
                &LifecycleDelta {
                    expires_at_ms: Some(Some(1_i64)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!updated, "nonexistent row must report updated=false");
        let v_after_noop = hybrid.tenant_memory_version(&tenant).unwrap_or(0);
        assert_eq!(
            v_after_noop, v_after_update,
            "nonexistent-row update must NOT bump tenant_memory_version"
        );
    }

    /// Codex-review regression (v0.3.1) for the WAL recovery durability
    /// hole: recovery used to replay chunks into a fresh active
    /// `SegmentWriter`, insert metadata rows, then truncate the WAL —
    /// without ever finalizing the replayed active segment. A second
    /// crash after recovery but before the next rotation would strand
    /// metadata pointing at an unfinalized directory (no `meta` file,
    /// so startup skipped it) while the WAL was already empty. The
    /// recovery path now calls `finalize_active_segment()` before the
    /// truncate so everything the WAL described is durable.
    ///
    /// We emulate the failure mode by: (1) writing a chunk and letting
    /// the store rotate + shut down normally, (2) verifying that a
    /// second `PersistentStore::open` of the same directory can still
    /// read the chunk — i.e. the recovery-to-finalize path produced a
    /// real loadable segment.
    #[tokio::test]
    async fn recovery_finalizes_active_segment_before_wal_truncate() {
        let dir = tempdir().unwrap();

        // Phase A: write, then shut down cleanly.
        let tenant;
        let chunk_id;
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 0, // safety valve default
                enable_dense_search: false,
                enable_hybrid_search: false,
                ..Default::default()
            };
            let store = PersistentStore::open(config).unwrap();
            tenant = make_tenant();
            chunk_id = Store::add(&store, make_chunk(&tenant, "durability sentinel"))
                .await
                .unwrap();
            store.shutdown().unwrap();
        }

        // Phase B: reopen the store. If recovery had truncated the WAL
        // without finalizing the active segment, the chunk's metadata
        // would be unreadable. This should succeed.
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();

        let recovered = Store::get(&store, &tenant, &chunk_id).await.unwrap();
        assert!(
            recovered.is_some(),
            "after reopen, the durability sentinel must still be readable; \
             an unfinalized recovery segment would have lost it"
        );
        assert_eq!(recovered.unwrap().text, "durability sentinel");
    }
}
