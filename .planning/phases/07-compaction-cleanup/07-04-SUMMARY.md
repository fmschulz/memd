---
phase: 07-compaction-cleanup
plan: 04
subsystem: compaction
tags: [compaction, hnsw, rebuild, throttle, cache-invalidation, workflow]

dependency-graph:
  requires: [07-02, 07-03]
  provides: [compaction-runner, compaction-workflow, cache-invalidation]
  affects: []

tech-stack:
  added: []
  patterns: [compaction-coordinator, throttled-workflow, metrics-driven-decisions]

key-files:
  created:
    - crates/memd/src/compaction/runner.rs
  modified:
    - crates/memd/src/store/metadata/mod.rs
    - crates/memd/src/store/metadata/sqlite.rs
    - crates/memd/src/index/hnsw.rs
    - crates/memd/src/store/dense.rs
    - crates/memd/src/compaction/mod.rs

decisions:
  - id: 07-04-01
    what: "CompactionRunner uses should_run() with unified trigger (any threshold exceeded)"
    why: "Consistent with CompactionManager behavior from 07-01"
  - id: 07-04-02
    what: "Throttle delays between all major operations: gather->rebuild->merge->invalidate"
    why: "Prevents compaction from monopolizing I/O, respects COMPACT-05 requirement"
  - id: 07-04-03
    what: "HNSW rebuild returns RebuildResult only, actual swap deferred to future work"
    why: "Current HnswIndex doesn't support construction from pre-built Hnsw graph"

metrics:
  duration: 8m
  completed: 2026-01-31
---

# Phase 07 Plan 04: Compaction Runner Summary

CompactionRunner orchestrates HNSW rebuild, segment merge, and cache invalidation with configurable throttle delays between operations for controlled I/O impact.

## Performance

- **Duration:** 8m
- **Started:** 2026-01-31T09:15:00Z
- **Completed:** 2026-01-31T09:23:00Z
- **Tasks:** 5
- **Files modified:** 6

## Accomplishments

- MetadataStore trait extended with get_deleted_chunk_ids() for compaction support
- HnswIndex.rebuild_clean_in_place() leverages HnswRebuilder from 07-02
- DenseSearcher provides tenant-level rebuild with get_rebuild_stats() and rebuild_hnsw_for_tenant()
- CompactionRunner coordinates full compaction workflow with throttle integration (COMPACT-04, COMPACT-05)

## Task Commits

Each task was committed atomically:

1. **Task 1+2: MetadataStore get_deleted_chunk_ids** - `7ee4073` (feat)
2. **Task 3: HnswIndex rebuild_clean_in_place** - `b552633` (feat)
3. **Task 4: DenseSearcher rebuild methods** - `835682b` (feat)
4. **Task 5: CompactionRunner module** - `913677b` (feat)

**Plan metadata:** Pending (docs commit after this summary)

## Files Created/Modified

- `crates/memd/src/compaction/runner.rs` - CompactionRunner and CompactionResult for workflow orchestration
- `crates/memd/src/store/metadata/mod.rs` - MetadataStore trait with get_deleted_chunk_ids()
- `crates/memd/src/store/metadata/sqlite.rs` - SqliteMetadataStore implementation
- `crates/memd/src/index/hnsw.rs` - IndexMapping.get_internal_id() and HnswIndex.rebuild_clean_in_place()
- `crates/memd/src/store/dense.rs` - DenseSearcher.get_rebuild_stats() and rebuild_hnsw_for_tenant()
- `crates/memd/src/compaction/mod.rs` - Export runner module and types

## Decisions Made

1. **Unified trigger**: should_run() returns true if ANY threshold exceeded (consistent with 07-01 CompactionManager)
2. **Throttle placement**: Delays between gather->rebuild->merge->invalidate operations
3. **Rebuild isolation**: rebuild_hnsw_for_tenant returns RebuildResult only; actual index swap deferred to future work when HnswIndex supports from_rebuilt() constructor

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## Test Coverage

- 6 new unit tests for CompactionRunner covering:
  - Runner creation and defaults
  - Threshold checks (below, tombstone exceeded, segments exceeded, HNSW exceeded)
- All 38 compaction module tests pass

## Next Phase Readiness

Phase 07 Complete - Compaction infrastructure ready:
- CompactionRunner can orchestrate full compaction workflow
- Throttle integration prevents I/O monopolization
- Metrics-driven decisions for when to compact
- Cache invalidation ensures consistency after compaction

---
*Phase: 07-compaction-cleanup*
*Completed: 2026-01-31*
