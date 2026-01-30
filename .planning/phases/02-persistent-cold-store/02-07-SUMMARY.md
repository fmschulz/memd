---
phase: 02-persistent-cold-store
plan: 07
subsystem: testing
tags: [eval-harness, persistence-tests, tenant-isolation, crash-recovery, soft-delete]

# Dependency graph
requires:
  - phase: 02-persistent-cold-store (02-06)
    provides: PersistentStore integration with --data-dir flag
provides:
  - Persistence eval tests (A3, A4, A5)
  - McpClient::start_with_args for custom arguments
  - 16 total eval tests covering MCP + persistence
affects: [03-vector-search, future-eval-suites]

# Tech tracking
tech-stack:
  added: [tempfile (for eval harness)]
  patterns: [extract_content_text helper for MCP response parsing]

key-files:
  created:
    - evals/harness/src/suites/persistence.rs
  modified:
    - evals/harness/src/main.rs
    - evals/harness/src/mcp_client.rs
    - evals/harness/src/suites/mod.rs
    - evals/harness/Cargo.toml

key-decisions:
  - "extract_content_text helper for consistent MCP response parsing"
  - "McpClient::start_with_args takes PathBuf reference for flexibility"

patterns-established:
  - "Persistence test pattern: TempDir for isolated data directories"
  - "Crash recovery test: two sessions sharing same data_path"

# Metrics
duration: 6min
completed: 2026-01-30
---

# Phase 02 Plan 07: Persistence Eval Tests Summary

**Eval harness extended with tenant isolation (A3), crash recovery (A4), and soft delete (A5) persistence tests - 16/16 tests passing**

## Performance

- **Duration:** 6 min
- **Started:** 2026-01-30T05:52:11Z
- **Completed:** 2026-01-30T05:58:00Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments
- A3_tenant_isolation: Verifies tenant B cannot see tenant A's data
- A4_crash_recovery: Verifies data survives daemon restart via WAL replay
- A5_soft_delete: Verifies deleted chunks never returned, stats show deleted count
- McpClient::start_with_args enables custom command-line arguments
- All 16 eval tests passing (13 MCP conformance + 3 persistence)

## Task Commits

Each task was committed atomically:

1. **Task 1: Create persistence test suite** - `e64c1c5` (feat)
2. **Task 2: Integrate persistence suite into harness** - `851c508` (feat)

## Files Created/Modified
- `evals/harness/src/suites/persistence.rs` - Persistence test suite with A3/A4/A5 tests
- `evals/harness/src/suites/mod.rs` - Added persistence module export
- `evals/harness/src/main.rs` - Added persistence suite to --suite options
- `evals/harness/src/mcp_client.rs` - Added start_with_args method
- `evals/harness/Cargo.toml` - Added tempfile dependency

## Decisions Made
- **extract_content_text helper:** Created helper function to consistently extract text from MCP response structure (result.content[0].text), reducing code duplication
- **PathBuf for start_with_args:** Takes &PathBuf rather than &str for type safety and consistency with persistence tests

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed MCP response path**
- **Found during:** Task 1 (persistence test implementation)
- **Issue:** Plan code used `r["content"][0]["text"]` but correct path is `r["result"]["content"][0]["text"]`
- **Fix:** Added extract_content_text and extract_chunk_id helper functions with correct path
- **Files modified:** evals/harness/src/suites/persistence.rs
- **Verification:** All 3 persistence tests pass
- **Committed in:** 851c508 (Task 2 commit)

**2. [Rule 1 - Bug] Fixed TestResult::pass signature**
- **Found during:** Task 1 (persistence test implementation)
- **Issue:** Plan code called `TestResult::pass(name, message)` but actual signature is `TestResult::pass(name)`
- **Fix:** Removed second argument from all pass calls
- **Files modified:** evals/harness/src/suites/persistence.rs
- **Verification:** Code compiles, tests pass
- **Committed in:** e64c1c5 (Task 1 commit)

**3. [Rule 1 - Bug] Fixed memory.add type parameter**
- **Found during:** Task 1 (persistence test implementation)
- **Issue:** Plan code used `"chunk_type": "doc"` but MCP conformance tests use `"type": "doc"`
- **Fix:** Changed all occurrences to `"type": "doc"`
- **Files modified:** evals/harness/src/suites/persistence.rs
- **Verification:** memory.add calls succeed, chunk_id returned
- **Committed in:** 851c508 (Task 2 commit)

---

**Total deviations:** 3 auto-fixed (all Rule 1 bugs)
**Impact on plan:** Bug fixes necessary for correctness. No scope creep.

## Issues Encountered
None - plan execution was straightforward after fixing API mismatches.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Phase 2 complete: all 7 plans executed successfully
- Persistence layer fully operational with:
  - Segment files with mmap reads
  - WAL for durability
  - SQLite metadata store
  - Roaring bitmap tombstones
  - Full crash recovery
- Ready for Phase 3 (Vector Search) - can add embedding storage to segments

---
*Phase: 02-persistent-cold-store*
*Completed: 2026-01-30*
