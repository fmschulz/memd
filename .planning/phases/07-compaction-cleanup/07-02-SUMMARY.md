---
phase: 07-compaction-cleanup
plan: 02
subsystem: compaction
tags: [compaction, hnsw, rebuild, segment, merge]

dependency-graph:
  requires: [07-01]
  provides: [hnsw-rebuild, segment-merge, compaction-operations]
  affects: [07-03]

tech-stack:
  added: []
  patterns: [stateless-rebuilder, tantivy-merge-policy, atomic-swap-ready]

key-files:
  created:
    - crates/memd/src/compaction/hnsw_rebuild.rs
    - crates/memd/src/compaction/segment_merge.rs
  modified:
    - crates/memd/src/compaction/mod.rs
    - crates/memd/src/index/hnsw.rs
    - crates/memd/src/index/bm25.rs

decisions:
  - id: 07-02-01
    what: "HnswRebuilder returns raw Hnsw, not HnswIndex"
    why: "Allows rebuild to run in background while old index serves queries; caller handles atomic swap"
  - id: 07-02-02
    what: "get_embedding_cache() returns &RwLock<EmbeddingCache>"
    why: "Provides read access to cache while preserving lock semantics for concurrent access"
  - id: 07-02-03
    what: "SegmentMerger triggers merge via commit(), relies on Tantivy LogMergePolicy"
    why: "Tantivy automatically merges during commit; more aggressive policies require writer recreation"
  - id: 07-02-04
    what: "Default min_segments_for_merge threshold is 4"
    why: "Tantivy typically creates one segment per commit; 4+ indicates fragmentation worth addressing"

metrics:
  duration: 7m
  completed: 2026-02-01
---

# Phase 07 Plan 02: Compaction Implementation Summary

HNSW rebuild from embedding cache excluding deleted entries, and Tantivy segment merge via built-in LogMergePolicy.

## What Was Built

### 1. HnswIndex Accessors

Added pub(crate) accessors for compaction to read internal state:

```rust
impl HnswIndex {
    /// Get read access to the embedding cache (for compaction rebuild)
    pub(crate) fn get_embedding_cache(&self) -> &RwLock<EmbeddingCache>

    /// Get read access to the index mapping (for compaction)
    pub(crate) fn get_mapping(&self) -> &RwLock<IndexMapping>

    /// Get the HNSW configuration
    pub fn config(&self) -> &HnswConfig
}
```

### 2. HnswRebuilder

Stateless utility for clean HNSW rebuild:

```rust
pub struct HnswRebuilder;

impl HnswRebuilder {
    pub fn new() -> Self;

    pub fn rebuild_clean(
        &self,
        source_index: &HnswIndex,
        deleted_internal_ids: &HashSet<usize>,
        config: &HnswConfig,
    ) -> Result<(Hnsw<'static, f32, DistCosine>, RebuildResult)>;
}

pub struct RebuildResult {
    pub embeddings_processed: usize,
    pub embeddings_included: usize,
    pub embeddings_excluded: usize,
    pub duration: Duration,
}
```

### 3. SegmentMerger

Triggers Tantivy's built-in merge policy:

```rust
pub struct SegmentMerger {
    min_segments_for_merge: usize,  // default 4
    max_docs_before_merge: usize,   // default 100_000
    del_docs_ratio: f32,            // default 0.2
}

impl SegmentMerger {
    pub fn new() -> Self;
    pub fn with_config(min_segments: usize, max_docs: usize, del_ratio: f32) -> Self;
    pub fn merge(&self, index: &Bm25Index) -> Result<MergeResult>;
    pub fn needs_merge(&self, current_segment_count: usize) -> bool;
}

pub struct MergeResult {
    pub segments_before: usize,
    pub segments_after: usize,
    pub segments_merged: usize,
    pub docs_before: u64,
    pub docs_after: u64,
    pub duration: Duration,
}
```

### 4. Bm25Index.segment_count()

Added method for compaction metrics:

```rust
impl Bm25Index {
    pub fn segment_count(&self) -> Result<usize>;
}
```

## Key Design Decisions

1. **Raw Hnsw return**: HnswRebuilder returns Hnsw graph, not HnswIndex, enabling background rebuild with atomic swap
2. **RwLock accessor**: get_embedding_cache() returns lock reference to preserve concurrent access semantics
3. **Tantivy policy**: SegmentMerger uses built-in LogMergePolicy via commit() rather than custom policy
4. **Segment threshold**: Default 4 segments before merge; each commit typically creates one segment

## Test Coverage

- 4 HnswRebuilder tests (empty, no deletions, with deletions, duration)
- 6 SegmentMerger tests (empty, with data, threshold checks, custom config)
- All existing HNSW and BM25 tests continue to pass (21 tests total)

## Deviations from Plan

None - plan executed exactly as written.

## Commits

| Hash | Description |
|------|-------------|
| d046ef1 | feat(07-02): add embedding cache accessor to HnswIndex |
| c3cacee | feat(07-02): create HNSW rebuild module |
| b58b36f | feat(07-02): create sparse segment merge module |
| 743aa6f | feat(07-02): add segment_count method to Bm25Index |

## Next Phase Readiness

Ready for 07-03 (Cleanup and Validation):
- HnswRebuilder ready to be called by CompactionManager
- SegmentMerger ready to compact sparse index
- Result structs provide metrics for monitoring
- segment_count() enables threshold checking
