# Roadmap: memd

## Overview

This roadmap delivers a complete Architecture A baseline for memd — a local daemon providing intelligent memory management for AI coding agents. The journey progresses from MCP server skeleton through persistent storage, dense/sparse retrieval, hot tier caching, structural indexes, and finally compaction/cleanup. Each phase delivers a coherent, testable capability that builds toward hybrid retrieval with hot/warm/cold tiering.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3, ...): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [x] **Phase 1: Skeleton + MCP Server** - Basic MCP server with stub tools and in-memory store
- [ ] **Phase 2: Persistent Cold Store** - Append-only segments, WAL, SQLite metadata, soft deletes
- [ ] **Phase 3: Dense Warm Index** - Embeddings interface, HNSW warm tier, basic search
- [ ] **Phase 4: Sparse Lexical + Fusion** - BM25 indexing, RRF fusion, feature-based reranker
- [ ] **Phase 5: Hot Tier + Cache** - Hot cache, semantic cache, promotion/demotion logic
- [ ] **Phase 6: Structural Indexes** - AST parsing, symbol tables, trace indexing, query router
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
- [ ] 02-01-PLAN.md — Add Phase 2 dependencies, segment format, segment writer
- [ ] 02-02-PLAN.md — WAL format and writer with fsync durability
- [ ] 02-03-PLAN.md — SQLite metadata store with tenant isolation indexes
- [ ] 02-04-PLAN.md — Tombstone bitset with roaring bitmap
- [ ] 02-05-PLAN.md — Segment reader (mmap), WAL reader and recovery
- [ ] 02-06-PLAN.md — PersistentStore integrating all components
- [ ] 02-07-PLAN.md — Eval Suite A: isolation, recovery, soft delete tests

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
**Plans**: TBD

Plans:
- [ ] 03-01: TBD
- [ ] 03-02: TBD

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
**Plans**: TBD

Plans:
- [ ] 04-01: TBD
- [ ] 04-02: TBD

### Phase 5: Hot Tier + Cache
**Goal**: Frequently accessed memories are served with low latency from hot tier and cache
**Depends on**: Phase 4
**Requirements**: HOT-01, HOT-02, HOT-03, HOT-04, HOT-05, HOT-06, HOT-07, HOT-08, HOT-09, OBS-04, OBS-05
**Success Criteria** (what must be TRUE):
  1. Hot tier queries return results significantly faster than warm tier queries
  2. Repeated similar queries hit semantic cache (visible in debug output)
  3. Cache entries invalidate when underlying memories change (version-based)
  4. Chunks are promoted to hot on repeated retrieval or active project match
  5. Debug flags show cache hit status and promotion/demotion reasoning
**Plans**: TBD

Plans:
- [ ] 05-01: TBD
- [ ] 05-02: TBD

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
**Plans**: TBD

Plans:
- [ ] 06-01: TBD
- [ ] 06-02: TBD
- [ ] 06-03: TBD

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
**Plans**: TBD

Plans:
- [ ] 07-01: TBD
- [ ] 07-02: TBD

## Progress

**Execution Order:**
Phases execute in numeric order: 1 -> 2 -> 3 -> 4 -> 5 -> 6 -> 7

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Skeleton + MCP Server | 4/4 | Complete | 2026-01-29 |
| 2. Persistent Cold Store | 0/7 | Planned | - |
| 3. Dense Warm Index | 0/TBD | Not started | - |
| 4. Sparse Lexical + Fusion | 0/TBD | Not started | - |
| 5. Hot Tier + Cache | 0/TBD | Not started | - |
| 6. Structural Indexes | 0/TBD | Not started | - |
| 7. Compaction + Cleanup | 0/TBD | Not started | - |

---
*Roadmap created: 2026-01-29*
*Last updated: 2026-01-29*
