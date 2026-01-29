---
phase: 01-skeleton-+-mcp-server
plan: 04
subsystem: tooling
tags: [rust, cli, evals, mcp, testing, harness]
dependency-graph:
  requires:
    - phase: 01-03
      provides: In-memory store, working MCP server with 6 tools
  provides:
    - CLI mode for direct tool invocation
    - Eval harness for MCP conformance testing
    - Suite A conformance tests (13 tests)
  affects: [02-*, 03-*]
tech-stack:
  added: []
  patterns: [dual-mode-binary, subprocess-testing, json-rpc-client]
key-files:
  created:
    - crates/memd/src/cli.rs
    - evals/harness/Cargo.toml
    - evals/harness/src/lib.rs
    - evals/harness/src/main.rs
    - evals/harness/src/mcp_client.rs
    - evals/harness/src/suites/mod.rs
    - evals/harness/src/suites/mcp_conformance.rs
  modified:
    - Cargo.toml
    - crates/memd/src/lib.rs
    - crates/memd/src/main.rs
key-decisions:
  - id: D01-04-01
    decision: "CLI mode uses pretty logging, MCP mode uses JSON logging"
    rationale: "Human-readable output for debugging, structured for agent consumption"
  - id: D01-04-02
    decision: "Eval harness builds memd before running tests"
    rationale: "Ensures binary is up-to-date, can skip with --skip-build flag"
  - id: D01-04-03
    decision: "Each eval test starts a fresh memd subprocess"
    rationale: "Complete isolation between tests, no shared state"
patterns-established:
  - "Dual-mode binary: --mode mcp (default) or --mode cli"
  - "CLI outputs JSON for scripting compatibility"
  - "Subprocess-based eval testing via stdio"
metrics:
  duration: 15m
  completed: 2026-01-29
---

# Phase 01 Plan 04: CLI Mode + Eval Harness Summary

**Dual-mode binary (MCP server + CLI) with eval harness passing 13 MCP conformance tests**

## Performance

- **Duration:** 15 min
- **Started:** 2026-01-29T22:30:00Z
- **Completed:** 2026-01-29T22:45:00Z
- **Tasks:** 3
- **Files modified:** 9

## Accomplishments

- CLI mode for direct memory operations without MCP overhead
- Subcommands: add, search, get, delete, stats
- JSON output format for scripting
- Pretty logging in CLI mode vs JSON in MCP mode
- Eval harness workspace member (memd-evals)
- McpClient for subprocess communication via stdio
- MCP conformance test suite (Suite A) with 13 tests
- All tests pass in ~65ms total

## Task Commits

Each task was committed atomically:

1. **Task 1: Implement CLI mode** - `65c8048` (feat)
2. **Task 2: Create eval harness infrastructure** - `0820fce` (feat)
3. **Task 3: Implement MCP conformance test suite** - `6fe150f` (feat)

## Files Created/Modified

Created:
- `crates/memd/src/cli.rs` - CliCommand enum and run_cli function
- `evals/harness/Cargo.toml` - Harness workspace member
- `evals/harness/src/lib.rs` - TestResult struct
- `evals/harness/src/main.rs` - Harness runner with suite selection
- `evals/harness/src/mcp_client.rs` - McpClient for subprocess testing
- `evals/harness/src/suites/mod.rs` - Suite module exports
- `evals/harness/src/suites/mcp_conformance.rs` - 13 conformance tests

Modified:
- `Cargo.toml` - Added evals/harness to workspace members
- `crates/memd/src/lib.rs` - Export cli module
- `crates/memd/src/main.rs` - Dual-mode operation

## Decisions Made

### D01-04-01: Logging Mode by Run Mode
- **Context:** Different output needs for CLI vs MCP
- **Decision:** CLI uses pretty logging, MCP uses JSON
- **Rationale:** Human-readable for debugging, structured for agents

### D01-04-02: Build Before Eval
- **Context:** Eval harness needs up-to-date binary
- **Decision:** Build memd before running tests (skippable)
- **Rationale:** Ensures correct binary, --skip-build for CI optimization

### D01-04-03: Subprocess Isolation
- **Context:** Need test isolation for conformance testing
- **Decision:** Each test starts a fresh memd subprocess
- **Rationale:** Complete isolation, no shared state, clean error handling

## Deviations from Plan

None - plan executed exactly as written.

## Test Coverage

85 unit tests total (1 new from this plan):
- cli::tests::parse_chunk_types

Plus 13 eval harness integration tests:
- A1_initialize, A1_tools_list, A1_tools_count
- A1_tool_add, A1_tool_search, A1_tool_get, A1_tool_delete, A1_tool_stats, A1_tool_add_batch
- A2_invalid_json, A2_unknown_method, A2_missing_tenant, A2_invalid_chunk_type

## Verification Results

All verification criteria passed:
- CLI mode works for all operations with JSON output
- Help text available for memd and memd-evals
- Eval harness builds and runs
- 13/13 MCP conformance tests pass
- Harness exits 0 on success, non-zero on failure

## CLI Usage Examples

```bash
# Add a chunk
cargo run -p memd -- --mode cli add --tenant-id test --text "hello" --chunk-type doc

# Search
cargo run -p memd -- --mode cli search --tenant-id test --query hello

# Stats
cargo run -p memd -- --mode cli stats --tenant-id test
```

## Eval Harness Usage

```bash
# Run all tests
cargo run -p memd-evals

# Run MCP suite only
cargo run -p memd-evals -- --suite mcp

# Skip build (for CI)
cargo run -p memd-evals -- --skip-build
```

## Next Phase Readiness

**Phase 1 Complete - Ready for Phase 2: Persistent Storage**

Prerequisites satisfied:
- Store trait ready for persistent implementation
- In-memory store provides working baseline
- CLI mode enables manual testing of persistent store
- Eval harness provides automated verification
- All 6 MCP tools verified working

---
*Phase: 01-skeleton-+-mcp-server*
*Completed: 2026-01-29*
