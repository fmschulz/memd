---
phase: 05-hot-tier-+-cache
plan: 04
subsystem: store
tags: [tiered-search, metrics, integration, hybrid-search]
depends_on: [05-03]

provides:
  - TieredMetrics and TieredQueryMetrics in MetricsCollector
  - WarmTierAdapter for DenseSearcher -> WarmTierSearch
  - Tiered integration in HybridSearcher
  - TieredStats and tier info in PersistentStore

affects:
  - 05-05 (MCP handlers need tiered stats)
  - Future observability/monitoring

tech-stack:
  patterns:
    - Per-tenant TieredSearcher instances
    - Shared SemanticCache across tenants
    - Adapter pattern for WarmTierSearch

key-files:
  modified:
    - crates/memd/src/metrics.rs
    - crates/memd/src/store/hybrid.rs
    - crates/memd/src/store/dense.rs
    - crates/memd/src/store/persistent.rs
    - crates/memd/src/index/bm25.rs

decisions:
  - id: per-tenant-tiered-searchers
    choice: Create TieredSearcher per tenant with shared SemanticCache
    rationale: Hot tier and access tracker are tenant-scoped, cache is global
  - id: warm-tier-adapter
    choice: Use adapter pattern to bridge DenseSearcher to WarmTierSearch
    rationale: Avoids circular dependencies, keeps DenseSearcher focused
  - id: tiered-metrics-in-collector
    choice: Add tiered metrics to existing MetricsCollector
    rationale: Consistent metrics infrastructure, single snapshot() call

metrics:
  duration: ~30 minutes
  completed: 2026-01-31
---

# Phase 5 Plan 04: Tiered Integration Summary

**One-liner:** Integrated TieredSearcher into HybridSearcher with tiered metrics tracking and cache/hot/warm fallback.

## What Was Built

### Task 1: Tiered Metrics in MetricsCollector
- Added `TieredMetrics` struct with cache/hot/warm statistics
- Added `TieredQueryMetrics` for per-query tier tracking
- Added `record_tiered_query()`, `record_promotion()`, `record_demotion()`
- Added `get_tiered_stats()` for aggregated tier statistics
- Updated `MetricsSnapshot` to include tiered stats

### Task 2: TieredSearcher Integration in HybridSearcher
- Created `WarmTierAdapter` to bridge DenseSearcher to WarmTierSearch trait
- Added `enable_tiered` and `tiered_config` to HybridConfig
- Created per-tenant TieredSearchers with shared SemanticCache
- Added `search_tiered()` path that uses cache->hot->warm fallback
- Added `run_tiered_maintenance()` for periodic tier management
- Added `invalidate_chunk_in_cache()` for delete handling
- Added `search_with_embedding()` and `index_len()` to DenseSearcher

### Task 3: PersistentStore Tiered Integration
- Added `enable_tiered_search` to PersistentStoreConfig
- Added `TieredStats` struct for combined statistics
- Added `get_tiered_stats()` for MCP handlers
- Added `run_maintenance()` for periodic tier management
- Added `invalidate_chunk()` for delete propagation
- Added `search_with_tier_info()` for debug output
- Record TieredQueryMetrics in search path
- Delete propagates to cache/hot tier invalidation

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] BM25 search reader reload order**
- **Found during:** Task 2 test verification
- **Issue:** BM25Index.search() called reader.searcher() before reader.reload(), causing deleted documents to still appear in search
- **Fix:** Reordered to reload() first, then searcher()
- **Files modified:** crates/memd/src/index/bm25.rs
- **Commit:** 499b273

## Architecture

```
PersistentStore
    |
    +-> HybridSearcher (tiered_enabled: bool)
           |
           +-> Per-tenant TieredSearcher<WarmTierAdapter>
           |      |
           |      +-> SemanticCache (shared)
           |      +-> HotTier (per-tenant)
           |      +-> AccessTracker (per-tenant)
           |      +-> WarmTierAdapter -> DenseSearcher
           |
           +-> DenseSearcher
           +-> Bm25Index
```

## Commits

| Hash | Type | Description |
|------|------|-------------|
| 074259f | feat | Add tiered metrics to MetricsCollector |
| 499b273 | feat | Integrate TieredSearcher into HybridSearcher |
| 39ddf6a | feat | Wire tiered search into PersistentStore |

## Verification

- [x] cargo check passes
- [x] cargo test --lib store:: passes (97 tests)
- [x] HybridSearcher uses TieredSearcher when enabled
- [x] Cache hits skip warm tier search
- [x] Hot tier hits reduce warm tier load
- [x] TieredMetrics track cache/hot/warm performance
- [x] Version propagation works for invalidation
- [x] Existing behavior preserved when tiered disabled

## Next Phase Readiness

Ready for 05-05 (MCP Handler Updates):
- TieredStats available via `PersistentStore.get_tiered_stats()`
- TieredMetrics included in `MetricsCollector.snapshot()`
- Search returns tier timing info
- Maintenance method ready for periodic calls
