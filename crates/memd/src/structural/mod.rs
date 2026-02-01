//! Structural indexing module for AST-based code analysis.
//!
//! This module provides tree-sitter integration for parsing source code
//! files and extracting structural information like symbols and call graphs.

mod parser;

pub use parser::{
    detect_language, parse_file, LanguageSupport, ParseError, ParseResult, SupportedLanguage,
};
