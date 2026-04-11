//! Structural indexing module for AST-based code analysis.
//!
//! This module provides tree-sitter integration for parsing source code
//! files and extracting structural information like symbols and call graphs.
//! Also includes trace capture and parsing for tool calls and stack traces.
//! Query routing classifies intent and routes to appropriate search backend.

mod call_graph;
mod parser;
mod queries;
mod router;
mod storage;
mod symbols;
mod traces;

pub use call_graph::{
    CallGraphExtractor, CallGraphIndexer, ExtractedCall, ExtractedImport,
    SymbolRecord as CallGraphSymbolRecord,
};
pub use parser::{
    LanguageSupport, ParseError, ParseResult, SupportedLanguage, detect_language, parse_file,
};
pub use queries::{
    CallerInfo, ErrorResult, ErrorSummary, FrameInfo, ImportInfo, SymbolLocation,
    SymbolQueryService, ToolCallResult, TraceQueryService, format_timestamp, parse_iso_datetime,
};
pub use router::{QueryIntent, QueryRouter, RouteResult};
pub use storage::{
    CallEdgeRecord, CallType, ImportRecord, StackFrameRecord, StackTraceRecord, StructuralStore,
    SymbolKind, SymbolRecord, TimeRange, ToolTraceRecord,
};
pub use symbols::{ExtractedSymbol, SymbolExtractor, SymbolIndexer};
pub use traces::{
    DefaultTraceIndexer, ParsedFrame, StackTraceParser, TraceCapture, TraceIndexer,
    normalize_error_signature,
};
