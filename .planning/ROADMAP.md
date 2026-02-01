# Roadmap: memd

## Overview

This roadmap delivers a complete Architecture A baseline for memd — a local daemon providing intelligent memory management for AI coding agents. The journey progresses from MCP server skeleton through persistent storage, dense/sparse retrieval, hot tier caching, structural indexes, and finally compaction/cleanup. Each phase delivers a coherent, testable capability that builds toward hybrid retrieval with hot/warm/cold tiering.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3, ...): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [x] **Phase 1: Skeleton + MCP Server** - Basic MCP server with stub tools and in-memory store
- [x] **Phase 2: Persistent Cold Store** - Append-only segments, WAL, SQLite metadata, soft deletes
- [x] **Phase 3: Dense Warm Index** - Embeddings interface, HNSW warm tier, basic search
- [x] **Phase 4: Sparse Lexical + Fusion** - BM25 indexing, RRF fusion, feature-based reranker
- [x] **Phase 4.1: Pooling Strategy Support (INSERTED)** - Enable mean/last-token pooling for Qwen3 upgrade
- [x] **Phase 5: Hot Tier + Cache** - Hot cache, semantic cache, promotion/demotion logic
- [x] **Phase 6: Structural Indexes** - AST parsing, symbol tables, trace indexing, query router
- [ ] **Phase 7: Compaction + Cleanup** - Tombstone filtering, segment merges, HNSW rebuild

## Phase Details

### Phase 1: Skeleton + MCP Server
**Goal**: Agents can connect to memd via MCP and invoke memory tools (stubbed) with proper protocol conformance
**Depends on**: Nothing (first phase)
**Requirements**: MCP-01, MCP-02, MCP-03, MCP-04, MCP-05, MCP-06, MCP-07, MCP-08, MCP-09, MCP-10, EVAL-01, EVAL-02, EVAL-03, OBS-01
**Success Criteria** (what must be TRUE):
  1. Agent can connect via stdio and receive tools/list response with all memory tools
  2. Agent can call memory.add and memory.search (returning stub responses)
  3. Agent can call memory.stats and see tenant directory structure
  4. Invalid tool calls return well-formed MCP error objects
  5. Structured JSON logging captures all operations
**Plans**: 4 plans in 4 waves

Plans:
- [x] 01-01-PLAN.md — Initialize Cargo workspace, core types, config loader
- [x] 01-02-PLAN.md — MCP server core with JSON-RPC protocol over stdio
- [x] 01-03-PLAN.md — In-memory store, tool handlers, tenant dirs, logging
- [x] 01-04-PLAN.md — CLI mode and eval harness with MCP conformance tests

### Phase 2: Persistent Cold Store
**Goal**: Memory chunks persist across restarts with crash recovery and tenant isolation
**Depends on**: Phase 1
**Requirements**: STOR-01, STOR-02, STOR-03, STOR-04, STOR-05, STOR-06, STOR-07, STOR-08, STOR-09, EVAL-04, EVAL-05, EVAL-06
**Success Criteria** (what must be TRUE):
  1. Chunks added via memory.add survive daemon restart
  2. Crash mid-ingestion followed by restart recovers without corruption (WAL replay)
  3. Tenant A's chunks are never returned when querying as Tenant B
  4. Deleted chunks (via memory.delete) never appear in any retrieval results
  5. Segment files use mmap for efficient reads
**Plans**: 7 plans in 5 waves

Plans:
- [x] 02-01-PLAN.md — Add Phase 2 dependencies, segment format, segment writer
- [x] 02-02-PLAN.md — WAL format and writer with fsync durability
- [x] 02-03-PLAN.md — SQLite metadata store with tenant isolation indexes
- [x] 02-04-PLAN.md — Tombstone bitset with roaring bitmap
- [x] 02-05-PLAN.md — Segment reader (mmap), WAL reader and recovery
- [x] 02-06-PLAN.md — PersistentStore integrating all components
- [x] 02-07-PLAN.md — Eval Suite A: isolation, recovery, soft delete tests

### Phase 3: Dense Warm Index
**Goal**: Agents can search by semantic similarity using dense vector retrieval
**Depends on**: Phase 2
**Requirements**: DENSE-01, DENSE-02, DENSE-03, DENSE-04, DENSE-05, DENSE-06, DENSE-07, DENSE-08, EVAL-07, EVAL-08, OBS-02, OBS-03
**Success Criteria** (what must be TRUE):
  1. memory.search returns semantically similar chunks ranked by score
  2. Embeddings are generated via ONNX model (with mock fallback for testing)
  3. HNSW warm index supports insert and search operations
  4. Retrieval quality metrics (Recall@k, MRR) measured on synthetic dataset
  5. Metrics endpoint reports index sizes and per-query latency breakdown
**Plans**: 6 plans in 4 waves

Plans:
- [x] 03-01-PLAN.md — Add Phase 3 dependencies, Embedder trait interface
- [x] 03-02-PLAN.md — ONNX embedder with automatic model download
- [x] 03-03-PLAN.md — HNSW warm index with insert, search, persistence
- [x] 03-04-PLAN.md — Integrate dense search into PersistentStore and MCP handlers
- [x] 03-05-PLAN.md — Retrieval quality eval suite with code similarity dataset
- [x] 03-06-PLAN.md — Metrics collection and memory.metrics endpoint

### Phase 4: Sparse Lexical + Fusion
**Goal**: Hybrid retrieval combining dense and lexical signals improves result quality
**Depends on**: Phase 3
**Requirements**: SPARSE-01, SPARSE-02, SPARSE-03, SPARSE-04, SPARSE-05, SPARSE-06, FUSION-01, FUSION-02, FUSION-03, FUSION-04, FUSION-05, FUSION-06, FUSION-07, FUSION-08, EVAL-09, EVAL-10, EVAL-11, EVAL-12
**Success Criteria** (what must be TRUE):
  1. Keyword queries (exact function names, file paths) return relevant results
  2. Hybrid (dense+lexical) retrieval shows measurable quality improvement over dense-only
  3. RRF fusion combines candidate lists with recency and project bonuses
  4. Context packer deduplicates and enforces diversity via MMR
  5. Performance baseline captured (p50/p90/p99 latency, QPS under load)
**Plans**: 6 plans in 5 waves

Plans:
- [x] 04-01-PLAN.md — Add Phase 4 dependencies, text processing module
- [x] 04-02-PLAN.md — BM25 sparse index with Tantivy
- [x] 04-03-PLAN.md — RRF fusion and feature-based reranker
- [x] 04-04-PLAN.md — Context packer with MMR diversity
- [x] 04-05-PLAN.md — Hybrid search integration into PersistentStore
- [x] 04-06-PLAN.md — Hybrid retrieval eval suite with performance baseline

### Phase 4.1: Pooling Strategy Support (INSERTED)
**Goal**: Enable support for multiple pooling strategies (mean, last-token) to unlock next-generation embedding models like Qwen3-Embedding-0.6B
**Depends on**: Phase 4
**Requirements**: Enable model flexibility for 64.33 MTEB score models (+15% improvement over current all-MiniLM-L6-v2)
**Success Criteria** (what must be TRUE):
  1. PoolingStrategy enum supports Mean and LastToken variants
  2. OnnxEmbedder implements last_token_pooling() method
  3. EmbeddingConfig and HnswConfig support configurable dimensions (384 or 1024)
  4. Qwen3-Embedding-0.6B model downloads and generates embeddings correctly
  5. Eval suite shows 92-95% recall improvement (from 87.5% baseline)
  6. All existing tests pass with mean pooling (backward compatibility)
**Plans**: 3 plans in 2 waves

Plans:
- [x] 04.1-01-PLAN.md — PoolingStrategy enum and EmbeddingModel configuration
- [x] 04.1-02-PLAN.md — Last-token pooling implementation and Qwen3 download
- [x] 04.1-03-PLAN.md — CLI integration, dimension validation, quality verification

**Details:**
This phase implements the architectural enhancement documented in `docs/QWEN3_UPGRADE.md`. Current blocker: Available ONNX exports of Qwen3-Embedding-0.6B use last-token pooling, incompatible with our mean-pooling pipeline. Solution adds pooling strategy abstraction enabling both approaches.

Expected effort: 2-4 hours
Expected improvement: 87.5% -> 92-95% recall, 56.3 -> 64.33 MTEB score

### Phase 5: Hot Tier + Cache
**Goal**: Frequently accessed memories are served with low latency from hot tier and cache
**Depends on**: Phase 4.1
**Requirements**: HOT-01, HOT-02, HOT-03, HOT-04, HOT-05, HOT-06, HOT-07, HOT-08, HOT-09, OBS-04, OBS-05
**Success Criteria** (what must be TRUE):
  1. Hot tier queries return results significantly faster than warm tier queries
  2. Repeated similar queries hit semantic cache (visible in debug output)
  3. Cache entries invalidate when underlying memories change (version-based)
  4. Chunks are promoted to hot on repeated retrieval or active project match
  5. Debug flags show cache hit status and promotion/demotion reasoning
**Plans**: 5 plans in 4 waves

Plans:
- [x] 05-01-PLAN.md — Add moka dependency, AccessTracker with multi-signal scoring, HotTier with separate HNSW
- [x] 05-02-PLAN.md — SemanticCache with similarity lookup, TTL, version invalidation
- [x] 05-03-PLAN.md — TieredSearcher coordinating cache/hot/warm fallback, promotion/demotion logic
- [x] 05-04-PLAN.md — Integrate TieredSearcher into HybridSearcher, add tiered metrics
- [x] 05-05-PLAN.md — Extend MCP handlers with tiered stats, tiered eval suite

### Phase 6: Structural Indexes
**Goal**: Code-aware queries find symbols, callers, and traces across the codebase
**Depends on**: Phase 5
**Requirements**: STRUCT-01, STRUCT-02, STRUCT-03, STRUCT-04, STRUCT-05, STRUCT-06, STRUCT-07, STRUCT-08, STRUCT-09, STRUCT-10, STRUCT-11, STRUCT-12, STRUCT-13, STRUCT-14
**Success Criteria** (what must be TRUE):
  1. find_symbol_definition returns function/class definitions by name
  2. find_callers returns all callers of a given function
  3. find_tool_calls retrieves past tool invocations by name and time range
  4. Query router classifies intent and weights retrieval sources appropriately
  5. Structural queries integrated into Suite B with measurable quality metrics
**Plans**: 8 plans in 5 waves

Plans:
- [x] 06-01-PLAN.md — Add tree-sitter dependencies, multi-language parser wrapper
- [x] 06-02-PLAN.md — Symbol extraction from AST, SQLite storage schema
- [x] 06-03-PLAN.md — Call graph extraction, import graph tracking
- [x] 06-04-PLAN.md — MCP tools: find_definition, find_references, find_callers, find_imports
- [x] 06-05-PLAN.md — Trace indexing for tool calls and stack traces
- [x] 06-06-PLAN.md — MCP tools: find_tool_calls, find_errors
- [x] 06-07-PLAN.md — Query router with intent classification, hybrid integration
- [x] 06-08-PLAN.md — Structural eval suite integrated with Suite B

### Phase 7: Compaction + Cleanup
**Goal**: System maintains performance and correctness as data grows and changes
**Depends on**: Phase 6
**Requirements**: COMPACT-01, COMPACT-02, COMPACT-03, COMPACT-04, COMPACT-05, COMPACT-06, EVAL-13
**Success Criteria** (what must be TRUE):
  1. Tombstone filtering ensures deleted chunks never returned in any code path
  2. Sparse segment merges reduce fragmentation without query impact
  3. Warm HNSW rebuild creates clean snapshot without deleted items
  4. Compaction runs with throttling to limit tail latency impact
  5. Results before/after compaction are equivalent (minus deleted chunks)
**Plans**: 6 plans in 5 waves

Plans:
- [ ] 07-01-PLAN.md — Compaction module foundation: metrics gathering and tombstone audit
- [ ] 07-02-PLAN.md — HNSW rebuild and sparse segment merge operations
- [ ] 07-03-PLAN.md — Throttle module for rate-limiting compaction
- [ ] 07-04-PLAN.md — CompactionRunner workflow coordinator
- [ ] 07-05-PLAN.md — PersistentStore and MCP integration (memory.compact tool)
- [ ] 07-06-PLAN.md — Compaction eval suite (Suite F)

## Progress

**Execution Order:**
Phases execute in numeric order: 1 -> 2 -> 3 -> 4 -> 4.1 -> 5 -> 6 -> 7

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Skeleton + MCP Server | 4/4 | Complete | 2026-01-29 |
| 2. Persistent Cold Store | 7/7 | Complete | 2026-01-30 |
| 3. Dense Warm Index | 6/6 | Complete | 2026-01-30 |
| 4. Sparse Lexical + Fusion | 6/6 | Complete | 2026-01-30 |
| 4.1. Pooling Strategy Support | 3/3 | Complete | 2026-01-31 |
| 5. Hot Tier + Cache | 5/5 | Complete | 2026-01-31 |
| 6. Structural Indexes | 8/8 | Complete | 2026-02-01 |
| 7. Compaction + Cleanup | 0/6 | Not started | - |

---
*Roadmap created: 2026-01-29*
*Last updated: 2026-01-31 (Phase 7 planned)*
