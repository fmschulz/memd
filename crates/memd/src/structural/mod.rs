//! Structural indexing module for AST-based code analysis.
//!
//! This module provides tree-sitter integration for parsing source code
//! files and extracting structural information like symbols and call graphs.

mod parser;
mod storage;

pub use parser::{
    detect_language, parse_file, LanguageSupport, ParseError, ParseResult, SupportedLanguage,
};
pub use storage::{CallEdgeRecord, CallType, ImportRecord, StructuralStore};
