# Requirements: memd

**Defined:** 2026-01-29
**Core Value:** Agents can find and use relevant past context—across sessions, projects, and time—without hitting context window limits or losing continuity.

## v1 Requirements

Requirements for Architecture A baseline (Milestones 1-7). Each maps to roadmap phases.

### MCP Server Foundation

- [x] **MCP-01**: MCP server implements stdio transport with JSON-RPC protocol
- [x] **MCP-02**: Server exposes memory.search tool with tenant_id, query, filters, k parameters
- [x] **MCP-03**: Server exposes memory.add tool with chunk fields (type, text, source, tenant, project)
- [x] **MCP-04**: Server exposes memory.add_batch tool for batch ingestion
- [x] **MCP-05**: Server exposes memory.get tool to fetch chunk by id
- [x] **MCP-06**: Server exposes memory.delete tool for soft deletes
- [x] **MCP-07**: Server exposes memory.stats tool for index sizes, tier counts, version
- [x] **MCP-08**: Config loader reads TOML configuration files
- [x] **MCP-09**: Tenant directory structure initialized per tenant_id
- [x] **MCP-10**: Simple in-memory store for initial development

### Storage & Persistence

- [ ] **STOR-01**: Append-only segment format with payload.bin, payload.idx, emb_int8.bin, meta, tombstone
- [ ] **STOR-02**: Segments support mmap reads for efficient cold tier access
- [ ] **STOR-03**: WAL (write-ahead log) records ingestion operations
- [ ] **STOR-04**: WAL recovery restores state after crashes without corruption
- [ ] **STOR-05**: SQLite metadata store with tenant isolation indexes
- [ ] **STOR-06**: Metadata queries filtered by tenant_id with no cross-tenant leakage
- [ ] **STOR-07**: Tombstone bitset tracks soft-deleted chunks per segment
- [ ] **STOR-08**: Soft deletes set status=deleted and tombstone bit
- [ ] **STOR-09**: Retrieval filters tombstoned chunks from all results

### Dense Vector Retrieval

- [ ] **DENSE-01**: Embeddings interface trait with embed_texts and embed_query methods
- [ ] **DENSE-02**: Mock embedder implementation for testing
- [ ] **DENSE-03**: ONNX embedder implementation with quantized models
- [ ] **DENSE-04**: HNSW warm tier index for main dense retrieval
- [ ] **DENSE-05**: HNSW insert operation adds new chunks to warm index
- [ ] **DENSE-06**: HNSW search returns topK candidates with similarity scores
- [ ] **DENSE-07**: Embedding storage with f16 or f32 precision
- [ ] **DENSE-08**: Optional int8 quantization for storage efficiency

### Sparse Lexical Retrieval

- [ ] **SPARSE-01**: BM25 lexical indexing using Tantivy or equivalent
- [ ] **SPARSE-02**: Tokenization splits natural language and code identifiers (camelCase, snake_case)
- [ ] **SPARSE-03**: File path and extension tokens included in index
- [ ] **SPARSE-04**: Term postings compressed with varint or roaring bitmaps
- [ ] **SPARSE-05**: Sparse index returns topK candidates with BM25 scores
- [ ] **SPARSE-06**: Delta segments for incremental updates merged periodically

### Hybrid Retrieval & Fusion

- [ ] **FUSION-01**: Parallel candidate generation from dense_hot, dense_warm, lexical sources
- [ ] **FUSION-02**: Reciprocal Rank Fusion (RRF) combines candidate lists
- [ ] **FUSION-03**: Bonuses applied for same project, recency, provenance confidence
- [ ] **FUSION-04**: Feature-based lightweight reranker (dense score, bm25, structural, recency, type match)
- [ ] **FUSION-05**: Context packer deduplicates near-duplicates by hash and similarity
- [ ] **FUSION-06**: Context packer enforces diversity via MMR across chunk types
- [ ] **FUSION-07**: Token budgeting with pluggable tokenizer (default chars/4 approximation)
- [ ] **FUSION-08**: Packed context includes text, source, citation metadata

### Hot Tier & Caching

- [ ] **HOT-01**: Hot cache LRU/LFU for recently accessed chunks
- [ ] **HOT-02**: Hot HNSW index for top 10k-200k active chunks
- [ ] **HOT-03**: Semantic cache maps query embeddings to packed context with confidence scores
- [ ] **HOT-04**: Cache entries store tenant_id, project_id, memory_version watermark
- [ ] **HOT-05**: Cache confidence increases on agent usage/repeated hits
- [ ] **HOT-06**: Cache confidence decays with time and memory_version changes
- [ ] **HOT-07**: Cache invalidation by memory_version delta threshold
- [ ] **HOT-08**: Promotion to hot on repeated retrieval or active project match
- [ ] **HOT-09**: Demotion from hot on N queries without access or semantic decay

### Structural Indexes

- [ ] **STRUCT-01**: Tree-sitter parser integration for multi-language AST extraction
- [ ] **STRUCT-02**: Symbol table extraction (functions, classes, definitions)
- [ ] **STRUCT-03**: Call graph extraction with caller -> callee edges
- [ ] **STRUCT-04**: Import graph extraction with file -> module dependencies
- [ ] **STRUCT-05**: find_symbol_definition(name) query support
- [ ] **STRUCT-06**: find_references(name) query support
- [ ] **STRUCT-07**: find_callers(name) query support
- [ ] **STRUCT-08**: find_imports(module) query support
- [ ] **STRUCT-09**: Trace indexing for tool calls (tool name, args, results, errors)
- [ ] **STRUCT-10**: Trace indexing for stack traces (frames, paths, signatures)
- [ ] **STRUCT-11**: find_tool_calls(tool_name, time_range) query support
- [ ] **STRUCT-12**: find_errors(error_signature) query support
- [ ] **STRUCT-13**: Query router classifies intent (code_search, debug_trace, doc_qa, decision_why, plan_next)
- [ ] **STRUCT-14**: Query router weights retrieval sources based on intent

### Compaction & Cleanup

- [ ] **COMPACT-01**: Tombstone filtering applied in all retrieval code paths
- [ ] **COMPACT-02**: Sparse segment merges triggered by fragmentation threshold
- [ ] **COMPACT-03**: Warm HNSW rebuild creates snapshot without deleted items
- [ ] **COMPACT-04**: Compaction job triggers on tombstone ratio > X% or segment fragmentation
- [ ] **COMPACT-05**: Compaction scheduling with throttling to limit query impact
- [ ] **COMPACT-06**: Results invariant: retrieval equivalent before/after compaction (minus deleted chunks)

### Evaluation & Quality

- [x] **EVAL-01**: Eval harness can start memd locally and run test suites
- [x] **EVAL-02**: Suite A (MCP conformance): tools/list, tools/call, error objects
- [x] **EVAL-03**: Suite A (schema validation): invalid args, missing tenant_id, large payloads
- [ ] **EVAL-04**: Suite A (isolation): ingest tenant A, query tenant B returns zero results
- [ ] **EVAL-05**: Suite A (recovery): crash mid-ingest, restart, WAL replay, no corruption
- [ ] **EVAL-06**: Suite A (soft delete): deleted chunks never returned in results
- [ ] **EVAL-07**: Suite B (retrieval quality): Recall@k, Precision@k, MRR, nDCG@k metrics
- [ ] **EVAL-08**: Suite B datasets: RepoBench-R, LongMemEval, MemoryAgentBench, MemBench
- [ ] **EVAL-09**: Suite B ablations: dense-only, lexical-only, dense+lexical, dense+lexical+struct
- [ ] **EVAL-10**: Suite C (performance): p50/p90/p99 latency per tier (hot/warm/cold)
- [ ] **EVAL-11**: Suite C (concurrency): QPS under concurrent load
- [ ] **EVAL-12**: Suite C (ingestion): batch ingestion latency benchmarks
- [ ] **EVAL-13**: Suite C (compaction): tail latency impact during compaction

### Observability

- [x] **OBS-01**: Structured JSON logging for all operations
- [ ] **OBS-02**: Metrics endpoint with Prometheus-style or JSON format
- [ ] **OBS-03**: Per-query latency breakdown (embed, dense_hot, dense_warm, lexical, ast/trace, fusion, rerank, pack, total)
- [ ] **OBS-04**: Debug flags return candidate source ranks and scores
- [ ] **OBS-05**: Debug flags return promotion/demotion reasoning

## v2 Requirements

Deferred to future releases. Tracked but not in current roadmap.

### Advanced Retrieval

- **ADV-01**: Quantized cross-encoder reranker (ONNX) with time budget fallback
- **ADV-02**: Cold-tier dense index with binary/PQ two-stage retrieval
- **ADV-03**: Learned query router (tiny trained model replacing heuristics)

### Architecture B Graph Memory

- **GRAPH-01**: Dynamic semantic graph module with node = chunks, edges = semantic/temporal/structural
- **GRAPH-02**: Graph traversal retrieval (seed from ANN, BFS with decay)
- **GRAPH-03**: Episodic clustering and condensation with summary nodes
- **GRAPH-04**: Hierarchical memory with episode drill-down support

### Advanced Caching

- **CACHE-01**: Multi-level cache hierarchy with L1/L2 semantic caches
- **CACHE-02**: Predictive pre-warming based on access patterns
- **CACHE-03**: Distributed cache coordination for multi-instance deployments

## Out of Scope

| Feature | Reason |
|---------|--------|
| GPU acceleration | CPU-only by design; keeps deployment simple and universally accessible |
| Cloud/distributed multi-node operation | Local-first priority; distributed later if needed |
| Real-time streaming ingestion | Batch ingestion sufficient for v1 agent use cases |
| Multi-user collaborative features | Single-user/single-agent focus initially |
| GUI/web interface | CLI and MCP tools sufficient; agents are primary consumers |
| Custom embedding model training | Use pre-trained ONNX models; training out of scope |
| End-to-end encryption at rest | Local deployment trust model; encryption is v2+ |

## Traceability

Which phases cover which requirements. Updated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| MCP-01 | Phase 1 | Complete |
| MCP-02 | Phase 1 | Complete |
| MCP-03 | Phase 1 | Complete |
| MCP-04 | Phase 1 | Complete |
| MCP-05 | Phase 1 | Complete |
| MCP-06 | Phase 1 | Complete |
| MCP-07 | Phase 1 | Complete |
| MCP-08 | Phase 1 | Complete |
| MCP-09 | Phase 1 | Complete |
| MCP-10 | Phase 1 | Complete |
| STOR-01 | Phase 2 | Complete |
| STOR-02 | Phase 2 | Complete |
| STOR-03 | Phase 2 | Complete |
| STOR-04 | Phase 2 | Complete |
| STOR-05 | Phase 2 | Complete |
| STOR-06 | Phase 2 | Complete |
| STOR-07 | Phase 2 | Complete |
| STOR-08 | Phase 2 | Complete |
| STOR-09 | Phase 2 | Complete |
| DENSE-01 | Phase 3 | Complete |
| DENSE-02 | Phase 3 | Complete |
| DENSE-03 | Phase 3 | Complete |
| DENSE-04 | Phase 3 | Complete |
| DENSE-05 | Phase 3 | Complete |
| DENSE-06 | Phase 3 | Complete |
| DENSE-07 | Phase 3 | Complete |
| DENSE-08 | Phase 3 | Complete |
| SPARSE-01 | Phase 4 | Pending |
| SPARSE-02 | Phase 4 | Pending |
| SPARSE-03 | Phase 4 | Pending |
| SPARSE-04 | Phase 4 | Pending |
| SPARSE-05 | Phase 4 | Pending |
| SPARSE-06 | Phase 4 | Pending |
| FUSION-01 | Phase 4 | Pending |
| FUSION-02 | Phase 4 | Pending |
| FUSION-03 | Phase 4 | Pending |
| FUSION-04 | Phase 4 | Pending |
| FUSION-05 | Phase 4 | Pending |
| FUSION-06 | Phase 4 | Pending |
| FUSION-07 | Phase 4 | Pending |
| FUSION-08 | Phase 4 | Pending |
| HOT-01 | Phase 5 | Pending |
| HOT-02 | Phase 5 | Pending |
| HOT-03 | Phase 5 | Pending |
| HOT-04 | Phase 5 | Pending |
| HOT-05 | Phase 5 | Pending |
| HOT-06 | Phase 5 | Pending |
| HOT-07 | Phase 5 | Pending |
| HOT-08 | Phase 5 | Pending |
| HOT-09 | Phase 5 | Pending |
| STRUCT-01 | Phase 6 | Pending |
| STRUCT-02 | Phase 6 | Pending |
| STRUCT-03 | Phase 6 | Pending |
| STRUCT-04 | Phase 6 | Pending |
| STRUCT-05 | Phase 6 | Pending |
| STRUCT-06 | Phase 6 | Pending |
| STRUCT-07 | Phase 6 | Pending |
| STRUCT-08 | Phase 6 | Pending |
| STRUCT-09 | Phase 6 | Pending |
| STRUCT-10 | Phase 6 | Pending |
| STRUCT-11 | Phase 6 | Pending |
| STRUCT-12 | Phase 6 | Pending |
| STRUCT-13 | Phase 6 | Pending |
| STRUCT-14 | Phase 6 | Pending |
| COMPACT-01 | Phase 7 | Pending |
| COMPACT-02 | Phase 7 | Pending |
| COMPACT-03 | Phase 7 | Pending |
| COMPACT-04 | Phase 7 | Pending |
| COMPACT-05 | Phase 7 | Pending |
| COMPACT-06 | Phase 7 | Pending |
| EVAL-01 | Phase 1 | Complete |
| EVAL-02 | Phase 1 | Complete |
| EVAL-03 | Phase 1 | Complete |
| EVAL-04 | Phase 2 | Complete |
| EVAL-05 | Phase 2 | Complete |
| EVAL-06 | Phase 2 | Complete |
| EVAL-07 | Phase 3 | Complete |
| EVAL-08 | Phase 3 | Complete |
| EVAL-09 | Phase 4 | Pending |
| EVAL-10 | Phase 4 | Pending |
| EVAL-11 | Phase 4 | Pending |
| EVAL-12 | Phase 4 | Pending |
| EVAL-13 | Phase 7 | Pending |
| OBS-01 | Phase 1 | Complete |
| OBS-02 | Phase 3 | Complete |
| OBS-03 | Phase 3 | Complete |
| OBS-04 | Phase 5 | Pending |
| OBS-05 | Phase 5 | Pending |

**Coverage:**
- v1 requirements: 88 total
- Mapped to phases: 88
- Unmapped: 0

---
*Requirements defined: 2026-01-29*
*Last updated: 2026-01-29 after roadmap creation*
