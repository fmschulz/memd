---
phase: 02-persistent-cold-store
plan: 04
subsystem: database
tags: [roaring, bitmap, tombstone, soft-delete, atomic-write]

# Dependency graph
requires:
  - phase: 02-01
    provides: Segment format and writer for storing chunks
provides:
  - TombstoneSet with roaring bitmap for space-efficient deletion tracking
  - Atomic persistence via temp file + rename + fsync pattern
  - O(1) is_deleted membership query
  - deleted_count for compaction decisions
affects: [02-05, compaction, segment-reader]

# Tech tracking
tech-stack:
  added: [roaring]
  patterns: [atomic-file-write, temp-rename-fsync]

key-files:
  created:
    - crates/memd/src/store/tombstone.rs
    - crates/memd/src/store/metadata/sqlite.rs (stub)
    - crates/memd/src/store/wal/writer.rs (stub)
  modified:
    - crates/memd/src/store/mod.rs
    - crates/memd/src/error.rs
    - Cargo.toml
    - Cargo.lock

key-decisions:
  - "Roaring bitmap for space-efficient sparse bitset storage"
  - "Atomic persistence: temp file + rename + fsync for durability"
  - "Dirty flag to avoid unnecessary disk writes"

patterns-established:
  - "atomic-file-write: temp file + write + sync_all + rename + dir sync"

# Metrics
duration: 2min
completed: 2026-01-30
---

# Phase 02 Plan 04: Tombstone Bitset Summary

**Roaring bitmap tombstone set with atomic persistence for soft-delete tracking**

## Performance

- **Duration:** 2 min
- **Started:** 2026-01-30T01:50:01Z
- **Completed:** 2026-01-30T01:52:17Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments

- TombstoneSet struct using roaring::RoaringBitmap for space-efficient deletion tracking
- Atomic persistence via temp file + rename + fsync pattern for durability
- O(1) is_deleted membership query for fast lookup
- Comprehensive test suite (6 tests: empty, mark/check, persist/reload, idempotent, iteration, clean-persist)

## Task Commits

1. **Task 1+2: Implement tombstone bitset with tests** - `2e4620f` (feat)

**Note:** Both tasks committed together as tests were in same file. Also included sqlite.rs and wal/writer.rs stubs to unblock compilation.

## Files Created/Modified

- `crates/memd/src/store/tombstone.rs` - TombstoneSet with roaring bitmap, atomic persistence, 6 unit tests
- `crates/memd/src/store/mod.rs` - Added tombstone module export
- `crates/memd/src/store/metadata/mod.rs` - MetadataStore trait (from prior plan)
- `crates/memd/src/store/metadata/sqlite.rs` - Stub for compilation
- `crates/memd/src/store/wal/format.rs` - WAL record format (from prior plan)
- `crates/memd/src/store/wal/mod.rs` - WAL module (from prior plan)
- `crates/memd/src/store/wal/writer.rs` - Stub for compilation
- `crates/memd/src/error.rs` - Added DatabaseError variant for rusqlite
- `Cargo.toml` - Added serde feature to bincode
- `Cargo.lock` - Updated dependencies

## Decisions Made

- **Roaring bitmap:** Using roaring::RoaringBitmap for space-efficient storage of sparse deletion sets
- **Atomic persistence:** temp file + rename + fsync pattern ensures durability and crash safety
- **Dirty flag:** Tracks whether changes need persisting, avoiding unnecessary disk I/O

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Created sqlite.rs stub**
- **Found during:** Task 1 (cargo check)
- **Issue:** metadata/mod.rs references `pub mod sqlite;` but file missing
- **Fix:** Created minimal stub with unimplemented! methods
- **Files modified:** crates/memd/src/store/metadata/sqlite.rs
- **Verification:** cargo check passes
- **Committed in:** 2e4620f

**2. [Rule 3 - Blocking] Included wal/writer.rs stub**
- **Found during:** Task 1 (cargo test)
- **Issue:** wal/mod.rs references `pub mod writer;` but file wasn't staged
- **Fix:** Staged existing stub file
- **Files modified:** crates/memd/src/store/wal/writer.rs
- **Verification:** cargo test passes
- **Committed in:** 2e4620f (amended)

**3. [Rule 3 - Blocking] Included uncommitted work from prior plans**
- **Found during:** Task 1 (git status)
- **Issue:** metadata/, wal/, error.rs, Cargo.toml changes from prior plans were uncommitted
- **Fix:** Included all store-related changes in commit to maintain compilable state
- **Files modified:** All listed above
- **Verification:** cargo test -p memd tombstone passes
- **Committed in:** 2e4620f

---

**Total deviations:** 3 auto-fixed (3 blocking)
**Impact on plan:** All auto-fixes necessary for compilation. Prior uncommitted work was bundled to maintain working codebase state.

## Issues Encountered

- Error type mismatch: Plan specified `MemdError::Storage` but actual type is `MemdError::StorageError` - corrected in implementation

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- TombstoneSet ready for integration with segment reader
- Atomic persistence pattern established for reuse
- Stubs created for sqlite and wal modules - full implementation in 02-03 and 02-02

---
*Phase: 02-persistent-cold-store*
*Plan: 04*
*Completed: 2026-01-30*
