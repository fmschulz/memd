---
phase: 02
plan: 06
subsystem: storage
tags: [persistence, store-trait, wal-recovery, sqlite, segments]
dependency-graph:
  requires: ["02-01", "02-02", "02-03", "02-04", "02-05"]
  provides: ["PersistentStore", "crash-recovery", "tenant-storage"]
  affects: ["03-*"]
tech-stack:
  added: []
  patterns: ["trait-object-dispatch", "wal-ahead-logging", "crash-recovery"]
key-files:
  created:
    - crates/memd/src/store/persistent.rs
  modified:
    - crates/memd/src/store/mod.rs
    - crates/memd/src/store/segment/writer.rs
    - crates/memd/src/store/metadata/sqlite.rs
    - crates/memd/src/lib.rs
    - crates/memd/src/main.rs
decisions:
  - id: "02-06-01"
    description: "INSERT OR REPLACE for crash recovery idempotency"
    rationale: "Handles orphan metadata when segment data lost on crash"
  - id: "02-06-02"
    description: "SegmentWriter::read_chunk flushes buffer before reading"
    rationale: "Ensures buffered data is on disk for active segment reads"
  - id: "02-06-03"
    description: "Recovery checks segment readability, not just metadata existence"
    rationale: "Detects crash scenarios where metadata committed but segment lost"
metrics:
  duration: "7m"
  completed: "2026-01-30"
---

# Phase 2 Plan 6: Persistent Store Integration Summary

**One-liner:** PersistentStore implementing Store trait with full WAL recovery, segment management, and daemon integration.

## What Was Built

### PersistentStore (crates/memd/src/store/persistent.rs)
- Implements `Store` trait with all operations (add, get, search, delete, stats)
- Per-tenant isolation with separate WAL and segments per tenant
- Full WAL recovery that replays Add/Delete records on startup
- Segment rotation when max chunks reached
- Graceful shutdown with segment finalization via Drop

### Daemon Integration (crates/memd/src/main.rs)
- Added `--data-dir` CLI argument for custom storage location
- Added `--in-memory` flag for testing with MemoryStore
- Default to PersistentStore for production use
- Both MCP and CLI modes support both store types

### Crash Recovery Features
- WAL written and synced before segment writes
- Recovery detects orphan metadata (committed but segment lost)
- INSERT OR REPLACE in SQLite for idempotent recovery
- WAL truncated after successful recovery

## Key Implementation Details

### Write Path (add)
1. Generate ChunkId (UUIDv7) and compute SHA-256 hash
2. Write to WAL (synced immediately)
3. Write to active segment (buffered)
4. Insert metadata to SQLite
5. Check for WAL checkpoint interval

### Read Path (get)
1. Query metadata for segment_id and ordinal
2. Check active segment first (may not be finalized)
3. Fall back to finalized segment readers
4. CRC-32 verification on read

### Recovery Path (open)
1. Open global metadata database
2. Discover tenant directories
3. Load finalized segments
4. Replay WAL records after last checkpoint
5. Truncate WAL after successful recovery

## Commits

| Commit | Type | Description |
|--------|------|-------------|
| 4339089 | feat | Implement PersistentStore with full WAL recovery |
| c8dcd76 | feat | Wire PersistentStore into daemon |
| cd0b77a | test | Add persistence unit tests with crash recovery |

## Tests Added

- `add_and_get` - Basic store operation
- `tenant_isolation` - Multi-tenant security
- `soft_delete` - Delete and verify not retrievable
- `persistence_across_restarts` - Data survives daemon restart
- `wal_recovery_after_crash` - Recovery after simulated crash
- `stats` - Statistics counting

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] SegmentWriter::read_chunk needed flush**
- **Found during:** Task 3 test execution
- **Issue:** BufWriter not flushed, reads returned stale data
- **Fix:** Made read_chunk mutable, added flush() before read
- **Commit:** cd0b77a

**2. [Rule 1 - Bug] Crash recovery skipped orphan metadata**
- **Found during:** Task 3 test execution
- **Issue:** Metadata committed but segment buffer lost on crash
- **Fix:** Recovery checks segment readability, INSERT OR REPLACE for idempotency
- **Commit:** cd0b77a

## Files Changed

| File | Lines | Change Type |
|------|-------|-------------|
| crates/memd/src/store/persistent.rs | +846 | Created |
| crates/memd/src/main.rs | +68 | Modified |
| crates/memd/src/store/mod.rs | +2 | Modified |
| crates/memd/src/store/segment/writer.rs | +49 | Modified |
| crates/memd/src/store/metadata/sqlite.rs | +2 | Modified |
| crates/memd/src/lib.rs | +1 | Modified |

## Verification Results

- `cargo test -p memd persistent` - 6/6 tests pass
- `cargo build -p memd` - Build succeeds
- Manual CLI test with `--data-dir` - Data persists across runs
- Directory structure verified: metadata.db, segments, WAL

## Next Phase Readiness

Phase 2 storage layer is complete:
- SegmentWriter/Reader for payload storage
- WAL for durability
- SQLite for metadata
- Tombstones for soft deletes
- PersistentStore integrating all components

Ready for Phase 3 (Vector Embeddings) which will add:
- Embedding generation for chunks
- Vector index (HNSW) for similarity search
- Integration with Store::search method
