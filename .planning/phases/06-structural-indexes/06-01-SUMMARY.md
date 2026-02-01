---
phase: 06-structural-indexes
plan: 01
subsystem: structural
tags: [tree-sitter, parser, ast, multi-language]
depends_on:
  requires: []
  provides: [tree-sitter-parser, language-detection, parse-file]
  affects: [06-02, 06-03, 06-04, 06-05]
tech-stack:
  added: [tree-sitter, tree-sitter-rust, tree-sitter-python, tree-sitter-typescript, tree-sitter-javascript, tree-sitter-go, tree-sitter-java, tree-sitter-cpp, regex]
  patterns: [parser-wrapper, language-detection, incremental-parsing]
key-files:
  created:
    - crates/memd/src/structural/mod.rs
    - crates/memd/src/structural/parser.rs
  modified:
    - Cargo.toml
    - crates/memd/Cargo.toml
    - crates/memd/src/lib.rs
    - Cargo.lock
decisions:
  - id: 06-01-ts-version
    choice: "tree-sitter 0.25"
    reason: "Grammar crates (python 0.25, go 0.25) require tree-sitter ABI version 15"
  - id: 06-01-fresh-parser
    choice: "Fresh LanguageSupport per parse"
    reason: "Parser is not Send/Sync, creating fresh parser is cheap (~microseconds)"
  - id: 06-01-c-as-cpp
    choice: "Map .c/.h to C++ grammar"
    reason: "C++ grammar handles C code, avoids separate tree-sitter-c dependency"
metrics:
  duration: 3m
  completed: 2026-02-01
---

# Phase 06 Plan 01: Tree-sitter Multi-language Parser Summary

Tree-sitter parsing foundation with 7-language support for structural indexing.

## One-liner

Tree-sitter parser wrapper supporting Rust, Python, TypeScript, JavaScript, Go, Java, C++ via extension detection.

## What Was Built

### Core Components

**SupportedLanguage Enum:**
- Represents 7 supported languages with tree-sitter grammar mappings
- Provides `tree_sitter_language()`, `name()`, and `extensions()` accessors
- Hash/Eq/Clone for use in collections

**LanguageSupport Struct:**
- Wraps tree-sitter Parser with language configuration
- `for_extension(ext)` - factory from file extension
- `for_language(lang)` - factory from SupportedLanguage enum
- `parse(source)` - parse source returning Tree
- `parse_with_old_tree(source, old)` - incremental parsing for editors

**ParseResult Struct:**
- Contains: tree, language, source_bytes
- `root_node()` - access AST root
- `has_errors()` - check for syntax errors

**Helper Functions:**
- `detect_language(path)` - extension to SupportedLanguage
- `parse_file(path, content)` - one-shot parse with language detection

### Extension Mappings

| Language | Extensions |
|----------|-----------|
| Rust | .rs |
| Python | .py |
| TypeScript | .ts, .tsx |
| JavaScript | .js, .jsx |
| Go | .go |
| Java | .java |
| C++ | .cpp, .cc, .cxx, .c, .h, .hpp |

## Decisions Made

| Decision | Choice | Rationale |
|----------|--------|-----------|
| tree-sitter version | 0.25 | Required for python/go grammar ABI v15 |
| Parser lifecycle | Fresh per parse | Not Send/Sync, cheap to create |
| C/C++ grammar | tree-sitter-cpp | Handles C code, fewer dependencies |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Tree-sitter version mismatch**
- **Found during:** Task 2 test execution
- **Issue:** tree-sitter 0.24 incompatible with tree-sitter-python/go 0.25 (ABI version 15)
- **Fix:** Updated tree-sitter to 0.25 in workspace Cargo.toml
- **Files modified:** Cargo.toml, Cargo.lock
- **Commit:** cb0c28c (included in Task 2 commit)

**2. [Rule 1 - Bug] Lifetime annotation warning**
- **Found during:** Task 2 cargo check
- **Issue:** `root_node(&self)` had elided lifetime not matching return type
- **Fix:** Added explicit `Node<'_>` return type annotation
- **Files modified:** crates/memd/src/structural/parser.rs
- **Commit:** cb0c28c (included in Task 2 commit)

## Verification Results

```
cargo check -p memd: PASS (3 warnings, unrelated to structural module)
cargo test -p memd --lib structural::parser: 15/15 tests PASS
```

**Tests:**
- test_parse_rust_function - PASS
- test_parse_python_function - PASS
- test_parse_typescript_function - PASS
- test_parse_javascript_function - PASS
- test_parse_go_function - PASS
- test_parse_java_class - PASS
- test_parse_cpp_function - PASS
- test_detect_language_extensions - PASS
- test_unsupported_extension_returns_none - PASS
- test_unsupported_extension_parse_error - PASS
- test_language_support_direct_usage - PASS
- test_incremental_parsing - PASS
- test_supported_language_name - PASS
- test_supported_language_extensions - PASS
- test_parse_with_syntax_errors - PASS

## Key Files

| File | Lines | Purpose |
|------|-------|---------|
| structural/mod.rs | 11 | Module exports |
| structural/parser.rs | 463 | Parser wrapper + tests |

## Commits

| Hash | Type | Message |
|------|------|---------|
| 654c0c4 | feat | add tree-sitter dependencies for structural parsing |
| cb0c28c | feat | create multi-language tree-sitter parser wrapper |

## Next Phase Readiness

**Ready for 06-02 (Symbol Extractor):**
- ParseResult provides AST tree for query operations
- SupportedLanguage enum available for language-specific query selection
- parse_file() ready for file-based symbol extraction

**Dependencies satisfied:**
- tree_sitter::Tree for walking/querying AST
- Language grammars loaded and tested
- Error handling via ParseError enum
