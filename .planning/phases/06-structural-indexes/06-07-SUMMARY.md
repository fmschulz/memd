# Phase 6 Plan 7: Query Router Summary

---
phase: 06
plan: 07
subsystem: structural-search
tags: [routing, intent-classification, hybrid-search, blending]

dependency-graph:
  requires: [06-04, 06-06]
  provides: [query-router, search-with-routing, blended-results]
  affects: [06-08]

tech-stack:
  added: []
  patterns: [regex-based-classification, intent-routing, result-blending]

key-files:
  created:
    - crates/memd/src/structural/router.rs
  modified:
    - crates/memd/src/structural/mod.rs
    - crates/memd/src/lib.rs
    - crates/memd/src/store/hybrid.rs

decisions:
  - id: query-router-patterns
    choice: "Regex patterns for natural language intent detection"
    rationale: "Fast classification without LLM, handles common developer queries"
  - id: explicit-prefix-override
    choice: "Prefixes (def:, callers:, refs:, etc.) override pattern detection"
    rationale: "Deterministic routing when user knows exactly what they want"
  - id: struct-14-blending
    choice: "BlendStrategy::StructuralPrimary as default"
    rationale: "Structural results are authoritative, semantic provides context"
  - id: query-intent-default
    choice: "QueryIntent::SemanticSearch as default variant"
    rationale: "Enables Default trait for result structs, safe fallback"

metrics:
  duration: 6m
  completed: 2026-02-01
---

## One-liner

Query router classifies natural language queries into intents and routes to structural/trace/semantic backends with STRUCT-14 blending.

## What Was Built

### QueryRouter (router.rs)

Intent classification system with pattern-based detection:

**QueryIntent enum:**
- `SemanticSearch` - default fallback
- `SymbolDefinition(name)` - "where is X defined"
- `SymbolCallers(name)` - "who calls X"
- `SymbolReferences(name)` - "usages of X"
- `ModuleImports(module)` - "who imports X"
- `FileSymbols(file)` - "symbols in X.rs"
- `ToolCalls(name)` - "calls to X"
- `ErrorSearch(sig)` - "errors about X"
- `DocQa`, `DecisionWhy`, `PlanNext` - document queries

**Pattern categories:**
- Definition: "where is X defined", "find function X", "what is X"
- Callers: "who calls X", "callers of X", "X is called by"
- References: "usages of X", "where is X used"
- Imports: "who imports X", "files that import X"
- Errors: "errors in X", "what went wrong", "recent errors"
- Tools: "calls to X", "recent tool calls", "tool history"

**Explicit prefixes (confidence 1.0):**
- `def:X` -> SymbolDefinition
- `callers:X` -> SymbolCallers
- `refs:X` -> SymbolReferences
- `imports:X` -> ModuleImports
- `errors:X` -> ErrorSearch
- `tools:X` -> ToolCalls
- `file:X` -> FileSymbols

### HybridSearcher Integration (hybrid.rs)

**SearchWithRoutingResult enum:**
- `Hybrid(Vec<HybridSearchResult>)` - standard semantic/hybrid results
- `Structural(StructuralSearchResult)` - pure structural results
- `Blended(BlendedSearchResult)` - structural + semantic context (STRUCT-14)
- `Trace(TraceSearchResult)` - tool calls and errors

**STRUCT-14 Blending:**
- Code-intent queries (definition, callers, references, imports) blend automatically
- Structural results returned as primary
- Semantic results added as supplementary context
- `BlendStrategy::StructuralPrimary` keeps structural authoritative

**Fallback behavior:**
- Empty structural results trigger semantic fallback (if enabled)
- `fell_back_to_semantic` flag indicates when fallback occurred

**New methods:**
- `search_with_routing()` - main routing entry point
- `classify_query()` - preview intent without executing search
- `with_query_services()` - constructor with services
- `set_symbol_query_service()`, `set_trace_query_service()` - runtime setup
- `structural_enabled()`, `trace_enabled()` - service availability checks

## Test Coverage

**Router tests (13):**
- test_classify_definition_queries
- test_classify_caller_queries
- test_classify_reference_queries
- test_classify_import_queries
- test_classify_error_queries
- test_classify_tool_queries
- test_explicit_prefix_override
- test_fallback_to_semantic
- test_case_insensitive_matching
- test_should_blend_semantic
- test_is_trace_query
- test_route_result_flags
- test_file_symbols_queries

**Hybrid routing tests (8 new):**
- test_route_to_semantic_search
- test_route_to_structural_search_no_service
- test_route_to_trace_search_no_service
- test_classify_query_intent
- test_blend_strategy_default
- test_structural_result_default
- test_searcher_service_flags
- test_route_error_search
- test_route_tool_calls

## Deviations from Plan

None - plan executed exactly as written.

## Commits

| Task | Description | Commit | Files |
|------|-------------|--------|-------|
| 1 | QueryRouter with pattern-based classification | 9422b27 | router.rs, mod.rs, lib.rs |
| 2 | Integrate router with HybridSearcher | 89189a0 | hybrid.rs, router.rs |

## Verification Results

- cargo check: PASSED (warnings only, pre-existing)
- cargo test router: 13/13 PASSED
- cargo test hybrid: 19/19 PASSED
- Total: 32 new tests passing

## Dependencies Satisfied

- 06-04: SymbolQueryService used for structural queries
- 06-06: TraceQueryService used for debug trace queries

## Next Phase Readiness

Ready for 06-08 (Eval Suite):
- Query router can classify eval queries
- Blending behavior can be evaluated
- Trace queries testable with mock data
