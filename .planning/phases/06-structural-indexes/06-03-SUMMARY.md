---
phase: 06-structural-indexes
plan: 03
subsystem: structural
tags: [tree-sitter, call-graph, imports, sqlite, ast-analysis]

# Dependency graph
requires:
  - phase: 06-01
    provides: Tree-sitter multi-language parser (SupportedLanguage, parse_file)
provides:
  - Call graph extraction with caller->callee edges
  - Import graph tracking with module dependencies
  - SQLite storage for call edges and imports
  - CallGraphIndexer for automatic file indexing
affects: [06-04, 06-05, 06-07]

# Tech tracking
tech-stack:
  added: [streaming-iterator]
  patterns: [tree-sitter queries, streaming iterator pattern, batch SQLite inserts]

key-files:
  created:
    - crates/memd/src/structural/call_graph.rs
    - crates/memd/src/structural/storage.rs
  modified:
    - crates/memd/src/structural/mod.rs
    - crates/memd/Cargo.toml

key-decisions:
  - "streaming-iterator for tree-sitter 0.25 API compatibility"
  - "CallType enum: Direct, Method, Qualified for call classification"
  - "Batch insert methods for efficient indexing"
  - "Re-indexing deletes old edges before inserting new"
  - "Aliased Python imports captured via aliased_import node pattern"

patterns-established:
  - "Tree-sitter query patterns per language for extraction"
  - "CallGraphIndexer wires extraction to storage"
  - "SymbolRecord stub for caller identification (to be replaced by real SymbolRecord)"

# Metrics
duration: 6min
completed: 2026-02-01
---

# Phase 6 Plan 03: Call Graph Summary

**Tree-sitter call graph extraction with 7-language support, import tracking, and SQLite storage integration**

## Performance

- **Duration:** 6 min
- **Started:** 2026-02-01T02:43:06Z
- **Completed:** 2026-02-01T02:49:29Z
- **Tasks:** 3
- **Files modified:** 4

## Accomplishments
- Call graph extraction from AST for all 7 languages (Rust, Python, TypeScript, JavaScript, Go, Java, C++)
- Import statement extraction with relative import detection
- SQLite tables for call_edges and imports with efficient indexes
- CallGraphIndexer wires extraction directly to storage
- 27 tests (12 call_graph + 15 storage)

## Task Commits

Each task was committed atomically:

1. **Task 1: Extend SQLite schema for call edges and imports** - `f154088` (feat)
2. **Task 2 & 3: Create call graph extractor and wire to storage** - `75e2ece` (feat)

## Files Created/Modified
- `crates/memd/src/structural/storage.rs` - SQLite tables for call_edges and imports with CRUD operations
- `crates/memd/src/structural/call_graph.rs` - CallGraphExtractor and CallGraphIndexer
- `crates/memd/src/structural/mod.rs` - Module exports
- `crates/memd/Cargo.toml` - Added streaming-iterator dependency

## Decisions Made
- **streaming-iterator for tree-sitter 0.25:** The tree-sitter 0.25 API changed QueryMatches to use streaming_iterator instead of std::iter::Iterator. Added dependency and used `while let Some(m) = matches.next()` pattern.
- **CallType classification:** Three types: Direct (foo()), Method (obj.method()), Qualified (module::func()). Determined by examining AST node structure.
- **Aliased Python imports:** Added `aliased_import` pattern to Python imports query to capture `import json as j` style imports.
- **Batch operations:** Both insert_call_edges_batch and insert_imports_batch use transactions for efficiency.
- **Re-indexing support:** delete_file_edges and delete_file_imports clear old data before new inserts.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed tree-sitter 0.25 API compatibility**
- **Found during:** Task 2 (Call graph extraction)
- **Issue:** tree-sitter 0.25 QueryMatches is not Iterator, uses streaming-iterator
- **Fix:** Added streaming-iterator dependency, changed for loop to while let Some pattern
- **Files modified:** crates/memd/Cargo.toml, crates/memd/src/structural/call_graph.rs
- **Verification:** cargo check passes
- **Committed in:** 75e2ece (Task 2/3 commit)

**2. [Rule 1 - Bug] Fixed Python aliased import extraction**
- **Found during:** Task 2 (Import extraction tests)
- **Issue:** `import json as j` not captured - Python grammar uses aliased_import node
- **Fix:** Added aliased_import pattern to PYTHON_IMPORTS_QUERY
- **Files modified:** crates/memd/src/structural/call_graph.rs
- **Verification:** test_extract_python_import passes
- **Committed in:** 75e2ece (Task 2/3 commit)

---

**Total deviations:** 2 auto-fixed (1 blocking, 1 bug)
**Impact on plan:** Both fixes necessary for correct operation. No scope creep.

## Issues Encountered
- storage.rs was expanded by parallel process with symbols and trace tables - integrated cleanly
- SymbolRecord name conflict resolved by renaming to CallGraphSymbolRecord in exports

## Next Phase Readiness
- Call graph foundation complete for structural search queries
- Ready for 06-04 (Structural Search MCP Tools)
- SymbolRecord stub should be replaced with real SymbolRecord from 06-02

---
*Phase: 06-structural-indexes*
*Completed: 2026-02-01*
