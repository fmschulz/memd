//! Trace capture and parsing utilities.
//!
//! Provides utilities for capturing tool call traces during MCP operations
//! and parsing various stack trace formats into structured frames.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::Value;

use super::storage::{StackFrameRecord, StackTraceRecord, StructuralStore, ToolTraceRecord};
use crate::error::Result;
use crate::types::TenantId;

/// Utility for capturing tool call traces.
pub struct TraceCapture;

impl TraceCapture {
    /// Capture a tool call and create a trace record.
    pub fn capture_tool_call(
        tenant_id: TenantId,
        tool_name: &str,
        input: &Value,
        output: Option<&Value>,
        error: Option<&Value>,
        session_id: Option<&str>,
        context_tags: Vec<String>,
        duration_ms: i64,
    ) -> ToolTraceRecord {
        let timestamp_ms = Self::current_timestamp_ms();

        let mut trace = ToolTraceRecord::new(tenant_id, tool_name, timestamp_ms);
        trace.input_json = Some(serde_json::to_string(input).unwrap_or_default());
        trace.output_json = output.map(|v| serde_json::to_string(v).unwrap_or_default());
        trace.error_json = error.map(|v| serde_json::to_string(v).unwrap_or_default());
        trace.session_id = session_id.map(String::from);
        trace.context_tags = context_tags;
        trace.duration_ms = Some(duration_ms);

        trace
    }

    /// Get current Unix timestamp in milliseconds.
    pub fn current_timestamp_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// A parsed stack frame extracted from a trace string.
#[derive(Debug, Clone, Default)]
pub struct ParsedFrame {
    /// Function or method name.
    pub function_name: Option<String>,
    /// File path.
    pub file_path: Option<String>,
    /// Line number (1-indexed).
    pub line_number: Option<u32>,
    /// Column number (1-indexed).
    pub col_number: Option<u32>,
}

/// Parser for various stack trace formats.
pub struct StackTraceParser;

impl StackTraceParser {
    /// Parse a Rust backtrace format.
    ///
    /// Rust backtraces typically look like:
    /// ```text
    /// thread 'main' panicked at 'message', src/main.rs:10:5
    ///    0: function_name
    ///              at /path/to/file.rs:line:col
    ///    1: another_function
    ///              at /path/to/file.rs:line
    /// ```
    pub fn parse_rust_backtrace(trace: &str) -> (String, Vec<ParsedFrame>) {
        let mut frames = Vec::new();
        let mut error_signature = String::new();

        // Try to extract panic message from first line
        if let Some(first_line) = trace.lines().next() {
            if first_line.contains("panicked at") {
                // Extract the panic message
                if let Some(start) = first_line.find("panicked at '") {
                    if let Some(end) = first_line[start + 13..].find('\'') {
                        error_signature =
                            format!("panic: {}", &first_line[start + 13..start + 13 + end]);
                    }
                }
            } else if first_line.starts_with("Error:") || first_line.starts_with("error[") {
                error_signature = first_line.to_string();
            }
        }

        if error_signature.is_empty() {
            error_signature = "rust_panic".to_string();
        }

        // Pattern for Rust backtrace frames
        // Matches lines like "   0: function_name" and "             at /path:line:col"
        let frame_num_re = Regex::new(r"^\s*(\d+):\s*(.+)$").unwrap();
        // Use non-greedy matching for file path and anchor the end properly
        let at_re = Regex::new(r"^\s+at\s+(.+?):(\d+):(\d+)$").unwrap();
        let at_re_no_col = Regex::new(r"^\s+at\s+(.+?):(\d+)$").unwrap();

        let lines: Vec<&str> = trace.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            if let Some(caps) = frame_num_re.captures(lines[i]) {
                let function_name = caps.get(2).map(|m| m.as_str().trim().to_string());

                let mut frame = ParsedFrame {
                    function_name,
                    ..Default::default()
                };

                // Check next line for "at path:line:col"
                if i + 1 < lines.len() {
                    if let Some(at_caps) = at_re.captures(lines[i + 1]) {
                        frame.file_path = at_caps.get(1).map(|m| m.as_str().to_string());
                        frame.line_number = at_caps.get(2).and_then(|m| m.as_str().parse().ok());
                        frame.col_number = at_caps.get(3).and_then(|m| m.as_str().parse().ok());
                        i += 1;
                    } else if let Some(at_caps) = at_re_no_col.captures(lines[i + 1]) {
                        frame.file_path = at_caps.get(1).map(|m| m.as_str().to_string());
                        frame.line_number = at_caps.get(2).and_then(|m| m.as_str().parse().ok());
                        i += 1;
                    }
                }

                frames.push(frame);
            }
            i += 1;
        }

        (error_signature, frames)
    }

    /// Parse a Python traceback format.
    ///
    /// Python tracebacks look like:
    /// ```text
    /// Traceback (most recent call last):
    ///   File "script.py", line 10, in main
    ///     do_something()
    ///   File "module.py", line 5, in do_something
    ///     raise ValueError("message")
    /// ValueError: message
    /// ```
    pub fn parse_python_traceback(trace: &str) -> (String, Vec<ParsedFrame>) {
        let mut frames = Vec::new();
        let mut error_signature = String::new();

        // Pattern for Python traceback lines
        let file_re = Regex::new(r#"^\s*File\s+"([^"]+)",\s+line\s+(\d+),\s+in\s+(\S+)"#).unwrap();

        for line in trace.lines() {
            if let Some(caps) = file_re.captures(line) {
                let frame = ParsedFrame {
                    file_path: caps.get(1).map(|m| m.as_str().to_string()),
                    line_number: caps.get(2).and_then(|m| m.as_str().parse().ok()),
                    function_name: caps.get(3).map(|m| m.as_str().to_string()),
                    col_number: None,
                };
                frames.push(frame);
            }
        }

        // Error is typically on the last non-empty line
        for line in trace.lines().rev() {
            let trimmed = line.trim();
            if !trimmed.is_empty()
                && !trimmed.starts_with("File ")
                && !trimmed.starts_with("Traceback")
            {
                // Check if it's an error line (ErrorType: message)
                if let Some(colon_pos) = trimmed.find(':') {
                    let error_type = &trimmed[..colon_pos];
                    // Skip if it looks like a file path (Windows or Unix)
                    if !error_type.contains('/')
                        && !error_type.contains('\\')
                        && error_type.chars().all(|c| c.is_alphanumeric() || c == '_')
                    {
                        error_signature = trimmed.to_string();
                        break;
                    }
                }
            }
        }

        if error_signature.is_empty() {
            error_signature = "python_exception".to_string();
        }

        // Reverse frames to have most recent call first
        frames.reverse();

        (error_signature, frames)
    }

    /// Parse a JavaScript/Node.js stack trace.
    ///
    /// JavaScript traces look like:
    /// ```text
    /// Error: Something went wrong
    ///     at functionName (/path/to/file.js:10:5)
    ///     at Object.<anonymous> (/path/to/file.js:20:10)
    ///     at Module._compile (internal/modules/cjs/loader.js:959:30)
    /// ```
    pub fn parse_javascript_stack(trace: &str) -> (String, Vec<ParsedFrame>) {
        let mut frames = Vec::new();
        let mut error_signature = String::new();

        // Pattern for JavaScript stack frames
        // Matches: "at function (file:line:col)" or "at file:line:col"
        let at_re = Regex::new(r"^\s+at\s+(?:(.+?)\s+\()?([^:]+):(\d+):(\d+)\)?$").unwrap();
        // Also try simpler pattern without column
        let at_simple_re = Regex::new(r"^\s+at\s+(?:(.+?)\s+\()?([^:]+):(\d+)\)?$").unwrap();

        for (i, line) in trace.lines().enumerate() {
            if i == 0 {
                // First line is typically the error
                error_signature = line.trim().to_string();
                continue;
            }

            if let Some(caps) = at_re.captures(line) {
                let frame = ParsedFrame {
                    function_name: caps.get(1).map(|m| m.as_str().to_string()),
                    file_path: caps.get(2).map(|m| m.as_str().to_string()),
                    line_number: caps.get(3).and_then(|m| m.as_str().parse().ok()),
                    col_number: caps.get(4).and_then(|m| m.as_str().parse().ok()),
                };
                frames.push(frame);
            } else if let Some(caps) = at_simple_re.captures(line) {
                let frame = ParsedFrame {
                    function_name: caps.get(1).map(|m| m.as_str().to_string()),
                    file_path: caps.get(2).map(|m| m.as_str().to_string()),
                    line_number: caps.get(3).and_then(|m| m.as_str().parse().ok()),
                    col_number: None,
                };
                frames.push(frame);
            }
        }

        if error_signature.is_empty() {
            error_signature = "javascript_error".to_string();
        }

        (error_signature, frames)
    }

    /// Try to parse a generic trace format.
    ///
    /// Attempts to extract file:line patterns from unknown formats.
    pub fn parse_generic(trace: &str) -> (String, Vec<ParsedFrame>) {
        let mut frames = Vec::new();

        // Generic pattern: file.ext:line or file.ext:line:col
        let file_line_re = Regex::new(r"([^\s:]+\.[a-zA-Z]+):(\d+)(?::(\d+))?").unwrap();

        for caps in file_line_re.captures_iter(trace) {
            let frame = ParsedFrame {
                file_path: caps.get(1).map(|m| m.as_str().to_string()),
                line_number: caps.get(2).and_then(|m| m.as_str().parse().ok()),
                col_number: caps.get(3).and_then(|m| m.as_str().parse().ok()),
                function_name: None,
            };
            frames.push(frame);
        }

        // Use first line as error signature if not empty
        let error_signature = trace
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| "unknown_error".to_string());

        (error_signature, frames)
    }

    /// Auto-detect trace format and parse accordingly.
    pub fn auto_detect_and_parse(trace: &str) -> (String, Vec<ParsedFrame>) {
        // Check for Python traceback
        if trace.contains("Traceback (most recent call last)") || trace.contains("File \"") {
            return Self::parse_python_traceback(trace);
        }

        // Check for JavaScript stack
        if trace.lines().any(|l| l.trim().starts_with("at ")) {
            return Self::parse_javascript_stack(trace);
        }

        // Check for Rust backtrace
        if trace.contains("panicked at") || trace.contains("stack backtrace:") {
            return Self::parse_rust_backtrace(trace);
        }

        // Fall back to generic
        Self::parse_generic(trace)
    }
}

/// Normalize an error signature for grouping similar errors.
///
/// Removes variable parts like memory addresses, timestamps, and specific values
/// while keeping the error type and core message structure.
pub fn normalize_error_signature(error: &str) -> String {
    let mut normalized = error.to_string();

    // Remove memory addresses (0x...)
    let hex_re = Regex::new(r"0x[0-9a-fA-F]+").unwrap();
    normalized = hex_re.replace_all(&normalized, "0x...").to_string();

    // Remove timestamps (various formats)
    let timestamp_re = Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}").unwrap();
    normalized = timestamp_re
        .replace_all(&normalized, "<timestamp>")
        .to_string();

    // Remove UUIDs
    let uuid_re =
        Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
            .unwrap();
    normalized = uuid_re.replace_all(&normalized, "<uuid>").to_string();

    // Remove line numbers (in some contexts)
    // Keep this minimal to avoid over-normalization

    // Truncate to reasonable length
    if normalized.len() > 200 {
        normalized.truncate(200);
        normalized.push_str("...");
    }

    normalized
}

/// Trait for indexing traces into storage.
pub trait TraceIndexer {
    /// Index a tool call trace.
    fn index_tool_call(&self, trace: ToolTraceRecord) -> Result<i64>;

    /// Index a raw stack trace, parsing it and storing frames.
    fn index_stack_trace(
        &self,
        raw_trace: &str,
        tenant_id: &TenantId,
        session_id: Option<&str>,
    ) -> Result<i64>;
}

/// Default implementation of TraceIndexer using StructuralStore.
pub struct DefaultTraceIndexer {
    store: Arc<StructuralStore>,
}

impl DefaultTraceIndexer {
    /// Create a new DefaultTraceIndexer.
    pub fn new(store: Arc<StructuralStore>) -> Self {
        Self { store }
    }
}

impl TraceIndexer for DefaultTraceIndexer {
    fn index_tool_call(&self, trace: ToolTraceRecord) -> Result<i64> {
        self.store.insert_tool_trace(&trace).map_err(Into::into)
    }

    fn index_stack_trace(
        &self,
        raw_trace: &str,
        tenant_id: &TenantId,
        session_id: Option<&str>,
    ) -> Result<i64> {
        let timestamp_ms = TraceCapture::current_timestamp_ms();

        // Parse the trace
        let (error_sig, parsed_frames) = StackTraceParser::auto_detect_and_parse(raw_trace);

        // Normalize the error signature
        let normalized_sig = normalize_error_signature(&error_sig);

        // Create the stack trace record
        let mut trace_record = StackTraceRecord::new(
            tenant_id.clone(),
            timestamp_ms,
            normalized_sig,
            error_sig,
            raw_trace,
        );
        trace_record.session_id = session_id.map(String::from);

        // Convert parsed frames to records
        let frame_records: Vec<StackFrameRecord> = parsed_frames
            .into_iter()
            .enumerate()
            .map(|(idx, pf)| StackFrameRecord {
                frame_id: None,
                trace_id: 0, // Will be set by insert
                frame_idx: idx as u32,
                file_path: pf.file_path,
                function_name: pf.function_name,
                line_number: pf.line_number,
                col_number: pf.col_number,
                context: None,
            })
            .collect();

        // Insert into storage
        self.store
            .insert_stack_trace(&trace_record, &frame_records)
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rust_backtrace() {
        let trace = r#"thread 'main' panicked at 'index out of bounds', src/main.rs:10:5
stack backtrace:
   0: std::panicking::begin_panic
             at /rustc/abc123/library/std/src/panicking.rs:519:12
   1: test::main
             at ./src/main.rs:10:5
   2: std::rt::lang_start
             at /rustc/abc123/library/std/src/rt.rs:134:17
"#;

        let (sig, frames) = StackTraceParser::parse_rust_backtrace(trace);

        assert!(sig.contains("index out of bounds"));
        assert_eq!(frames.len(), 3);
        assert_eq!(
            frames[0].function_name.as_deref(),
            Some("std::panicking::begin_panic")
        );
        assert_eq!(frames[1].function_name.as_deref(), Some("test::main"));
        assert_eq!(frames[1].line_number, Some(10));
    }

    #[test]
    fn test_parse_python_traceback() {
        let trace = r#"Traceback (most recent call last):
  File "main.py", line 10, in main
    do_something()
  File "utils.py", line 5, in do_something
    raise ValueError("invalid input")
ValueError: invalid input
"#;

        let (sig, frames) = StackTraceParser::parse_python_traceback(trace);

        assert_eq!(sig, "ValueError: invalid input");
        assert_eq!(frames.len(), 2);
        // Frames are reversed (most recent first)
        assert_eq!(frames[0].file_path.as_deref(), Some("utils.py"));
        assert_eq!(frames[0].function_name.as_deref(), Some("do_something"));
        assert_eq!(frames[0].line_number, Some(5));
        assert_eq!(frames[1].file_path.as_deref(), Some("main.py"));
    }

    #[test]
    fn test_parse_javascript_stack() {
        let trace = r#"Error: Something went wrong
    at processData (/app/src/handler.js:25:10)
    at handleRequest (/app/src/server.js:50:5)
    at Object.<anonymous> (/app/src/index.js:10:1)
"#;

        let (sig, frames) = StackTraceParser::parse_javascript_stack(trace);

        assert_eq!(sig, "Error: Something went wrong");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].function_name.as_deref(), Some("processData"));
        assert_eq!(frames[0].file_path.as_deref(), Some("/app/src/handler.js"));
        assert_eq!(frames[0].line_number, Some(25));
        assert_eq!(frames[0].col_number, Some(10));
    }

    #[test]
    fn test_auto_detect_python() {
        let trace = r#"Traceback (most recent call last):
  File "test.py", line 1, in module
KeyError: 'missing'
"#;
        let (sig, frames) = StackTraceParser::auto_detect_and_parse(trace);
        assert!(sig.contains("KeyError"));
        assert!(!frames.is_empty());
    }

    #[test]
    fn test_auto_detect_javascript() {
        let trace = r#"TypeError: Cannot read property 'x' of undefined
    at foo (/bar.js:1:1)
"#;
        let (sig, frames) = StackTraceParser::auto_detect_and_parse(trace);
        assert!(sig.contains("TypeError"));
        assert!(!frames.is_empty());
    }

    #[test]
    fn test_normalize_error_signature() {
        let sig1 = "Error at 0x7f1234567890: something failed";
        let normalized1 = normalize_error_signature(sig1);
        assert!(normalized1.contains("0x..."));
        assert!(!normalized1.contains("0x7f1234567890"));

        let sig2 = "Error at 2024-01-15T10:30:00: timeout";
        let normalized2 = normalize_error_signature(sig2);
        assert!(normalized2.contains("<timestamp>"));

        let sig3 = "Failed for ID: 550e8400-e29b-41d4-a716-446655440000";
        let normalized3 = normalize_error_signature(sig3);
        assert!(normalized3.contains("<uuid>"));
    }

    #[test]
    fn test_capture_tool_call() {
        let tenant = TenantId::new("test").unwrap();
        let input = serde_json::json!({"path": "/test.rs"});
        let output = serde_json::json!({"content": "fn main() {}"});

        let trace = TraceCapture::capture_tool_call(
            tenant,
            "read_file",
            &input,
            Some(&output),
            None,
            Some("session_1"),
            vec!["rust".to_string()],
            100,
        );

        assert_eq!(trace.tool_name, "read_file");
        assert!(trace.input_json.is_some());
        assert!(trace.output_json.is_some());
        assert!(trace.error_json.is_none());
        assert_eq!(trace.session_id.as_deref(), Some("session_1"));
        assert_eq!(trace.context_tags, vec!["rust"]);
        assert_eq!(trace.duration_ms, Some(100));
    }
}
