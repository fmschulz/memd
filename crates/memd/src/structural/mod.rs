//! Structural indexing module for AST-based code analysis.
//!
//! This module provides tree-sitter integration for parsing source code
//! files and extracting structural information like symbols and call graphs.

mod call_graph;
mod parser;
mod storage;

pub use call_graph::{
    CallGraphExtractor, CallGraphIndexer, ExtractedCall, ExtractedImport,
    SymbolRecord as CallGraphSymbolRecord,
};
pub use parser::{
    detect_language, parse_file, LanguageSupport, ParseError, ParseResult, SupportedLanguage,
};
pub use storage::{
    CallEdgeRecord, CallType, ImportRecord, StackFrameRecord, StackTraceRecord, StructuralStore,
    SymbolKind, SymbolRecord, TimeRange, ToolTraceRecord,
};
