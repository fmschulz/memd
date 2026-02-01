---
phase: 06-structural-indexes
plan: 06
subsystem: debugging
tags: [traces, tool-calls, stack-traces, mcp, query-service]

# Dependency graph
requires:
  - phase: 06-structural-indexes/02
    provides: Trace storage with ToolTraceRecord and StackTraceRecord in StructuralStore
provides:
  - TraceQueryService with find_tool_calls and find_errors methods
  - debug.find_tool_calls MCP tool for querying tool invocation history
  - debug.find_errors MCP tool for querying stack trace data
  - Integration tests for trace query handlers
affects: [07-mcp-integration, future debugging features]

# Tech tracking
tech-stack:
  added: []
  patterns: [trace-query-service, time-range-filtering, iso8601-parsing]

key-files:
  created:
    - crates/memd/tests/trace_tools.rs
  modified:
    - crates/memd/src/structural/queries.rs
    - crates/memd/src/structural/mod.rs
    - crates/memd/src/mcp/tools.rs
    - crates/memd/src/mcp/handlers.rs
    - crates/memd/src/mcp/server.rs

key-decisions:
  - "Use alias StructuralTimeRange to avoid collision with existing TimeRange in handlers"
  - "Parse ISO 8601 timestamps manually without chrono dependency"
  - "Keep include_frames optional for debug.find_errors to reduce payload size"

patterns-established:
  - "TraceQueryService pattern: Wrap StructuralStore for high-level trace queries"
  - "Time range parsing: Use parse_trace_time_range for ISO 8601 to TimeRange conversion"

# Metrics
duration: 12min
completed: 2026-02-01
---

# Phase 6 Plan 6: Debug Trace Tools Summary

**TraceQueryService with find_tool_calls/find_errors methods and MCP tools for debugging tool invocations and stack traces**

## Performance

- **Duration:** 12 min
- **Started:** 2026-02-01T02:56:55Z
- **Completed:** 2026-02-01T03:08:58Z
- **Tasks:** 4
- **Files modified:** 5

## Accomplishments
- TraceQueryService with comprehensive query methods for tool calls and errors
- Two new MCP tools: debug.find_tool_calls and debug.find_errors
- Full handler chain from MCP request to SQLite query
- 13 integration tests covering all query paths

## Task Commits

Each task was committed atomically:

1. **Task 1: Add trace query methods** - `1605c11` (feat)
2. **Task 2: Add MCP tools for trace queries** - `bcc31e3` (feat)
3. **Task 3: Wire handlers to server** - `5ed8d80` (feat)
4. **Task 4: Add integration test suite** - `bd39888` (test)

## Files Created/Modified

- `crates/memd/src/structural/queries.rs` - Extended with TraceQueryService, ToolCallResult, ErrorResult types
- `crates/memd/src/structural/mod.rs` - Added exports for trace query types
- `crates/memd/src/mcp/tools.rs` - Added debug.find_tool_calls and debug.find_errors tool definitions
- `crates/memd/src/mcp/handlers.rs` - Added handle_find_tool_calls and handle_find_errors handlers
- `crates/memd/src/mcp/server.rs` - Wired trace handlers with routing
- `crates/memd/tests/trace_tools.rs` - New integration test file with 13 tests

## Decisions Made

- **Manual ISO 8601 parsing:** Avoided adding chrono dependency by implementing RFC 3339 parsing manually. This keeps dependencies minimal while supporting standard datetime formats.
- **TimeRange alias:** Used `StructuralTimeRange` alias to avoid collision with existing `TimeRange` in handlers module.
- **Optional frames:** Made `include_frames` a toggle in debug.find_errors to allow lightweight queries when full stack traces aren't needed.

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None - all tasks completed as specified.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- Trace query tools are ready for use via MCP
- TraceQueryService can be extended with additional query patterns
- Test coverage provides confidence for future enhancements
- Integration with tiered search remains for future phases

---
*Phase: 06-structural-indexes*
*Completed: 2026-02-01*
