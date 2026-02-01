---
phase: 06-structural-indexes
plan: 02
subsystem: structural-indexing
tags: [tree-sitter, sqlite, symbols, extraction]

dependency_graph:
  requires: [06-01]
  provides: [symbol-extraction, symbol-storage, symbol-indexing]
  affects: [06-03, 06-04]

tech-stack:
  added: []
  patterns: [tree-sitter-queries, sqlite-tenant-isolation, streaming-iterator]

file-tracking:
  key-files:
    created:
      - crates/memd/src/structural/symbols.rs
    modified:
      - crates/memd/src/structural/storage.rs
      - crates/memd/src/structural/mod.rs
      - crates/memd/src/lib.rs

decisions:
  - id: dec-06-02-1
    decision: "Used streaming_iterator for tree-sitter query matches"
    rationale: "Matches existing call_graph.rs pattern, required by tree-sitter API"
  - id: dec-06-02-2
    decision: "TypeScript queries use type_identifier for class/interface names"
    rationale: "Tree-sitter-typescript grammar uses type_identifier, not identifier"
  - id: dec-06-02-3
    decision: "SymbolIndexer deletes before insert for re-indexing"
    rationale: "Clean slate approach avoids orphaned symbols from removed definitions"

metrics:
  duration: ~20min
  completed: 2026-02-01
---

# Phase 06 Plan 02: Symbol Extraction + Storage Summary

**One-liner:** Multi-language symbol extraction with tree-sitter queries and SQLite storage with tenant isolation.

## What Was Built

### 1. SQLite Symbols Table (storage.rs)

Extended the existing `StructuralStore` with a symbols table and CRUD operations:

**Schema:**
```sql
CREATE TABLE IF NOT EXISTS symbols (
    symbol_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT,
    file_path TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    col_start INTEGER NOT NULL,
    col_end INTEGER NOT NULL,
    parent_symbol_id INTEGER,
    signature TEXT,
    docstring TEXT,
    visibility TEXT,
    language TEXT NOT NULL,
    FOREIGN KEY (parent_symbol_id) REFERENCES symbols(symbol_id)
);
```

**Key Types:**
- `SymbolKind` enum: Function, Class, Method, Variable, Type, Module, Interface, Enum, Constant
- `SymbolRecord` struct with full metadata fields

**Operations:**
- `insert_symbol()` / `insert_symbols_batch()` - single and batch insert
- `find_symbols_by_name()` / `find_symbols_by_name_prefix()` - exact and prefix search
- `find_symbols_by_file()` - get all symbols in a file
- `delete_file_symbols()` - delete for re-indexing
- `get_symbol_children()` - nested symbol relationships

### 2. Symbol Extractor (symbols.rs)

Created `SymbolExtractor` with tree-sitter queries for 7 languages:

**Supported Languages:**
- Rust: functions, methods, structs, enums, types, constants, modules, traits
- Python: functions, classes, module-level variables
- TypeScript: functions, classes, interfaces, types, methods, variables, enums
- JavaScript: functions, classes, methods, variables
- Go: functions, methods, types, constants, variables
- Java: classes, interfaces, enums, methods, constructors, fields
- C++: functions, classes, structs, enums, typedefs

**Extracted Metadata:**
- Name, kind, location (line/column start/end)
- Function signature (parameters and return type)
- Docstring (language-specific comment extraction)
- Visibility (public/private/protected)
- Parent symbol name (for nested definitions)

### 3. Symbol Indexer (symbols.rs)

Created `SymbolIndexer` to wire extraction to storage:

```rust
pub struct SymbolIndexer {
    extractor: SymbolExtractor,
    store: Arc<StructuralStore>,
}

impl SymbolIndexer {
    pub fn index_file(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        file_path: &str,
        tree: &Tree,
        source: &[u8],
        language: SupportedLanguage,
    ) -> Result<usize>
}
```

**Features:**
- Deletes existing symbols before re-indexing (clean slate)
- Batch insert for performance
- Returns count of indexed symbols

## Test Coverage

**Storage tests (15 tests):**
- `test_insert_and_find_symbol`
- `test_find_symbols_by_name_prefix`
- `test_symbol_tenant_isolation`
- `test_delete_file_symbols`
- `test_nested_symbols`
- `test_batch_insert_symbols`
- Plus existing call_edges and imports tests

**Symbol extraction tests (7 tests):**
- `test_extract_rust_function`
- `test_extract_python_class`
- `test_extract_typescript_interface`
- `test_extract_go_function`
- `test_extract_java_class`
- `test_index_file_creates_symbols`
- `test_reindex_file_replaces_symbols`

All 65 structural module tests pass.

## Deviations from Plan

None - plan executed exactly as written.

## Files Changed

| File | Change | Lines |
|------|--------|-------|
| `crates/memd/src/structural/storage.rs` | Extended with symbols table | +830 |
| `crates/memd/src/structural/symbols.rs` | Created symbol extractor | +1023 |
| `crates/memd/src/structural/mod.rs` | Export new types | +7 |
| `crates/memd/src/lib.rs` | Export structural types | +5 |

## Key Links Verified

1. **symbols.rs -> parser.rs:** Uses `tree.root_node()` for AST traversal
2. **storage.rs -> rusqlite:** Uses `Connection::open` for SQLite
3. **symbols.rs -> storage.rs:** Calls `store.insert_symbols_batch()` in indexer

## Next Phase Readiness

**Ready for:**
- 06-03: Call graph extraction (already has call_edges table and CallGraphIndexer)
- 06-04: Query tools (can query symbols by name/file/kind)

**No blockers identified.**
