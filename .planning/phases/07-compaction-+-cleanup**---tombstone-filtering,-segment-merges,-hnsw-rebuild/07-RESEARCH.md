# Phase 7: Compaction + Cleanup - Research

**Researched:** 2026-01-31
**Domain:** Storage compaction, index maintenance, background task scheduling
**Confidence:** HIGH

## Summary

This phase implements system maintenance operations to ensure memd maintains performance and correctness as data grows and changes. The work involves three distinct compaction domains:

1. **Tombstone filtering enforcement** - Ensuring deleted chunks never appear in any code path (already partially implemented via `TombstoneSet` and metadata filtering, needs audit)
2. **Sparse segment merges** - Consolidating fragmented Tantivy segments using built-in `LogMergePolicy`
3. **Warm HNSW rebuild** - Creating clean indexes without deleted items by rebuilding from embedding cache

The standard approach uses threshold-based triggering with a unified compaction job that coordinates all three operations, running in background with throttling to limit query impact.

**Primary recommendation:** Use Tantivy's built-in merge policies for sparse index compaction, implement warm HNSW rebuild using existing `EmbeddingCache` infrastructure, and add a unified `CompactionManager` that monitors metrics and triggers maintenance during low-activity periods.

## Standard Stack

The existing codebase provides most required infrastructure. New dependencies are minimal.

### Core (Already Present)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| tantivy | 0.24 | Sparse index with built-in segment merge | Has `LogMergePolicy` for automatic segment consolidation |
| hnsw_rs | 0.3 | Warm tier index | Rebuild from cached embeddings |
| roaring | 0.11 | Tombstone bitmaps | Already used for efficient deletion tracking |
| parking_lot | 0.12 | Concurrency primitives | Already used throughout codebase |
| tokio | 1.0 | Async runtime | `spawn_blocking` for compaction, background tasks |

### Supporting (Already Present)
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| rusqlite | 0.38 | Metadata queries | Gathering compaction statistics |
| tracing | 0.1 | Logging | Progress and event reporting |
| moka | 0.12 | Cache invalidation | Semantic cache cleanup during compaction |

### New Dependencies
None required. All functionality can be built using existing dependencies.

**Installation:**
No new dependencies needed.

## Architecture Patterns

### Recommended Project Structure
```
crates/memd/src/
├── compaction/
│   ├── mod.rs           # CompactionManager, CompactionConfig
│   ├── metrics.rs       # CompactionMetrics, threshold detection
│   ├── segment_merge.rs # Sparse segment merge coordinator
│   ├── hnsw_rebuild.rs  # Warm HNSW clean rebuild
│   └── throttle.rs      # Rate limiting for background ops
├── store/
│   ├── persistent.rs    # Add compaction hooks
│   └── tombstone.rs     # (existing) May need iterator for cleanup
└── tiered/
    └── hot_tier.rs      # (existing) Add cleanup integration
```

### Pattern 1: Unified Threshold-Based Triggering
**What:** Single check evaluates all compaction metrics against thresholds
**When to use:** Per user decision - unified threshold triggers all compaction types

```rust
// Source: User decisions from CONTEXT.md
pub struct CompactionThresholds {
    /// Trigger when tombstone ratio exceeds this percentage
    pub tombstone_ratio_pct: f32,  // Default: 20%
    /// Trigger when segment count exceeds this value
    pub max_segment_count: usize,  // Default: 10
    /// Trigger when HNSW staleness exceeds this ratio
    pub hnsw_staleness_pct: f32,   // Default: 15%
}

impl CompactionManager {
    pub fn check_thresholds(&self) -> bool {
        let metrics = self.gather_metrics();

        // Unified: ANY threshold triggers ALL compaction types
        metrics.tombstone_ratio > self.thresholds.tombstone_ratio_pct
            || metrics.segment_count > self.thresholds.max_segment_count
            || metrics.hnsw_staleness > self.thresholds.hnsw_staleness_pct
    }
}
```

### Pattern 2: Background Task with spawn_blocking
**What:** Run compaction in blocking context to avoid starving async runtime
**When to use:** For all compaction operations (they are I/O and CPU intensive)

```rust
// Source: Tokio best practices for blocking work
pub async fn run_compaction(&self) -> Result<CompactionResult> {
    let manager = self.clone(); // Arc-wrapped

    tokio::task::spawn_blocking(move || {
        manager.run_compaction_sync()
    }).await.map_err(|e| MemdError::StorageError(e.to_string()))?
}
```

### Pattern 3: Warm HNSW Rebuild from Cache
**What:** Build new HNSW from cached embeddings, filtering out deleted chunk IDs
**When to use:** When HNSW staleness threshold exceeded

```rust
// Source: Existing HnswIndex::load pattern + EmbeddingCache
pub fn rebuild_hnsw_clean(
    &self,
    deleted_ids: &HashSet<ChunkId>,
) -> Result<HnswIndex> {
    let cache = self.embedding_cache.read();
    let mapping = self.mapping.read();

    let mut new_hnsw = Hnsw::new(
        self.config.max_connections,
        self.config.max_elements,
        16,
        self.config.ef_construction,
        DistCosine {},
    );

    let mut new_mapping = IndexMapping::new();

    for (internal_id, embedding) in cache.iter_valid() {
        if let Some(chunk_id) = mapping.get_chunk_id(internal_id) {
            if !deleted_ids.contains(&chunk_id) {
                let new_id = new_mapping.insert(&chunk_id);
                new_hnsw.insert_slice((embedding, new_id));
            }
        }
    }

    // Atomic swap
    Ok(HnswIndex { hnsw: new_hnsw, mapping: new_mapping, ... })
}
```

### Pattern 4: Tantivy Segment Merge via Policy
**What:** Use Tantivy's built-in `LogMergePolicy` for segment compaction
**When to use:** Automatic - policy runs during commit

```rust
// Source: tantivy::merge_policy documentation
use tantivy::merge_policy::LogMergePolicy;

pub fn configure_sparse_merge_policy(&self) -> LogMergePolicy {
    let mut policy = LogMergePolicy::default();
    // Merge when segment count exceeds threshold
    policy.set_min_num_segments(4);
    // Allow larger merged segments
    policy.set_max_docs_before_merge(100_000);
    // Set deletion tolerance to trigger merge on tombstones
    policy.set_del_docs_ratio_before_merge(0.2);
    policy
}
```

### Pattern 5: Throttled Operations
**What:** Limit compaction I/O rate to preserve query latency
**When to use:** During all compaction operations

```rust
// Source: User decisions + rate limiting patterns
pub struct ThrottleConfig {
    /// Delay between batch operations (ms)
    pub batch_delay_ms: u64,
    /// Max items to process per batch
    pub batch_size: usize,
}

impl Throttle {
    pub async fn delay_if_needed(&self) {
        tokio::time::sleep(Duration::from_millis(self.config.batch_delay_ms)).await;
    }

    pub fn process_batched<T, F>(&self, items: Vec<T>, f: F) -> Result<()>
    where F: Fn(&[T]) -> Result<()>
    {
        for chunk in items.chunks(self.config.batch_size) {
            f(chunk)?;
            std::thread::sleep(Duration::from_millis(self.config.batch_delay_ms));
        }
        Ok(())
    }
}
```

### Anti-Patterns to Avoid
- **Don't hand-roll segment merge logic:** Tantivy has battle-tested merge policies
- **Don't rebuild HNSW from segments:** Use embedding cache (50-100x faster per existing code)
- **Don't run compaction synchronously in async context:** Use `spawn_blocking`
- **Don't pause queries during compaction:** Hot tier should continue serving
- **Don't implement complex cascading triggers:** User chose unified threshold

## Don't Hand-Roll

Problems that look simple but have existing solutions:

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Sparse segment consolidation | Custom segment merger | Tantivy `LogMergePolicy` | Handles edge cases, tested at scale |
| Tombstone bitmap persistence | Custom format | Existing `TombstoneSet` with roaring | Already atomic, CRC-verified |
| Embedding storage | Custom cache | Existing `EmbeddingCache` | Already handles validation, CRC |
| Background task scheduling | Custom thread pool | Tokio `spawn_blocking` | Integrates with runtime, avoids starvation |
| Rate limiting | Custom token bucket | Simple sleep-based throttle | Sufficient for single-user local system |

**Key insight:** The codebase already has robust infrastructure for the hard parts. Compaction is about coordination and triggering, not reimplementing storage primitives.

## Common Pitfalls

### Pitfall 1: Blocking Async Runtime During Compaction
**What goes wrong:** Running CPU/IO-intensive compaction in async task blocks worker threads
**Why it happens:** Compaction looks like regular async work but isn't
**How to avoid:** Always use `tokio::task::spawn_blocking` for compaction operations
**Warning signs:** Query latency spikes during compaction, async timeout errors

### Pitfall 2: HNSW Orphaned Nodes After Deletion
**What goes wrong:** Deleted entries remain in HNSW graph, wasting memory and degrading search
**Why it happens:** HNSW doesn't support true deletion, only marks entries
**How to avoid:** Periodic rebuild from embedding cache, filter by metadata during search
**Warning signs:** Index size grows despite deletions, search returns tombstoned results

### Pitfall 3: Stale Cache After Compaction
**What goes wrong:** Semantic cache returns results containing deleted chunks
**Why it happens:** Cache not invalidated when compaction removes chunks
**How to avoid:** Invalidate cache entries containing deleted chunk IDs during compaction
**Warning signs:** Deleted chunks appear in cached query results

### Pitfall 4: Unbounded Compaction Duration
**What goes wrong:** Compaction takes hours, blocks shutdown, causes OOM
**Why it happens:** No limits on compaction batch size or duration
**How to avoid:** Process in bounded batches with checkpoints
**Warning signs:** Memory grows during compaction, compaction never completes

### Pitfall 5: Compaction During High Load
**What goes wrong:** Compaction degrades p99 latency during peak usage
**Why it happens:** I/O contention between compaction and queries
**How to avoid:** Check query rate before starting, throttle or defer if busy
**Warning signs:** Latency spikes correlate with compaction events

### Pitfall 6: Lost Progress on Crash
**What goes wrong:** Compaction restarts from beginning after crash
**Why it happens:** No checkpoint mechanism for partial progress
**How to avoid:** Atomic operations or checkpoint-based resumption
**Warning signs:** Same segments reprocessed repeatedly after restarts

## Code Examples

Verified patterns from official sources and existing codebase:

### Example 1: Gathering Compaction Metrics
```rust
// Source: Existing codebase patterns
impl CompactionMetrics {
    pub fn gather(
        metadata: &SqliteMetadataStore,
        sparse_index: &Bm25Index,
        hnsw_index: &HnswIndex,
        tenant_id: &TenantId,
    ) -> Result<Self> {
        // Tombstone ratio from metadata
        let (active, deleted) = metadata.count_by_status(tenant_id)?;
        let tombstone_ratio = if active + deleted > 0 {
            deleted as f32 / (active + deleted) as f32
        } else {
            0.0
        };

        // Segment count from Tantivy
        let segment_count = sparse_index.segment_count()?;

        // HNSW staleness (orphaned entries / total entries)
        let (cache_size, hnsw_size) = hnsw_index.rebuild_stats();
        let hnsw_staleness = if hnsw_size > 0 {
            let orphaned = hnsw_size.saturating_sub(cache_size);
            orphaned as f32 / hnsw_size as f32
        } else {
            0.0
        };

        Ok(Self {
            tombstone_ratio,
            segment_count,
            hnsw_staleness,
        })
    }
}
```

### Example 2: Tombstone Filtering Audit
```rust
// Source: Existing SegmentReader::read_chunk pattern
// CRITICAL: All retrieval paths must check tombstones

// In SegmentReader (already correct):
pub fn read_chunk(&self, ordinal: u32) -> io::Result<Option<Vec<u8>>> {
    if self.tombstones.is_deleted(ordinal) {
        return Ok(None);  // Correctly filters tombstoned
    }
    // ... read from mmap
}

// In PersistentStore::get_chunk (already correct):
let meta = self.metadata.get(tenant_id, chunk_id)?;
let meta = match meta {
    Some(m) if m.status != ChunkStatus::Deleted => m,
    _ => return Ok(None),  // Correctly filters deleted
};

// In search paths (verify HybridSearcher filters):
// Dense search: Results filtered by metadata lookup before return
// Sparse search: BM25 deletion marks applied
```

### Example 3: Manual Compaction MCP Tool
```rust
// Source: User decision for manual override command
#[derive(Debug, Deserialize)]
pub struct CompactArgs {
    pub tenant_id: String,
    #[serde(default)]
    pub force: bool,  // Override threshold check
}

pub async fn handle_memory_compact(
    store: &PersistentStore,
    args: CompactArgs,
) -> Result<CompactResult> {
    let tenant_id = TenantId::new(&args.tenant_id)?;

    let result = if args.force {
        store.run_compaction(&tenant_id).await?
    } else {
        store.run_compaction_if_needed(&tenant_id).await?
    };

    Ok(CompactResult {
        tombstones_removed: result.tombstones_removed,
        segments_merged: result.segments_merged,
        hnsw_rebuilt: result.hnsw_rebuilt,
        duration_ms: result.duration.as_millis() as u64,
    })
}
```

### Example 4: Tier-Aware Compaction
```rust
// Source: User decision - hot tier serves queries during warm tier compaction
impl CompactionManager {
    pub async fn run_warm_tier_compaction(&self, tenant_id: &TenantId) -> Result<()> {
        // Hot tier continues serving queries
        // Warm tier compaction runs in background

        let warm_tier = self.get_warm_tier(tenant_id)?;
        let deleted_ids = self.get_deleted_chunk_ids(tenant_id)?;

        // Rebuild creates new index without affecting current queries
        let new_hnsw = warm_tier.rebuild_hnsw_clean(&deleted_ids)?;

        // Atomic swap - minimal disruption window
        warm_tier.swap_hnsw(new_hnsw)?;

        // Invalidate semantic cache entries with deleted chunks
        self.semantic_cache.invalidate_chunks(&deleted_ids);

        Ok(())
    }
}
```

### Example 5: Progress Logging
```rust
// Source: User decision - log start/end events and periodic progress
impl CompactionManager {
    pub fn run_with_logging(&self, tenant_id: &TenantId) -> Result<CompactionResult> {
        tracing::info!(
            tenant_id = %tenant_id,
            "compaction started"
        );

        let start = Instant::now();
        let mut progress = CompactionProgress::default();

        // Tombstone cleanup
        progress.tombstones_processed = self.cleanup_tombstones(tenant_id)?;
        tracing::info!(
            tenant_id = %tenant_id,
            tombstones = progress.tombstones_processed,
            "tombstone cleanup complete"
        );

        // Segment merge
        progress.segments_merged = self.merge_segments(tenant_id)?;
        tracing::info!(
            tenant_id = %tenant_id,
            segments = progress.segments_merged,
            "segment merge complete"
        );

        // HNSW rebuild
        progress.hnsw_rebuilt = self.rebuild_hnsw(tenant_id)?;
        tracing::info!(
            tenant_id = %tenant_id,
            rebuilt = progress.hnsw_rebuilt,
            "HNSW rebuild complete"
        );

        let duration = start.elapsed();
        tracing::info!(
            tenant_id = %tenant_id,
            duration_ms = duration.as_millis(),
            tombstones = progress.tombstones_processed,
            segments = progress.segments_merged,
            hnsw = progress.hnsw_rebuilt,
            "compaction complete"
        );

        Ok(CompactionResult { progress, duration })
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Full index rebuild on delete | Soft delete + periodic rebuild | Standard practice | 100x faster deletes |
| Single-threaded compaction | Background async with throttling | Standard practice | No query disruption |
| Eager compaction | Threshold-based triggering | Standard practice | Reduced I/O churn |
| Level-only compaction | Tiered + leveled hybrid (Tantivy) | Tantivy 0.20+ | Better write amplification |

**Deprecated/outdated:**
- **Blocking compaction:** Never block query path for compaction
- **Full HNSW delete support:** hnsw_rs doesn't support it; use rebuild pattern

## Open Questions

Things that couldn't be fully resolved:

1. **Optimal threshold values**
   - What we know: 20% tombstone ratio is common, 10+ segments triggers merge
   - What's unclear: Best values for memd's specific workload
   - Recommendation: Start with defaults, tune based on EVAL-13 results

2. **hnsw_rs rebuild atomicity**
   - What we know: Can create new HNSW and swap pointers
   - What's unclear: Whether hnsw_rs supports concurrent read during rebuild
   - Recommendation: Verify with test, may need RwLock coordination

3. **Segment merge during active writes**
   - What we know: Tantivy handles this via copy-on-write
   - What's unclear: Interaction with our WAL-based writes
   - Recommendation: Test with concurrent ingestion benchmark

## Sources

### Primary (HIGH confidence)
- [Tantivy merge_policy documentation](https://docs.rs/tantivy/latest/tantivy/merge_policy/index.html) - Segment merge configuration
- [Tantivy Life of a Segment](https://github.com/quickwit-oss/tantivy/wiki/Life-of-a-Segment) - Segment lifecycle
- [Tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/) - Background task patterns
- [Tokio cooperative yielding](https://tokio.rs/blog/2020-04-preemption) - Tail latency reduction

### Secondary (MEDIUM confidence)
- [RocksDB Write Stalls](https://github.com/facebook/rocksdb/wiki/Write-Stalls) - Throttling strategies
- [RocksDB Tuning Guide](https://github.com/facebook/rocksdb/wiki/RocksDB-Tuning-Guide) - Compaction configuration
- [SILK I/O Scheduler](https://www.usenix.org/system/files/atc19-balmau.pdf) - Latency spike prevention
- [hnswlib deletion support](https://github.com/nmslib/hnswlib/issues/4) - HNSW deletion patterns

### Tertiary (LOW confidence)
- [Mini-LSM Compaction Tutorial](https://skyzh.github.io/mini-lsm/week2-01-compaction.html) - General LSM concepts
- [HNSW unreachable points research](https://arxiv.org/html/2407.07871v1) - Academic analysis of deletion issues

### Codebase Analysis (HIGH confidence)
- `crates/memd/src/store/tombstone.rs` - Existing tombstone implementation
- `crates/memd/src/index/hnsw.rs` - HNSW with EmbeddingCache rebuild
- `crates/memd/src/index/embedding_cache.rs` - Embedding persistence
- `crates/memd/src/store/segment/reader.rs` - Segment reading with tombstone filter

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - Using existing codebase infrastructure
- Architecture: HIGH - Well-established patterns from Tantivy, RocksDB
- Pitfalls: HIGH - Documented in production systems and research
- Throttling: MEDIUM - Optimal values need tuning

**Research date:** 2026-01-31
**Valid until:** 30 days (stable domain, no fast-moving dependencies)
