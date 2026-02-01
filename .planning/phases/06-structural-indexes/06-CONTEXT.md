# Phase 6: Structural Indexes - Context

**Gathered:** 2026-01-31
**Status:** Ready for planning

<domain>
## Phase Boundary

Enable code-aware search capabilities that find symbols (functions, classes, variables, types), call relationships (who calls what), and execution traces (tool invocations, stack traces) across the codebase. This builds on top of the existing hybrid retrieval foundation (Phases 1-5) and adds structural understanding of code.

</domain>

<decisions>
## Implementation Decisions

### Symbol extraction depth
- Index functions, classes, methods, AND significant variables (module-level, class fields, types)
- Support top 5-7 languages: Rust, Python, TypeScript, Go, JavaScript, Java, C++
- Store comprehensive metadata for each symbol:
  - Name and location (file path, line/column)
  - Scope context (parent class/module, visibility modifiers)
  - Signature info (parameter types, return types for functions; declared types for variables)
  - Docstrings/comments (for semantic search integration on 'what does this do' queries)

### Call graph scope
- Configurable depth traversal (1-3 hops) for caller queries
- Static analysis only - track direct function calls visible in AST
  - No heuristics for dynamic calls, callbacks, or function pointers
  - Deterministic and fast at the cost of missing some dynamic patterns
- Hybrid cross-file resolution: local graph + import resolution
  - Index each file independently for incremental updates
  - Use import statements to link across files
- Type-aware method call tracking when available
  - Use type information when language/code provides it
  - Fall back to name-based tracking when types unknown

### Trace indexing granularity
- Capture full trace information from agent tool calls:
  - Tool name and timestamp (basic timeline)
  - Input parameters (enables 'all reads of file X' queries)
  - Output/results (enables 'when did we see this error' queries)
  - Context tags (session ID, project, current task for grouping)
- Store everything - no truncation of large outputs
- Keep forever - no expiration or rotation
- Index full stack trace frame details:
  - File, function, line number for each frame
  - Enables precise error location queries

### Query routing logic
- Hybrid approach: pattern detection first, explicit prefixes as override
  - Smart defaults with explicit control when needed
- Trigger structural search on these patterns:
  - Definition queries: 'where is X defined', 'definition of Y', 'find class Z'
  - Caller/reference queries: 'who calls X', 'references to Y', 'usages of Z'
  - Trace/debug queries: 'errors in X', 'stack trace containing Y', 'tool calls to Z'
  - Code structure queries: 'all functions in file X', 'methods of class Y'
- Empty structural results → automatic fallback to semantic search
- Result strategy: structural first, semantic expands
  - Start with precise structural results
  - Use semantic search to add related context
  - Avoids complex fusion ranking while providing comprehensive results

### Claude's Discretion
- Tree-sitter grammar selection and AST traversal patterns
- Exact pattern matching rules for query routing
- Storage format for symbol table and call graph data
- Indexing update strategy (incremental vs full rebuild)

</decisions>

<specifics>
## Specific Ideas

No specific product references mentioned - open to standard code navigation patterns.

Key design principle: Build on existing hybrid retrieval foundation without disrupting Phases 1-5 functionality.

</specifics>

<deferred>
## Deferred Ideas

None - discussion stayed within phase scope.

</deferred>

---

*Phase: 06-structural-indexes*
*Context gathered: 2026-01-31*
