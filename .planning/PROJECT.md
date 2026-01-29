# memd - Agent Memory Service

## What This Is

A local daemon service (memd) that provides intelligent memory management for AI coding agents like Claude Code and Codex CLI. It handles hybrid retrieval (dense vector + sparse lexical + structural code indexes) with hot/warm/cold tiering, multi-tenant isolation, and operates entirely offline on CPU. Agents access it via Model Context Protocol (MCP) tools for ingesting code/docs/traces and retrieving relevant context across sessions and projects.

## Core Value

Agents can find and use relevant past context—across sessions, projects, and time—without hitting context window limits or losing continuity.

## Requirements

### Validated

(None yet — ship to validate)

### Active

#### Milestone 1 — Skeleton + MCP Server
- [ ] MCP server with stub tools (search, add, get, delete, stats)
- [ ] Config loader (TOML)
- [ ] Tenant directory initialization
- [ ] Simple in-memory store
- [ ] Pass evals A1, A2 (MCP conformance basic)

#### Milestone 2 — Persistent Cold Store
- [ ] Append-only segments with mmap reads
- [ ] WAL with crash recovery
- [ ] Metadata queries with tenant isolation (SQLite)
- [ ] Tombstone-based soft deletes
- [ ] Pass evals A3, A4, A5 (isolation, recovery, deletes)

#### Milestone 3 — Dense Warm Index + Retrieval
- [ ] Embeddings interface (mock → ONNX)
- [ ] HNSW warm tier insert/search
- [ ] memory.search returns topK with scores
- [ ] Pass Suite B with synthetic dataset

#### Milestone 4 — Sparse Lexical Index + Fusion
- [ ] BM25 lexical indexing (Tantivy or equivalent)
- [ ] RRF fusion (dense + lexical)
- [ ] Feature-based lightweight reranker
- [ ] Suite B metrics improve vs dense-only
- [ ] Capture C2 perf baseline

#### Milestone 5 — Hot Tier + Cache + Promotion
- [ ] Hot cache with hot HNSW
- [ ] Semantic cache with confidence scoring
- [ ] Cache invalidation by memory version
- [ ] Promotion/demotion logic
- [ ] Pass C1 hot latency + cache hit tests

#### Milestone 6 — Structural Indexes
- [ ] Tree-sitter AST parsing pipeline
- [ ] Symbol table + call graph extraction
- [ ] Symbol/reference/caller lookup tools
- [ ] Trace indexing (tool calls, stack traces)
- [ ] Query router with intent heuristics (code_search, debug_trace, doc_qa, etc.)
- [ ] Suite B includes structural queries

#### Milestone 7 — Compaction + Cleanup
- [ ] Tombstone filtering in all retrieval paths
- [ ] Sparse segment merges
- [ ] Warm HNSW rebuild snapshots
- [ ] Compaction scheduling with throttling
- [ ] Pass C5 compaction impact benchmark
- [ ] Invariant tests: results equivalent before/after compaction (minus deleted)

### Out of Scope

- **GPU acceleration** — CPU-only by design; keeps deployment simple and local
- **Cloud/distributed operation** — Local-first; multi-machine later if needed
- **Learned query router** — Heuristics first; tiny model is v2+
- **Full Architecture B graph module** — Ship Architecture A baseline first; graph memory as pluggable module later
- **Quantized cross-encoder reranker** — Nice-to-have; feature ranker sufficient for v1
- **Cold-tier dense index with PQ/binary** — Defer to v2; warm + sparse covers cold for now
- **Episodic clustering/condensation** — Beyond simple episode summaries; later enhancement

## Context

### Pain Points Being Solved

Current agent workflows suffer from four critical memory failures:

1. **Context window fills up fast** — Relevant past work gets evicted mid-conversation
2. **No cross-project learning** — Patterns from one codebase don't transfer to another
3. **Can't find relevant past work** — "I know we solved this before" but agent can't retrieve it
4. **Lose context between sessions** — Every conversation starts cold; yesterday's work is invisible

### Technical Environment

- **Target agents:** Claude Code, Codex CLI, and other MCP-capable agents
- **Integration protocol:** Model Context Protocol (MCP) - standard tool/context integration
- **Deployment:** Local daemon (stdio transport), single binary
- **Resources:** CPU-only (no GPU dependency), works offline

### Use Case

Personal dogfooding first to validate and refine. Solo development. Quality-focused with no external timeline pressure. Open source potential after validation.

### Reference Implementation Plan

Comprehensive technical specification in `docs/implementation_v0.md` covering:
- Data model (MemoryChunk, segment format, metadata schema)
- Indexing strategies (HNSW, BM25, AST, trace)
- Retrieval pipeline (candidate generation, fusion, reranking, packing)
- Tiering policies (hot/warm/cold promotion/demotion)
- MCP tool interface design
- Eval harness structure (correctness, retrieval quality, performance, e2e)
- Build milestones with acceptance criteria

## Constraints

- **Stack: Rust** — Needed for mmap control, low-level memory layout, high concurrency without GC pauses, easy single-binary packaging
- **Protocol: MCP** — Model Context Protocol for agent tool integration; explicitly supported by Codex and Claude ecosystem
- **CPU-only** — No GPU dependency; all embeddings and retrieval CPU-based (ONNX quantized models)
- **Local-first** — Entire system runs offline; no external service dependencies
- **Multi-tenant isolation** — Strict tenant_id partitioning; no cross-tenant data leakage
- **Quality gates: Codex CLI review** — Use Codex CLI code review after every implementation plan and milestone completion to check and refine code quality

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Architecture A baseline first, Architecture B as module | Ship production-ready hybrid retrieval before adding graph memory complexity; keeps scope contained while enabling "bold" path later | — Pending |
| MCP over custom protocol | MCP is standard for agent tool integration; supported by Codex, Claude, and growing ecosystem; future-proof | — Pending |
| Rust over Python/TypeScript | Need mmap, memory control, and high concurrency for tiering; Rust cultural alignment with Codex CLI | — Pending |
| Milestones 1-7 = "done" for v1 | Full Architecture A with all retrieval modes, tiering, evals, and correctness guarantees before exploring advanced features | — Pending |
| Codex CLI as review checkpoint | Leverage Codex for code quality review at implementation plan and milestone boundaries; catch issues early | — Pending |

---
*Last updated: 2026-01-29 after initialization*
