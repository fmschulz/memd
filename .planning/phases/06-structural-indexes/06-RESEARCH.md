# Phase 6: Structural Indexes - Research

**Researched:** 2026-01-31
**Domain:** Code-aware search with AST-based symbol extraction, call graph building, and query routing
**Confidence:** MEDIUM

## Summary

Phase 6 adds structural code navigation capabilities to memd's existing hybrid retrieval system. The phase requires integrating tree-sitter for multi-language AST parsing to extract symbols (functions, classes, methods, variables), build call graphs (caller -> callee relationships), and index execution traces (tool calls, stack traces). A query router will classify user intent to appropriately blend structural and semantic search results.

The standard approach uses tree-sitter with language-specific grammar crates for AST parsing, combined with SQLite for persistent graph storage (extending the existing metadata store pattern). Tree-sitter provides sub-millisecond incremental parsing, making it ideal for file-change-triggered reindexing. The query router should use pattern matching with explicit prefix overrides for intent classification.

**Primary recommendation:** Use tree-sitter with official grammar crates for symbol extraction, store graph data in SQLite with adjacency list schema, and implement a pattern-based query router that falls back from structural to semantic search.

## Standard Stack

The established libraries/tools for this domain:

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| tree-sitter | 0.24+ | Incremental parser library | Official Rust bindings, battle-tested by GitHub, Neovim, Helix |
| tree-sitter-rust | 0.24.0 | Rust grammar | Official grammar, maintained by tree-sitter org |
| tree-sitter-python | 0.25.0 | Python grammar | Official grammar, latest release |
| tree-sitter-typescript | 0.23.2 | TypeScript/TSX grammars | Official grammar, includes TSX support |
| tree-sitter-go | 0.25.0 | Go grammar | Official grammar |
| tree-sitter-javascript | 0.23+ | JavaScript grammar | Official grammar |
| tree-sitter-java | 0.23+ | Java grammar | Official grammar |
| tree-sitter-cpp | 0.23+ | C++ grammar | Official grammar |
| rusqlite | 0.38 | SQLite access | Already in workspace, proven patterns |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| tree-sitter-graph | 0.12+ | Graph construction DSL | Complex cross-file analysis if needed |
| petgraph | 0.6+ | In-memory graph algorithms | Call chain traversal, cycle detection |
| regex | 1.10+ | Query pattern matching | Intent classification patterns |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| tree-sitter | rust-analyzer/LSP | More accurate but requires build system, much heavier |
| SQLite graphs | petgraph only | petgraph is faster but loses persistence |
| tree-sitter-graph | Manual AST traversal | DSL is more declarative but adds learning curve |
| tree-sitter-stack-graphs | tree-sitter-graph | Stack-graphs is for full name resolution, overkill here |

**Installation:**
```toml
[dependencies]
tree-sitter = "0.24"
tree-sitter-rust = "0.24"
tree-sitter-python = "0.25"
tree-sitter-typescript = "0.23"
tree-sitter-go = "0.25"
tree-sitter-javascript = "0.23"
tree-sitter-java = "0.23"
tree-sitter-cpp = "0.23"
# petgraph only if traversal algorithms needed beyond SQL queries
petgraph = { version = "0.6", optional = true }
```

## Architecture Patterns

### Recommended Project Structure
```
crates/memd/src/
├── structural/           # New module for Phase 6
│   ├── mod.rs           # Module exports
│   ├── parser.rs        # Multi-language tree-sitter wrapper
│   ├── symbols.rs       # Symbol extraction (definitions, references)
│   ├── call_graph.rs    # Call graph builder
│   ├── traces.rs        # Tool call and stack trace indexing
│   ├── storage.rs       # SQLite schema for structural data
│   └── router.rs        # Query intent classification
├── index/
│   └── structural.rs    # StructuralIndex trait + impl (analog to BM25)
└── mcp/
    └── tools.rs         # Extended with find_* tools
```

### Pattern 1: Language-Agnostic Parser Wrapper
**What:** Single abstraction over multiple tree-sitter language parsers
**When to use:** All symbol extraction and call graph building
**Example:**
```rust
// Source: tree-sitter docs + Drift project pattern
use tree_sitter::{Language, Parser, Query, QueryCursor};

pub struct LanguageSupport {
    language: Language,
    parser: Parser,
    symbols_query: Query,
    calls_query: Query,
}

impl LanguageSupport {
    pub fn for_extension(ext: &str) -> Option<Self> {
        let (language, symbols_scm, calls_scm) = match ext {
            "rs" => (
                tree_sitter_rust::LANGUAGE.into(),
                include_str!("queries/rust/symbols.scm"),
                include_str!("queries/rust/calls.scm"),
            ),
            "py" => (
                tree_sitter_python::LANGUAGE.into(),
                include_str!("queries/python/symbols.scm"),
                include_str!("queries/python/calls.scm"),
            ),
            // ... other languages
            _ => return None,
        };

        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;

        Some(Self {
            language,
            parser,
            symbols_query: Query::new(&language, symbols_scm).ok()?,
            calls_query: Query::new(&language, calls_scm).ok()?,
        })
    }
}
```

### Pattern 2: SQLite Graph Storage with Adjacency List
**What:** Store call graph as adjacency list in SQLite tables
**When to use:** Persistent graph storage matching existing metadata patterns
**Example:**
```rust
// Source: Existing SqliteMetadataStore pattern in codebase
// Schema for structural data (new tables in same DB)

const STRUCTURAL_SCHEMA: &str = r#"
-- Symbols table: functions, classes, methods, variables
CREATE TABLE IF NOT EXISTS symbols (
    symbol_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT,
    file_path TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,  -- 'function', 'class', 'method', 'variable'
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    col_start INTEGER NOT NULL,
    col_end INTEGER NOT NULL,
    parent_symbol_id INTEGER,  -- For nested scopes
    signature TEXT,
    docstring TEXT,
    visibility TEXT,  -- 'public', 'private', etc.
    FOREIGN KEY (parent_symbol_id) REFERENCES symbols(symbol_id)
);

-- Call graph edges: caller -> callee
CREATE TABLE IF NOT EXISTS call_edges (
    edge_id INTEGER PRIMARY KEY,
    caller_symbol_id INTEGER NOT NULL,
    callee_name TEXT NOT NULL,  -- May be unresolved
    callee_symbol_id INTEGER,   -- NULL if unresolved
    call_line INTEGER NOT NULL,
    call_col INTEGER NOT NULL,
    FOREIGN KEY (caller_symbol_id) REFERENCES symbols(symbol_id),
    FOREIGN KEY (callee_symbol_id) REFERENCES symbols(symbol_id)
);

-- Tool call traces
CREATE TABLE IF NOT EXISTS tool_traces (
    trace_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT,
    session_id TEXT,
    tool_name TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    input_json TEXT,
    output_json TEXT,
    error_json TEXT,
    context_tags TEXT  -- JSON array
);

-- Stack traces from errors
CREATE TABLE IF NOT EXISTS stack_traces (
    trace_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT,
    timestamp_ms INTEGER NOT NULL,
    error_signature TEXT,
    full_trace TEXT
);

CREATE TABLE IF NOT EXISTS stack_frames (
    frame_id INTEGER PRIMARY KEY,
    trace_id INTEGER NOT NULL,
    frame_idx INTEGER NOT NULL,
    file_path TEXT,
    function_name TEXT,
    line_number INTEGER,
    FOREIGN KEY (trace_id) REFERENCES stack_traces(trace_id)
);

-- Indexes for common queries
CREATE INDEX IF NOT EXISTS idx_symbols_tenant_name
    ON symbols(tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_symbols_tenant_file
    ON symbols(tenant_id, file_path);
CREATE INDEX IF NOT EXISTS idx_call_edges_caller
    ON call_edges(caller_symbol_id);
CREATE INDEX IF NOT EXISTS idx_call_edges_callee
    ON call_edges(callee_symbol_id);
CREATE INDEX IF NOT EXISTS idx_tool_traces_tenant_tool
    ON tool_traces(tenant_id, tool_name, timestamp_ms DESC);
CREATE INDEX IF NOT EXISTS idx_stack_frames_function
    ON stack_frames(function_name);
"#;
```

### Pattern 3: Query Intent Router with Pattern Matching
**What:** Classify queries by intent to route to appropriate search backend
**When to use:** All memory.search calls go through router first
**Example:**
```rust
// Source: Sourcegraph Smart Search + CONTEXT.md decisions
use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub enum QueryIntent {
    CodeSearch,      // General semantic code search
    DebugTrace,      // Tool calls, errors, stack traces
    DocQa,           // Documentation questions
    DecisionWhy,     // Why was X decided
    PlanNext,        // What's the next step
    SymbolDefinition(String),  // Where is X defined
    SymbolReferences(String),  // Who uses X
    SymbolCallers(String),     // Who calls X
}

pub struct QueryRouter {
    definition_patterns: Vec<Regex>,
    caller_patterns: Vec<Regex>,
    reference_patterns: Vec<Regex>,
    trace_patterns: Vec<Regex>,
}

impl QueryRouter {
    pub fn classify(&self, query: &str) -> QueryIntent {
        // Check explicit prefixes first (override)
        if let Some(rest) = query.strip_prefix("def:") {
            return QueryIntent::SymbolDefinition(rest.trim().to_string());
        }
        if let Some(rest) = query.strip_prefix("callers:") {
            return QueryIntent::SymbolCallers(rest.trim().to_string());
        }
        if let Some(rest) = query.strip_prefix("refs:") {
            return QueryIntent::SymbolReferences(rest.trim().to_string());
        }

        // Pattern detection for natural language
        for pattern in &self.definition_patterns {
            if let Some(caps) = pattern.captures(query) {
                if let Some(name) = caps.get(1) {
                    return QueryIntent::SymbolDefinition(name.as_str().to_string());
                }
            }
        }

        for pattern in &self.caller_patterns {
            if let Some(caps) = pattern.captures(query) {
                if let Some(name) = caps.get(1) {
                    return QueryIntent::SymbolCallers(name.as_str().to_string());
                }
            }
        }

        // ... similar for other patterns

        // Default to semantic code search
        QueryIntent::CodeSearch
    }
}

// Example patterns (from CONTEXT.md decisions):
// Definition: "where is (\w+) defined", "definition of (\w+)", "find class (\w+)"
// Callers: "who calls (\w+)", "callers of (\w+)"
// References: "references to (\w+)", "usages of (\w+)"
```

### Anti-Patterns to Avoid
- **Cross-file type inference:** Don't try to resolve types across files without a type checker. Use name-based matching with confidence scores instead.
- **Full AST serialization:** Don't store entire ASTs - extract only the needed symbols and relationships.
- **Synchronous reindexing:** Don't block MCP responses on index updates - use background tasks.
- **Deep call chain queries by default:** Limit traversal depth (1-3 hops) to avoid expensive recursive queries.

## Don't Hand-Roll

Problems that look simple but have existing solutions:

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Multi-language parsing | Custom parser per language | tree-sitter grammars | 100k+ lines/sec, incremental, battle-tested |
| AST query patterns | Recursive node visitors | tree-sitter Query DSL | S-expression queries are declarative and efficient |
| Identifier tokenization | Regex-based splitting | tree-sitter node types | Grammar knows identifier boundaries |
| Graph traversal algorithms | BFS/DFS from scratch | petgraph (if needed) | Handles cycles, provides standard algorithms |
| SQLite connection pooling | Manual mutex management | rusqlite with WAL mode | Already proven in codebase |

**Key insight:** Tree-sitter grammars encode years of language-specific parsing knowledge. Writing custom parsers loses this and introduces bugs. The grammar files define node types, field names, and query patterns that are correct by construction.

## Common Pitfalls

### Pitfall 1: Blocking on Tree-sitter Parse
**What goes wrong:** Parsing large files (10k+ lines) takes 20-100ms, blocking MCP response
**Why it happens:** Synchronous parsing in request handler
**How to avoid:** Parse asynchronously, use cached results, only reparse on file change
**Warning signs:** Search latency spikes with large files in results

### Pitfall 2: Storing Unresolved Call Targets as Errors
**What goes wrong:** Treating unresolved callee names as failures loses useful information
**Why it happens:** Static analysis cannot resolve all calls (dynamic dispatch, callbacks)
**How to avoid:** Store callee_name always, callee_symbol_id only when resolved. Query by name for completeness.
**Warning signs:** Call graph has very few edges despite code having many calls

### Pitfall 3: Not Handling Grammar Version Mismatches
**What goes wrong:** Query captures return empty because node types changed
**Why it happens:** Tree-sitter grammar updates may rename node types
**How to avoid:** Pin grammar versions, test queries against each supported grammar version
**Warning signs:** find_symbol_definition returns empty for valid symbols

### Pitfall 4: Query Router Over-Classification
**What goes wrong:** Too many queries get routed to structural search and return empty
**Why it happens:** Aggressive pattern matching catches general queries
**How to avoid:** Require strong signals (specific phrases), fall back to semantic on empty results
**Warning signs:** Users see "no results" when semantic search would have found relevant content

### Pitfall 5: SQLite Lock Contention During Index Updates
**What goes wrong:** Writes block reads during reindexing
**Why it happens:** Long transactions while processing many files
**How to avoid:** Batch commits (every N files), use WAL mode (already configured), consider separate DB for structural data
**Warning signs:** Search latency degrades during file indexing

## Code Examples

Verified patterns from official sources:

### Tree-sitter Basic Parsing
```rust
// Source: https://docs.rs/tree-sitter
use tree_sitter::{Parser, Language};

fn parse_rust_code(source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    let language: Language = tree_sitter_rust::LANGUAGE.into();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}
```

### Tree-sitter Query for Function Definitions (Rust)
```rust
// Source: tree-sitter query syntax docs + tags.scm conventions
const RUST_FUNCTIONS_QUERY: &str = r#"
(function_item
  name: (identifier) @name) @definition.function

(impl_item
  body: (declaration_list
    (function_item
      name: (identifier) @name) @definition.method))

(struct_item
  name: (type_identifier) @name) @definition.class
"#;

fn extract_symbols(tree: &tree_sitter::Tree, source: &[u8], query: &Query) -> Vec<Symbol> {
    let mut cursor = QueryCursor::new();
    let mut symbols = Vec::new();

    for match_ in cursor.matches(query, tree.root_node(), source) {
        for capture in match_.captures {
            let node = capture.node;
            let capture_name = query.capture_names()[capture.index as usize];

            if capture_name.starts_with("definition.") {
                let kind = capture_name.strip_prefix("definition.").unwrap();
                let name_node = match_.captures.iter()
                    .find(|c| query.capture_names()[c.index as usize] == "name")
                    .map(|c| c.node);

                if let Some(name_node) = name_node {
                    symbols.push(Symbol {
                        name: name_node.utf8_text(source).unwrap().to_string(),
                        kind: kind.to_string(),
                        line_start: node.start_position().row,
                        line_end: node.end_position().row,
                        // ... other fields
                    });
                }
            }
        }
    }

    symbols
}
```

### Tree-sitter Query for Call Expressions (Rust)
```rust
// Source: tree-sitter query syntax docs
const RUST_CALLS_QUERY: &str = r#"
; Direct function calls
(call_expression
  function: (identifier) @callee) @call

; Method calls
(call_expression
  function: (field_expression
    field: (field_identifier) @callee)) @call

; Qualified path calls (e.g., std::env::var)
(call_expression
  function: (scoped_identifier
    name: (identifier) @callee)) @call
"#;
```

### Find Callers SQL Query
```sql
-- Source: Adjacency list pattern for call graph traversal
-- Find all callers of a function (1 hop)
SELECT DISTINCT s.file_path, s.name, s.line_start
FROM call_edges ce
JOIN symbols s ON ce.caller_symbol_id = s.symbol_id
WHERE ce.callee_symbol_id = ?
  AND s.tenant_id = ?;

-- Find callers up to N hops (recursive CTE)
WITH RECURSIVE callers AS (
    -- Base case: direct callers
    SELECT ce.caller_symbol_id, 1 as depth
    FROM call_edges ce
    WHERE ce.callee_symbol_id = ?

    UNION ALL

    -- Recursive case: callers of callers
    SELECT ce.caller_symbol_id, c.depth + 1
    FROM call_edges ce
    JOIN callers c ON ce.callee_symbol_id = c.caller_symbol_id
    WHERE c.depth < ?  -- max_depth parameter
)
SELECT DISTINCT s.file_path, s.name, s.line_start, c.depth
FROM callers c
JOIN symbols s ON c.caller_symbol_id = s.symbol_id
WHERE s.tenant_id = ?
ORDER BY c.depth, s.file_path;
```

### Tool Trace Insertion
```rust
// Source: Existing codebase patterns (MCP server + SQLite store)
pub struct ToolTraceRecord {
    pub tenant_id: TenantId,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: String,
    pub timestamp_ms: i64,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
    pub context_tags: Vec<String>,
}

impl StructuralStore {
    pub fn insert_tool_trace(&self, trace: &ToolTraceRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tool_traces
             (tenant_id, project_id, session_id, tool_name, timestamp_ms,
              input_json, output_json, error_json, context_tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                trace.tenant_id.as_str(),
                trace.project_id.as_deref(),
                trace.session_id.as_deref(),
                &trace.tool_name,
                trace.timestamp_ms,
                serde_json::to_string(&trace.input)?,
                trace.output.as_ref().map(|v| serde_json::to_string(v)).transpose()?,
                trace.error.as_ref().map(|v| serde_json::to_string(v)).transpose()?,
                serde_json::to_string(&trace.context_tags)?,
            ],
        )?;
        Ok(())
    }
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Regex-based symbol extraction | Tree-sitter AST parsing | 2018+ | Accurate boundaries, handles all syntax |
| Full reparse on edit | Incremental parsing | Tree-sitter core feature | Sub-ms updates |
| Custom per-language parsers | Official grammar crates | 2023+ | Maintained by community |
| Keyword search for code | Hybrid structural + semantic | 2024+ | Better precision for navigation queries |

**Deprecated/outdated:**
- **CTAGS/etags**: Still works but tree-sitter provides richer structure
- **Regex-based symbol finders**: Breaks on edge cases, no scope awareness
- **SCIP for incremental indexing**: Heavy dependency, tree-sitter-native approach preferred

## Open Questions

Things that couldn't be fully resolved:

1. **Cross-language call graph linking**
   - What we know: Import statements can link files, tree-sitter parses imports
   - What's unclear: Best approach for FFI boundaries (Rust calling Python, etc.)
   - Recommendation: Start with single-language graphs, add cross-language as enhancement

2. **Optimal batch size for index commits**
   - What we know: Frequent commits hurt performance, rare commits hurt visibility
   - What's unclear: Exact threshold (100 files? 1000? time-based?)
   - Recommendation: Start with commit every 100 files, tune based on metrics

3. **Query router pattern tuning**
   - What we know: Need patterns for definition/caller/reference queries
   - What's unclear: Exact regex patterns that balance precision/recall
   - Recommendation: Start conservative, add patterns based on actual query analysis

## Sources

### Primary (HIGH confidence)
- [tree-sitter docs.rs](https://docs.rs/tree-sitter) - Rust API reference
- [tree-sitter official site](https://tree-sitter.github.io/) - Query syntax, code navigation docs
- [tree-sitter-rust GitHub](https://github.com/tree-sitter/tree-sitter-rust) - Grammar, version info
- [Sourcegraph code navigation docs](https://docs.sourcegraph.com/code_navigation/explanations/features) - Navigation patterns

### Secondary (MEDIUM confidence)
- [GitHub semantic code team paper](https://dl.acm.org/doi/fullHtml/10.1145/3487019.3487022) - Static analysis patterns
- [Drift call graph wiki](https://github.com/dadbodgeoff/drift/wiki/Call-Graph-Analysis) - Call extraction approach
- [Langfuse MCP tracing](https://langfuse.com/docs/observability/features/mcp-tracing) - Tool trace patterns

### Tertiary (LOW confidence)
- Various Medium/DEV.to articles on tree-sitter usage - General patterns but not authoritative
- WebSearch results on query classification - Concept validation only

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - Official tree-sitter crates, well-documented
- Architecture: MEDIUM - Patterns derived from multiple sources, not verified in this codebase
- Pitfalls: MEDIUM - Based on documented issues and general experience
- Query routing: LOW - Design space, needs validation with real queries

**Research date:** 2026-01-31
**Valid until:** 2026-03-01 (30 days - tree-sitter ecosystem stable)
