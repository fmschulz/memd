//! Persistent store implementation
//!
//! Integrates segments, WAL, SQLite metadata, and tombstones.
//! Implements crash recovery via WAL replay on startup.
//! Uses hybrid search (dense + sparse) for retrieval.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, warn};

use super::dense::DenseSearcher;
use super::hybrid::{HybridConfig, HybridSearcher};
use super::metadata::{ChunkMetadata, MetadataStore, SqliteMetadataStore};
use crate::compaction::{CompactionConfig, CompactionMetrics, CompactionResult, CompactionRunner};
use crate::metrics::TieredMetrics;
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
use super::wal::{WalReader, WalRecordType, WalWriter};
use super::{Store, StoreStats};
use crate::embeddings::EmbeddingModel;
use crate::error::{MemdError, Result};
use crate::index::Bm25Index;
use crate::metrics::{IndexStats, MetricsCollector, QueryMetrics, TieredQueryMetrics};
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
}

impl Default for PersistentStoreConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            segment_max_chunks: 10_000,
            wal_checkpoint_interval: 100,
            enable_dense_search: true,
            enable_hybrid_search: true,
            enable_tiered_search: true,
            hybrid_config: None,
            embedding_model: EmbeddingModel::default(),
        }
    }
}

/// Persistent store with crash recovery
pub struct PersistentStore {
    config: PersistentStoreConfig,
    /// Per-tenant state
    tenants: RwLock<HashMap<String, Arc<TenantStore>>>,
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

impl PersistentStore {
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
            tenants: RwLock::new(HashMap::new()),
            metadata,
            dense_searcher,
            sparse_index,
            hybrid_searcher,
            metrics: Arc::new(MetricsCollector::default()),
            compaction_runner,
        };

        // Recover existing tenants
        store.discover_and_recover_tenants()?;

        Ok(store)
    }

    /// Get reference to metrics collector
    pub fn metrics(&self) -> &MetricsCollector {
        &self.metrics
    }

    /// Get shared metrics collector
    pub fn metrics_arc(&self) -> Arc<MetricsCollector> {
        Arc::clone(&self.metrics)
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
                if let Some(chunk) = self.get(tenant_id, &result.chunk_id).await? {
                    results.push((chunk, result.final_score));
                }
            }

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
                if let Some(tenant_id) = entry.file_name().to_str() {
                    info!(tenant_id, "recovering tenant");
                    let _ = self.get_or_create_tenant(tenant_id)?;
                }
            }
        }

        Ok(())
    }

    fn get_or_create_tenant(&self, tenant_id: &str) -> Result<Arc<TenantStore>> {
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

                            // Mark tombstone in segment
                            let mut segments = self.segments.write();
                            if let Some(reader) = segments.get_mut(&meta.segment_id) {
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
                WalRecordType::Checkpoint => {
                    // Checkpoint records are filtered out by records_for_recovery()
                    // but handle gracefully if encountered
                }
            }
        }

        info!(
            adds,
            deletes,
            skipped,
            tenant = %self.tenant_id,
            "WAL recovery complete"
        );

        // After successful recovery, truncate WAL to start fresh
        {
            let mut wal = self.wal.lock();
            wal.truncate()?;
        }

        Ok(())
    }

    fn next_segment_id(&self) -> u64 {
        let segments = self.segments.read();
        segments.keys().max().map(|id| id + 1).unwrap_or(1)
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
        let tenant_id_str = chunk.tenant_id.to_string();
        info!(text_len = chunk.text.len(), "[PERSISTENT_ADD] called");
        debug!(tenant_id = %tenant_id_str, "[ADD] step 1: getting tenant");
        let _tenant = self.get_or_create_tenant(&tenant_id_str)?;

        let mut chunks = super::split_for_add(chunk);
        if chunks.len() == 1 {
            return self
                .add_single_chunk(
                    chunks
                        .pop()
                        .ok_or_else(|| MemdError::StorageError("no chunk to add".into()))?,
                )
                .await;
        }

        info!(chunk_count = chunks.len(), "[CHUNKING] splitting document");

        let mut chunk_ids = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            chunk_ids.push(self.add_single_chunk(chunk).await?);
        }

        chunk_ids
            .into_iter()
            .next()
            .ok_or_else(|| MemdError::StorageError("no chunk id produced".into()))
    }

    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>> {
        let mut ids = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let id = self.add(chunk).await?;
            ids.push(id);
        }
        Ok(ids)
    }

    async fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<MemoryChunk>> {
        self.get_chunk(tenant_id, chunk_id).await
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
        self.hybrid_search(tenant_id, query, k).await
    }

    async fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        self.delete_chunk(tenant_id, chunk_id).await
    }

    async fn stats(&self, tenant_id: &TenantId) -> Result<StoreStats> {
        self.get_stats(tenant_id).await
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
}

impl PersistentStore {
    /// Add a single chunk without automatic chunking
    async fn add_single_chunk(&self, mut chunk: MemoryChunk) -> Result<ChunkId> {
        let tenant_id_str = chunk.tenant_id.to_string();
        debug!(tenant_id = %tenant_id_str, "[ADD] step 1: getting tenant");
        let tenant = self.get_or_create_tenant(&tenant_id_str)?;

        // Generate chunk ID
        let chunk_id = ChunkId::new();
        chunk.chunk_id = chunk_id.clone();
        debug!(chunk_id = %chunk_id, "[ADD] step 2: generated chunk_id");

        // Compute hash
        chunk.hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(chunk.text.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let timestamp = chunk.timestamp_created;

        // Serialize chunk for storage
        let payload = serde_json::to_vec(&chunk)
            .map_err(|e| MemdError::StorageError(format!("serialize chunk: {}", e)))?;
        debug!(
            payload_len = payload.len(),
            "[ADD] step 3: serialized payload"
        );

        // Write to WAL first (durability)
        debug!("[ADD] step 4: acquiring WAL lock");
        {
            let mut wal = tenant.wal.lock();
            debug!("[ADD] step 4b: acquired WAL lock, writing");
            wal.append_add(
                &tenant_id_str,
                &chunk_id.to_string(),
                timestamp,
                payload.clone(),
            )?;
        }
        debug!("[ADD] step 4c: WAL write complete");

        // Write to segment
        debug!("[ADD] step 5: creating active segment");
        tenant.get_or_create_active_segment(self.config.segment_max_chunks)?;
        let (segment_id, ordinal) = {
            debug!("[ADD] step 5b: acquiring segment lock");
            let mut active = tenant.active_segment.lock();
            debug!("[ADD] step 5c: acquired segment lock");
            let seg = active
                .as_mut()
                .ok_or_else(|| MemdError::StorageError("no active segment".into()))?;
            let ordinal = seg.writer.append_chunk(&payload)?;
            seg.chunk_count += 1;
            (seg.writer.id(), ordinal)
        };
        debug!(segment_id, ordinal, "[ADD] step 5d: segment write complete");

        // Write to metadata
        debug!("[ADD] step 6: writing metadata");
        let metadata = ChunkMetadata {
            chunk_id: chunk_id.clone(),
            tenant_id: chunk.tenant_id.clone(),
            project_id: chunk.project_id.as_option().map(|s| s.to_string()),
            segment_id,
            ordinal,
            chunk_type: chunk.chunk_type,
            status: chunk.status,
            timestamp_created: chunk.timestamp_created,
            hash: chunk.hash.clone(),
            source_uri: chunk.source.uri.clone(),
        };
        self.metadata.insert(&metadata)?;
        debug!("[ADD] step 6b: metadata write complete");

        // Index in hybrid searcher (handles both dense and sparse)
        debug!("[ADD] step 7: starting hybrid indexing");
        warn!(
            "[DEBUG] Indexing path check: hybrid_searcher={}, dense_searcher={}",
            self.hybrid_searcher.is_some(),
            self.dense_searcher.is_some()
        );
        if let Some(ref hybrid) = self.hybrid_searcher {
            debug!("[ADD] step 7b: calling hybrid.index_chunk");
            warn!(
                "[DEBUG] About to call hybrid.index_chunk for tenant={}, chunk_id={}, text_len={}",
                chunk.tenant_id,
                chunk_id,
                chunk.text.len()
            );
            if let Err(e) = hybrid
                .index_chunk(&chunk.tenant_id, &chunk_id, &chunk.text)
                .await
            {
                warn!(
                    chunk_id = %chunk_id,
                    error = %e,
                    "❌ [DEBUG] ERROR in hybrid.index_chunk - THIS IS WHY INDEXING FAILED"
                );
                // Don't fail the add - search will fall back to text matching
            } else {
                warn!(
                    "[DEBUG] ✅ hybrid.index_chunk succeeded for chunk_id={}",
                    chunk_id
                );
            }
            debug!("[ADD] step 7c: hybrid indexing complete");
        } else if let Some(ref searcher) = self.dense_searcher {
            debug!("[ADD] step 7d: falling back to dense-only");
            warn!(
                "[DEBUG] About to call dense_searcher.index_chunk for tenant={}, chunk_id={}",
                chunk.tenant_id, chunk_id
            );
            // Fallback to dense-only if hybrid not available
            if let Err(e) = searcher
                .index_chunk(&chunk.tenant_id, &chunk_id, &chunk.text)
                .await
            {
                warn!(
                    chunk_id = %chunk_id,
                    error = %e,
                    "❌ [DEBUG] ERROR in dense_searcher.index_chunk - THIS IS WHY INDEXING FAILED"
                );
            } else {
                warn!(
                    "[DEBUG] ✅ dense_searcher.index_chunk succeeded for chunk_id={}",
                    chunk_id
                );
            }
        } else {
            warn!("⚠️ [DEBUG] CRITICAL: Neither hybrid_searcher nor dense_searcher exists - NO INDEXING HAPPENING!");
        }

        // Check if we need checkpoint
        debug!("[ADD] step 8: checkpoint check");
        {
            let mut count = tenant.writes_since_checkpoint.lock();
            *count += 1;
            if *count >= self.config.wal_checkpoint_interval {
                let mut wal = tenant.wal.lock();
                wal.append_checkpoint(&tenant_id_str, timestamp)?;
                *count = 0;
            }
        }

        info!(tenant_id = %tenant_id_str, chunk_id = %chunk_id, segment_id, ordinal, "[ADD] step 9: COMPLETE - chunk added");
        Ok(chunk_id)
    }
}

impl PersistentStore {
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

        // Check finalized segments
        let segments = tenant.segments.read();
        let reader = match segments.get(&meta.segment_id) {
            Some(r) => r,
            None => return Ok(None),
        };

        let payload = reader.read_chunk(meta.ordinal)?;
        match payload {
            Some(bytes) => {
                let chunk: MemoryChunk = serde_json::from_slice(&bytes)
                    .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
                Ok(Some(chunk))
            }
            None => Ok(None), // Tombstoned
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

        let tenant_str = tenant_id.to_string();
        let tenant = match self.tenants.read().get(&tenant_str) {
            Some(t) => Arc::clone(t),
            None => return Ok(Vec::new()),
        };

        let segments = tenant.segments.read();
        let mut results = Vec::new();

        for meta in metadata_list {
            if meta.status == ChunkStatus::Deleted {
                continue;
            }

            // Try active segment first
            if let Some(bytes) = tenant.read_from_active_segment(meta.segment_id, meta.ordinal)? {
                if let Ok(chunk) = serde_json::from_slice::<MemoryChunk>(&bytes) {
                    // Basic text match
                    if query.is_empty() || chunk.text.to_lowercase().contains(&query.to_lowercase())
                    {
                        results.push(chunk);
                        if results.len() >= k {
                            break;
                        }
                    }
                }
                continue;
            }

            // Try finalized segments
            if let Some(reader) = segments.get(&meta.segment_id) {
                if let Some(bytes) = reader.read_chunk(meta.ordinal)? {
                    if let Ok(chunk) = serde_json::from_slice::<MemoryChunk>(&bytes) {
                        // Basic text match
                        if query.is_empty()
                            || chunk.text.to_lowercase().contains(&query.to_lowercase())
                        {
                            results.push(chunk);
                            if results.len() >= k {
                                break;
                            }
                        }
                    }
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
            let mut results = Vec::with_capacity(hybrid_results.len());
            for result in hybrid_results {
                if let Some(chunk) = self.get(tenant_id, &result.chunk_id).await? {
                    results.push((chunk, result.final_score));
                }
            }
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
                if let Some(chunk) = self.get(tenant_id, &result.chunk_id).await? {
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

        // Update tombstone in segment
        {
            let mut segments = tenant.segments.write();
            if let Some(reader) = segments.get_mut(&meta.segment_id) {
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
    use crate::types::ChunkType;
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
}
