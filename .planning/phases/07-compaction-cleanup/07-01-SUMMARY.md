---
phase: 07-compaction-cleanup
plan: 01
subsystem: compaction
tags: [compaction, metrics, tombstone, audit]

dependency-graph:
  requires: [06-structural-indexes]
  provides: [compaction-metrics, tombstone-audit, compaction-config]
  affects: [07-02, 07-03]

tech-stack:
  added: []
  patterns: [threshold-based-triggers, metric-gathering, audit-verification]

key-files:
  created:
    - crates/memd/src/compaction/mod.rs
    - crates/memd/src/compaction/metrics.rs
    - crates/memd/src/compaction/tombstone_audit.rs
  modified:
    - crates/memd/src/lib.rs

decisions:
  - id: 07-01-01
    what: "CompactionThresholds defaults: tombstone 20%, segments 10, HNSW staleness 15%"
    why: "Conservative defaults that trigger compaction before significant performance degradation"
  - id: 07-01-02
    what: "CompactionConfig includes batch_delay_ms and batch_size for throttling"
    why: "Compaction should not monopolize I/O, allow interleaving with normal operations"
  - id: 07-01-03
    what: "TombstoneAudit verifies both SegmentReader and MetadataStore filtering"
    why: "Multiple code paths retrieve data, all must respect tombstone status"
  - id: 07-01-04
    what: "HNSW staleness = (index_size - cache_size) / index_size"
    why: "Cache size represents valid mappings, difference indicates orphaned index entries"

metrics:
  duration: 5m
  completed: 2026-02-01
---

# Phase 07 Plan 01: Compaction Module Foundation Summary

Compaction infrastructure with metrics gathering, threshold-based triggers, and tombstone audit verification for deleted chunk filtering.

## What Was Built

### 1. CompactionThresholds and CompactionConfig

Configuration structures for compaction decision-making:

```rust
pub struct CompactionThresholds {
    pub tombstone_ratio_pct: f32,    // default 0.20 (20%)
    pub max_segment_count: usize,    // default 10
    pub hnsw_staleness_pct: f32,     // default 0.15 (15%)
}

pub struct CompactionConfig {
    pub thresholds: CompactionThresholds,
    pub batch_delay_ms: u64,         // default 10
    pub batch_size: usize,           // default 100
    pub enabled: bool,               // default true
}
```

### 2. CompactionManager

Skeleton manager that coordinates compaction decisions:

```rust
impl CompactionManager {
    pub fn new(config: CompactionConfig) -> Self;
    pub fn check_thresholds(&self, metrics: &CompactionMetrics) -> bool;
}
```

Returns true if ANY threshold is exceeded, signaling compaction should be considered.

### 3. CompactionMetrics

Gathers metrics from metadata store and HNSW index:

```rust
pub struct CompactionMetrics {
    pub tombstone_ratio: f32,
    pub active_chunks: usize,
    pub deleted_chunks: usize,
    pub segment_count: usize,
    pub hnsw_staleness: f32,
    pub hnsw_cache_size: usize,
    pub hnsw_index_size: usize,
}

impl CompactionMetrics {
    pub fn gather(
        metadata: &SqliteMetadataStore,
        hnsw_stats: (usize, usize),
        segment_count: usize,
        tenant_id: &TenantId,
    ) -> Result<Self>;
}
```

### 4. TombstoneAudit

Verifies tombstone filtering works correctly:

```rust
pub struct TombstoneAudit;

impl TombstoneAudit {
    pub fn audit_segment_reader(&self, reader: &SegmentReader, metadata: &SqliteMetadataStore, segment_id: u64) -> Result<AuditResult>;
    pub fn audit_metadata_store(&self, metadata: &SqliteMetadataStore, tenant_id: &TenantId) -> Result<AuditResult>;
}

pub struct AuditResult {
    pub total_deleted: usize,
    pub tombstone_leaks: Vec<ChunkId>,
    pub paths_audited: Vec<String>,
    pub passed: bool,
}
```

## Key Design Decisions

1. **Threshold-based triggers**: Any single threshold exceeding its limit triggers compaction consideration
2. **Batch throttling**: batch_delay_ms and batch_size prevent I/O monopolization
3. **Multi-path audit**: TombstoneAudit checks both segment and metadata retrieval paths
4. **HNSW staleness calculation**: Uses difference between index size and cache mappings

## Test Coverage

- 15 unit tests covering all threshold checks, config defaults, and audit result handling
- All tests pass

## Deviations from Plan

None - plan executed exactly as written.

## Commits

| Hash | Description |
|------|-------------|
| 392fac8 | feat(07-01): create compaction module with config and thresholds |
| a56b3ec | feat(07-01): export compaction types from lib.rs |

## Next Phase Readiness

Ready for 07-02 (Compaction Implementation):
- CompactionManager skeleton ready for compaction logic
- CompactionMetrics ready to gather real metrics
- TombstoneAudit ready to verify compaction correctness
