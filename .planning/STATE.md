# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-01-29)

**Core value:** Agents can find and use relevant past context--across sessions, projects, and time--without hitting context window limits or losing continuity.
**Current focus:** Phase 4 COMPLETE - Ready for Phase 5 (Streaming)

## Current Position

Phase: 4 of 7 (Sparse Lexical + Fusion) - COMPLETE
Plan: 6 of 6 in current phase
Status: Phase complete
Last activity: 2026-01-30 -- Completed 04-06-PLAN.md (Hybrid Evaluation Suite)

Progress: [=============================================------] ~100% (23 of ~23 total plans estimated)

## Performance Metrics

**Velocity:**
- Total plans completed: 23
- Average duration: 6m
- Total execution time: ~136 minutes

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 01 | 4 | 39m | 10m |
| 02 | 7 | 38m | 5m |
| 03 | 6 | 37m | 6m |
| 04 | 6 | 27m | 4m |

**Recent Trend:**
- Last 5 plans: 04-02 (5m), 04-03 (4m), 04-04 (4m), 04-05 (5m), 04-06 (5m)
- Trend: Phase 4 complete with hybrid eval suite

*Updated after each plan completion*

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- Roadmap: Architecture A first (Milestones 1-7), Architecture B as pluggable module later
- Roadmap: MCP over custom protocol for agent integration
- Roadmap: Rust for mmap control, concurrency, single-binary packaging
- 01-01: UUIDv7 for ChunkId (time-sortable identifiers)
- 01-01: TenantId restricted to alphanumeric + underscore (safe for paths)
- 01-01: XDG config location (~/.config/memd/config.toml)
- 01-02: Protocol version 2024-11-05 for MCP compatibility
- 01-02: Logs to stderr in MCP mode, responses to stdout
- 01-02: Tool responses use MCP content format with type=text
- 01-03: SHA-256 for content hashing (industry standard)
- 01-03: RwLock for thread-safe in-memory store
- 01-03: Lazy tenant directory creation (on first add)
- 01-04: CLI mode uses pretty logging, MCP mode uses JSON logging
- 01-04: Eval harness builds memd before running tests
- 01-04: Each eval test starts a fresh memd subprocess
- 02-01: PayloadIndexRecord is 16-byte repr(C) for consistent memory layout
- 02-01: Little-endian encoding via byteorder for cross-platform compatibility
- 02-01: bincode with serde feature for metadata serialization
- 02-01: 6-digit zero-padded segment IDs (seg_000001) for sorting
- 02-04: Roaring bitmap for space-efficient tombstone storage
- 02-04: Atomic file persistence: temp file + rename + fsync
- 02-02: sync_all() after EVERY WAL write for durability
- 02-02: open_or_create() primary entry for WAL startup
- 02-03: WAL mode with synchronous=NORMAL for SQLite
- 02-03: 5s busy_timeout to prevent SQLITE_BUSY
- 02-03: All queries filter tenant_id first in WHERE clause
- 02-05: parse_all() on PayloadIndexRecord for batch index parsing
- 02-05: Recovery replay skips existing chunk_ids (idempotent)
- 02-05: WalReader tolerates partial records (stops at first error)
- 02-06: INSERT OR REPLACE for crash recovery idempotency
- 02-06: SegmentWriter::read_chunk flushes buffer before reading
- 02-06: Recovery checks segment readability, not just metadata existence
- 02-07: extract_content_text helper for consistent MCP response parsing
- 02-07: McpClient::start_with_args takes PathBuf reference for flexibility
- 03-01: ort 2.0.0-rc.11 for ONNX Runtime (prerelease, stable not yet released)
- 03-01: tls-native feature required for ort download-binaries
- 03-01: DefaultHasher for deterministic mock embeddings (reproducible tests)
- 03-01: Default dimension 384 matching all-MiniLM-L6-v2 model
- 03-02: ort std feature required for commit_from_file
- 03-02: ndarray 0.17 for ort 2.0.0-rc.11 compatibility
- 03-02: Mutex<Session> for thread-safe inference
- 03-02: Mean pooling with attention mask for sentence embeddings
- 03-03: anndists 0.1 for DistCosine distance function (required by hnsw_rs)
- 03-03: HnswConfig defaults M=16, efConstruction=200, efSearch=50
- 03-03: IndexMapping for bidirectional chunk_id to internal ID mapping
- 03-03: Partial persistence - save works, load returns empty graph due to hnsw_rs lifetime
- 03-04: DenseSearcher coordinates embedder + HNSW per tenant
- 03-04: search_with_scores as trait default returning score 1.0
- 03-04: Index failure doesn't fail add() operation (best-effort)
- 03-05: Handcrafted code samples for Phase 3 (Phase 4 adds benchmark datasets)
- 03-05: Document IDs tracked via tags field for retrieval evaluation
- 03-05: Quality thresholds: Recall@10 > 0.8, MRR > 0.6
- 03-06: Circular buffer for recent queries (default 1000)
- 03-06: Atomic counters for cumulative totals (lock-free accumulation)
- 03-06: search_with_timing returns (results, embed_time, search_time) tuple
- 03-06: Memory estimate uses 2x multiplier on embedding bytes for HNSW overhead
- 04-01: tantivy 0.24 for BM25 (mature, battle-tested inverted index)
- 04-01: rust-stemmers for Porter algorithm (simple, effective for English prose)
- 04-01: Acronyms (2+ uppercase) preserved during normalization
- 04-01: Heuristic code detection via syntax patterns (braces, keywords, operators)
- 04-01: Code blocks kept together as single 'sentences' for indexing
- 04-02: 50MB default writer memory budget for Tantivy IndexWriter
- 04-02: Commit after batch insert for immediate searchability
- 04-02: BooleanQuery for tenant isolation (must match tenant AND query)
- 04-02: Sentence-level indexing with sentence_idx for fine-grained results
- 04-02: IndexReader with OnCommitWithDelay reload policy
- 04-03: RRF fusion with configurable k constant (default 60)
- 04-03: Source weights for dense/sparse contribution balance
- 04-03: FeatureReranker with recency/project/type bonuses
- 04-04: Hash-based dedup before similarity-based (cheap operation first)
- 04-04: MMR lambda default 0.7 (favor relevance slightly over diversity)
- 04-04: Type diversity fallback when no embeddings available
- 04-04: Chars per token = 4 for token estimation
- 04-05: HybridSearcher accessed via PersistentStore.search_with_scores()
- 04-05: Sparse index path at data_dir/sparse_index
- 04-05: Fallback chain: hybrid -> dense -> text search
- 04-06: Quality thresholds: keyword 0.9, semantic 0.7, mixed 0.75
- 04-06: Performance targets: p50 < 100ms, p99 < 500ms
- 04-06: 3 iterations for performance sampling (36 queries total)

### Pending Todos

None.

### Blockers/Concerns

- Pre-existing linker issue: mold linker incompatible with ort-sys glibc C23 symbols
  - Affects: cargo test/build (linking phase)
  - Does not affect: cargo check
  - Resolution: Update system libc or change linker config

## Session Continuity

Last session: 2026-01-30 08:47 UTC
Stopped at: Completed 04-06-PLAN.md (Hybrid Evaluation Suite) - PHASE 4 COMPLETE
Resume file: None
