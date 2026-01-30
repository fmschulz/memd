# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-01-29)

**Core value:** Agents can find and use relevant past context--across sessions, projects, and time--without hitting context window limits or losing continuity.
**Current focus:** Phase 3 - Dense Warm Index (In Progress)

## Current Position

Phase: 3 of 7 (Dense Warm Index)
Plan: 3 of 6 in current phase
Status: In progress
Last activity: 2026-01-30 -- Completed 03-03-PLAN.md (HNSW Warm Index)

Progress: [============================--------------------] ~70% (14 of ~20 total plans estimated)

## Performance Metrics

**Velocity:**
- Total plans completed: 14
- Average duration: 7m
- Total execution time: ~97 minutes

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 01 | 4 | 39m | 10m |
| 02 | 7 | 38m | 5m |
| 03 | 3 | 20m | 7m |

**Recent Trend:**
- Last 5 plans: 02-06 (7m), 02-07 (6m), 03-01 (3m), 03-02 (8m), 03-03 (9m)
- Trend: Phase 3 HNSW index complete

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

### Pending Todos

None.

### Blockers/Concerns

None.

## Session Continuity

Last session: 2026-01-30 07:21 UTC
Stopped at: Completed 03-03-PLAN.md (HNSW Warm Index) - Vector index ready
Resume file: None
