//! Multi-language tree-sitter parser wrapper.
//!
//! Provides language-agnostic parsing for 7 supported languages:
//! Rust, Python, TypeScript, JavaScript, Go, Java, and C++.

use std::path::Path;
use tree_sitter::{Language, Parser, Tree};

/// Error type for parsing operations.
#[derive(Debug, Clone)]
pub enum ParseError {
    /// File extension not recognized or unsupported.
    UnsupportedLanguage(String),
    /// Tree-sitter failed to parse the source.
    ParseFailed,
    /// Failed to set language on parser.
    LanguageSetFailed,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::UnsupportedLanguage(ext) => {
                write!(f, "unsupported language for extension: {}", ext)
            }
            ParseError::ParseFailed => write!(f, "tree-sitter failed to parse source"),
            ParseError::LanguageSetFailed => write!(f, "failed to set language on parser"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Supported programming languages for structural parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportedLanguage {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Java,
    Cpp,
}

impl SupportedLanguage {
    /// Get the tree-sitter Language for this language type.
    pub fn tree_sitter_language(&self) -> Language {
        match self {
            SupportedLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
            SupportedLanguage::Python => tree_sitter_python::LANGUAGE.into(),
            SupportedLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            SupportedLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            SupportedLanguage::Go => tree_sitter_go::LANGUAGE.into(),
            SupportedLanguage::Java => tree_sitter_java::LANGUAGE.into(),
            SupportedLanguage::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        }
    }

    /// Get language name for display.
    pub fn name(&self) -> &'static str {
        match self {
            SupportedLanguage::Rust => "Rust",
            SupportedLanguage::Python => "Python",
            SupportedLanguage::TypeScript => "TypeScript",
            SupportedLanguage::JavaScript => "JavaScript",
            SupportedLanguage::Go => "Go",
            SupportedLanguage::Java => "Java",
            SupportedLanguage::Cpp => "C++",
        }
    }

    /// Get common file extensions for this language.
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            SupportedLanguage::Rust => &["rs"],
            SupportedLanguage::Python => &["py"],
            SupportedLanguage::TypeScript => &["ts", "tsx"],
            SupportedLanguage::JavaScript => &["js", "jsx"],
            SupportedLanguage::Go => &["go"],
            SupportedLanguage::Java => &["java"],
            SupportedLanguage::Cpp => &["cpp", "cc", "cxx", "c", "h", "hpp"],
        }
    }
}

/// Detect language from file path extension.
///
/// Returns None if the extension is not recognized or unsupported.
pub fn detect_language(path: &Path) -> Option<SupportedLanguage> {
    let ext = path.extension()?.to_str()?;
    extension_to_language(ext)
}

/// Map file extension to supported language.
fn extension_to_language(ext: &str) -> Option<SupportedLanguage> {
    match ext.to_lowercase().as_str() {
        "rs" => Some(SupportedLanguage::Rust),
        "py" => Some(SupportedLanguage::Python),
        "ts" | "tsx" => Some(SupportedLanguage::TypeScript),
        "js" | "jsx" => Some(SupportedLanguage::JavaScript),
        "go" => Some(SupportedLanguage::Go),
        "java" => Some(SupportedLanguage::Java),
        "cpp" | "cc" | "cxx" | "c" | "h" | "hpp" => Some(SupportedLanguage::Cpp),
        _ => None,
    }
}

/// Result of parsing a source file.
#[derive(Debug)]
pub struct ParseResult {
    /// The parsed AST tree.
    pub tree: Tree,
    /// The language that was detected/used.
    pub language: SupportedLanguage,
    /// Size of the source in bytes.
    pub source_bytes: usize,
}

impl ParseResult {
    /// Get the root node of the parse tree.
    pub fn root_node(&self) -> tree_sitter::Node<'_> {
        self.tree.root_node()
    }

    /// Check if the parse tree has any errors.
    pub fn has_errors(&self) -> bool {
        self.tree.root_node().has_error()
    }
}

/// Language-aware tree-sitter parser wrapper.
///
/// Creates a parser configured for a specific language. Note that tree-sitter
/// Parser is not Send/Sync, so this wrapper is also not thread-safe.
/// Create a fresh LanguageSupport per thread or per parse operation.
pub struct LanguageSupport {
    parser: Parser,
    language: SupportedLanguage,
}

impl LanguageSupport {
    /// Create a LanguageSupport for the given extension.
    ///
    /// Returns None if the extension is not recognized.
    pub fn for_extension(ext: &str) -> Option<Self> {
        let lang = extension_to_language(ext)?;
        Some(Self::for_language(lang))
    }

    /// Create a LanguageSupport for the given language.
    pub fn for_language(lang: SupportedLanguage) -> Self {
        let mut parser = Parser::new();
        // This should not fail for our known languages
        parser
            .set_language(&lang.tree_sitter_language())
            .expect("failed to set language on parser");

        Self {
            parser,
            language: lang,
        }
    }

    /// Get the language this parser is configured for.
    pub fn language(&self) -> SupportedLanguage {
        self.language
    }

    /// Parse source code and return the AST tree.
    ///
    /// Returns None if parsing fails (should be rare for valid source).
    pub fn parse(&mut self, source: &str) -> Option<Tree> {
        self.parser.parse(source, None)
    }

    /// Parse source code with incremental parsing from old tree.
    ///
    /// This is more efficient when making small edits to previously parsed code.
    /// Pass the old tree from a previous parse to enable incremental parsing.
    pub fn parse_with_old_tree(&mut self, source: &str, old_tree: &Tree) -> Option<Tree> {
        self.parser.parse(source, Some(old_tree))
    }
}

/// Parse a file given its path and content.
///
/// Detects language from file extension, creates appropriate parser,
/// and returns the parse result with AST tree and metadata.
pub fn parse_file(path: &Path, content: &str) -> Result<ParseResult, ParseError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let language = extension_to_language(ext)
        .ok_or_else(|| ParseError::UnsupportedLanguage(ext.to_string()))?;

    let mut support = LanguageSupport::for_language(language);

    let tree = support.parse(content).ok_or(ParseError::ParseFailed)?;

    Ok(ParseResult {
        tree,
        language,
        source_bytes: content.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_parse_rust_function() {
        let source = "fn main() {}";
        let path = PathBuf::from("test.rs");

        let result = parse_file(&path, source).expect("parsing should succeed");

        assert_eq!(result.language, SupportedLanguage::Rust);
        assert_eq!(result.source_bytes, source.len());
        assert!(!result.has_errors());

        let root = result.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(root.child_count() > 0);
    }

    #[test]
    fn test_parse_python_function() {
        let source = "def main():\n    pass";
        let path = PathBuf::from("test.py");

        let result = parse_file(&path, source).expect("parsing should succeed");

        assert_eq!(result.language, SupportedLanguage::Python);
        assert!(!result.has_errors());

        let root = result.root_node();
        assert_eq!(root.kind(), "module");
    }

    #[test]
    fn test_parse_typescript_function() {
        let source = "function main(): void {}";
        let path = PathBuf::from("test.ts");

        let result = parse_file(&path, source).expect("parsing should succeed");

        assert_eq!(result.language, SupportedLanguage::TypeScript);
        assert!(!result.has_errors());

        let root = result.root_node();
        assert_eq!(root.kind(), "program");
    }

    #[test]
    fn test_parse_javascript_function() {
        let source = "function main() { return 42; }";
        let path = PathBuf::from("test.js");

        let result = parse_file(&path, source).expect("parsing should succeed");

        assert_eq!(result.language, SupportedLanguage::JavaScript);
        assert!(!result.has_errors());
    }

    #[test]
    fn test_parse_go_function() {
        let source = "package main\n\nfunc main() {}";
        let path = PathBuf::from("test.go");

        let result = parse_file(&path, source).expect("parsing should succeed");

        assert_eq!(result.language, SupportedLanguage::Go);
        assert!(!result.has_errors());

        let root = result.root_node();
        assert_eq!(root.kind(), "source_file");
    }

    #[test]
    fn test_parse_java_class() {
        let source = "public class Main { public static void main(String[] args) {} }";
        let path = PathBuf::from("Main.java");

        let result = parse_file(&path, source).expect("parsing should succeed");

        assert_eq!(result.language, SupportedLanguage::Java);
        assert!(!result.has_errors());

        let root = result.root_node();
        assert_eq!(root.kind(), "program");
    }

    #[test]
    fn test_parse_cpp_function() {
        let source = "int main() { return 0; }";
        let path = PathBuf::from("test.cpp");

        let result = parse_file(&path, source).expect("parsing should succeed");

        assert_eq!(result.language, SupportedLanguage::Cpp);
        assert!(!result.has_errors());

        let root = result.root_node();
        assert_eq!(root.kind(), "translation_unit");
    }

    #[test]
    fn test_detect_language_extensions() {
        // Rust
        assert_eq!(
            detect_language(Path::new("foo.rs")),
            Some(SupportedLanguage::Rust)
        );

        // Python
        assert_eq!(
            detect_language(Path::new("foo.py")),
            Some(SupportedLanguage::Python)
        );

        // TypeScript
        assert_eq!(
            detect_language(Path::new("foo.ts")),
            Some(SupportedLanguage::TypeScript)
        );
        assert_eq!(
            detect_language(Path::new("foo.tsx")),
            Some(SupportedLanguage::TypeScript)
        );

        // JavaScript
        assert_eq!(
            detect_language(Path::new("foo.js")),
            Some(SupportedLanguage::JavaScript)
        );
        assert_eq!(
            detect_language(Path::new("foo.jsx")),
            Some(SupportedLanguage::JavaScript)
        );

        // Go
        assert_eq!(
            detect_language(Path::new("foo.go")),
            Some(SupportedLanguage::Go)
        );

        // Java
        assert_eq!(
            detect_language(Path::new("Foo.java")),
            Some(SupportedLanguage::Java)
        );

        // C++
        assert_eq!(
            detect_language(Path::new("foo.cpp")),
            Some(SupportedLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.cc")),
            Some(SupportedLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.cxx")),
            Some(SupportedLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.c")),
            Some(SupportedLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.h")),
            Some(SupportedLanguage::Cpp)
        );
        assert_eq!(
            detect_language(Path::new("foo.hpp")),
            Some(SupportedLanguage::Cpp)
        );
    }

    #[test]
    fn test_unsupported_extension_returns_none() {
        assert_eq!(detect_language(Path::new("foo.txt")), None);
        assert_eq!(detect_language(Path::new("foo.md")), None);
        assert_eq!(detect_language(Path::new("foo.json")), None);
        assert_eq!(detect_language(Path::new("Makefile")), None);
    }

    #[test]
    fn test_unsupported_extension_parse_error() {
        let result = parse_file(Path::new("foo.txt"), "some text");
        assert!(result.is_err());

        if let Err(ParseError::UnsupportedLanguage(ext)) = result {
            assert_eq!(ext, "txt");
        } else {
            panic!("expected UnsupportedLanguage error");
        }
    }

    #[test]
    fn test_language_support_direct_usage() {
        let mut support = LanguageSupport::for_extension("rs").expect("rs should be supported");
        assert_eq!(support.language(), SupportedLanguage::Rust);

        let tree = support.parse("fn foo() {}").expect("should parse");
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn test_incremental_parsing() {
        let mut support = LanguageSupport::for_language(SupportedLanguage::Rust);

        // First parse
        let tree1 = support.parse("fn foo() {}").expect("should parse");

        // Incremental parse with same content (simulates edit)
        let tree2 = support
            .parse_with_old_tree("fn foo() { let x = 1; }", &tree1)
            .expect("should parse");

        assert_eq!(tree2.root_node().kind(), "source_file");
    }

    #[test]
    fn test_supported_language_name() {
        assert_eq!(SupportedLanguage::Rust.name(), "Rust");
        assert_eq!(SupportedLanguage::Python.name(), "Python");
        assert_eq!(SupportedLanguage::TypeScript.name(), "TypeScript");
        assert_eq!(SupportedLanguage::JavaScript.name(), "JavaScript");
        assert_eq!(SupportedLanguage::Go.name(), "Go");
        assert_eq!(SupportedLanguage::Java.name(), "Java");
        assert_eq!(SupportedLanguage::Cpp.name(), "C++");
    }

    #[test]
    fn test_supported_language_extensions() {
        assert_eq!(SupportedLanguage::Rust.extensions(), &["rs"]);
        assert_eq!(SupportedLanguage::TypeScript.extensions(), &["ts", "tsx"]);
        assert_eq!(
            SupportedLanguage::Cpp.extensions(),
            &["cpp", "cc", "cxx", "c", "h", "hpp"]
        );
    }

    #[test]
    fn test_parse_with_syntax_errors() {
        // Intentionally malformed Rust code
        let source = "fn main( { }"; // missing closing paren
        let path = PathBuf::from("test.rs");

        let result = parse_file(&path, source).expect("parsing should succeed");

        // Tree-sitter still produces a tree, but with error nodes
        assert!(result.has_errors());
    }
}
