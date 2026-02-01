---
phase: 06-structural-indexes
plan: 05
subsystem: structural
tags: [sqlite, traces, stack-traces, tool-calls, regex, parsing]

# Dependency graph
requires:
  - phase: 06-structural-indexes
    provides: structural storage module with SQLite
provides:
  - SQLite schema for tool traces and stack traces
  - Multi-format stack trace parsing (Rust, Python, JavaScript)
  - Trace capture utilities with timestamp and context
  - Error signature normalization for grouping
affects: [mcp-handlers, debugging-tools, observability]

# Tech tracking
tech-stack:
  added: [regex]
  patterns: [dynamic SQL with parameter counting, trace auto-detection]

key-files:
  created:
    - crates/memd/src/structural/traces.rs
  modified:
    - crates/memd/src/structural/storage.rs
    - crates/memd/src/structural/mod.rs

key-decisions:
  - "Store tool call input/output as JSON strings for flexibility"
  - "Use dynamic SQL with parameter counting for optional filters"
  - "Auto-detect trace format based on content patterns"
  - "Normalize error signatures by removing addresses, timestamps, UUIDs"

patterns-established:
  - "TimeRange struct for filtering time-based queries"
  - "ParsedFrame struct for language-agnostic stack frame representation"
  - "TraceIndexer trait for pluggable trace storage backends"

# Metrics
duration: 18min
completed: 2026-01-31
---

# Phase 6 Plan 5: Trace Storage Summary

**SQLite schema for tool and stack traces with multi-format parsing supporting Rust, Python, and JavaScript stack traces**

## Performance

- **Duration:** 18 min
- **Started:** 2026-01-31T18:30:00Z
- **Completed:** 2026-01-31T18:48:00Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments
- Extended SQLite schema with tool_traces, stack_traces, and stack_frames tables
- Implemented stack trace parsing for Rust panic backtraces, Python tracebacks, and JavaScript stack traces
- Added error signature normalization for grouping similar errors
- Created trace capture utilities with timestamp and context support

## Task Commits

Each task was committed atomically:

1. **Task 1: Extend SQLite schema** - `0347dd4` (feat)
2. **Task 2: Add trace parsing utilities** - `97ce327` (feat)

## Files Created/Modified
- `crates/memd/src/structural/storage.rs` - Added tool_traces, stack_traces, stack_frames tables with CRUD operations
- `crates/memd/src/structural/traces.rs` - Stack trace parsing for multiple formats, TraceCapture utility
- `crates/memd/src/structural/mod.rs` - Export new trace types

## Decisions Made
- Used dynamic SQL with parameter counting for optional filters in find_tool_traces and find_stack_traces to avoid passing unused parameters
- Normalized error signatures by removing memory addresses (0x...), timestamps, and UUIDs for better error grouping
- Auto-detect trace format by looking for characteristic patterns (e.g., "Traceback" for Python, "at " lines for JavaScript)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- Fixed parameter count mismatch in dynamic SQL queries by building parameter list dynamically based on which filters are actually provided
- Fixed Rust backtrace regex to properly parse file:line:col format using non-greedy matching

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Trace storage and parsing complete
- Ready for MCP handler integration to capture tool calls
- Ready for error handling integration to capture stack traces

---
*Phase: 06-structural-indexes*
*Completed: 2026-01-31*
