---
phase: "04"
plan: "05"
subsystem: retrieval
tags: [hybrid-search, rrf-fusion, bm25, dense-search, integration]
depends_on:
  requires: ["04-03", "04-04"]
  provides: ["hybrid-search-integration", "memory-search-hybrid"]
  affects: ["05-*"]
tech_stack:
  added: []
  patterns: ["hybrid-search", "rrf-fusion", "store-trait"]
key_files:
  created:
    - crates/memd/src/store/hybrid.rs
  modified:
    - crates/memd/src/store/persistent.rs
    - crates/memd/src/store/mod.rs
    - crates/memd/src/retrieval/mod.rs
decisions:
  - key: "hybrid-via-store-trait"
    value: "HybridSearcher accessed via PersistentStore.search_with_scores()"
    rationale: "Store trait unchanged - hybrid search is implementation detail"
  - key: "sparse-path"
    value: "data_dir/sparse_index for persistent BM25 index"
    rationale: "Consistent with dense index path pattern"
  - key: "hybrid-fallback"
    value: "Fallback to dense-only if sparse unavailable"
    rationale: "Graceful degradation if sparse index fails to initialize"
metrics:
  duration: "5m"
  completed: "2026-01-30"
---

# Phase 4 Plan 5: Hybrid Search Integration Summary

HybridSearcher wiring dense + sparse search through PersistentStore with RRF fusion.

## Completed Tasks

| # | Task | Commit | Key Changes |
|---|------|--------|-------------|
| 1 | Create HybridSearcher | bd365a2 | New hybrid.rs with HybridSearcher, HybridConfig, SearchContext, HybridTiming |
| 2 | Integrate into PersistentStore | 21f572a | Add hybrid_searcher/sparse_index fields, wire add/search/delete |
| 3 | Add integration tests | f3960a3 | 10 tests covering fusion, tenant isolation, edge cases |

## Implementation Details

### HybridSearcher (crates/memd/src/store/hybrid.rs)

```rust
pub struct HybridSearcher {
    dense: Arc<DenseSearcher>,
    sparse: Option<Arc<Bm25Index>>,
    text_processor: TextProcessor,
    fusion: RrfFusion,
    reranker: FeatureReranker,
    packer: ContextPacker,
    config: HybridConfig,
}
```

**Key methods:**
- `index_chunk()` - Index in both dense and sparse
- `search()` - Parallel dense+sparse with RRF fusion
- `search_with_timing()` - Returns HybridTiming breakdown
- `delete_chunk()` - Remove from sparse (dense deletion TODO)
- `rerank_with_metadata()` - Apply recency/project/type bonuses

### Search Flow

1. Dense search: `dense.search_with_timing(tenant_id, query, dense_k)`
2. Sparse search: `sparse.search(tenant_id, query, sparse_k)` if enabled
3. Build FusionCandidate list with source + rank
4. RRF fusion: `fusion.fuse(candidates)` -> sorted by RRF score
5. Return top K as HybridSearchResult

### PersistentStore Integration

```rust
pub struct PersistentStore {
    // ... existing fields
    sparse_index: Option<Arc<Bm25Index>>,
    hybrid_searcher: Option<Arc<HybridSearcher>>,
}
```

**Changes:**
- `open()`: Create Bm25Index at data_dir/sparse_index, create HybridSearcher
- `add()`: Index via HybridSearcher (handles both dense + sparse)
- `search_with_scores()`: Route through HybridSearcher if available
- `delete()`: Remove from hybrid/sparse index
- `shutdown()`: Commit sparse index

## Decisions Made

1. **Store trait unchanged**: HybridSearcher is internal to PersistentStore - MCP handlers use `search_with_scores()` unchanged

2. **Sparse index path**: `data_dir/sparse_index` for persistent BM25

3. **Fallback chain**: HybridSearcher -> DenseSearcher -> text search

4. **Timing metrics**: Dense time + (sparse + fusion time) as search time

## Tests Added

- `test_hybrid_search_basic` - Basic index and search
- `test_keyword_match_improvement` - Sparse finds unique identifiers
- `test_index_and_delete` - Delete removes from sparse
- `test_timing_breakdown` - All timing components populated
- `test_sparse_disabled` - Dense-only fallback works
- `test_rerank_with_metadata` - Reranking with context
- `test_multiple_chunks_fusion` - Multi-chunk RRF fusion
- `test_tenant_isolation` - Sparse enforces tenant separation
- `test_empty_query` - Edge case handling
- `test_config_defaults` - HybridConfig defaults

## Deviations from Plan

None - plan executed exactly as written.

## Configuration

```rust
pub struct HybridConfig {
    pub dense_k: usize,        // default 100
    pub sparse_k: usize,       // default 100
    pub rrf: RrfConfig,        // k=60, equal weights
    pub reranker: RerankerConfig,
    pub packer: PackerConfig,
    pub enable_sparse: bool,   // default true
}

pub struct PersistentStoreConfig {
    // ... existing fields
    pub enable_hybrid_search: bool,  // default true
    pub hybrid_config: Option<HybridConfig>,
}
```

## Verification

- [x] cargo check passes
- [x] memory.search returns hybrid results (via Store trait)
- [x] Keyword queries (exact function names) return relevant results
- [x] Chunks indexed in both dense and sparse on add
- [x] Deleted chunks removed from sparse index

## Next Phase Readiness

Phase 5 (Evaluation & Benchmarks) can proceed:
- HybridSearcher provides search_with_timing for latency measurement
- Full retrieval pipeline operational for quality evaluation
- MCP search returns hybrid results for end-to-end testing
