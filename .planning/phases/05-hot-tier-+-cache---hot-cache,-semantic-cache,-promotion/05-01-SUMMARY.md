---
phase: 05-hot-tier-+-cache
plan: 01
subsystem: tiered-storage
tags: [hot-tier, access-tracking, hnsw, caching, moka]
requires:
  - 03-dense-retrieval (HnswIndex)
  - 04-hybrid-retrieval (search infrastructure)
provides:
  - AccessTracker for multi-signal promotion scoring
  - HotTier with separate HNSW index
  - Foundation for tiered retrieval (hot/warm)
affects:
  - 05-02 (semantic cache uses access tracker)
  - 05-03 (tiered searcher integrates hot tier)
tech-stack:
  added: [moka]
  patterns: [tiered-storage, access-tracking, promotion-scoring]
key-files:
  created:
    - crates/memd/src/tiered/mod.rs
    - crates/memd/src/tiered/access_tracker.rs
    - crates/memd/src/tiered/hot_tier.rs
  modified:
    - Cargo.toml
    - crates/memd/Cargo.toml
    - crates/memd/src/lib.rs
decisions:
  - id: 05-01-01
    choice: "Frequency weight 0.4, recency weight 0.4, project weight 0.2"
    reason: "Balanced multi-signal scoring without dominant factor"
  - id: 05-01-02
    choice: "Log-normalized frequency scoring"
    reason: "Fair comparison across chunks with different access counts"
  - id: 05-01-03
    choice: "Exponential recency decay with 24h half-life"
    reason: "Recent accesses matter more, but decay not too aggressive"
  - id: 05-01-04
    choice: "Hot tier ef_search=30 (vs warm tier 50)"
    reason: "Faster queries on smaller index, acceptable recall tradeoff"
  - id: 05-01-05
    choice: "Capacity = min(10% of total, 50K hard cap)"
    reason: "Scales with dataset but prevents unbounded memory growth"
metrics:
  duration: 4m
  completed: 2026-02-01
---

# Phase 5 Plan 1: Hot Tier Foundation Summary

Multi-signal access tracking and hot tier with separate HNSW index for frequently accessed chunks.

## Completed Tasks

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Add moka dependency | 8cb298b | Cargo.toml (x2) |
| 2 | Create AccessTracker | c0bf1c1 | access_tracker.rs, mod.rs, lib.rs |
| 3 | Create HotTier | a931a39 | hot_tier.rs |

## Implementation Details

### AccessTracker

Multi-signal scoring for promotion decisions:

- **Frequency**: log2(access_count + 1) normalized by max observed
- **Recency**: exp(-hours_since_last / 24) for exponential decay
- **Project context**: 1.0 if accessed from current project, 0.0 otherwise

Weighted combination: `0.4 * frequency + 0.4 * recency + 0.2 * project`

Key features:
- Thread-safe with parking_lot::RwLock
- Configurable weights and decay half-life
- Minimum accesses threshold for eligibility (default 2)
- Periodic decay_all() to prune low-score entries

### HotTier

Separate HNSW index for promoted chunks:

- Uses ef_search=30 (vs 50 for warm tier) for faster queries
- Stores chunk embeddings for immediate availability
- Version tracking via AtomicU64 for cache invalidation
- Capacity-based eviction of lowest-score chunks

Key methods:
- promote(chunk_id, embedding, tenant_id, score) - add to hot tier
- demote(chunk_id) - remove from hot tier
- search(embedding, k) - search with automatic access recording
- evict_if_needed(total_indexed) - maintain capacity limits

## Deviations from Plan

None - plan executed exactly as written.

## Test Coverage

25 tests across the tiered module:
- 7 access_tracker tests (scoring, eligibility, project context)
- 9 hot_tier tests (promote/demote/search/eviction/stats)
- 9 semantic_cache tests (pre-existing from concurrent work)

## Next Phase Readiness

Ready for 05-02 (Semantic Cache) and 05-03 (Tiered Retrieval Integration):
- AccessTracker provides promotion scoring API
- HotTier provides hot tier search infrastructure
- Version tracking enables cache invalidation
- moka available for semantic cache implementation
