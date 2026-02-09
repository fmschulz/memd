//! Symbol extraction from tree-sitter AST.
//!
//! Extracts functions, classes, methods, and variables from parsed source code
//! using tree-sitter queries for each supported language.

use std::collections::HashMap;
use std::sync::Arc;

use tree_sitter::{Query, QueryCursor, Tree};

use super::parser::SupportedLanguage;
use super::storage::{StructuralStore, SymbolKind, SymbolRecord};
use crate::error::Result;
use crate::types::TenantId;

// --- Tree-sitter Query Patterns for Symbol Extraction ---

/// Rust symbol query patterns
const RUST_SYMBOLS_QUERY: &str = r#"
; Top-level functions
(function_item name: (identifier) @name) @definition.function

; Methods in impl blocks
(impl_item body: (declaration_list (function_item name: (identifier) @name) @definition.method))

; Structs
(struct_item name: (type_identifier) @name) @definition.class

; Enums
(enum_item name: (type_identifier) @name) @definition.enum

; Type aliases
(type_item name: (type_identifier) @name) @definition.type

; Constants
(const_item name: (identifier) @name) @definition.constant

; Statics
(static_item name: (identifier) @name) @definition.constant

; Modules
(mod_item name: (identifier) @name) @definition.module

; Traits (as interfaces)
(trait_item name: (type_identifier) @name) @definition.interface
"#;

/// Python symbol query patterns
const PYTHON_SYMBOLS_QUERY: &str = r#"
; Function definitions
(function_definition name: (identifier) @name) @definition.function

; Class definitions
(class_definition name: (identifier) @name) @definition.class

; Top-level assignments (module-level variables)
(expression_statement (assignment left: (identifier) @name)) @definition.variable
"#;

/// TypeScript symbol query patterns
const TS_SYMBOLS_QUERY: &str = r#"
; Function declarations
(function_declaration name: (identifier) @name) @definition.function

; Class declarations
(class_declaration name: (type_identifier) @name) @definition.class

; Interface declarations
(interface_declaration name: (type_identifier) @name) @definition.interface

; Type aliases
(type_alias_declaration name: (type_identifier) @name) @definition.type

; Methods
(method_definition name: (property_identifier) @name) @definition.method

; Variable declarations (const, let)
(lexical_declaration (variable_declarator name: (identifier) @name)) @definition.variable

; Var declarations
(variable_declaration (variable_declarator name: (identifier) @name)) @definition.variable

; Enum declarations
(enum_declaration name: (identifier) @name) @definition.enum
"#;

/// JavaScript symbol query patterns (similar to TypeScript but simpler)
const JS_SYMBOLS_QUERY: &str = r#"
; Function declarations
(function_declaration name: (identifier) @name) @definition.function

; Class declarations
(class_declaration name: (identifier) @name) @definition.class

; Methods
(method_definition name: (property_identifier) @name) @definition.method

; Variable declarations
(lexical_declaration (variable_declarator name: (identifier) @name)) @definition.variable

; Var declarations
(variable_declaration (variable_declarator name: (identifier) @name)) @definition.variable
"#;

/// Go symbol query patterns
const GO_SYMBOLS_QUERY: &str = r#"
; Function declarations
(function_declaration name: (identifier) @name) @definition.function

; Method declarations
(method_declaration name: (field_identifier) @name) @definition.method

; Type declarations (struct, interface, etc.)
(type_declaration (type_spec name: (type_identifier) @name)) @definition.type

; Const declarations
(const_declaration (const_spec name: (identifier) @name)) @definition.constant

; Var declarations
(var_declaration (var_spec name: (identifier) @name)) @definition.variable
"#;

/// Java symbol query patterns
const JAVA_SYMBOLS_QUERY: &str = r#"
; Class declarations
(class_declaration name: (identifier) @name) @definition.class

; Interface declarations
(interface_declaration name: (identifier) @name) @definition.interface

; Enum declarations
(enum_declaration name: (identifier) @name) @definition.enum

; Method declarations
(method_declaration name: (identifier) @name) @definition.method

; Constructor declarations
(constructor_declaration name: (identifier) @name) @definition.method

; Field declarations
(field_declaration (variable_declarator name: (identifier) @name)) @definition.variable
"#;

/// C++ symbol query patterns
const CPP_SYMBOLS_QUERY: &str = r#"
; Function definitions
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function

; Class definitions
(class_specifier name: (type_identifier) @name) @definition.class

; Struct definitions
(struct_specifier name: (type_identifier) @name) @definition.class

; Enum definitions
(enum_specifier name: (type_identifier) @name) @definition.enum

; Type aliases (typedef)
(type_definition declarator: (type_identifier) @name) @definition.type

; Variable declarations at namespace/class scope
(declaration declarator: (init_declarator declarator: (identifier) @name)) @definition.variable
"#;

/// An extracted symbol from the AST before storage.
#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    /// Symbol name.
    pub name: String,
    /// Symbol kind.
    pub kind: SymbolKind,
    /// Start line (0-indexed).
    pub line_start: u32,
    /// End line (0-indexed).
    pub line_end: u32,
    /// Start column (0-indexed).
    pub col_start: u32,
    /// End column (0-indexed).
    pub col_end: u32,
    /// Function signature or type annotation.
    pub signature: Option<String>,
    /// Extracted documentation.
    pub docstring: Option<String>,
    /// Visibility (public, private, etc.).
    pub visibility: Option<String>,
    /// Parent symbol name for nesting.
    pub parent_name: Option<String>,
}

/// Extracts symbols from tree-sitter AST.
pub struct SymbolExtractor {
    /// Pre-compiled queries for each language.
    queries: HashMap<SupportedLanguage, Query>,
}

impl SymbolExtractor {
    /// Create a new symbol extractor with pre-compiled queries.
    pub fn new() -> Self {
        let mut queries = HashMap::new();

        // Compile queries for each language
        if let Ok(q) = Query::new(
            &SupportedLanguage::Rust.tree_sitter_language(),
            RUST_SYMBOLS_QUERY,
        ) {
            queries.insert(SupportedLanguage::Rust, q);
        }
        if let Ok(q) = Query::new(
            &SupportedLanguage::Python.tree_sitter_language(),
            PYTHON_SYMBOLS_QUERY,
        ) {
            queries.insert(SupportedLanguage::Python, q);
        }
        if let Ok(q) = Query::new(
            &SupportedLanguage::TypeScript.tree_sitter_language(),
            TS_SYMBOLS_QUERY,
        ) {
            queries.insert(SupportedLanguage::TypeScript, q);
        }
        if let Ok(q) = Query::new(
            &SupportedLanguage::JavaScript.tree_sitter_language(),
            JS_SYMBOLS_QUERY,
        ) {
            queries.insert(SupportedLanguage::JavaScript, q);
        }
        if let Ok(q) = Query::new(
            &SupportedLanguage::Go.tree_sitter_language(),
            GO_SYMBOLS_QUERY,
        ) {
            queries.insert(SupportedLanguage::Go, q);
        }
        if let Ok(q) = Query::new(
            &SupportedLanguage::Java.tree_sitter_language(),
            JAVA_SYMBOLS_QUERY,
        ) {
            queries.insert(SupportedLanguage::Java, q);
        }
        if let Ok(q) = Query::new(
            &SupportedLanguage::Cpp.tree_sitter_language(),
            CPP_SYMBOLS_QUERY,
        ) {
            queries.insert(SupportedLanguage::Cpp, q);
        }

        Self { queries }
    }

    /// Extract symbols from a parsed AST.
    pub fn extract(
        &self,
        tree: &Tree,
        source: &[u8],
        language: SupportedLanguage,
        _file_path: &str,
    ) -> Vec<ExtractedSymbol> {
        use streaming_iterator::StreamingIterator;

        let query = match self.queries.get(&language) {
            Some(q) => q,
            None => return Vec::new(),
        };

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source);

        let mut symbols = Vec::new();

        while let Some(m) = matches.next() {
            let mut name: Option<String> = None;
            let mut kind = SymbolKind::Function;
            let mut def_node = None;

            for capture in m.captures {
                let capture_name = query.capture_names()[capture.index as usize];
                let node = capture.node;

                if capture_name == "name" {
                    if let Ok(text) = node.utf8_text(source) {
                        name = Some(text.to_string());
                    }
                } else if capture_name.starts_with("definition.") {
                    def_node = Some(node);
                    kind = match capture_name {
                        "definition.function" => SymbolKind::Function,
                        "definition.method" => SymbolKind::Method,
                        "definition.class" => SymbolKind::Class,
                        "definition.interface" => SymbolKind::Interface,
                        "definition.type" => SymbolKind::Type,
                        "definition.enum" => SymbolKind::Enum,
                        "definition.constant" => SymbolKind::Constant,
                        "definition.variable" => SymbolKind::Variable,
                        "definition.module" => SymbolKind::Module,
                        _ => SymbolKind::Function,
                    };
                }
            }

            if let (Some(name), Some(node)) = (name, def_node) {
                // Extract docstring from preceding comments
                let docstring = extract_docstring(node, source, language);

                // Extract visibility
                let visibility = extract_visibility(node, source, language);

                // Extract signature (function parameters and return type)
                let signature = extract_signature(node, source, language, kind);

                // Find parent symbol (for nested methods/classes)
                let parent_name = find_parent_symbol(node, source, language);

                symbols.push(ExtractedSymbol {
                    name,
                    kind,
                    line_start: node.start_position().row as u32,
                    line_end: node.end_position().row as u32,
                    col_start: node.start_position().column as u32,
                    col_end: node.end_position().column as u32,
                    signature,
                    docstring,
                    visibility,
                    parent_name,
                });
            }
        }

        symbols
    }
}

impl Default for SymbolExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract docstring from comments preceding a node.
fn extract_docstring(
    node: tree_sitter::Node,
    source: &[u8],
    language: SupportedLanguage,
) -> Option<String> {
    // Look for preceding sibling that is a comment
    let mut prev = node.prev_sibling();
    let mut doc_lines = Vec::new();

    while let Some(prev_node) = prev {
        let node_kind = prev_node.kind();
        let is_doc_comment = match language {
            SupportedLanguage::Rust => node_kind == "line_comment" || node_kind == "block_comment",
            SupportedLanguage::Python => {
                // Python docstrings are inside the function body
                false
            }
            SupportedLanguage::TypeScript | SupportedLanguage::JavaScript => node_kind == "comment",
            SupportedLanguage::Go => node_kind == "comment",
            SupportedLanguage::Java => node_kind == "line_comment" || node_kind == "block_comment",
            SupportedLanguage::Cpp => node_kind == "comment",
        };

        if is_doc_comment {
            if let Ok(text) = prev_node.utf8_text(source) {
                let cleaned = clean_comment(text, language);
                doc_lines.insert(0, cleaned);
            }
            prev = prev_node.prev_sibling();
        } else if prev_node.kind().contains("whitespace") || prev_node.kind() == "\n" {
            prev = prev_node.prev_sibling();
        } else {
            break;
        }
    }

    // For Python, check for docstring in first child
    if language == SupportedLanguage::Python && doc_lines.is_empty() {
        if let Some(body) = node.child_by_field_name("body") {
            if let Some(first_stmt) = body.child(0) {
                if first_stmt.kind() == "expression_statement" {
                    if let Some(expr) = first_stmt.child(0) {
                        if expr.kind() == "string" {
                            if let Ok(text) = expr.utf8_text(source) {
                                let cleaned = text
                                    .trim()
                                    .trim_start_matches("\"\"\"")
                                    .trim_end_matches("\"\"\"")
                                    .trim_start_matches("'''")
                                    .trim_end_matches("'''")
                                    .trim()
                                    .to_string();
                                if !cleaned.is_empty() {
                                    return Some(cleaned);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join("\n"))
    }
}

/// Clean a comment string, removing comment markers.
fn clean_comment(text: &str, language: SupportedLanguage) -> String {
    let text = text.trim();

    match language {
        SupportedLanguage::Rust => {
            if text.starts_with("///") {
                text.strip_prefix("///").unwrap_or(text).trim().to_string()
            } else if text.starts_with("//!") {
                text.strip_prefix("//!").unwrap_or(text).trim().to_string()
            } else if text.starts_with("//") {
                text.strip_prefix("//").unwrap_or(text).trim().to_string()
            } else if text.starts_with("/*") && text.ends_with("*/") {
                text.strip_prefix("/*")
                    .and_then(|s| s.strip_suffix("*/"))
                    .unwrap_or(text)
                    .trim()
                    .to_string()
            } else {
                text.to_string()
            }
        }
        SupportedLanguage::Python => text.to_string(),
        SupportedLanguage::TypeScript
        | SupportedLanguage::JavaScript
        | SupportedLanguage::Go
        | SupportedLanguage::Java
        | SupportedLanguage::Cpp => {
            if text.starts_with("//") {
                text.strip_prefix("//").unwrap_or(text).trim().to_string()
            } else if text.starts_with("/*") && text.ends_with("*/") {
                text.strip_prefix("/*")
                    .and_then(|s| s.strip_suffix("*/"))
                    .unwrap_or(text)
                    .trim()
                    .lines()
                    .map(|l| l.trim().trim_start_matches('*').trim())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                text.to_string()
            }
        }
    }
}

/// Extract visibility information from a node.
fn extract_visibility(
    node: tree_sitter::Node,
    source: &[u8],
    language: SupportedLanguage,
) -> Option<String> {
    match language {
        SupportedLanguage::Rust => {
            // Look for visibility_modifier child
            if let Some(vis) = node.child_by_field_name("visibility") {
                vis.utf8_text(source).ok().map(|s| s.to_string())
            } else {
                // Check first child for pub keyword
                if let Some(first) = node.child(0) {
                    if first.kind() == "visibility_modifier" {
                        return first.utf8_text(source).ok().map(|s| s.to_string());
                    }
                }
                None
            }
        }
        SupportedLanguage::Python => {
            // Python: underscore prefix indicates private
            if let Ok(text) = node.utf8_text(source) {
                if text.contains("def _") || text.contains("class _") {
                    return Some("private".to_string());
                }
            }
            Some("public".to_string())
        }
        SupportedLanguage::TypeScript | SupportedLanguage::JavaScript => {
            // Look for modifiers
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                let kind = child.kind();
                if kind == "public" || kind == "private" || kind == "protected" {
                    return Some(kind.to_string());
                }
                if kind == "accessibility_modifier" {
                    return child.utf8_text(source).ok().map(|s| s.to_string());
                }
            }
            None
        }
        SupportedLanguage::Java => {
            // Look for modifiers
            if let Some(modifiers) = node.child_by_field_name("modifiers") {
                let mut cursor = modifiers.walk();
                for child in modifiers.children(&mut cursor) {
                    let kind = child.kind();
                    if kind == "public" || kind == "private" || kind == "protected" {
                        return Some(kind.to_string());
                    }
                }
            }
            None
        }
        SupportedLanguage::Go => {
            // Go: exported if name starts with uppercase
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    if name
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase())
                        .unwrap_or(false)
                    {
                        return Some("public".to_string());
                    } else {
                        return Some("private".to_string());
                    }
                }
            }
            None
        }
        SupportedLanguage::Cpp => {
            // C++: access specifiers at class level
            // This is a simplification - would need to track access specifier state
            None
        }
    }
}

/// Extract function/method signature.
fn extract_signature(
    node: tree_sitter::Node,
    source: &[u8],
    language: SupportedLanguage,
    kind: SymbolKind,
) -> Option<String> {
    // Only extract signatures for functions/methods
    if kind != SymbolKind::Function && kind != SymbolKind::Method {
        return None;
    }

    match language {
        SupportedLanguage::Rust => {
            // Look for parameters and return_type children
            let params = node.child_by_field_name("parameters");
            let ret = node.child_by_field_name("return_type");

            let mut sig = String::new();
            if let Some(params_node) = params {
                if let Ok(text) = params_node.utf8_text(source) {
                    sig.push_str(text);
                }
            }
            if let Some(ret_node) = ret {
                if let Ok(text) = ret_node.utf8_text(source) {
                    sig.push_str(" -> ");
                    sig.push_str(text);
                }
            }

            if sig.is_empty() {
                None
            } else {
                Some(sig)
            }
        }
        SupportedLanguage::Python => {
            // Look for parameters
            if let Some(params) = node.child_by_field_name("parameters") {
                params.utf8_text(source).ok().map(|s| s.to_string())
            } else {
                None
            }
        }
        SupportedLanguage::TypeScript | SupportedLanguage::JavaScript => {
            // Look for formal_parameters and type_annotation
            let params = node.child_by_field_name("parameters");
            let ret = node.child_by_field_name("return_type");

            let mut sig = String::new();
            if let Some(params_node) = params {
                if let Ok(text) = params_node.utf8_text(source) {
                    sig.push_str(text);
                }
            }
            if let Some(ret_node) = ret {
                if let Ok(text) = ret_node.utf8_text(source) {
                    sig.push_str(": ");
                    sig.push_str(text);
                }
            }

            if sig.is_empty() {
                None
            } else {
                Some(sig)
            }
        }
        SupportedLanguage::Go => {
            // Look for parameters and result
            if let Some(params) = node.child_by_field_name("parameters") {
                let mut sig = params.utf8_text(source).ok()?.to_string();
                if let Some(result) = node.child_by_field_name("result") {
                    if let Ok(text) = result.utf8_text(source) {
                        sig.push(' ');
                        sig.push_str(text);
                    }
                }
                Some(sig)
            } else {
                None
            }
        }
        SupportedLanguage::Java => {
            // Look for formal_parameters
            if let Some(params) = node.child_by_field_name("parameters") {
                params.utf8_text(source).ok().map(|s| s.to_string())
            } else {
                None
            }
        }
        SupportedLanguage::Cpp => {
            // C++ is complex - simplify by looking for declarator
            None
        }
    }
}

/// Find parent symbol name for nested definitions.
fn find_parent_symbol(
    node: tree_sitter::Node,
    source: &[u8],
    language: SupportedLanguage,
) -> Option<String> {
    let mut parent = node.parent();

    while let Some(p) = parent {
        let kind = p.kind();

        let is_container = match language {
            SupportedLanguage::Rust => {
                kind == "impl_item" || kind == "struct_item" || kind == "mod_item"
            }
            SupportedLanguage::Python => kind == "class_definition",
            SupportedLanguage::TypeScript | SupportedLanguage::JavaScript => {
                kind == "class_declaration" || kind == "class"
            }
            SupportedLanguage::Go => kind == "type_declaration",
            SupportedLanguage::Java => {
                kind == "class_declaration" || kind == "interface_declaration"
            }
            SupportedLanguage::Cpp => kind == "class_specifier" || kind == "struct_specifier",
        };

        if is_container {
            // Try to extract the name of the container
            if let Some(name_node) = p.child_by_field_name("name") {
                return name_node.utf8_text(source).ok().map(|s| s.to_string());
            }
            // For Rust impl blocks, try to get the type
            if kind == "impl_item" && language == SupportedLanguage::Rust {
                if let Some(type_node) = p.child_by_field_name("type") {
                    return type_node.utf8_text(source).ok().map(|s| s.to_string());
                }
            }
        }

        parent = p.parent();
    }

    None
}

/// Indexes symbols from parsed files into storage.
pub struct SymbolIndexer {
    extractor: SymbolExtractor,
    store: Arc<StructuralStore>,
}

impl SymbolIndexer {
    /// Create a new symbol indexer with the given store.
    pub fn new(store: Arc<StructuralStore>) -> Self {
        Self {
            extractor: SymbolExtractor::new(),
            store,
        }
    }

    /// Index all symbols in a file.
    ///
    /// Deletes existing symbols for the file (re-indexing support),
    /// extracts symbols from the AST, and inserts them into storage.
    pub fn index_file(
        &self,
        tenant_id: &TenantId,
        project_id: Option<&str>,
        file_path: &str,
        tree: &Tree,
        source: &[u8],
        language: SupportedLanguage,
    ) -> Result<usize> {
        // Delete existing symbols for this file (re-indexing support)
        self.store.delete_file_symbols(tenant_id, file_path)?;

        // Extract symbols from AST
        let extracted = self.extractor.extract(tree, source, language, file_path);

        if extracted.is_empty() {
            return Ok(0);
        }

        // Convert to SymbolRecords
        let records: Vec<SymbolRecord> = extracted
            .iter()
            .map(|e| to_symbol_record(e, tenant_id, project_id, file_path, language.name()))
            .collect();

        // Insert in batch
        let ids = self.store.insert_symbols_batch(&records)?;

        Ok(ids.len())
    }
}

/// Convert an ExtractedSymbol to a SymbolRecord.
fn to_symbol_record(
    extracted: &ExtractedSymbol,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    file_path: &str,
    language: &str,
) -> SymbolRecord {
    SymbolRecord {
        symbol_id: None,
        tenant_id: tenant_id.clone(),
        project_id: project_id.map(|s| s.to_string()),
        file_path: file_path.to_string(),
        name: extracted.name.clone(),
        kind: extracted.kind,
        line_start: extracted.line_start,
        line_end: extracted.line_end,
        col_start: extracted.col_start,
        col_end: extracted.col_end,
        parent_symbol_id: None, // Parent resolution would require a second pass
        signature: extracted.signature.clone(),
        docstring: extracted.docstring.clone(),
        visibility: extracted.visibility.clone(),
        language: language.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structural::parse_file;
    use std::path::Path;

    #[test]
    fn test_extract_rust_function() {
        let source = r#"
/// This is a doc comment
pub fn process_data(input: &str) -> Result<String> {
    Ok(input.to_string())
}

fn private_helper() {
    // helper
}
"#;
        let path = Path::new("test.rs");
        let result = parse_file(path, source).expect("parsing should succeed");

        let extractor = SymbolExtractor::new();
        let symbols =
            extractor.extract(&result.tree, source.as_bytes(), result.language, "test.rs");

        assert!(!symbols.is_empty());

        // Find process_data function
        let process_data = symbols.iter().find(|s| s.name == "process_data");
        assert!(process_data.is_some());
        let pd = process_data.unwrap();
        assert_eq!(pd.kind, SymbolKind::Function);
        assert!(pd.signature.is_some());
        assert!(pd.visibility.is_some());

        // Find private_helper function
        let helper = symbols.iter().find(|s| s.name == "private_helper");
        assert!(helper.is_some());
    }

    #[test]
    fn test_extract_python_class() {
        let source = r#"
class MyClass:
    """A class docstring."""

    def __init__(self, name):
        self.name = name

    def get_name(self):
        """Returns the name."""
        return self.name
"#;
        let path = Path::new("test.py");
        let result = parse_file(path, source).expect("parsing should succeed");

        let extractor = SymbolExtractor::new();
        let symbols =
            extractor.extract(&result.tree, source.as_bytes(), result.language, "test.py");

        // Find the class
        let my_class = symbols.iter().find(|s| s.name == "MyClass");
        assert!(my_class.is_some());
        let mc = my_class.unwrap();
        assert_eq!(mc.kind, SymbolKind::Class);

        // Find methods
        let init = symbols.iter().find(|s| s.name == "__init__");
        assert!(init.is_some());
        assert_eq!(init.unwrap().kind, SymbolKind::Function);

        let get_name = symbols.iter().find(|s| s.name == "get_name");
        assert!(get_name.is_some());
    }

    #[test]
    fn test_extract_typescript_interface() {
        let source = r#"
interface User {
    name: string;
    age: number;
}

function greet(user: User): string {
    return `Hello, ${user.name}`;
}

class UserService {
    private users: User[] = [];

    public addUser(user: User): void {
        this.users.push(user);
    }
}
"#;
        let path = Path::new("test.ts");
        let result = parse_file(path, source).expect("parsing should succeed");

        let extractor = SymbolExtractor::new();
        let symbols =
            extractor.extract(&result.tree, source.as_bytes(), result.language, "test.ts");

        // Find interface
        let user_interface = symbols.iter().find(|s| s.name == "User");
        assert!(user_interface.is_some());
        assert_eq!(user_interface.unwrap().kind, SymbolKind::Interface);

        // Find function
        let greet = symbols.iter().find(|s| s.name == "greet");
        assert!(greet.is_some());
        assert_eq!(greet.unwrap().kind, SymbolKind::Function);

        // Find class
        let service = symbols.iter().find(|s| s.name == "UserService");
        assert!(service.is_some());
        assert_eq!(service.unwrap().kind, SymbolKind::Class);
    }

    #[test]
    fn test_extract_go_function() {
        let source = r#"
package main

// ProcessData processes the input data.
func ProcessData(input string) (string, error) {
    return input, nil
}

func privateHelper() {
    // helper
}

type User struct {
    Name string
    Age  int
}
"#;
        let path = Path::new("test.go");
        let result = parse_file(path, source).expect("parsing should succeed");

        let extractor = SymbolExtractor::new();
        let symbols =
            extractor.extract(&result.tree, source.as_bytes(), result.language, "test.go");

        // Find exported function
        let process_data = symbols.iter().find(|s| s.name == "ProcessData");
        assert!(process_data.is_some());
        let pd = process_data.unwrap();
        assert_eq!(pd.kind, SymbolKind::Function);
        assert_eq!(pd.visibility, Some("public".to_string()));

        // Find private function
        let helper = symbols.iter().find(|s| s.name == "privateHelper");
        assert!(helper.is_some());
        assert_eq!(helper.unwrap().visibility, Some("private".to_string()));

        // Find type
        let user = symbols.iter().find(|s| s.name == "User");
        assert!(user.is_some());
    }

    #[test]
    fn test_extract_java_class() {
        let source = r#"
public class UserService {
    private String name;

    public UserService(String name) {
        this.name = name;
    }

    public String getName() {
        return name;
    }
}
"#;
        let path = Path::new("UserService.java");
        let result = parse_file(path, source).expect("parsing should succeed");

        let extractor = SymbolExtractor::new();
        let symbols = extractor.extract(
            &result.tree,
            source.as_bytes(),
            result.language,
            "UserService.java",
        );

        // Find class
        let service = symbols.iter().find(|s| s.name == "UserService");
        assert!(service.is_some());
        assert_eq!(service.unwrap().kind, SymbolKind::Class);

        // Find constructor and method
        let constructor = symbols
            .iter()
            .find(|s| s.name == "UserService" && s.kind == SymbolKind::Method);
        // The class and constructor have the same name, which is expected
        let get_name = symbols.iter().find(|s| s.name == "getName");
        assert!(get_name.is_some());
        assert_eq!(get_name.unwrap().kind, SymbolKind::Method);
    }

    #[test]
    fn test_index_file_creates_symbols() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let indexer = SymbolIndexer::new(store.clone());

        let source = r#"
fn main() {
    println!("Hello");
}

fn helper() {}
"#;
        let path = Path::new("test.rs");
        let result = parse_file(path, source).expect("parsing should succeed");

        let tenant_id = TenantId::new("test_tenant").unwrap();
        let count = indexer
            .index_file(
                &tenant_id,
                None,
                "src/main.rs",
                &result.tree,
                source.as_bytes(),
                result.language,
            )
            .unwrap();

        assert!(count >= 2);

        // Verify symbols are in store
        let found = store
            .find_symbols_by_file(&tenant_id, "src/main.rs")
            .unwrap();
        assert!(!found.is_empty());
        assert!(found.iter().any(|s| s.name == "main"));
        assert!(found.iter().any(|s| s.name == "helper"));
    }

    #[test]
    fn test_reindex_file_replaces_symbols() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let indexer = SymbolIndexer::new(store.clone());

        let tenant_id = TenantId::new("test_tenant").unwrap();

        // First indexing
        let source1 = "fn foo() {}\nfn bar() {}";
        let path = Path::new("test.rs");
        let result1 = parse_file(path, source1).expect("parsing should succeed");
        indexer
            .index_file(
                &tenant_id,
                None,
                "src/lib.rs",
                &result1.tree,
                source1.as_bytes(),
                result1.language,
            )
            .unwrap();

        let found1 = store
            .find_symbols_by_file(&tenant_id, "src/lib.rs")
            .unwrap();
        assert_eq!(found1.len(), 2);
        assert!(found1.iter().any(|s| s.name == "foo"));
        assert!(found1.iter().any(|s| s.name == "bar"));

        // Re-indexing with different content
        let source2 = "fn baz() {}\nfn qux() {}\nfn quux() {}";
        let result2 = parse_file(path, source2).expect("parsing should succeed");
        indexer
            .index_file(
                &tenant_id,
                None,
                "src/lib.rs",
                &result2.tree,
                source2.as_bytes(),
                result2.language,
            )
            .unwrap();

        let found2 = store
            .find_symbols_by_file(&tenant_id, "src/lib.rs")
            .unwrap();
        assert_eq!(found2.len(), 3);
        assert!(found2.iter().any(|s| s.name == "baz"));
        assert!(found2.iter().any(|s| s.name == "qux"));
        assert!(found2.iter().any(|s| s.name == "quux"));
        // Old symbols should be gone
        assert!(!found2.iter().any(|s| s.name == "foo"));
        assert!(!found2.iter().any(|s| s.name == "bar"));
    }
}
