# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-01-29)

**Core value:** Agents can find and use relevant past context--across sessions, projects, and time--without hitting context window limits or losing continuity.
**Current focus:** Phase 6 (Structural Indexes)

## Current Position

Phase: 6 of 6 (Structural Indexes)
Plan: 07 of 08 (Query Router)
Status: Plan complete
Last activity: 2026-02-01 -- Completed 06-07-PLAN.md (Query Router)

Progress: [================================================----] 97% (38 of 39 total plans)
**Phase 6 PROGRESS**: Query router complete (intent classification, routing, STRUCT-14 blending)

## Performance Metrics

**Velocity:**
- Total plans completed: 37
- Average duration: 6m
- Total execution time: ~241 minutes

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 01 | 4 | 39m | 10m |
| 02 | 7 | 38m | 5m |
| 03 | 6 | 37m | 6m |
| 04 | 6 | 27m | 4m |
| 04.1 | 3 | 24m | 8m |
| 05 | 5 | 53m | 11m |
| 06 | 8 | 55m | 7m |

**Recent Trend:**
- Last 5 plans: 06-04 (7m), 06-05 (18m), 06-06 (12m), 06-07 (6m)
- Trend: Phase 6 progressing - query router complete

*Updated after each plan completion*

## Accumulated Context

### Roadmap Evolution

- Phase 4.1 inserted after Phase 4: Pooling Strategy Support (URGENT)
  - Reason: Enable next-generation embedding models (Qwen3-Embedding-0.6B)
  - Impact: +15% MTEB score improvement (56.3 -> 64.33), projected 92-95% recall
  - Blocker removed: Incompatible pooling strategies (last-token vs mean)
  - Inserted: 2026-01-30
  - **Benchmark results** (2026-01-30): No quality difference on hybrid eval dataset
    - all-MiniLM: Recall@10: 0.833, p50: 10.4ms (6/7 tests passed)
    - Qwen3: Recall@10: 0.833, p50: 49.2ms (6/7 tests passed, 4.7x slower)
    - Conclusion: Keep all-MiniLM as default; infrastructure ready for future testing

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
- 04.1-01: PoolingStrategy derives Default with Mean as default (backward compatible)
- 04.1-01: EmbeddingModel enum encapsulates dimension, pooling, URLs per model
- 04.1-01: Legacy download functions preserved for backward compatibility
- 04.1-01: Model enum pattern: all model-specific config in one place
- 04.1-02: ArrayViewD for pooling methods (matches ONNX output type)
- 04.1-02: Match-based retry for token_type_ids fallback (avoids borrow issues)
- 04.1-02: Instruction prefix only for queries (documents don't need it)
- 04.1-02: Left-padding detection via all() before per-batch processing
- 04.1-03: ModelChoice CLI enum converts to EmbeddingModel via From trait
- 04.1-03: Default embedding model remains all-MiniLM-L6-v2 for backward compatibility
- 04.1-03: DenseSearcher updates HNSW dimension based on model selection
- 04.1-03: Dimension mismatch errors suggest delete data dir or --rebuild-index
- 04.1-BENCH: Comparative benchmark infrastructure via evals/scripts/compare_models.sh
- 04.1-BENCH: all-MiniLM remains default (identical quality, 4.7x faster on hybrid eval)
- 04.1-BENCH: KV-cache support enables decoder-style models (position_ids + 56 cache tensors)
- 05-01: Multi-signal promotion scoring with frequency, recency, and project context
- 05-01: Exponential decay for access recency (configurable half-life)
- 05-01: AccessTracker with promotion_score() method for hot tier decisions
- 05-02: Similarity threshold 0.85 for semantic cache hits
- 05-02: Initial confidence 0.5 with 0.1 boost per hit
- 05-02: TTL 45 minutes for semantic cache entries
- 05-02: SHA-256 first 16 bytes for cache key generation
- 05-02: Version >= for cache validity (not stale if version matches or newer)
- 05-03: WarmTierSearch trait abstracts warm tier for testability
- 05-03: Demotion threshold at 50% of promotion threshold (hysteresis)
- 05-03: Query counter resets after demotion check (periodic checks)
- 05-03: Auto-promotion requires non-zero project component
- 05-04: Per-tenant TieredSearchers with shared SemanticCache
- 05-04: WarmTierAdapter bridges DenseSearcher to WarmTierSearch
- 05-04: TieredMetrics added to MetricsCollector for tier tracking
- 05-04: Delete propagates to cache/hot tier invalidation
- 05-05: search_with_tier_info trait method with default impl for Store
- 05-05: get_tiered_stats trait method returns Option<TieredStats>
- 05-05: Tiered eval suite D1-D7 following existing A/B/C suite patterns
- 05-05: Quality thresholds: cache hit rate >= 80%, hot p50 <= warm p50
- 06-01: tree-sitter 0.25 for grammar ABI version 15 compatibility
- 06-01: Fresh LanguageSupport per parse (Parser not Send/Sync, cheap creation)
- 06-01: Map .c/.h to C++ grammar (avoids separate tree-sitter-c)
- 06-02: streaming_iterator for tree-sitter query matches (matches call_graph.rs pattern)
- 06-02: TypeScript queries use type_identifier for class/interface names
- 06-02: SymbolIndexer deletes before insert for re-indexing (clean slate)
- 06-03: streaming-iterator for tree-sitter 0.25 QueryMatches API
- 06-03: CallType enum: Direct, Method, Qualified for call classification
- 06-03: Batch insert methods for efficient indexing
- 06-03: Re-indexing deletes old edges before inserting new
- 06-03: Aliased Python imports via aliased_import node pattern
- 06-04: Kind priority sorting: function > method > class > interface > type > enum > variable > constant > module
- 06-04: Multi-hop caller traversal limited to 1-3 hops with cycle detection
- 06-04: SymbolQueryService uses Optional initialization in McpServer
- 06-04: Depth parameter for find_callers clamped to valid range (1-3)
- 06-05: TimeRange struct for filtering time-based trace queries
- 06-05: Dynamic SQL with parameter counting for optional filters
- 06-05: Auto-detect trace format based on content patterns
- 06-05: Normalize error signatures by removing addresses/timestamps/UUIDs
- 06-06: Use alias StructuralTimeRange to avoid collision with handlers.TimeRange
- 06-06: Parse ISO 8601 timestamps manually without chrono dependency
- 06-06: TraceQueryService pattern wraps StructuralStore for high-level trace queries
- 06-07: Regex patterns for natural language intent detection
- 06-07: Explicit prefixes (def:, callers:, refs:, etc.) override pattern detection
- 06-07: BlendStrategy::StructuralPrimary as default (structural authoritative)
- 06-07: QueryIntent::SemanticSearch as default variant (safe fallback)

### Pending Todos

None.

### Blockers/Concerns

- **ACTIVE: Migrating to Candle (Pure Rust) for Production**
  - **Problem**: ONNX Runtime (C++) has glibc 2.38+ requirement, Python subprocess too slow (30-80 req/s)
  - **Solution**: Candle (pure Rust) - no C++, no Python, 100+ req/s capable
  - **Status**: Implementation plan ready based on 2 Codex critical reviews
  - **Reviews**:
    - `docs/codex-review.md` - Python implementation review (NEEDS CHANGES)
    - `docs/codex-candle-review.md` - Candle strategy review (APPROVED with fixes)
  - **Plan**: `.planning/CANDLE_IMPLEMENTATION_PLAN.md` (addresses all critical findings)
  - **Estimated Time**: 1.5-2 days (12-17 hours)
  - **Critical Fixes Required**:
    1. HNSW lock order inversion (deadlock risk)
    2. Cache integrity gaps (validity flags not in CRC)
    3. Global mutex bottleneck (add model pool)
    4. Proper tokenization (truncation + padding)
    5. Robust model loading (validation + error handling)
  - **Expected Results**: 120-250 req/s, <12ms p50 latency, single binary
  - **Start Point**: `START_HERE.md` (for context reset)

- ~~System glibc version prevents ort-sys linking~~ **SUPERSEDED BY CANDLE**
  - Root cause: System glibc 2.35 < ort-sys requires glibc 2.38+ (C23 standard)
  - Docker workaround worked but added complexity
  - **Permanent fix**: Candle (pure Rust, no C++ dependencies)

## Session Continuity

Last session: 2026-02-01 03:17 UTC
Stopped at: Completed 06-07-PLAN.md (Query Router)
Resume file: None

**Latest work:**
- Completed 06-07: Query router for intent classification and routing with STRUCT-14 blending

**Phase 6 Progress:**
- Plan 01: Tree-sitter Parser - COMPLETE
- Plan 02: Symbol Extractor + Storage - COMPLETE
- Plan 03: Call Graph - COMPLETE
- Plan 04: Structural Search - COMPLETE
- Plan 05: Trace Storage - COMPLETE
- Plan 06: Debug Trace Tools - COMPLETE
- Plan 07: Query Router - COMPLETE
- Plan 08: Eval Suite - pending
