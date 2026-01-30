---
phase: "03"
plan: "04"
subsystem: store
tags: [dense-search, embeddings, hnsw, store-trait]
depends_on:
  requires: ["03-02", "03-03"]
  provides: ["dense-search-integration", "store-search_with_scores"]
  affects: ["03-05", "03-06"]
tech_stack:
  added: []
  patterns: ["coordinator-pattern", "trait-default-method"]
key_files:
  created:
    - crates/memd/src/store/dense.rs
  modified:
    - crates/memd/src/store/mod.rs
    - crates/memd/src/store/persistent.rs
    - crates/memd/src/mcp/handlers.rs
decisions:
  - id: "dense-search-coordinator"
    choice: "DenseSearcher coordinates embedder + HNSW per tenant"
    rationale: "Clean separation of concerns, enables testing with MockEmbedder"
  - id: "trait-default-method"
    choice: "search_with_scores as trait default returning score 1.0"
    rationale: "Backward compatible - MemoryStore unchanged, PersistentStore overrides"
  - id: "best-effort-indexing"
    choice: "Index failure doesn't fail add() operation"
    rationale: "Search falls back to text matching if dense search unavailable"
metrics:
  duration: "7m"
  completed: "2026-01-30"
---

# Phase 03 Plan 04: Dense Search Integration Summary

DenseSearcher coordinates embeddings and HNSW index for semantic search in PersistentStore.

## Commits

| Hash | Type | Description |
|------|------|-------------|
| 8f1945d | feat | Create DenseSearcher coordinator |
| 6c393a7 | feat | Integrate DenseSearcher into PersistentStore |
| 07b75e0 | feat | Update MCP handlers to use search_with_scores |

## What Was Built

### DenseSearcher Coordinator (dense.rs)
- Per-tenant HNSW indices with optional persistence
- `index_chunk()` / `index_batch()` for embedding and indexing
- `search()` returns chunk IDs with cosine similarity scores
- `with_embedder()` constructor for testing with MockEmbedder
- Automatic index save on drop when persistence enabled

### Store Trait Enhancement
- `search_with_scores()` default method returns (chunk, score) pairs
- Default implementation calls `search()` with score 1.0
- MemoryStore inherits default behavior unchanged
- PersistentStore overrides with real dense search

### PersistentStore Integration
- `enable_dense_search` config flag (default: true)
- DenseSearcher initialized on open() if enabled
- `add()` indexes chunks in HNSW after storage
- `search_with_scores()` uses dense search when available
- Graceful fallback to text search if dense search fails

### MCP Handler Update
- `handle_memory_search` uses `search_with_scores` uniformly
- No type-specific branching - works with any Store
- Scores pass through from store implementation

## Architecture

```
MCP Handler
    |
    v
Store::search_with_scores()
    |
    +---> MemoryStore (default impl, score=1.0)
    |
    +---> PersistentStore
              |
              +---> DenseSearcher available?
                        |
                        +-- Yes --> embed query --> HNSW search --> get chunks
                        |
                        +-- No --> fallback to text search (score=1.0)
```

## Test Status

Code compiles and passes `cargo check`. Unit tests cannot run due to pre-existing
linker compatibility issue (mold linker + ort-sys glibc C23 symbols). This is an
environmental issue, not a code issue.

## Deviations from Plan

None - plan executed exactly as written.

## Next Phase Readiness

Ready for 03-05 (Integration Tests) and 03-06 (Eval Tests).

Dependencies satisfied:
- Embedder trait implemented (03-01)
- OnnxEmbedder implemented (03-02)
- HnswIndex implemented (03-03)
- DenseSearcher integration complete (this plan)
