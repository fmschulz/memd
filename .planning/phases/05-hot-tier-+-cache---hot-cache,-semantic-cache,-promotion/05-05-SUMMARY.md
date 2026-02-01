---
phase: 05
plan: 05
subsystem: tiered-exposure
tags: [mcp, tiered, eval, debug, cache]
dependency-graph:
  requires: [05-04]
  provides: [mcp-tiered-output, tiered-eval-suite]
  affects: [06-xx]
tech-stack:
  added: []
  patterns: [trait-method-extension, eval-suite-pattern]
key-files:
  created:
    - evals/harness/src/suites/tiered.rs
    - evals/datasets/retrieval/tiered_eval.json
  modified:
    - crates/memd/src/mcp/handlers.rs
    - crates/memd/src/store/mod.rs
    - crates/memd/src/store/persistent.rs
    - evals/harness/src/suites/mod.rs
    - evals/harness/src/main.rs
decisions:
  - id: extend-store-trait
    choice: Added search_with_tier_info and get_tiered_stats to Store trait
    reason: Allow generic handlers to access tiered functionality without downcasting
  - id: default-impl-pattern
    choice: Default implementations return None for non-tiered stores
    reason: Backward compatible with MemoryStore and other non-tiered implementations
  - id: eval-suite-structure
    choice: D1-D7 test hierarchy with warmup, cache, hot, warm, comparison tests
    reason: Follows existing suite patterns (A/B/C) with tiered-specific validation
metrics:
  duration: 8m 10s
  completed: 2026-01-31
---

# Phase 5 Plan 5: MCP Tiered Exposure and Eval Suite Summary

MCP handlers extended with tiered debug output, dedicated eval suite validates cache hits and tier latency.

## What Changed

### MCP Handler Updates (handlers.rs)

1. **SearchParams extended**:
   - Added `debug_tiers: Option<bool>` parameter (default false)
   - When true, response includes per-result tier source and timing info

2. **New response types**:
   - `TierDebugInfo`: Overall tier timing (cache_lookup_ms, hot_tier_ms, warm_tier_ms)
   - `source_tier` field on `ChunkResult`: Which tier returned this result
   - `TieredStatsResult`, `CacheStatsResult`, `HotTierStatsResult`, `TieredMetricsResult`: Tiered stats structures

3. **MetricsParams extended**:
   - Added `include_tiered: bool` parameter (default true)
   - Controls whether tiered stats are included in metrics snapshot

### Store Trait Extension (store/mod.rs)

1. **search_with_tier_info method**:
   - Returns `(Vec<(MemoryChunk, f32)>, Option<TieredTiming>)`
   - Default implementation delegates to search_with_scores with None timing
   - PersistentStore overrides with real tiered timing

2. **get_tiered_stats method**:
   - Returns `Option<TieredStats>` for stores that support tiered search
   - Default returns None, PersistentStore returns real stats

### Tiered Eval Dataset (tiered_eval.json)

- 20 documents total: 10 hot tier, 10 warm tier
- 15 queries total: 5 cache hit, 5 hot tier, 5 warm tier
- Document types: code (functions), decision (ADRs), doc (guides)
- Tags: frequently_accessed (hot), rarely_accessed (warm)

### Tiered Eval Suite (tiered.rs)

Tests organized as Suite D:
- **D1**: Index and warmup (populate access patterns)
- **D2**: Cache hit tests (repeated queries should hit cache)
- **D3**: Hot tier tests (promoted chunks faster)
- **D4**: Warm tier tests (baseline latency)
- **D5**: Latency comparison (hot p50 vs warm p50)
- **D6**: Cache hit rate (>= 80% threshold)
- **D7**: Overall quality thresholds

## Commits

| Commit | Type | Description |
|--------|------|-------------|
| 9cecf34 | feat | Extend MCP handlers with tiered debug output |
| 6090176 | feat | Add tiered eval dataset |
| c132f31 | feat | Add tiered eval suite (Suite D) |

## Decisions Made

### 1. Extend Store Trait vs Downcast

**Choice**: Add trait methods with default implementations

**Reason**:
- Keeps handlers generic over Store
- No unsafe downcasting needed
- MemoryStore and other implementations work unchanged
- PersistentStore provides real tiered info when available

### 2. Eval Suite Structure

**Choice**: D1-D7 hierarchy following existing suite patterns

**Reason**:
- Consistent with A/B/C suites in codebase
- Clear separation of concerns (warmup, cache, hot, warm, comparison)
- Independent tests that can be debugged individually

### 3. Quality Thresholds

**Choice**:
- Cache hit rate >= 80%
- Hot tier p50 <= warm tier p50
- Pass rate >= 80%

**Reason**:
- 80% hit rate realistic for semantic cache with similarity matching
- Latency comparison validates tier promotion works
- Pass rate threshold allows some tolerance for timing variance

## Deviations from Plan

None - plan executed exactly as written.

## Test Evidence

```
# Handler tests
test mcp::handlers::tests::search_with_debug_tiers ... ok
test mcp::handlers::tests::search_empty_store ... ok
... 11 passed

# Tiered suite tests
test suites::tiered::tests::test_tiered_eval_config_defaults ... ok
test suites::tiered::tests::test_calculate_recall ... ok
test suites::tiered::tests::test_percentile ... ok
... 5 passed
```

## Phase 5 Completion Status

All 5 plans in Phase 5 complete:
- 05-01: Semantic cache implementation
- 05-02: Hot tier with HNSW
- 05-03: Access tracker and promotion scoring
- 05-04: TieredSearcher integration
- 05-05: MCP exposure and eval suite

Phase 5 deliverables:
- Semantic query cache with configurable similarity threshold
- Hot tier with HNSW index for promoted chunks
- Access-based promotion scoring with decay
- TieredSearcher coordinating cache->hot->warm fallback
- MCP debug output for tier visibility
- Tiered eval suite validating behavior

## Next Phase Readiness

Phase 6 can proceed. Key integration points:
- `store.search_with_tier_info()` for tiered search with timing
- `store.get_tiered_stats()` for observability
- `debug_tiers` parameter in memory.search for debugging
- Tiered eval suite for regression testing
