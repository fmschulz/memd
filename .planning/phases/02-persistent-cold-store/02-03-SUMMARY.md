---
phase: 02-persistent-cold-store
plan: 03
subsystem: storage
tags: [sqlite, metadata, tenant-isolation, indexes]
completed: 2026-01-30
duration: 4m
requires: [02-01]
provides: [metadata-store-trait, sqlite-metadata-impl]
affects: [02-02, 03-01]
tech-stack:
  added: []
  patterns: [wal-mode, tenant-isolation-indexes]
key-files:
  created: []
  modified: [crates/memd/src/store/metadata/sqlite.rs]
decisions:
  - id: sqlite-wal-mode
    choice: "WAL mode with synchronous=NORMAL"
    rationale: "Crash safety with good write performance"
  - id: busy-timeout
    choice: "5000ms busy_timeout"
    rationale: "Prevents SQLITE_BUSY under concurrent load"
  - id: tenant-first-queries
    choice: "All queries filter tenant_id first"
    rationale: "Enforces isolation at query level, uses composite indexes"
---

# Phase 02 Plan 03: SQLite Metadata Store Summary

**One-liner:** SQLite metadata store with WAL mode, 5s busy timeout, and tenant isolation via composite indexes.

## What Was Built

Implemented the full SQLite-backed metadata store that handles chunk metadata queries with strict tenant isolation.

### Key Components

1. **SqliteMetadataStore struct**
   - Single writer protected by Mutex<Connection>
   - WAL mode for crash safety + concurrent readers
   - Configurable pragmas: synchronous=NORMAL, busy_timeout=5000, cache_size=-64000

2. **Schema Design**
   ```sql
   CREATE TABLE chunks (
       chunk_id TEXT PRIMARY KEY,
       tenant_id TEXT NOT NULL,
       project_id TEXT,
       segment_id INTEGER NOT NULL,
       ordinal INTEGER NOT NULL,
       chunk_type TEXT NOT NULL,
       status TEXT NOT NULL DEFAULT 'final',
       timestamp_created INTEGER NOT NULL,
       hash TEXT NOT NULL,
       source_uri TEXT,
       UNIQUE(segment_id, ordinal)
   )
   ```

3. **Indexes (Critical for Tenant Isolation)**
   - `idx_chunks_tenant(tenant_id, status)` - Primary isolation index
   - `idx_chunks_tenant_type(tenant_id, chunk_type, timestamp_created DESC)` - Type queries
   - `idx_chunks_segment(segment_id)` - Tombstone sync queries

4. **MetadataStore Trait Implementation**
   - `insert()` - Insert new chunk metadata
   - `get()` - Get by tenant_id + chunk_id (filtered by status != 'deleted')
   - `list()` - Paginated list with DESC timestamp ordering
   - `mark_deleted()` - Soft delete (sets status = 'deleted')
   - `get_by_segment()` - Get all chunks in a segment (for tombstone sync)
   - `count_by_status()` - Count (active, deleted) for stats

## Tests Added

| Test | Purpose |
|------|---------|
| `insert_and_get` | Basic CRUD operations |
| `tenant_isolation` | Verify tenant_b cannot see tenant_a's data |
| `list_pagination` | Verify LIMIT/OFFSET and DESC ordering |
| `soft_delete` | Verify mark_deleted hides from get/list |
| `count_by_status` | Verify accurate counts before/after delete |
| `get_by_segment` | Verify segment queries return ordered by ordinal |
| `wal_mode_enabled` | Verify PRAGMA journal_mode=WAL is set |

## Verification Results

All success criteria met:
- [x] SQLite opens in WAL mode (verified via PRAGMA query in test)
- [x] busy_timeout=5000 set to prevent SQLITE_BUSY
- [x] chunks table with tenant_id composite indexes
- [x] All queries include tenant_id in WHERE clause
- [x] Tenant isolation explicitly tested (tenant_isolation test)
- [x] Soft delete updates status field
- [x] All 7 tests pass

## Deviations from Plan

None - plan executed exactly as written.

## Decisions Made

1. **WAL mode with synchronous=NORMAL**: Provides crash safety while maintaining good write performance. The combination of WAL + NORMAL is the standard recommendation for SQLite in production.

2. **5 second busy timeout**: Prevents SQLITE_BUSY errors under concurrent load while not blocking indefinitely on deadlocks.

3. **Tenant-first queries**: Every query method takes `tenant_id` as the first parameter and includes it first in the WHERE clause. This both enforces API-level isolation and enables the composite indexes to be used efficiently.

4. **cache_size=-64000**: 64MB cache for better read performance on repeated queries.

## Next Steps

This plan provides the metadata layer needed by:
- **02-02 (WAL)**: WAL recovery will use metadata store to check for existing chunks during replay
- **03-01 (Search)**: Search will query metadata to find candidates, then load payloads from segments

## Files Changed

| File | Change |
|------|--------|
| `crates/memd/src/store/metadata/sqlite.rs` | Full implementation replacing stub |

## Commit

- `ad0e2c4`: feat(02-03): implement SQLite metadata store with tenant isolation
