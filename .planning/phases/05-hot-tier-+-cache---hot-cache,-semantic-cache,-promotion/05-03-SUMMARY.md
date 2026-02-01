---
phase: 05-hot-tier-+-cache
plan: 03
subsystem: tiered-search
tags: [tiered-search, promotion, demotion, cache, hot-tier, warm-tier]
requires: ["05-01", "05-02"]
provides: ["TieredSearcher coordinator with fallback chain"]
affects: ["05-04"]

tech-stack:
  patterns: ["fallback chain", "tier promotion", "multi-signal scoring"]

key-files:
  created:
    - crates/memd/src/tiered/tiered_searcher.rs
  modified:
    - crates/memd/src/tiered/mod.rs

decisions:
  - id: 05-03-01
    desc: "WarmTierSearch trait abstracts warm tier for testability"
  - id: 05-03-02
    desc: "Demotion threshold at 50% of promotion threshold"
  - id: 05-03-03
    desc: "Query counter resets after demotion check"
  - id: 05-03-04
    desc: "Auto-promotion requires non-zero project component"

metrics:
  duration: 5m
  completed: 2026-02-01
---

# Phase 05 Plan 03: TieredSearcher Coordination Summary

TieredSearcher with cache->hot->warm fallback chain, multi-signal promotion/demotion, and debug instrumentation.

## What Was Built

### TieredSearcher Coordinator (1169 lines)
Central search coordinator that routes queries through tiered architecture:

1. **Fallback Chain**
   - Semantic cache lookup first (sub-ms on hit)
   - Hot tier search on cache miss (faster HNSW)
   - Warm tier search as fallback (main index)

2. **Result Merging**
   - Deduplicates by chunk_id
   - Prefers hot tier scores when chunk in both tiers
   - Sorts by score descending, returns top k

3. **Access Recording**
   - Records AccessEvent for all returned chunks
   - Includes project context when provided
   - Feeds promotion scoring system

4. **Promotion Logic**
   - `check_promotions()` evaluates top 100 access tracker candidates
   - Promotes if score >= threshold (0.4 default) and not in hot tier
   - Gets embedding from warm tier for hot tier insertion
   - Returns TierDecision with reasoning

5. **Demotion Logic**
   - `check_demotions()` runs after N queries (100 default)
   - Demotes if score < 50% of promotion threshold
   - Resets query counter after check

6. **Maintenance**
   - `run_maintenance()` runs promotions, demotions, evictions, cache pruning
   - Returns MaintenanceResult with counts and decisions

7. **Project-based Fast Promotion**
   - `maybe_promote_on_access()` for immediate promotion
   - Requires project context match and eligible score
   - Enables hot tier for active project context

### Data Structures

| Structure | Purpose |
|-----------|---------|
| TieredSearcherConfig | Cache/hot tier enable, thresholds, debug flag |
| SourceTier | Cache, Hot, Warm enum for result origin |
| TierAction | Promote, Demote, None actions |
| TierDecision | Decision record with reason and score |
| ScoredChunk | Chunk with score and source tier |
| TieredTiming | Per-tier latency breakdown |
| TieredSearchResult | Results, timing, cache/hot hit flags |
| MaintenanceResult | Promotion/demotion/eviction counts |
| WarmTierSearch | Trait for warm tier abstraction |

### Key Patterns

1. **Tier Fallback Chain**
   ```
   Query -> Cache (hit?) -> Hot (results?) -> Warm -> Merge -> Cache insert
   ```

2. **Promotion Flow**
   ```
   AccessTracker candidates -> Filter (not in hot, >= threshold)
   -> Get embedding from warm -> Promote to hot -> TierDecision
   ```

3. **Demotion Flow**
   ```
   Query count >= threshold -> Reset counter -> Check hot tier chunks
   -> Score < 50% threshold -> Demote -> TierDecision
   ```

## Verification Results

All tests pass (13/13):
- test_config_defaults
- test_tiered_searcher_creation
- test_source_tier_equality
- test_mock_warm_tier
- test_warm_tier_fallback
- test_hot_tier_fallback
- test_cache_hit_fast_path
- test_promotion_on_repeated_access
- test_demotion_after_inactivity
- test_project_based_promotion
- test_debug_tier_decisions
- test_maintenance_result
- test_query_counter_increments

## Decisions Made

| ID | Decision | Rationale |
|----|----------|-----------|
| 05-03-01 | WarmTierSearch trait | Enables testing with MockWarmTier without real index |
| 05-03-02 | Demotion at 50% threshold | Hysteresis prevents rapid promote/demote cycles |
| 05-03-03 | Query counter reset | Ensures periodic demotion checks, not continuous |
| 05-03-04 | Project component required | Auto-promotion only for active project context |

## Deviations from Plan

None - plan executed exactly as written.

## Files Changed

| File | Lines | Change |
|------|-------|--------|
| crates/memd/src/tiered/tiered_searcher.rs | +1169 | New file with TieredSearcher |
| crates/memd/src/tiered/mod.rs | +8 | Export tiered_searcher types |

## Next Phase Readiness

Ready for 05-04 (MCP integration):
- TieredSearcher can be integrated into MCP handlers
- WarmTierSearch trait allows wrapping existing DenseSearcher/HybridSearcher
- Maintenance can be run periodically or on-demand
- Debug output available for diagnostics
