//! Persistent store implementation
//!
//! Integrates segments, WAL, SQLite metadata, and tombstones.
//! Implements crash recovery via WAL replay on startup.
//! Uses hybrid search (dense + sparse) for retrieval.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use rusqlite::{Connection as ProbeConnection, OpenFlags};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::dense::DenseSearcher;
use super::hybrid::{ChunkMetaForRerank, HybridConfig, HybridSearchResult, HybridSearcher};
use super::metadata::{ChunkMetadata, MetadataStore, SqliteMetadataStore};
use crate::compaction::{CompactionConfig, CompactionMetrics, CompactionResult, CompactionRunner};
use crate::metrics::TieredMetrics;
use crate::store::{
    apply_feedback_scores, FeedbackConfig, FeedbackEntry, OutcomeEvent, RetrievalEpisode,
    RetrievalEpisodeId, RetrievalEpisodeItem,
};
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
use super::usage::{usage_ledger_enabled, usage_retention_ms, UsageEvent};
use super::wal::{TaskArtifactWalPayload, WalReader, WalRecord, WalRecordType, WalWriter};
use super::writer_lock::{acquire_writer_lock_capped, WriterLockGuard};
use super::{
    rank_candidate_chunks, score_candidate_chunk, ExternalMutationOutcome, RywProbeStats, Store,
    StoreHealthSnapshot, StoreStats,
};
use crate::embeddings::{CandleModel, EmbeddingModel};
use crate::error::{MemdError, Result};
use crate::index::{Bm25Index, SparseIndex};
use crate::metrics::{IndexStats, MetricsCollector, QueryMetrics, TieredQueryMetrics};
use crate::retrieval::RerankerMode;
use crate::types::lifecycle::{LifecycleDelta, ResolvedChunk};
use crate::types::{ChunkId, ChunkStatus, MemoryChunk, TenantId};

mod indexing;
mod lifecycle;
mod read;
mod recovery;
mod retrieval;
mod write;

use indexing::{
    await_index_ack, run_async_index_job, run_canonical_text_backfill, run_hnsw_backfill,
    sparse_self_heal_enabled, sweep_pending_index_jobs,
};

/// Configuration for persistent store
#[derive(Debug, Clone)]
pub struct PersistentStoreConfig {
    /// Base data directory
    pub data_dir: PathBuf,
    /// Open without writer privileges.
    ///
    /// Read-only mode does not acquire the process-wide writer flock,
    /// avoids disk mutations outside SQLite metadata side files, makes
    /// mutating APIs return `MemdError::ReadOnlyStore`, and serves
    /// WAL-pending chunks from an in-memory overlay instead of replaying
    /// and truncating WAL.
    pub read_only: bool,
    /// Optional upper bound for writer-lock acquisition.
    pub writer_lock_timeout_cap: Option<Duration>,
    /// Maximum chunks per segment before rotation
    pub segment_max_chunks: u32,
    /// Minimum chunks in an active segment for graceful shutdown / Drop
    /// to finalize it. Active segments below this threshold are left
    /// unfinalized between invocations and recovered via WAL replay on
    /// the next startup; they continue to grow across runs until they
    /// cross either this threshold (on a graceful exit) or
    /// `segment_max_chunks` (on rotation). Prevents the "5,000 tiny
    /// segments" pathology on CLI workloads where each invocation
    /// wrote 1-2 chunks. The WAL durability barrier inside
    /// `recover_from_wal` ignores this gate so recovered chunks always
    /// land in a finalized segment before WAL truncation.
    pub min_finalize_chunks: u32,
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
    ///
    /// Vestigial ONNX-era selector (retained for config compatibility); the
    /// live dense embedder is the Candle backend selected via `candle_model`.
    pub embedding_model: EmbeddingModel,
    /// Candle embedder model actually loaded for dense search. Set from the
    /// `--embedding-model` CLI choice.
    pub candle_model: CandleModel,
    /// Enable async/background indexing of newly added chunks
    pub enable_async_indexing: bool,
    /// Max pending chunks processed per async indexer tick
    pub async_index_batch_size: usize,
    /// Poll interval for async indexer in milliseconds
    pub async_index_poll_ms: u64,
    /// Bound the dense search-path lock waits so a contended search fails
    /// fast with `IndexBusy` instead of parking its thread behind a long
    /// index write hold. Off by default: single-shot CLI processes should
    /// wait out in-process repairs/bulk inserts rather than error. The
    /// warm worker enables it (its event loop must never park) via
    /// `apply_warm_worker_availability_defaults`.
    pub bounded_search_locks: bool,
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
            .unwrap_or(false);
        let backfill_canonical_text_on_startup =
            std::env::var("MEMD_BACKFILL_CANONICAL_TEXT_ON_STARTUP")
                .ok()
                .map(|v| {
                    let normalized = v.trim().to_ascii_lowercase();
                    matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
                })
                .unwrap_or(false);

        Self {
            data_dir: PathBuf::from("data"),
            read_only: false,
            writer_lock_timeout_cap: None,
            segment_max_chunks: 10_000,
            // 256 ≈ minutes of typical write rate. Below this we leave
            // the active segment unfinalized between graceful shutdowns
            // so consecutive CLI runs don't each create a near-empty
            // segment dir. Tuned to keep finalized segments large
            // enough that segment lookup cost stays bounded, while not
            // delaying durability for any meaningful workload — the WAL
            // already persists every chunk synchronously.
            min_finalize_chunks: 256,
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
            candle_model: CandleModel::default(),
            enable_async_indexing,
            async_index_batch_size,
            async_index_poll_ms,
            bounded_search_locks: false,
            backfill_hnsw_on_startup,
            backfill_canonical_text_on_startup,
        }
    }
}

/// Search-lock budget applied to worker processes when
/// `bounded_search_locks` is enabled: an order of magnitude above ordinary
/// insert holds (micro/millisecond scale) while keeping the worst-case
/// event-loop stall small under a repair-length write hold.
const WORKER_SEARCH_LOCK_BUDGET_MS: u64 = 50;

impl PersistentStoreConfig {
    /// Availability defaults for a warm-worker process.
    ///
    /// The worker's event loop and its command futures share one task, so a
    /// synchronous add-indexing path that parks on the dense index write
    /// lock (held by a repair or bulk insert for minutes) froze the whole
    /// worker — accept loop, ping, everything — until clients timed out.
    /// Async indexing acknowledges writes after WAL + metadata and lets the
    /// background indexer absorb the lock wait, so the worker defaults it
    /// on. An operator who explicitly set `MEMD_ASYNC_INDEXING` (either
    /// way) keeps that choice: pass the raw env value; `None` means unset.
    /// Explicitly disabling it on a worker is warned about, because the
    /// write path then indexes synchronously on the event-loop task again
    /// and a long index write hold can freeze the worker for its duration.
    pub fn apply_warm_worker_availability_defaults(&mut self, async_indexing_env: Option<&str>) {
        // The worker's searches must never park its event-loop thread
        // behind an index write hold; contended reads fail fast with a
        // busy reply and clients fall back to the cold path.
        self.bounded_search_locks = true;
        match async_indexing_env {
            None => self.enable_async_indexing = true,
            Some(_) if !self.enable_async_indexing => {
                warn!(
                    "MEMD_ASYNC_INDEXING is explicitly disabled for this warm worker: \
                     adds index synchronously on the event loop, so a long dense-index \
                     write hold (repair/bulk insert) can freeze the worker for its duration"
                );
            }
            Some(_) => {}
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
    /// Monotonic counter bumped before each PersistentStore-owned write.
    write_epoch: Arc<AtomicU64>,
    /// Warn-only detector for metadata.db changes not attributable to this store.
    external_mutation_probe: Option<ExternalMutationProbe>,
    /// Shared single-flight state + telemetry for store-owned HNSW repairs.
    repair_state: Arc<HnswRepairState>,
    /// JoinHandle of the most recent store-owned HNSW repair, aborted on
    /// shutdown/Drop so a repair can never race the dense index-save.
    repair_task: Mutex<Option<JoinHandle<()>>>,
    /// Last time this store attempted a usage-ledger retention sweep.
    usage_sweep_last_ms: AtomicI64,
    /// Process-wide exclusive writer lock. Last field so it drops last.
    _writer_lock: Option<WriterLockGuard>,
}

/// Durable boundaries inside one journaled candidate payload write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidatePersistenceStage {
    WalAppended,
    MetadataInserted,
}

/// Per-tenant storage state
struct TenantStore {
    tenant_id: String,
    base_dir: PathBuf,
    read_only: bool,
    /// Current active segment writer (None if read-only)
    active_segment: Mutex<Option<ActiveSegment>>,
    /// Loaded segment readers
    segments: RwLock<HashMap<u64, SegmentReader>>,
    /// WAL writer (absent in read-only mode)
    wal: Mutex<Option<WalWriter>>,
    /// In-memory payloads from WAL Add records not yet finalized.
    wal_overlay: RwLock<HashMap<ChunkId, Vec<u8>>>,
    /// Counter for WAL checkpoint
    writes_since_checkpoint: Mutex<u32>,
    /// Max chunks per segment (for rotation)
    segment_max_chunks: u32,
    /// Min active-segment chunk count below which shutdown/Drop will
    /// skip finalization. See `PersistentStoreConfig::min_finalize_chunks`.
    min_finalize_chunks: u32,
    /// Mirror of `PersistentStoreConfig::wal_checkpoint_interval` so
    /// `finalize_active_segment_if_above_threshold` can disable the
    /// gate whenever checkpointing is on. With checkpointing enabled,
    /// the WAL may have been truncated past the unfinalized segment's
    /// records, so the segment MUST be finalized on shutdown or its
    /// chunks become unrecoverable.
    wal_checkpoint_interval: u32,
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

/// Result of physically rewriting finalized segment files for a tenant.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SegmentRewriteResult {
    /// Finalized segment directories compacted or removed.
    pub segments_rewritten: usize,
    /// Old segment directories removed because they contained no live rows.
    pub segments_removed: usize,
    /// Live chunk payloads copied into replacement segment files.
    pub chunks_moved: usize,
    /// Bytes occupied by old rewritten segment directories.
    pub bytes_before: u64,
    /// Bytes occupied by replacement segment directories.
    pub bytes_after: u64,
    /// Best-effort byte delta from old segment directories to replacements.
    pub bytes_reclaimed: u64,
    /// Non-fatal cleanup warnings, typically stale old segment directories
    /// that could not be removed after metadata had already moved.
    pub warnings: Vec<String>,
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

/// Foreground wait budget for an on-probe HNSW repair. After this elapses
/// the warm request is served anyway and the store-owned repair keeps
/// running in the background. Kept well under the warm client timeout so a
/// detected external mutation can never stall a request for minutes.
const HNSW_REPAIR_FOREGROUND_BUDGET: Duration = Duration::from_millis(1500);

/// Shared, single-flight state for store-owned HNSW repairs, shared by the
/// startup backfill and on-probe repairs so the two can never index the same
/// `missing` set concurrently (`run_hnsw_backfill` snapshots `missing` before
/// inserting, so overlapping runs would double-index).
///
/// `inner` is a brief-hold bookkeeping lock (never held across the backfill
/// await). `pending` re-arms the running repair: when a probe detects a fresh
/// external mutation while a repair is already running, the in-flight repair's
/// metadata snapshot may predate that write, so we flag that one more pass is
/// required rather than dropping the coalesced signal (the probe has already
/// advanced its `data_version`, so no later probe would re-detect it).
#[derive(Debug, Default)]
struct HnswRepairState {
    inner: Mutex<RepairBookkeeping>,
    /// Count of completed probe-triggered repair passes, surfaced via `warm
    /// status`. Startup backfills are not counted as repairs.
    repairs: AtomicU64,
}

#[derive(Debug, Default)]
struct RepairBookkeeping {
    /// True while a repair task owns the single-flight slot.
    in_flight: bool,
    /// True when an external mutation was observed during an in-flight repair
    /// and a follow-up pass is required to cover writes added after its
    /// snapshot.
    pending: bool,
}

impl HnswRepairState {
    /// Try to claim the single-flight slot. Returns `true` if the caller
    /// became the owner and must spawn the repair; `false` if a repair is
    /// already running (a probe additionally re-arms `pending` so the running
    /// repair does a follow-up pass).
    fn try_begin_or_arm(&self, kind: RepairKind) -> bool {
        let mut g = self.inner.lock();
        if g.in_flight {
            if kind == RepairKind::Probe {
                g.pending = true;
            }
            false
        } else {
            g.in_flight = true;
            g.pending = false;
            true
        }
    }

    /// Called by the repair task after each pass. Returns `true` if another
    /// pass is required (an external mutation was observed mid-pass); returns
    /// `false` after releasing the slot when no follow-up is pending. All
    /// transitions are under the same lock as `try_begin_or_arm`, so a probe
    /// that arms `pending` after the slot is released instead becomes the next
    /// owner — no signal is lost.
    fn finish_or_continue(&self) -> bool {
        let mut g = self.inner.lock();
        if g.pending {
            g.pending = false;
            true
        } else {
            g.in_flight = false;
            false
        }
    }

    fn repair_in_progress(&self) -> bool {
        self.inner.lock().in_flight
    }
}

/// Panic-safety reset for the single-flight slot. The repair task defuses it
/// (`armed = false`) on normal exit, where `finish_or_continue` has already
/// released the slot; if the task panics or is aborted mid-pass the guard runs
/// and frees the slot so future repairs aren't wedged.
struct RepairInFlightGuard {
    state: Arc<HnswRepairState>,
    armed: bool,
}

impl Drop for RepairInFlightGuard {
    fn drop(&mut self) {
        if self.armed {
            let mut g = self.state.inner.lock();
            g.in_flight = false;
            g.pending = false;
        }
    }
}

/// Why a repair was scheduled. Only probe-triggered repairs bump the
/// `repairs` telemetry counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepairKind {
    Startup,
    Probe,
}

/// Result of trying to schedule a store-owned HNSW repair.
enum RepairSchedule {
    /// This call won the single-flight race and spawned the repair. The
    /// receiver resolves to `true` on success / `false` on error once the
    /// repair finishes.
    Scheduled(oneshot::Receiver<bool>),
    /// A repair was already running; this call scheduled nothing.
    AlreadyInFlight,
    /// Nothing to schedule: no async runtime (sync test context).
    Skipped,
}

struct IndexJob {
    tenant_id: TenantId,
    chunk_ids: Vec<ChunkId>,
    index_rows: Vec<(ChunkId, String)>,
    /// When present, signalled after the job's rows are indexed (`Ok`) or
    /// marked failed (`Err`). Write handlers hold their acknowledgement on
    /// this so "add returned" keeps implying "chunk is searchable" under
    /// the async lane — awaiting a channel, not parking a thread, so a
    /// warm worker's event loop stays responsive while the indexer works.
    completion: Option<oneshot::Sender<std::result::Result<(), String>>>,
}

struct ExternalMutationProbe {
    connection: Mutex<ProbeConnection>,
    snapshot: Mutex<ExternalMutationSnapshot>,
    write_epoch: Arc<AtomicU64>,
    checks: AtomicU64,
    external_detected: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct ExternalMutationSnapshot {
    last_data_version: i64,
    last_write_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalMutationCheck {
    Clean,
    OwnWrites,
    External {
        prev_data_version: i64,
        data_version: i64,
    },
}

impl ExternalMutationProbe {
    fn open(path: &Path, write_epoch: Arc<AtomicU64>) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY;
        let connection = ProbeConnection::open_with_flags(path, flags)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let last_data_version = Self::query_data_version(&connection)?;
        let last_write_epoch = write_epoch.load(Ordering::Acquire);
        Ok(Self {
            connection: Mutex::new(connection),
            snapshot: Mutex::new(ExternalMutationSnapshot {
                last_data_version,
                last_write_epoch,
            }),
            write_epoch,
            checks: AtomicU64::new(0),
            external_detected: AtomicU64::new(0),
        })
    }

    fn check(&self) -> Result<ExternalMutationCheck> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        let data_version = {
            let connection = self.connection.lock();
            Self::query_data_version(&connection)?
        };
        // Ordering caveat: `ensure_writable` bumps the epoch before a
        // PersistentStore operation's first SQLite commit. One probe
        // round can therefore observe the epoch bump, classify
        // `OwnWrites`, and snapshot the new epoch while that own commit
        // is still in flight. The next round then sees `data_version`
        // change with an unchanged epoch and misclassifies it as
        // `External`: a one-round false positive caused by an in-flight
        // own write. Symmetrically, a concurrent own write can mask an
        // external commit for one round. Both cases are acceptable: the
        // probe only drives telemetry and index refresh/repair, so a
        // false positive wastes repair work but never affects
        // correctness. `PRAGMA data_version` changes only when another
        // connection commits. The metadata pool's 16 connections are
        // "another connection" from this probe connection's
        // perspective, which is why epoch comparison is still useful.
        let write_epoch = self.write_epoch.load(Ordering::Acquire);
        let mut snapshot = self.snapshot.lock();
        let check = if data_version == snapshot.last_data_version {
            ExternalMutationCheck::Clean
        } else if write_epoch != snapshot.last_write_epoch {
            ExternalMutationCheck::OwnWrites
        } else {
            self.external_detected.fetch_add(1, Ordering::Relaxed);
            ExternalMutationCheck::External {
                prev_data_version: snapshot.last_data_version,
                data_version,
            }
        };
        *snapshot = ExternalMutationSnapshot {
            last_data_version: data_version,
            last_write_epoch: write_epoch,
        };
        Ok(check)
    }

    fn stats(&self) -> RywProbeStats {
        // `repairs` / `repair_in_progress` are owned by `HnswRepairState`
        // and filled in by `PersistentStore::ryw_probe_stats`.
        RywProbeStats {
            checks: self.checks.load(Ordering::Relaxed),
            external_detected: self.external_detected.load(Ordering::Relaxed),
            ..RywProbeStats::default()
        }
    }

    fn query_data_version(connection: &ProbeConnection) -> Result<i64> {
        Ok(connection.pragma_query_value(None, "data_version", |row| row.get(0))?)
    }
}

impl PersistentStore {
    /// Base data directory backing this store (the resolved
    /// `~/.memd/data`-style path). Used to locate side ledgers such as
    /// the central per-chunk hit log.
    pub fn data_dir(&self) -> &Path {
        &self.config.data_dir
    }

    /// Borrow the hybrid searcher when hybrid retrieval is enabled.
    pub fn hybrid(&self) -> Option<&HybridSearcher> {
        self.hybrid_searcher.as_deref()
    }

    /// Borrow the sparse index when sparse retrieval is enabled.
    pub fn sparse_index(&self) -> Option<&Bm25Index> {
        self.sparse_index.as_deref()
    }

    /// Whether a persistent sparse index exists even when this handle did
    /// not open it. Consolidation recovery handles intentionally disable
    /// search construction, so they must defer physical sparse cleanup when
    /// an index is present for a later hybrid-enabled writer.
    pub(crate) fn sparse_index_exists_on_disk(&self) -> bool {
        self.config.data_dir.join("sparse_index").exists()
    }

    /// Borrow the metadata store.
    pub fn metadata(&self) -> &SqliteMetadataStore {
        self.metadata.as_ref()
    }

    pub(crate) fn ensure_writable(&self, op: &'static str) -> Result<()> {
        if self.config.read_only {
            Err(MemdError::ReadOnlyStore { op: op.to_string() })
        } else {
            // Write-epoch invariant for the RYW probe: every mutating
            // entry point calls `ensure_writable` as its first statement,
            // so this release-store happens before that operation's
            // first SQLite commit. That ordering can produce one-round
            // telemetry mistakes: a probe may snapshot this epoch before
            // the commit lands, then classify the next `data_version`
            // change as external even though it was our in-flight write;
            // a concurrent own write can also mask an external commit
            // for one round. The probe only triggers telemetry and
            // index refresh/repair, so false positives cost extra work
            // and false negatives delay repair by one round; neither
            // changes store correctness.
            self.write_epoch.fetch_add(1, Ordering::Release);
            Ok(())
        }
    }

    pub fn is_read_only(&self) -> bool {
        self.config.read_only
    }

    /// Open a short-lived writer used by read-only session-start commands to
    /// reconcile stale consolidation journals. Search/index construction is
    /// disabled: promoted rows remain index-pending for the normal writer,
    /// while metadata and WAL recovery can complete without loading a model.
    pub(crate) fn open_consolidation_recovery_writer(&self) -> Result<Self> {
        let mut config = self.config.clone();
        config.read_only = false;
        config.writer_lock_timeout_cap = Some(Duration::from_millis(500));
        config.enable_dense_search = false;
        config.enable_hybrid_search = false;
        config.enable_tiered_search = false;
        config.enable_async_indexing = false;
        config.backfill_hnsw_on_startup = false;
        config.backfill_canonical_text_on_startup = false;
        Self::open(config)
    }

    /// Open or create persistent store
    pub fn open(config: PersistentStoreConfig) -> Result<Self> {
        let read_only_missing_data_dir = config.read_only && !config.data_dir.exists();
        if !read_only_missing_data_dir {
            std::fs::create_dir_all(&config.data_dir)?;
            // The store holds every tenant's memory in plaintext (metadata.db,
            // WAL, segments). Restrict the data dir to the owner so another
            // local user cannot read it directly, matching the 0600/0700
            // hardening already applied to the warm-worker socket. Best-effort:
            // a failure here must not block opening the store.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &config.data_dir,
                    std::fs::Permissions::from_mode(0o700),
                );
            }
        }
        let writer_lock = if config.read_only {
            None
        } else {
            Some(acquire_writer_lock_capped(
                &config.data_dir,
                config.writer_lock_timeout_cap,
            )?)
        };

        // Open global metadata database
        let metadata_path = config.data_dir.join("metadata.db");
        let metadata = if read_only_missing_data_dir {
            Arc::new(SqliteMetadataStore::open_in_memory()?)
        } else {
            Arc::new(SqliteMetadataStore::open(&metadata_path)?)
        };
        let usage_sweep_initial_ms = if !config.read_only && usage_ledger_enabled() {
            let now = current_time_ms();
            let cutoff = now.saturating_sub(usage_retention_ms());
            if let Err(error) = metadata.sweep_usage_events_before(cutoff) {
                debug!(error = %error, "usage ledger startup sweep failed");
            }
            now
        } else {
            0
        };
        let write_epoch = Arc::new(AtomicU64::new(0));

        // Initialize dense searcher if enabled
        let dense_searcher = if config.enable_dense_search {
            use super::dense::DenseSearchConfig;

            let mut dense_config = DenseSearchConfig {
                model: config.candle_model,
                ..DenseSearchConfig::default()
            };
            // Propagate the model-switch migration opt-in so a dimension
            // mismatch re-embeds from segments instead of hard-erroring.
            dense_config.hnsw.backfill_hnsw_on_startup = config.backfill_hnsw_on_startup;
            if config.bounded_search_locks {
                dense_config.hnsw.search_lock_budget_ms = Some(WORKER_SEARCH_LOCK_BUDGET_MS);
            }
            match DenseSearcher::new(dense_config) {
                Ok(searcher) => {
                    let searcher = searcher
                        .with_base_path(config.data_dir.clone())
                        .with_read_only(config.read_only);
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
            if config.read_only {
                match Bm25Index::with_path_read_only(sparse_path) {
                    Ok(Some(index)) => Some(Arc::new(index)),
                    Ok(None) => None,
                    Err(e) => {
                        warn!(
                            error = %e,
                            "failed to initialize read-only sparse index, hybrid search disabled"
                        );
                        None
                    }
                }
            } else {
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
            }
        } else {
            None
        };

        // Initialize hybrid searcher if both dense and sparse available (or just dense)
        let hybrid_searcher = if let (true, Some(dense_searcher)) =
            (config.enable_hybrid_search, dense_searcher.as_ref())
        {
            let mut hybrid_config = config.hybrid_config.clone().unwrap_or_default();
            // Apply tiered search configuration
            hybrid_config.enable_tiered = config.enable_tiered_search;
            let hybrid = HybridSearcher::new(
                Arc::clone(dense_searcher),
                sparse_index.clone(),
                hybrid_config,
            );
            Some(Arc::new(hybrid))
        } else {
            None
        };

        // Initialize compaction runner
        let compaction_runner = Some(CompactionRunner::new(CompactionConfig::default()));

        let mut store = Self {
            config,
            tenants: Arc::new(RwLock::new(HashMap::new())),
            metadata,
            dense_searcher,
            sparse_index,
            hybrid_searcher,
            metrics: Arc::new(MetricsCollector::default()),
            compaction_runner,
            async_indexer: None,
            write_epoch,
            external_mutation_probe: None,
            repair_state: Arc::new(HnswRepairState::default()),
            repair_task: Mutex::new(None),
            usage_sweep_last_ms: AtomicI64::new(usage_sweep_initial_ms),
            _writer_lock: writer_lock,
        };

        // Recover existing tenants
        store.discover_and_recover_tenants()?;

        // Baseline the external-mutation probe only after startup recovery.
        // Recovery can replay WAL records, update metadata, and finalize
        // segments using the store's own connections; snapshotting before that
        // makes the first warm-worker request misclassify startup recovery as
        // an external writer and run expensive repair work on the request path.
        if !store.config.read_only {
            store.external_mutation_probe =
                match ExternalMutationProbe::open(&metadata_path, Arc::clone(&store.write_epoch)) {
                    Ok(probe) => Some(probe),
                    Err(error) => {
                        warn!(
                            path = %metadata_path.display(),
                            error = %error,
                            "RYW external mutation probe unavailable"
                        );
                        None
                    }
                };
        }

        let async_indexer = store.start_async_indexer_if_enabled();
        store.async_indexer = async_indexer;

        if !store.config.read_only && store.config.backfill_hnsw_on_startup {
            store.spawn_startup_hnsw_backfill();
        }
        if !store.config.read_only && store.config.backfill_canonical_text_on_startup {
            store.spawn_startup_canonical_backfill();
        }
        // Sparse self-heal trigger. Unlike the opt-in HNSW startup
        // backfill, this check is cheap (one doc-count query per tenant)
        // and only schedules the shared single-flight repair when a
        // tenant is actually degraded: active metadata rows but an empty
        // sparse index — the state a crash leaves behind after the
        // tantivy directory is lost, which would otherwise silently
        // downgrade hybrid search to dense-only forever.
        if !store.config.read_only && sparse_self_heal_enabled() && store.any_tenant_sparse_cold() {
            warn!("sparse index empty for tenant(s) with active chunks — scheduling rebuild");
            store.spawn_startup_hnsw_backfill();
        }

        Ok(store)
    }

    /// Schedule a one-shot background task that re-indexes any tenants
    /// whose HNSW state is colder than their metadata. No-op when no
    /// Tokio runtime is available (e.g., sync test contexts) — callers
    /// can invoke `backfill_hnsw_for_cold_tenants` explicitly in that case.
    fn spawn_startup_hnsw_backfill(&self) {
        // Routed through the shared single-flight scheduler so a startup
        // backfill and an on-probe repair can never double-index the same
        // `missing` set. Fire-and-forget: the returned handle is tracked in
        // `repair_task` for abort-on-shutdown.
        let _ = self.schedule_hnsw_repair(RepairKind::Startup);
    }

    /// Spawn a store-owned, single-flight HNSW repair and return a handle to
    /// its completion. Shared by the startup backfill and the on-probe
    /// repair: only the thread that flips the single-flight flag gets to
    /// spawn, so overlapping runs can't double-index. The spawned task owns
    /// its `JoinHandle` (stored in `repair_task`) so `shutdown`/`Drop` can
    /// abort it before the dense index-save. `RepairInFlightGuard` resets the
    /// flag on every exit path of the task, including a panic.
    fn schedule_hnsw_repair(&self, kind: RepairKind) -> RepairSchedule {
        // Best-effort: with no Tokio runtime (sync test context) there is
        // nothing to spawn onto. A caller that needs the backfill in that
        // case calls `backfill_hnsw_for_cold_tenants` directly.
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle,
            Err(_) => return RepairSchedule::Skipped,
        };

        // Single-flight: only the owner spawns. A concurrent startup backfill
        // or probe repair backs off; a probe additionally arms `pending` so the
        // running repair does a follow-up pass covering writes that landed after
        // its metadata snapshot (otherwise the coalesced signal — whose
        // `data_version` the probe already advanced — would be lost).
        if !self.repair_state.try_begin_or_arm(kind) {
            return RepairSchedule::AlreadyInFlight;
        }

        let state = Arc::clone(&self.repair_state);
        let guard = RepairInFlightGuard {
            state: Arc::clone(&self.repair_state),
            armed: true,
        };
        let dense_searcher = self.dense_searcher.clone();
        let hybrid_searcher = self.hybrid_searcher.clone();
        let metadata = Arc::clone(&self.metadata);
        let tenants = Arc::clone(&self.tenants);
        let (tx, rx) = oneshot::channel::<bool>();

        let task = runtime.spawn(async move {
            let mut guard = guard; // owned by the task; defused on normal exit
                                   // Drain loop: repeat while probes keep arming `pending`, so a write
                                   // that arrives after one pass's snapshot is picked up by the next.
                                   // The loop yields the final pass's outcome.
            let final_ok = loop {
                let ok = match run_hnsw_backfill(
                    dense_searcher.as_ref(),
                    hybrid_searcher.as_ref(),
                    metadata.as_ref(),
                    tenants.as_ref(),
                )
                .await
                {
                    Ok(stats) => {
                        // Count only probe-triggered repairs; a startup backfill
                        // is not a "repair" for telemetry purposes.
                        if kind == RepairKind::Probe {
                            state.repairs.fetch_add(1, Ordering::Relaxed);
                        }
                        if stats.tenants_backfilled > 0 || stats.chunks_indexed > 0 {
                            info!(
                                kind = ?kind,
                                tenants = stats.tenants_backfilled,
                                chunks = stats.chunks_indexed,
                                skipped = stats.chunks_skipped,
                                "store-owned HNSW repair completed"
                            );
                        }
                        true
                    }
                    Err(e) => {
                        warn!(kind = ?kind, error = %e, "store-owned HNSW repair failed");
                        false
                    }
                };
                if !state.finish_or_continue() {
                    break ok;
                }
            };
            guard.armed = false; // normal exit: the slot is already released
                                 // The receiver is gone if the foreground budget already elapsed;
                                 // that is the expected non-blocking case, so ignore send errors.
            let _ = tx.send(final_ok);
        });

        // Track the handle so shutdown/Drop can abort the repair before the
        // dense index-save. Replaces any prior (already-finished) handle.
        *self.repair_task.lock() = Some(task);
        RepairSchedule::Scheduled(rx)
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
        self.ensure_writable("backfill_hnsw_for_cold_tenants")?;
        run_hnsw_backfill(
            self.dense_searcher.as_ref(),
            self.hybrid_searcher.as_ref(),
            self.metadata.as_ref(),
            self.tenants.as_ref(),
        )
        .await
    }

    /// True when any tenant has active metadata rows but an empty sparse
    /// index — the degraded state left behind when the tantivy directory
    /// is lost and silently recreated empty on open.
    fn any_tenant_sparse_cold(&self) -> bool {
        let Some(sparse) = self.hybrid_searcher.as_ref().and_then(|h| h.sparse_index()) else {
            return false;
        };
        let tenant_strs: Vec<String> = self.tenants.read().keys().cloned().collect();
        for tenant_str in tenant_strs {
            let Ok(tenant_id) = TenantId::new(&tenant_str) else {
                continue;
            };
            let has_active = self
                .metadata
                .list(&tenant_id, 1, 0)
                .map(|rows| !rows.is_empty())
                .unwrap_or(false);
            // Treat a doc-count error as "not cold" so a broken index
            // cannot trigger repair loops.
            if has_active && sparse.doc_count(&tenant_id).unwrap_or(1) == 0 {
                return true;
            }
        }
        false
    }

    /// Populate `canonical_text` for any chunk row whose value is NULL.
    ///
    /// Pre-D2 production rows were inserted with `canonical_text: None`
    /// and the `idx_chunks_canonical` partial index never sees them.
    /// This pass restores Track D's exact-mode dedup contract for those
    /// rows without requiring a destructive migration. Best-effort: a
    /// single-row failure (deserialization, missing segment) is logged
    /// and counted, not fatal — subsequent runs reattempt the same row.
    pub fn backfill_canonical_text_for_legacy_chunks(&self) -> Result<CanonicalBackfillStats> {
        self.ensure_writable("backfill_canonical_text_for_legacy_chunks")?;
        Ok(run_canonical_text_backfill(
            self.metadata.as_ref(),
            self.tenants.as_ref(),
        ))
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

    pub async fn probe_external_mutation(&self) -> ExternalMutationOutcome {
        let Some(probe) = self.external_mutation_probe.as_ref() else {
            return ExternalMutationOutcome::Unavailable;
        };

        match probe.check() {
            Ok(ExternalMutationCheck::Clean) => ExternalMutationOutcome::Clean,
            Ok(ExternalMutationCheck::OwnWrites) => ExternalMutationOutcome::OwnWrites,
            Ok(ExternalMutationCheck::External {
                prev_data_version,
                data_version,
            }) => {
                warn!(
                    prev_data_version,
                    data_version,
                    "external metadata.db mutation detected while holding the writer lock; scheduling background HNSW repair"
                );
                // Do NOT await the backfill in the foreground request path: on
                // a large or cold tenant it can run for minutes and trip the
                // warm client timeout. Schedule a store-owned, single-flight
                // repair and wait only a bounded budget for it to finish; if it
                // is still running when the budget elapses we serve the request
                // anyway and the repair continues in the background.
                //
                // Freshness relaxation: when we serve before the repair lands,
                // this one request's dense/hybrid results may miss chunks that
                // another writer just added. Sparse/tantivy is unaffected
                // (`SparseIndex::search()` does `commit_if_dirty` +
                // `reader.reload()` per query) and SQLite reads hit metadata
                // directly, so those paths stay correct. Same-worker
                // read-your-writes is also unaffected — our own writes index on
                // the synchronous write path.
                // `repaired` is true only when the repair finished within the
                // bounded budget. A repair that is still running, was already
                // in flight, or was skipped serves now with `repaired: false`;
                // `warm status` reflects an ongoing repair via
                // `repair_in_progress`. Timing out the wait drops only the
                // receiver — the spawned repair task runs to completion.
                let repaired = match self.schedule_hnsw_repair(RepairKind::Probe) {
                    RepairSchedule::Scheduled(rx) => matches!(
                        tokio::time::timeout(HNSW_REPAIR_FOREGROUND_BUDGET, rx).await,
                        Ok(Ok(true))
                    ),
                    RepairSchedule::AlreadyInFlight | RepairSchedule::Skipped => false,
                };
                ExternalMutationOutcome::External { repaired }
            }
            Err(error) => {
                warn!(
                    error = %error,
                    "RYW external mutation probe failed; serving request without probe result"
                );
                ExternalMutationOutcome::Unavailable
            }
        }
    }

    pub fn ryw_probe_stats(&self) -> Option<RywProbeStats> {
        let probe = self.external_mutation_probe.as_ref()?;
        let mut stats = probe.stats();
        stats.repairs = self.repair_state.repairs.load(Ordering::Relaxed);
        stats.repair_in_progress = self.repair_state.repair_in_progress();
        Some(stats)
    }

    fn start_async_indexer_if_enabled(&self) -> Option<AsyncIndexerHandle> {
        if self.config.read_only {
            return None;
        }
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
        let write_epoch = Arc::clone(&self.write_epoch);

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
                            &write_epoch,
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
                            &write_epoch,
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
        if self.config.read_only {
            tracing::debug!(
                tenant_id = %tenant_id,
                "skipping tiered maintenance for read-only store"
            );
            return None;
        }
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
        self.ensure_writable("run_compaction")?;
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

    /// Rewrite finalized segment files for a tenant, omitting payloads
    /// that no longer have a live metadata row.
    pub fn rewrite_segments_for_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<SegmentRewriteResult> {
        self.ensure_writable("rewrite_segments_for_tenant")?;
        let tenant = self.get_or_create_tenant(tenant_id.as_str())?;
        tenant.rewrite_finalized_segments(&self.metadata, tenant_id)
    }

    #[cfg(test)]
    pub(crate) fn set_dense_searcher_for_tests(&mut self, dense_searcher: Arc<DenseSearcher>) {
        self.dense_searcher = Some(dense_searcher);
    }

    /// Run compaction for a tenant if thresholds are exceeded
    ///
    /// Returns None if no compaction needed (all thresholds below limits).
    /// Returns Some(CompactionResult) if compaction was performed.
    pub fn run_compaction_if_needed(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Option<CompactionResult>> {
        self.ensure_writable("run_compaction_if_needed")?;
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
        let hidden_index_entries_present = if let Some(dense) = self.dense_searcher.as_ref() {
            let deleted_chunk_ids = self.metadata.get_deleted_chunk_ids(tenant_id)?;
            let lifecycle_hidden_ids = self.metadata.list_lifecycle_hidden(tenant_id)?;
            let mut excluded_chunk_ids = std::collections::HashSet::with_capacity(
                deleted_chunk_ids.len() + lifecycle_hidden_ids.len(),
            );
            excluded_chunk_ids.extend(deleted_chunk_ids);
            excluded_chunk_ids.extend(lifecycle_hidden_ids);
            dense.has_valid_embeddings_for_chunks(tenant_id, &excluded_chunk_ids)
        } else {
            false
        };

        if !runner.should_run(&metrics) && !hidden_index_entries_present {
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
            self.config.min_finalize_chunks,
            self.config.wal_checkpoint_interval,
            self.config.read_only,
        )?;

        let tenant = Arc::new(tenant);
        tenants.insert(tenant_id.to_string(), Arc::clone(&tenant));

        Ok(tenant)
    }

    /// Graceful shutdown - finalizes all active segments
    pub fn shutdown(&self) -> Result<()> {
        info!("PersistentStore shutting down");

        if self.config.read_only {
            tracing::debug!("read-only PersistentStore shutdown skips persistence writes");
            return Ok(());
        }

        // Stop any in-flight store-owned HNSW repair before saving indices, so
        // the repair does not concurrently mutate the dense index while
        // `save_all()` serializes it. `abort()` is cooperative (the task stops
        // at its next await), so wait a bounded moment for it to unwind: on the
        // multi-threaded warm-worker runtime the aborted task is cancelled on
        // another worker thread and clears the slot well within this budget,
        // giving a real barrier. If it does not clear in time (e.g. a
        // single-worker runtime that cannot poll the aborted task while this
        // thread blocks) we fall through best-effort — the dense index is
        // internally locked so the save cannot tear, and any batch not yet
        // persisted is re-indexed cheaply by the cache-aware backfill on the
        // next start.
        let repair_task = self.repair_task.lock().take();
        if let Some(task) = repair_task {
            task.abort();
            let deadline = Instant::now() + Duration::from_millis(500);
            while self.repair_state.repair_in_progress() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }

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
            // Gated: skip if active segment is below min_finalize_chunks.
            // Recovered on next start via WAL replay.
            if let Err(e) = tenant.finalize_active_segment_if_above_threshold() {
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

#[async_trait::async_trait]
impl Store for PersistentStore {
    async fn add(&self, chunk: MemoryChunk) -> Result<ChunkId> {
        self.ensure_writable("add")?;
        self.add_chunks_internal(vec![chunk])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| MemdError::StorageError("no chunk id produced".into()))
    }

    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>> {
        self.ensure_writable("add_batch")?;
        self.add_chunks_internal(chunks).await
    }

    async fn add_feedback(&self, feedback: FeedbackEntry) -> Result<()> {
        self.ensure_writable("add_feedback")?;
        self.metadata.insert_feedback(&feedback)
    }

    async fn add_task_artifact(
        &self,
        artifact: TaskArtifact,
        projections: Vec<TaskProjection>,
    ) -> Result<TaskArtifactWriteResult> {
        self.add_task_artifact_internal(artifact, projections).await
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

    async fn record_retrieval_episode(
        &self,
        episode: RetrievalEpisode,
        items: Vec<RetrievalEpisodeItem>,
    ) -> Result<()> {
        self.metadata.insert_retrieval_episode(&episode, &items)
    }

    async fn get_retrieval_episode(
        &self,
        tenant_id: &TenantId,
        episode_id: &RetrievalEpisodeId,
    ) -> Result<Option<(RetrievalEpisode, Vec<RetrievalEpisodeItem>)>> {
        self.metadata.get_retrieval_episode(tenant_id, episode_id)
    }

    async fn finalize_retrieval_episode(
        &self,
        tenant_id: &TenantId,
        episode_id: &RetrievalEpisodeId,
        rendered_chunk_ids: &[ChunkId],
    ) -> Result<()> {
        self.metadata
            .finalize_retrieval_episode(tenant_id, episode_id, rendered_chunk_ids)
    }

    async fn record_outcome(&self, tenant_id: &TenantId, event: OutcomeEvent) -> Result<()> {
        self.metadata.insert_outcome_event(tenant_id, &event)
    }

    async fn list_outcomes_for_episode(
        &self,
        tenant_id: &TenantId,
        episode_id: &RetrievalEpisodeId,
    ) -> Result<Vec<OutcomeEvent>> {
        self.metadata
            .list_outcome_events_for_episode(tenant_id, episode_id)
    }

    async fn outcome_priors(
        &self,
        scope_tenant_id: &TenantId,
        scope_project_id: Option<&str>,
        chunk_ids: &[ChunkId],
        now_ms: i64,
    ) -> Result<Vec<crate::store::OutcomePrior>> {
        self.metadata
            .outcome_priors(scope_tenant_id, scope_project_id, chunk_ids, now_ms)
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

    async fn probe_external_mutation(&self) -> ExternalMutationOutcome {
        PersistentStore::probe_external_mutation(self).await
    }

    fn ryw_probe_stats(&self) -> Option<RywProbeStats> {
        PersistentStore::ryw_probe_stats(self)
    }

    fn record_usage_event(&self, event: UsageEvent) {
        if self.config.read_only {
            debug!("usage ledger recording skipped for read-only store");
            return;
        }
        if !usage_ledger_enabled() {
            return;
        }

        let ts = current_time_ms();
        if let Err(error) = self.metadata.insert_usage_event(ts, &event) {
            debug!(error = %error, "usage ledger insert failed");
        }

        let last_sweep = self.usage_sweep_last_ms.load(Ordering::Relaxed);
        if ts.saturating_sub(last_sweep) >= 3_600_000
            && self
                .usage_sweep_last_ms
                .compare_exchange(last_sweep, ts, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            let cutoff = ts.saturating_sub(usage_retention_ms());
            if let Err(error) = self.metadata.sweep_usage_events_before(cutoff) {
                debug!(error = %error, "usage ledger retention sweep failed");
            }
        }
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

    fn retrieval_mode(&self) -> &'static str {
        // Mirrors search_with_scores_real's dispatch: hybrid (dense + sparse
        // fusion) when the hybrid searcher is present, dense-only when just the
        // dense searcher is, and substring matching at constant score when
        // neither embedding path is available.
        if self.hybrid_searcher.is_some() {
            "hybrid"
        } else if self.dense_searcher.is_some() {
            "dense"
        } else {
            "text_fallback"
        }
    }

    async fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        self.ensure_writable("delete")?;
        self.delete_chunk(tenant_id, chunk_id).await
    }

    async fn stats(&self, tenant_id: &TenantId) -> Result<StoreStats> {
        self.get_stats(tenant_id).await
    }

    async fn health_snapshot(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        duplicate_limit: usize,
    ) -> Result<Option<StoreHealthSnapshot>> {
        self.metadata
            .health_snapshot(tenant_id, project_id, duplicate_limit)
            .map(Some)
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
            if let Some(chunk) = self
                .get_chunk_for_retrieval(tenant_id, &meta.chunk_id, "list_chunks")
                .await?
            {
                chunks.push(chunk);
            }
        }
        Ok(chunks)
    }

    async fn list_chunks_for_project(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MemoryChunk>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let metadata_rows = self
            .metadata
            .list_for_project(tenant_id, project_id, limit, offset)?;
        let mut chunks = Vec::with_capacity(metadata_rows.len());
        for meta in metadata_rows {
            if let Some(chunk) = self
                .get_chunk_for_retrieval(tenant_id, &meta.chunk_id, "list_chunks_for_project")
                .await?
            {
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

#[cfg(test)]
mod tests;
