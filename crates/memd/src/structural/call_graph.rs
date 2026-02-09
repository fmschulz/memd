//! Call graph and import extraction from tree-sitter AST.
//!
//! Provides extraction of function calls and import statements from source code
//! using tree-sitter queries for each supported language.

use std::collections::HashMap;
use std::sync::Arc;

use tree_sitter::{Query, QueryCursor, Tree};

use super::parser::SupportedLanguage;
use super::storage::{CallEdgeRecord, CallType, ImportRecord, StructuralStore};
use crate::types::TenantId;

// --- Tree-sitter Query Patterns ---

/// Rust call patterns
const RUST_CALLS_QUERY: &str = r#"
; Direct function calls
(call_expression function: (identifier) @callee) @call

; Method calls
(call_expression function: (field_expression field: (field_identifier) @callee)) @call

; Qualified path calls (e.g., std::env::var)
(call_expression function: (scoped_identifier name: (identifier) @callee)) @call

; Macro invocations (commonly used like functions)
(macro_invocation macro: (identifier) @callee) @call
"#;

/// Rust import patterns
const RUST_IMPORTS_QUERY: &str = r#"
(use_declaration argument: (scoped_identifier) @import)
(use_declaration argument: (identifier) @import)
(use_declaration argument: (scoped_use_list path: (scoped_identifier) @import))
(use_declaration argument: (scoped_use_list path: (identifier) @import))
"#;

/// Python call patterns
const PYTHON_CALLS_QUERY: &str = r#"
; Function calls
(call function: (identifier) @callee) @call

; Method calls
(call function: (attribute attribute: (identifier) @callee)) @call
"#;

/// Python import patterns
const PYTHON_IMPORTS_QUERY: &str = r#"
(import_statement name: (dotted_name) @import)
(import_statement name: (aliased_import name: (dotted_name) @import))
(import_from_statement module_name: (dotted_name) @module)
(import_from_statement module_name: (relative_import) @module)
"#;

/// TypeScript/JavaScript call patterns
const TS_CALLS_QUERY: &str = r#"
; Function calls
(call_expression function: (identifier) @callee) @call

; Method calls
(call_expression function: (member_expression property: (property_identifier) @callee)) @call

; new expressions
(new_expression constructor: (identifier) @callee) @call
"#;

/// TypeScript/JavaScript import patterns
const TS_IMPORTS_QUERY: &str = r#"
(import_statement source: (string) @source)
"#;

/// Go call patterns
const GO_CALLS_QUERY: &str = r#"
; Function calls
(call_expression function: (identifier) @callee) @call

; Method calls
(call_expression function: (selector_expression field: (field_identifier) @callee)) @call
"#;

/// Go import patterns
const GO_IMPORTS_QUERY: &str = r#"
(import_declaration (import_spec path: (interpreted_string_literal) @import))
(import_declaration (import_spec_list (import_spec path: (interpreted_string_literal) @import)))
"#;

/// Java call patterns
const JAVA_CALLS_QUERY: &str = r#"
; Method calls
(method_invocation name: (identifier) @callee) @call

; Object creation
(object_creation_expression type: (type_identifier) @callee) @call
"#;

/// Java import patterns
const JAVA_IMPORTS_QUERY: &str = r#"
(import_declaration (scoped_identifier) @import)
"#;

/// C++ call patterns
const CPP_CALLS_QUERY: &str = r#"
; Function calls
(call_expression function: (identifier) @callee) @call

; Method calls
(call_expression function: (field_expression field: (field_identifier) @callee)) @call

; Qualified calls
(call_expression function: (qualified_identifier name: (identifier) @callee)) @call
"#;

/// C++ import patterns
const CPP_IMPORTS_QUERY: &str = r#"
(preproc_include path: (string_literal) @include)
(preproc_include path: (system_lib_string) @include)
"#;

/// Extracted call information before storage.
#[derive(Debug, Clone)]
pub struct ExtractedCall {
    /// Name of the called function/method.
    pub callee_name: String,
    /// Line number where the call occurs.
    pub call_line: u32,
    /// Column number where the call occurs.
    pub call_col: u32,
    /// Type of call (direct, method, qualified).
    pub call_type: CallType,
}

/// Extracted import information before storage.
#[derive(Debug, Clone)]
pub struct ExtractedImport {
    /// Module or package being imported.
    pub imported_module: String,
    /// Specific name if importing a single item.
    pub imported_name: Option<String>,
    /// Alias if present.
    pub alias: Option<String>,
    /// Line number of the import statement.
    pub import_line: u32,
    /// Whether this is a relative import.
    pub is_relative: bool,
}

/// Extracts call graphs and imports from parsed AST.
pub struct CallGraphExtractor {
    call_queries: HashMap<SupportedLanguage, Query>,
    import_queries: HashMap<SupportedLanguage, Query>,
}

impl CallGraphExtractor {
    /// Create a new extractor with pre-compiled queries.
    pub fn new() -> Self {
        let mut call_queries = HashMap::new();
        let mut import_queries = HashMap::new();

        // Pre-compile all queries
        Self::add_queries(
            &mut call_queries,
            &mut import_queries,
            SupportedLanguage::Rust,
            RUST_CALLS_QUERY,
            RUST_IMPORTS_QUERY,
        );
        Self::add_queries(
            &mut call_queries,
            &mut import_queries,
            SupportedLanguage::Python,
            PYTHON_CALLS_QUERY,
            PYTHON_IMPORTS_QUERY,
        );
        Self::add_queries(
            &mut call_queries,
            &mut import_queries,
            SupportedLanguage::TypeScript,
            TS_CALLS_QUERY,
            TS_IMPORTS_QUERY,
        );
        Self::add_queries(
            &mut call_queries,
            &mut import_queries,
            SupportedLanguage::JavaScript,
            TS_CALLS_QUERY,
            TS_IMPORTS_QUERY,
        );
        Self::add_queries(
            &mut call_queries,
            &mut import_queries,
            SupportedLanguage::Go,
            GO_CALLS_QUERY,
            GO_IMPORTS_QUERY,
        );
        Self::add_queries(
            &mut call_queries,
            &mut import_queries,
            SupportedLanguage::Java,
            JAVA_CALLS_QUERY,
            JAVA_IMPORTS_QUERY,
        );
        Self::add_queries(
            &mut call_queries,
            &mut import_queries,
            SupportedLanguage::Cpp,
            CPP_CALLS_QUERY,
            CPP_IMPORTS_QUERY,
        );

        Self {
            call_queries,
            import_queries,
        }
    }

    fn add_queries(
        call_queries: &mut HashMap<SupportedLanguage, Query>,
        import_queries: &mut HashMap<SupportedLanguage, Query>,
        lang: SupportedLanguage,
        call_pattern: &str,
        import_pattern: &str,
    ) {
        let ts_lang = lang.tree_sitter_language();

        if let Ok(query) = Query::new(&ts_lang, call_pattern) {
            call_queries.insert(lang, query);
        }

        if let Ok(query) = Query::new(&ts_lang, import_pattern) {
            import_queries.insert(lang, query);
        }
    }

    /// Extract function/method calls from a parsed tree.
    pub fn extract_calls(
        &self,
        tree: &Tree,
        source: &[u8],
        language: SupportedLanguage,
    ) -> Vec<ExtractedCall> {
        use streaming_iterator::StreamingIterator;

        let query = match self.call_queries.get(&language) {
            Some(q) => q,
            None => return Vec::new(),
        };

        let mut cursor = QueryCursor::new();
        let root = tree.root_node();
        let mut matches = cursor.matches(query, root, source);

        let mut calls = Vec::new();

        while let Some(m) = matches.next() {
            let mut callee_name: Option<String> = None;
            let mut call_node = None;

            for capture in m.captures {
                let name = query.capture_names()[capture.index as usize];
                if name == "callee" {
                    if let Ok(text) = capture.node.utf8_text(source) {
                        callee_name = Some(text.to_string());
                    }
                } else if name == "call" {
                    call_node = Some(capture.node);
                }
            }

            if let (Some(name), Some(node)) = (callee_name, call_node) {
                let call_type = self.determine_call_type(&node, language);
                let point = node.start_position();

                calls.push(ExtractedCall {
                    callee_name: name,
                    call_line: point.row as u32 + 1,
                    call_col: point.column as u32 + 1,
                    call_type,
                });
            }
        }

        calls
    }

    /// Extract import statements from a parsed tree.
    pub fn extract_imports(
        &self,
        tree: &Tree,
        source: &[u8],
        language: SupportedLanguage,
    ) -> Vec<ExtractedImport> {
        use streaming_iterator::StreamingIterator;

        let query = match self.import_queries.get(&language) {
            Some(q) => q,
            None => return Vec::new(),
        };

        let mut cursor = QueryCursor::new();
        let root = tree.root_node();
        let mut matches = cursor.matches(query, root, source);

        let mut imports = Vec::new();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                if let Ok(text) = capture.node.utf8_text(source) {
                    let point = capture.node.start_position();
                    let module = Self::clean_import_text(text, language);
                    let is_relative = Self::is_relative_import(&module, language);

                    imports.push(ExtractedImport {
                        imported_module: module,
                        imported_name: None,
                        alias: None,
                        import_line: point.row as u32 + 1,
                        is_relative,
                    });
                }
            }
        }

        // Deduplicate imports by module name and line
        imports.sort_by(|a, b| {
            a.import_line
                .cmp(&b.import_line)
                .then(a.imported_module.cmp(&b.imported_module))
        });
        imports.dedup_by(|a, b| {
            a.import_line == b.import_line && a.imported_module == b.imported_module
        });

        imports
    }

    fn determine_call_type(
        &self,
        node: &tree_sitter::Node,
        language: SupportedLanguage,
    ) -> CallType {
        // Examine the structure of the call node
        if let Some(func_node) = node.child_by_field_name("function") {
            let kind = func_node.kind();
            match language {
                SupportedLanguage::Rust => {
                    if kind == "field_expression" {
                        return CallType::Method;
                    } else if kind == "scoped_identifier" {
                        return CallType::Qualified;
                    }
                }
                SupportedLanguage::Python => {
                    if kind == "attribute" {
                        return CallType::Method;
                    }
                }
                SupportedLanguage::TypeScript | SupportedLanguage::JavaScript => {
                    if kind == "member_expression" {
                        return CallType::Method;
                    }
                }
                SupportedLanguage::Go => {
                    if kind == "selector_expression" {
                        return CallType::Method;
                    }
                }
                SupportedLanguage::Java => {
                    if kind == "field_access" {
                        return CallType::Method;
                    }
                }
                SupportedLanguage::Cpp => {
                    if kind == "field_expression" {
                        return CallType::Method;
                    } else if kind == "qualified_identifier" {
                        return CallType::Qualified;
                    }
                }
            }
        }

        // Check parent for method invocation in Java
        if language == SupportedLanguage::Java && node.kind() == "method_invocation" {
            if node.child_by_field_name("object").is_some() {
                return CallType::Method;
            }
        }

        CallType::Direct
    }

    fn clean_import_text(text: &str, language: SupportedLanguage) -> String {
        match language {
            SupportedLanguage::TypeScript
            | SupportedLanguage::JavaScript
            | SupportedLanguage::Go => {
                // Remove quotes from string literals
                text.trim_matches('"').trim_matches('\'').to_string()
            }
            SupportedLanguage::Cpp => {
                // Remove angle brackets or quotes
                text.trim_matches('"')
                    .trim_matches('\'')
                    .trim_matches('<')
                    .trim_matches('>')
                    .to_string()
            }
            _ => text.to_string(),
        }
    }

    fn is_relative_import(module: &str, language: SupportedLanguage) -> bool {
        match language {
            SupportedLanguage::Python => module.starts_with('.'),
            SupportedLanguage::TypeScript | SupportedLanguage::JavaScript => {
                module.starts_with("./") || module.starts_with("../")
            }
            _ => false,
        }
    }
}

impl Default for CallGraphExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Symbol record stub for caller identification.
/// This will be replaced with the actual SymbolRecord from the symbol extractor.
#[derive(Debug, Clone)]
pub struct SymbolRecord {
    /// Unique identifier in the database.
    pub symbol_id: i64,
    /// Name of the symbol.
    pub name: String,
    /// Starting line of the symbol.
    pub start_line: u32,
    /// Ending line of the symbol.
    pub end_line: u32,
}

/// Indexes call graphs and imports into storage.
pub struct CallGraphIndexer {
    extractor: CallGraphExtractor,
    store: Arc<StructuralStore>,
}

impl CallGraphIndexer {
    /// Create a new indexer with the given store.
    pub fn new(store: Arc<StructuralStore>) -> Self {
        Self {
            extractor: CallGraphExtractor::new(),
            store,
        }
    }

    /// Index all function calls in a file.
    pub fn index_file_calls(
        &self,
        tenant_id: &TenantId,
        file_path: &str,
        tree: &Tree,
        source: &[u8],
        language: SupportedLanguage,
        file_symbols: &[SymbolRecord],
    ) -> Result<usize, rusqlite::Error> {
        // Delete existing edges for this file (re-indexing support)
        self.store.delete_file_edges(tenant_id, file_path)?;

        // Extract all calls from the file
        let calls = self.extractor.extract_calls(tree, source, language);

        // Match calls to their containing symbols (callers)
        let mut records = Vec::new();

        for call in &calls {
            // Find the symbol that contains this call
            let caller = file_symbols
                .iter()
                .find(|s| call.call_line >= s.start_line && call.call_line <= s.end_line);

            if let Some(caller_symbol) = caller {
                records.push(CallEdgeRecord {
                    edge_id: None,
                    tenant_id: tenant_id.clone(),
                    caller_symbol_id: caller_symbol.symbol_id,
                    callee_name: call.callee_name.clone(),
                    callee_symbol_id: None, // Unresolved, linked later
                    call_file: file_path.to_string(),
                    call_line: call.call_line,
                    call_col: call.call_col,
                    call_type: call.call_type,
                });
            }
        }

        let count = records.len();
        if !records.is_empty() {
            self.store.insert_call_edges_batch(&records)?;
        }

        Ok(count)
    }

    /// Index all imports in a file.
    pub fn index_file_imports(
        &self,
        tenant_id: &TenantId,
        file_path: &str,
        tree: &Tree,
        source: &[u8],
        language: SupportedLanguage,
    ) -> Result<usize, rusqlite::Error> {
        // Delete existing imports for this file (re-indexing support)
        self.store.delete_file_imports(tenant_id, file_path)?;

        // Extract imports from AST
        let extracted = self.extractor.extract_imports(tree, source, language);

        // Convert to storage records
        let records: Vec<ImportRecord> = extracted
            .iter()
            .map(|e| ImportRecord {
                import_id: None,
                tenant_id: tenant_id.clone(),
                source_file: file_path.to_string(),
                imported_module: e.imported_module.clone(),
                imported_name: e.imported_name.clone(),
                alias: e.alias.clone(),
                import_line: e.import_line,
                is_relative: e.is_relative,
            })
            .collect();

        let count = records.len();
        if !records.is_empty() {
            self.store.insert_imports_batch(&records)?;
        }

        Ok(count)
    }

    /// Index both calls and imports for a file.
    pub fn index_file(
        &self,
        tenant_id: &TenantId,
        file_path: &str,
        tree: &Tree,
        source: &[u8],
        language: SupportedLanguage,
        file_symbols: &[SymbolRecord],
    ) -> Result<(usize, usize), rusqlite::Error> {
        let call_count =
            self.index_file_calls(tenant_id, file_path, tree, source, language, file_symbols)?;
        let import_count = self.index_file_imports(tenant_id, file_path, tree, source, language)?;

        Ok((call_count, import_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structural::parse_file;
    use std::path::PathBuf;

    #[test]
    fn test_extract_rust_direct_call() {
        let source = r#"
fn helper() {}

fn main() {
    helper();
}
"#;
        let path = PathBuf::from("test.rs");
        let result = parse_file(&path, source).unwrap();

        let extractor = CallGraphExtractor::new();
        let calls =
            extractor.extract_calls(&result.tree, source.as_bytes(), SupportedLanguage::Rust);

        assert!(!calls.is_empty());
        assert!(calls.iter().any(|c| c.callee_name == "helper"));
    }

    #[test]
    fn test_extract_rust_method_call() {
        let source = r#"
fn main() {
    let s = String::new();
    s.push_str("hello");
}
"#;
        let path = PathBuf::from("test.rs");
        let result = parse_file(&path, source).unwrap();

        let extractor = CallGraphExtractor::new();
        let calls =
            extractor.extract_calls(&result.tree, source.as_bytes(), SupportedLanguage::Rust);

        let method_call = calls.iter().find(|c| c.callee_name == "push_str");
        assert!(method_call.is_some());
        assert_eq!(method_call.unwrap().call_type, CallType::Method);
    }

    #[test]
    fn test_extract_rust_qualified_call() {
        let source = r#"
fn main() {
    let val = std::env::var("HOME");
}
"#;
        let path = PathBuf::from("test.rs");
        let result = parse_file(&path, source).unwrap();

        let extractor = CallGraphExtractor::new();
        let calls =
            extractor.extract_calls(&result.tree, source.as_bytes(), SupportedLanguage::Rust);

        let qualified_call = calls.iter().find(|c| c.callee_name == "var");
        assert!(qualified_call.is_some());
        assert_eq!(qualified_call.unwrap().call_type, CallType::Qualified);
    }

    #[test]
    fn test_extract_python_import() {
        let source = r#"
import os
import json as j
from pathlib import Path
"#;
        let path = PathBuf::from("test.py");
        let result = parse_file(&path, source).unwrap();

        let extractor = CallGraphExtractor::new();
        let imports =
            extractor.extract_imports(&result.tree, source.as_bytes(), SupportedLanguage::Python);

        assert!(imports.iter().any(|i| i.imported_module == "os"));
        assert!(imports.iter().any(|i| i.imported_module == "json"));
        assert!(imports.iter().any(|i| i.imported_module == "pathlib"));
    }

    #[test]
    fn test_extract_typescript_import() {
        let source = r#"
import { useState } from 'react';
import axios from 'axios';
import { helper } from './utils';
"#;
        let path = PathBuf::from("test.ts");
        let result = parse_file(&path, source).unwrap();

        let extractor = CallGraphExtractor::new();
        let imports = extractor.extract_imports(
            &result.tree,
            source.as_bytes(),
            SupportedLanguage::TypeScript,
        );

        assert!(imports.iter().any(|i| i.imported_module == "react"));
        assert!(imports.iter().any(|i| i.imported_module == "axios"));

        let relative = imports.iter().find(|i| i.imported_module == "./utils");
        assert!(relative.is_some());
        assert!(relative.unwrap().is_relative);
    }

    #[test]
    fn test_extract_python_relative_import() {
        let source = r#"
from .utils import helper
from ..models import User
"#;
        let path = PathBuf::from("test.py");
        let result = parse_file(&path, source).unwrap();

        let extractor = CallGraphExtractor::new();
        let imports =
            extractor.extract_imports(&result.tree, source.as_bytes(), SupportedLanguage::Python);

        assert!(imports.iter().any(|i| i.is_relative));
    }

    #[test]
    fn test_extract_go_import() {
        let source = r#"
package main

import (
    "fmt"
    "os"
)
"#;
        let path = PathBuf::from("test.go");
        let result = parse_file(&path, source).unwrap();

        let extractor = CallGraphExtractor::new();
        let imports =
            extractor.extract_imports(&result.tree, source.as_bytes(), SupportedLanguage::Go);

        assert!(imports.iter().any(|i| i.imported_module == "fmt"));
        assert!(imports.iter().any(|i| i.imported_module == "os"));
    }

    #[test]
    fn test_extract_java_call() {
        let source = r#"
public class Main {
    public static void main(String[] args) {
        System.out.println("Hello");
        helper();
    }

    static void helper() {}
}
"#;
        let path = PathBuf::from("Main.java");
        let result = parse_file(&path, source).unwrap();

        let extractor = CallGraphExtractor::new();
        let calls =
            extractor.extract_calls(&result.tree, source.as_bytes(), SupportedLanguage::Java);

        assert!(calls.iter().any(|c| c.callee_name == "println"));
        assert!(calls.iter().any(|c| c.callee_name == "helper"));
    }

    #[test]
    fn test_index_file_calls_creates_edges() {
        let source = r#"
fn helper() {}

fn main() {
    helper();
}
"#;
        let path = PathBuf::from("test.rs");
        let result = parse_file(&path, source).unwrap();

        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let indexer = CallGraphIndexer::new(store.clone());

        let tenant = TenantId::new("test").unwrap();
        let symbols = vec![SymbolRecord {
            symbol_id: 1,
            name: "main".to_string(),
            start_line: 4,
            end_line: 6,
        }];

        let count = indexer
            .index_file_calls(
                &tenant,
                "test.rs",
                &result.tree,
                source.as_bytes(),
                SupportedLanguage::Rust,
                &symbols,
            )
            .unwrap();

        assert_eq!(count, 1);

        // Verify edge was stored
        let callers = store.find_callers(&tenant, "helper").unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller_symbol_id, 1);
    }

    #[test]
    fn test_index_file_imports_creates_records() {
        let source = r#"
import os
import json
"#;
        let path = PathBuf::from("test.py");
        let result = parse_file(&path, source).unwrap();

        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let indexer = CallGraphIndexer::new(store.clone());

        let tenant = TenantId::new("test").unwrap();

        let count = indexer
            .index_file_imports(
                &tenant,
                "test.py",
                &result.tree,
                source.as_bytes(),
                SupportedLanguage::Python,
            )
            .unwrap();

        assert_eq!(count, 2);

        // Verify imports were stored
        let imports = store.find_imports_by_file(&tenant, "test.py").unwrap();
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn test_reindex_replaces_edges() {
        let source1 = r#"
fn helper() {}
fn main() {
    helper();
}
"#;
        let source2 = r#"
fn other() {}
fn main() {
    other();
}
"#;
        let path = PathBuf::from("test.rs");

        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let indexer = CallGraphIndexer::new(store.clone());

        let tenant = TenantId::new("test").unwrap();
        let symbols = vec![SymbolRecord {
            symbol_id: 1,
            name: "main".to_string(),
            start_line: 3,
            end_line: 5,
        }];

        // First index
        let result1 = parse_file(&path, source1).unwrap();
        indexer
            .index_file_calls(
                &tenant,
                "test.rs",
                &result1.tree,
                source1.as_bytes(),
                SupportedLanguage::Rust,
                &symbols,
            )
            .unwrap();

        let callers = store.find_callers(&tenant, "helper").unwrap();
        assert_eq!(callers.len(), 1);

        // Re-index with different content
        let result2 = parse_file(&path, source2).unwrap();
        indexer
            .index_file_calls(
                &tenant,
                "test.rs",
                &result2.tree,
                source2.as_bytes(),
                SupportedLanguage::Rust,
                &symbols,
            )
            .unwrap();

        // Old edges should be gone
        let old_callers = store.find_callers(&tenant, "helper").unwrap();
        assert_eq!(old_callers.len(), 0);

        // New edges should exist
        let new_callers = store.find_callers(&tenant, "other").unwrap();
        assert_eq!(new_callers.len(), 1);
    }

    #[test]
    fn test_caller_callee_relationship() {
        let source = r#"
fn a() {}
fn b() {}

fn caller1() {
    a();
    b();
}

fn caller2() {
    a();
}
"#;
        let path = PathBuf::from("test.rs");
        let result = parse_file(&path, source).unwrap();

        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let indexer = CallGraphIndexer::new(store.clone());

        let tenant = TenantId::new("test").unwrap();
        let symbols = vec![
            SymbolRecord {
                symbol_id: 1,
                name: "caller1".to_string(),
                start_line: 5,
                end_line: 8,
            },
            SymbolRecord {
                symbol_id: 2,
                name: "caller2".to_string(),
                start_line: 10,
                end_line: 12,
            },
        ];

        indexer
            .index_file_calls(
                &tenant,
                "test.rs",
                &result.tree,
                source.as_bytes(),
                SupportedLanguage::Rust,
                &symbols,
            )
            .unwrap();

        // 'a' is called by both caller1 and caller2
        let a_callers = store.find_callers(&tenant, "a").unwrap();
        assert_eq!(a_callers.len(), 2);

        // 'b' is called only by caller1
        let b_callers = store.find_callers(&tenant, "b").unwrap();
        assert_eq!(b_callers.len(), 1);
        assert_eq!(b_callers[0].caller_symbol_id, 1);

        // caller1 calls both a and b
        let caller1_callees = store.find_callees(1).unwrap();
        assert_eq!(caller1_callees.len(), 2);

        // caller2 calls only a
        let caller2_callees = store.find_callees(2).unwrap();
        assert_eq!(caller2_callees.len(), 1);
    }
}
