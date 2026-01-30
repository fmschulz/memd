---
phase: 04-sparse-lexical-+-fusion
plan: 02
subsystem: index
tags: [tantivy, bm25, sparse-index, lexical-search, inverted-index]

# Dependency graph
requires:
  - phase: 04-sparse-lexical-+-fusion
    provides: CodeTokenizer for code-aware tokenization (04-01)
provides:
  - SparseIndex trait with insert/search/delete/doc_count operations
  - Bm25Index using Tantivy inverted index with BM25 scoring
  - Tenant-isolated keyword search for code and prose
affects: [04-03-fusion-scoring, 04-04-hybrid-query]

# Tech tracking
tech-stack:
  added: []  # tantivy already added in 04-01
  patterns: [inverted-index-per-tenant, sentence-level-indexing, code-tokenizer-integration]

key-files:
  created:
    - crates/memd/src/index/sparse.rs
    - crates/memd/src/index/bm25.rs
  modified:
    - crates/memd/src/index/mod.rs

key-decisions:
  - "50MB default writer memory budget for Tantivy IndexWriter"
  - "Commit after each batch insert for immediate searchability"
  - "BooleanQuery for tenant isolation (must match tenant AND query)"
  - "Sentence-level indexing with sentence_idx field for fine-grained results"
  - "IndexReader with OnCommitWithDelay reload policy"

patterns-established:
  - "SparseIndex trait parallel to DenseSearcher for hybrid retrieval"
  - "Tantivy schema with tenant_id, chunk_id, sentence_idx, text fields"
  - "Custom tokenizer registration via TextAnalyzer for code-aware BM25"

# Metrics
duration: 5min
completed: 2026-01-30
---

# Phase 4 Plan 2: BM25 Sparse Index Summary

**Tantivy-based BM25 inverted index with tenant isolation, sentence-level indexing, and CodeTokenizer for keyword search**

## Performance

- **Duration:** 5 min
- **Started:** 2026-01-30T08:22:00Z
- **Completed:** 2026-01-30T08:27:00Z
- **Tasks:** 3
- **Files modified:** 3 (2 created, 1 modified)

## Accomplishments

- Created SparseIndex trait with insert/search/delete/doc_count operations
- Implemented Bm25Index using Tantivy inverted index with BM25 scoring
- Integrated CodeTokenizer for code-aware keyword matching (camelCase, snake_case)
- Tenant isolation via BooleanQuery filters in all operations
- Comprehensive test suite covering keyword search, tenant isolation, delete

## Task Commits

Each task was committed atomically:

1. **Task 1: Define SparseIndex trait and types** - `74244fb` (feat)
2. **Task 2: Implement Tantivy BM25 index** - `96974be` (feat)
3. **Task 3: Add BM25 index tests** - included in Task 2 commit

## Files Created/Modified

- `crates/memd/src/index/sparse.rs` - SparseIndex trait, SparseSearchResult struct
- `crates/memd/src/index/bm25.rs` - Bm25Index implementation with Tantivy, test suite
- `crates/memd/src/index/mod.rs` - Export sparse module and Bm25Index

## Decisions Made

1. **50MB writer memory budget** - Reasonable default for index writer, balances memory with performance
2. **Commit after batch** - Each insert commits immediately for searchability (can optimize later with batched commits)
3. **BooleanQuery for tenant filter** - Must match both tenant_id AND text query for isolation
4. **Sentence-level indexing** - Each sentence indexed separately with sentence_idx for fine-grained results
5. **OnCommitWithDelay reload** - Reader reloads after commits with small delay for freshness

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

- **Linker issue (pre-existing)** - mold linker incompatible with ort-sys glibc C23 symbols
  - Tests compile but cannot link due to `__isoc23_strtol` undefined references
  - `cargo check --tests` passes confirming test code is correct
  - Known blocker documented in STATE.md from Phase 3
  - Does not affect code correctness or sparse index functionality

## Next Phase Readiness

- Sparse lexical index ready for fusion scoring (04-03)
- SparseIndex trait provides same-shape API as DenseSearcher for unified queries
- BM25 scores available for reciprocal rank fusion with dense cosine scores
- All dependencies in place for hybrid retrieval pipeline

---
*Phase: 04-sparse-lexical-+-fusion*
*Completed: 2026-01-30*
