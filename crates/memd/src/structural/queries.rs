//! High-level symbol and trace query API.
//!
//! Provides convenient methods for finding symbol definitions, references,
//! callers, imports, tool call traces, and stack trace errors using the
//! underlying StructuralStore.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::types::TenantId;

use super::storage::{StackFrameRecord, StructuralStore, SymbolKind, SymbolRecord, TimeRange};

/// Location information for a symbol.
#[derive(Debug, Clone)]
pub struct SymbolLocation {
    /// File path containing the symbol.
    pub file_path: String,
    /// Symbol name.
    pub name: String,
    /// Symbol kind (function, class, method, etc.).
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
    /// Source language.
    pub language: String,
}

impl From<SymbolRecord> for SymbolLocation {
    fn from(r: SymbolRecord) -> Self {
        Self {
            file_path: r.file_path,
            name: r.name,
            kind: r.kind,
            line_start: r.line_start,
            line_end: r.line_end,
            col_start: r.col_start,
            col_end: r.col_end,
            signature: r.signature,
            docstring: r.docstring,
            visibility: r.visibility,
            language: r.language,
        }
    }
}

/// Information about a caller of a function.
#[derive(Debug, Clone)]
pub struct CallerInfo {
    /// Name of the calling function.
    pub caller_name: String,
    /// File containing the caller.
    pub caller_file: String,
    /// Line where the call is made.
    pub call_line: u32,
    /// Column where the call is made.
    pub call_col: u32,
    /// Kind of the calling symbol.
    pub caller_kind: SymbolKind,
    /// Depth from the original callee (1 = direct caller).
    pub depth: u32,
}

/// Information about a file that imports a module.
#[derive(Debug, Clone)]
pub struct ImportInfo {
    /// File that imports the module.
    pub importing_file: String,
    /// Line where the import occurs.
    pub import_line: u32,
    /// Alias used for the import, if any.
    pub alias: Option<String>,
}

/// High-level query service for structural code data.
///
/// Provides convenient methods for symbol lookup, reference finding,
/// caller discovery, and import tracking.
pub struct SymbolQueryService {
    store: Arc<StructuralStore>,
}

impl SymbolQueryService {
    /// Create a new query service backed by the given store.
    pub fn new(store: Arc<StructuralStore>) -> Self {
        Self { store }
    }

    /// Find symbol definitions by name.
    ///
    /// Returns all symbols with the given name, ordered by kind priority
    /// (function > method > class > type > variable > constant > etc).
    /// Optionally filters by project_id.
    pub fn find_symbol_definition(
        &self,
        tenant_id: &TenantId,
        name: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<SymbolLocation>> {
        let symbols = self.store.find_symbols_by_name(tenant_id, name)?;

        // Filter by project_id if specified
        let filtered: Vec<_> = if let Some(proj_id) = project_id {
            symbols
                .into_iter()
                .filter(|s| s.project_id.as_deref() == Some(proj_id))
                .collect()
        } else {
            symbols
        };

        // Sort by kind priority
        let mut locations: Vec<SymbolLocation> = filtered.into_iter().map(Into::into).collect();
        locations.sort_by_key(|l| kind_priority(&l.kind));

        Ok(locations)
    }

    /// Find all references to a symbol.
    ///
    /// Returns both:
    /// - Symbol definitions (the definition itself is a reference)
    /// - Call sites where the symbol is invoked
    ///
    /// Results are deduplicated by location.
    pub fn find_references(
        &self,
        tenant_id: &TenantId,
        name: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<SymbolLocation>> {
        let mut locations = Vec::new();
        let mut seen_locations: HashSet<(String, u32, u32)> = HashSet::new();

        // 1. Find symbol definitions
        let definitions = self.find_symbol_definition(tenant_id, name, project_id)?;
        for def in definitions {
            let key = (def.file_path.clone(), def.line_start, def.col_start);
            if seen_locations.insert(key) {
                locations.push(def);
            }
        }

        // 2. Find call edges where callee_name matches
        let call_edges = self.store.find_callers(tenant_id, name)?;
        for edge in call_edges {
            // Filter by project_id if specified
            // Call edges don't have project_id directly, so we check via caller symbol
            if project_id.is_some() {
                // For now, skip project filtering on call edges
                // A full implementation would look up caller symbol's project_id
            }

            let key = (edge.call_file.clone(), edge.call_line, edge.call_col);
            if seen_locations.insert(key) {
                // Create a SymbolLocation for the call site
                locations.push(SymbolLocation {
                    file_path: edge.call_file,
                    name: name.to_string(),
                    kind: SymbolKind::Function, // Call sites refer to functions
                    line_start: edge.call_line,
                    line_end: edge.call_line,
                    col_start: edge.call_col,
                    col_end: edge.call_col,
                    signature: None,
                    docstring: None,
                    visibility: None,
                    language: String::new(), // Unknown at call site
                });
            }
        }

        Ok(locations)
    }

    /// Find all callers of a function.
    ///
    /// Supports multi-hop traversal with cycle detection.
    /// `max_depth` controls how many levels deep to search (1-3).
    pub fn find_callers(
        &self,
        tenant_id: &TenantId,
        name: &str,
        max_depth: u32,
        project_id: Option<&str>,
    ) -> Result<Vec<CallerInfo>> {
        let max_depth = max_depth.clamp(1, 3);
        let mut callers = Vec::new();
        let mut visited: HashSet<i64> = HashSet::new();
        let mut to_visit: Vec<(String, u32)> = vec![(name.to_string(), 1)];

        while let Some((callee_name, depth)) = to_visit.pop() {
            if depth > max_depth {
                continue;
            }

            let edges = self.store.find_callers(tenant_id, &callee_name)?;

            for edge in edges {
                // Avoid cycles
                if visited.contains(&edge.caller_symbol_id) {
                    continue;
                }
                visited.insert(edge.caller_symbol_id);

                // Look up caller symbol to get name and kind
                let caller_symbols = self
                    .store
                    .find_symbols_by_name(tenant_id, &edge.callee_name);

                // Get caller info from symbol if available
                let (caller_name, caller_kind) = if let Ok(symbols) = &caller_symbols {
                    if let Some(sym) = symbols.first() {
                        (sym.name.clone(), sym.kind)
                    } else {
                        // Fallback: use edge info
                        (
                            format!("caller_{}", edge.caller_symbol_id),
                            SymbolKind::Function,
                        )
                    }
                } else {
                    (
                        format!("caller_{}", edge.caller_symbol_id),
                        SymbolKind::Function,
                    )
                };

                // Filter by project_id if specified
                if let Some(proj_id) = project_id {
                    if let Ok(symbols) = &caller_symbols {
                        let matches_project = symbols
                            .iter()
                            .any(|s| s.project_id.as_deref() == Some(proj_id));
                        if !matches_project {
                            continue;
                        }
                    }
                }

                callers.push(CallerInfo {
                    caller_name: caller_name.clone(),
                    caller_file: edge.call_file,
                    call_line: edge.call_line,
                    call_col: edge.call_col,
                    caller_kind,
                    depth,
                });

                // Queue for next depth level
                if depth < max_depth {
                    to_visit.push((caller_name, depth + 1));
                }
            }
        }

        Ok(callers)
    }

    /// Find all files that import a given module.
    pub fn find_imports(
        &self,
        tenant_id: &TenantId,
        module: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<ImportInfo>> {
        let imports = self.store.find_importers(tenant_id, module)?;

        // Filter by project_id if specified (imports don't have project_id, so skip)
        let _ = project_id;

        let infos: Vec<ImportInfo> = imports
            .into_iter()
            .map(|i| ImportInfo {
                importing_file: i.source_file,
                import_line: i.import_line,
                alias: i.alias,
            })
            .collect();

        Ok(infos)
    }

    /// Attempt to resolve unresolved callee_symbol_id by matching names.
    ///
    /// Called after indexing new files to link call edges to their targets.
    /// Returns the count of newly linked edges.
    pub fn link_callees(&self, _tenant_id: &TenantId) -> Result<usize> {
        // This would require additional store methods to update call edges
        // For now, return 0 as the linking is a future enhancement
        Ok(0)
    }
}

/// Get priority for symbol kinds (lower = higher priority).
fn kind_priority(kind: &SymbolKind) -> u8 {
    match kind {
        SymbolKind::Function => 0,
        SymbolKind::Method => 1,
        SymbolKind::Class => 2,
        SymbolKind::Interface => 3,
        SymbolKind::Type => 4,
        SymbolKind::Enum => 5,
        SymbolKind::Variable => 6,
        SymbolKind::Constant => 7,
        SymbolKind::Module => 8,
    }
}

// ============================================================================
// Trace Query Types and Service
// ============================================================================

/// Result of a tool call query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// Database ID.
    pub trace_id: i64,
    /// Name of the tool called.
    pub tool_name: String,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: i64,
    /// ISO 8601 formatted timestamp.
    pub timestamp_formatted: String,
    /// Input parameters (parsed JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Output result (parsed JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Error if any (parsed JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    /// Duration in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Session identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Context tags.
    #[serde(default)]
    pub context_tags: Vec<String>,
}

/// Result of an error/stack trace query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResult {
    /// Database ID.
    pub trace_id: i64,
    /// Normalized error signature for grouping.
    pub error_signature: String,
    /// Full error message.
    pub error_message: String,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: i64,
    /// ISO 8601 formatted timestamp.
    pub timestamp_formatted: String,
    /// Stack frames (top of stack first).
    pub frames: Vec<FrameInfo>,
    /// Session identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Information about a single stack frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameInfo {
    /// Frame index (0 = top of stack).
    pub index: u32,
    /// Function or method name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_name: Option<String>,
    /// File path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Line number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<u32>,
    /// Column number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col_number: Option<u32>,
}

impl From<StackFrameRecord> for FrameInfo {
    fn from(record: StackFrameRecord) -> Self {
        Self {
            index: record.frame_idx,
            function_name: record.function_name,
            file_path: record.file_path,
            line_number: record.line_number,
            col_number: record.col_number,
        }
    }
}

/// Summary of errors grouped by signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorSummary {
    /// Normalized error signature.
    pub error_signature: String,
    /// Number of occurrences.
    pub count: u64,
    /// First occurrence (Unix ms).
    pub first_seen_ms: i64,
    /// Last occurrence (Unix ms).
    pub last_seen_ms: i64,
    /// First occurrence formatted.
    pub first_seen_formatted: String,
    /// Last occurrence formatted.
    pub last_seen_formatted: String,
}

/// Service for querying trace data (tool calls and stack traces).
pub struct TraceQueryService {
    store: Arc<StructuralStore>,
}

impl TraceQueryService {
    /// Create a new trace query service.
    pub fn new(store: Arc<StructuralStore>) -> Self {
        Self { store }
    }

    /// Find tool calls with optional filters.
    ///
    /// Returns tool calls ordered by timestamp descending (most recent first).
    pub fn find_tool_calls(
        &self,
        tenant_id: &TenantId,
        tool_name: Option<&str>,
        time_range: Option<TimeRange>,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ToolCallResult>> {
        // If session_id filter is specified, use session-specific query
        if let Some(session) = session_id {
            let traces = self.store.find_tool_traces_by_session(session)?;

            let results: Vec<ToolCallResult> = traces
                .into_iter()
                .filter(|t| t.tenant_id == *tenant_id)
                .filter(|t| tool_name.is_none() || tool_name == Some(t.tool_name.as_str()))
                .filter(|t| {
                    if let Some(ref range) = time_range {
                        range.from_ms.is_none_or(|from| t.timestamp_ms >= from)
                            && range.to_ms.is_none_or(|to| t.timestamp_ms <= to)
                    } else {
                        true
                    }
                })
                .take(limit)
                .map(|t| self.trace_to_result(t))
                .collect();

            return Ok(results);
        }

        // Use standard query
        let traces = self
            .store
            .find_tool_traces(tenant_id, tool_name, time_range)?;

        let results: Vec<ToolCallResult> = traces
            .into_iter()
            .take(limit)
            .map(|t| self.trace_to_result(t))
            .collect();

        Ok(results)
    }

    /// Find tool calls that resulted in errors.
    pub fn find_tool_calls_with_errors(
        &self,
        tenant_id: &TenantId,
        time_range: Option<TimeRange>,
    ) -> Result<Vec<ToolCallResult>> {
        let traces = self.store.find_tool_traces_with_error(tenant_id)?;

        let results: Vec<ToolCallResult> = traces
            .into_iter()
            .filter(|t| {
                if let Some(ref range) = time_range {
                    range.from_ms.is_none_or(|from| t.timestamp_ms >= from)
                        && range.to_ms.is_none_or(|to| t.timestamp_ms <= to)
                } else {
                    true
                }
            })
            .map(|t| self.trace_to_result(t))
            .collect();

        Ok(results)
    }

    /// Find stack traces/errors with optional filters.
    ///
    /// Returns errors ordered by timestamp descending (most recent first).
    pub fn find_errors(
        &self,
        tenant_id: &TenantId,
        error_signature: Option<&str>,
        function_name: Option<&str>,
        file_path: Option<&str>,
        time_range: Option<TimeRange>,
        limit: usize,
    ) -> Result<Vec<ErrorResult>> {
        // If function_name filter is specified, use function-specific query
        if let Some(func_name) = function_name {
            let traces = self
                .store
                .find_stack_traces_by_function(tenant_id, func_name)?;

            let mut results = Vec::new();
            for trace in traces.into_iter().take(limit) {
                // Apply additional filters
                if let Some(sig) = error_signature {
                    if trace.error_signature != sig {
                        continue;
                    }
                }

                if let Some(ref range) = time_range {
                    let in_range = range.from_ms.is_none_or(|from| trace.timestamp_ms >= from)
                        && range.to_ms.is_none_or(|to| trace.timestamp_ms <= to);
                    if !in_range {
                        continue;
                    }
                }

                let trace_id = trace.trace_id.unwrap_or(0);
                let frames = self.store.get_stack_frames(trace_id)?;

                // Apply file_path filter on frames
                if let Some(path) = file_path {
                    if !frames.iter().any(|f| f.file_path.as_deref() == Some(path)) {
                        continue;
                    }
                }

                results.push(self.trace_to_error_result(trace, frames));
            }

            return Ok(results);
        }

        // Use standard query
        let traces = self
            .store
            .find_stack_traces(tenant_id, error_signature, time_range)?;

        let mut results = Vec::new();
        for trace in traces.into_iter().take(limit) {
            let trace_id = trace.trace_id.unwrap_or(0);
            let frames = self.store.get_stack_frames(trace_id)?;

            // Apply file_path filter on frames
            if let Some(path) = file_path {
                if !frames.iter().any(|f| f.file_path.as_deref() == Some(path)) {
                    continue;
                }
            }

            results.push(self.trace_to_error_result(trace, frames));
        }

        Ok(results)
    }

    /// Find errors where a specific function appears in the stack.
    pub fn find_errors_in_function(
        &self,
        tenant_id: &TenantId,
        function_name: &str,
    ) -> Result<Vec<ErrorResult>> {
        let traces = self
            .store
            .find_stack_traces_by_function(tenant_id, function_name)?;

        let mut results = Vec::new();
        for trace in traces {
            let trace_id = trace.trace_id.unwrap_or(0);
            let frames = self.store.get_stack_frames(trace_id)?;
            results.push(self.trace_to_error_result(trace, frames));
        }

        Ok(results)
    }

    /// Get a summary of errors grouped by error signature.
    pub fn get_error_summary(
        &self,
        tenant_id: &TenantId,
        time_range: Option<TimeRange>,
    ) -> Result<Vec<ErrorSummary>> {
        let traces = self.store.find_stack_traces(tenant_id, None, time_range)?;

        // Group by error_signature
        let mut groups: HashMap<String, (u64, i64, i64)> = HashMap::new();

        for trace in traces {
            let entry =
                groups
                    .entry(trace.error_signature.clone())
                    .or_insert((0, i64::MAX, i64::MIN));
            entry.0 += 1;
            entry.1 = entry.1.min(trace.timestamp_ms);
            entry.2 = entry.2.max(trace.timestamp_ms);
        }

        let mut summaries: Vec<ErrorSummary> = groups
            .into_iter()
            .map(|(sig, (count, first, last))| ErrorSummary {
                error_signature: sig,
                count,
                first_seen_ms: first,
                last_seen_ms: last,
                first_seen_formatted: format_timestamp(first),
                last_seen_formatted: format_timestamp(last),
            })
            .collect();

        // Sort by count descending
        summaries.sort_by(|a, b| b.count.cmp(&a.count));

        Ok(summaries)
    }

    /// Convert a tool trace record to a query result.
    fn trace_to_result(&self, trace: super::storage::ToolTraceRecord) -> ToolCallResult {
        ToolCallResult {
            trace_id: trace.trace_id.unwrap_or(0),
            tool_name: trace.tool_name,
            timestamp_ms: trace.timestamp_ms,
            timestamp_formatted: format_timestamp(trace.timestamp_ms),
            input: trace.input_json.and_then(|s| serde_json::from_str(&s).ok()),
            output: trace
                .output_json
                .and_then(|s| serde_json::from_str(&s).ok()),
            error: trace.error_json.and_then(|s| serde_json::from_str(&s).ok()),
            duration_ms: trace.duration_ms,
            session_id: trace.session_id,
            context_tags: trace.context_tags,
        }
    }

    /// Convert a stack trace record to an error result.
    fn trace_to_error_result(
        &self,
        trace: super::storage::StackTraceRecord,
        frames: Vec<StackFrameRecord>,
    ) -> ErrorResult {
        ErrorResult {
            trace_id: trace.trace_id.unwrap_or(0),
            error_signature: trace.error_signature,
            error_message: trace.error_message,
            timestamp_ms: trace.timestamp_ms,
            timestamp_formatted: format_timestamp(trace.timestamp_ms),
            frames: frames.into_iter().map(FrameInfo::from).collect(),
            session_id: trace.session_id,
        }
    }
}

/// Format a Unix millisecond timestamp as ISO 8601 string.
///
/// Uses a simple format without external dependencies.
/// Format: "2024-01-15T10:30:45.123Z"
pub fn format_timestamp(ms: i64) -> String {
    use std::time::{Duration, UNIX_EPOCH};

    // Handle negative or very large timestamps
    if ms < 0 || ms > i64::MAX / 2 {
        return format!("{}ms", ms);
    }

    let duration = Duration::from_millis(ms as u64);
    let datetime = UNIX_EPOCH + duration;

    // Format using SystemTime (limited precision but no external deps)
    match datetime.duration_since(UNIX_EPOCH) {
        Ok(dur) => {
            let secs = dur.as_secs();
            let millis = dur.subsec_millis();

            // Calculate date/time components manually
            let days = secs / 86400;
            let remaining = secs % 86400;
            let hours = remaining / 3600;
            let minutes = (remaining % 3600) / 60;
            let seconds = remaining % 60;

            // Calculate year, month, day from days since epoch (1970-01-01)
            let (year, month, day) = days_to_ymd(days as i64);

            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                year, month, day, hours, minutes, seconds, millis
            )
        }
        Err(_) => format!("{}ms", ms),
    }
}

/// Convert days since Unix epoch to year, month, day.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    // Algorithm based on Howard Hinnant's date algorithms
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m, d)
}

/// Parse an ISO 8601 datetime string to Unix milliseconds.
///
/// Supports formats:
/// - "2024-01-15T10:30:45Z" (UTC)
/// - "2024-01-15T10:30:45.123Z" (with millis)
/// - "2024-01-15T10:30:45+00:00" (with offset)
pub fn parse_iso_datetime(s: &str) -> Result<i64> {
    // Try to parse RFC3339 format manually
    parse_rfc3339(s)
        .ok_or_else(|| crate::error::MemdError::ValidationError(format!("Invalid datetime: {}", s)))
}

/// Parse RFC3339 datetime string to Unix milliseconds.
fn parse_rfc3339(s: &str) -> Option<i64> {
    // Expected format: YYYY-MM-DDTHH:MM:SS[.mmm](Z|+HH:MM|-HH:MM)
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }

    // Parse date part
    let year: i32 = s.get(0..4)?.parse().ok()?;
    if s.get(4..5)? != "-" {
        return None;
    }
    let month: u32 = s.get(5..7)?.parse().ok()?;
    if s.get(7..8)? != "-" {
        return None;
    }
    let day: u32 = s.get(8..10)?.parse().ok()?;
    if s.get(10..11)? != "T" {
        return None;
    }

    // Parse time part
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    if s.get(13..14)? != ":" {
        return None;
    }
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    if s.get(16..17)? != ":" {
        return None;
    }
    let second: u32 = s.get(17..19)?.parse().ok()?;

    // Parse optional milliseconds and timezone
    let rest = &s[19..];
    let (millis, tz_offset_mins) = parse_millis_and_tz(rest)?;

    // Convert to Unix timestamp
    let days = ymd_to_days(year, month, day)?;
    let secs =
        (days as i64) * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64)
            - (tz_offset_mins as i64) * 60;
    let ms = secs * 1000 + (millis as i64);

    Some(ms)
}

/// Parse milliseconds and timezone offset from the remainder of an RFC3339 string.
fn parse_millis_and_tz(s: &str) -> Option<(u32, i32)> {
    let s = s.trim();
    if s.is_empty() {
        return Some((0, 0));
    }

    let (millis, rest) = if s.starts_with('.') {
        // Parse milliseconds
        let end = s[1..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| i + 1)
            .unwrap_or(s.len());
        let frac = &s[1..end];
        let millis: u32 = if frac.len() >= 3 {
            frac[..3].parse().ok()?
        } else {
            let padded = format!("{:0<3}", frac);
            padded.parse().ok()?
        };
        (millis, &s[end..])
    } else {
        (0, s)
    };

    let tz_offset = if rest == "Z" || rest.is_empty() {
        0
    } else if rest.starts_with('+') || rest.starts_with('-') {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let tz = &rest[1..];
        let (hours, minutes) = if tz.contains(':') {
            let parts: Vec<&str> = tz.split(':').collect();
            if parts.len() != 2 {
                return None;
            }
            (parts[0].parse::<i32>().ok()?, parts[1].parse::<i32>().ok()?)
        } else if tz.len() == 4 {
            (tz[..2].parse::<i32>().ok()?, tz[2..].parse::<i32>().ok()?)
        } else {
            return None;
        };
        sign * (hours * 60 + minutes)
    } else {
        return None;
    };

    Some((millis, tz_offset))
}

/// Convert year, month, day to days since Unix epoch.
fn ymd_to_days(year: i32, month: u32, day: u32) -> Option<i64> {
    if month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }

    // Howard Hinnant's algorithm
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 12 } else { month };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (m - 3) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era as i64) * 146097 + (doe as i64) - 719468;
    Some(days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structural::{CallEdgeRecord, CallType, ImportRecord};

    fn test_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    fn create_test_symbol(name: &str, kind: SymbolKind, file: &str) -> SymbolRecord {
        SymbolRecord {
            symbol_id: None,
            tenant_id: test_tenant(),
            project_id: None,
            file_path: file.to_string(),
            name: name.to_string(),
            kind,
            line_start: 10,
            line_end: 20,
            col_start: 0,
            col_end: 1,
            parent_symbol_id: None,
            signature: Some(format!("fn {}()", name)),
            docstring: Some(format!("Doc for {}", name)),
            visibility: Some("public".to_string()),
            language: "rust".to_string(),
        }
    }

    #[test]
    fn test_find_definition_exact_match() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let query_service = SymbolQueryService::new(store.clone());
        let tenant = test_tenant();

        // Insert a symbol
        store
            .insert_symbol(&create_test_symbol(
                "process_data",
                SymbolKind::Function,
                "src/lib.rs",
            ))
            .unwrap();

        // Find it
        let results = query_service
            .find_symbol_definition(&tenant, "process_data", None)
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "process_data");
        assert_eq!(results[0].kind, SymbolKind::Function);
        assert_eq!(results[0].file_path, "src/lib.rs");
    }

    #[test]
    fn test_find_definition_multiple_matches() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let query_service = SymbolQueryService::new(store.clone());
        let tenant = test_tenant();

        // Insert multiple symbols with same name but different kinds
        store
            .insert_symbol(&create_test_symbol(
                "Handler",
                SymbolKind::Class,
                "src/a.rs",
            ))
            .unwrap();
        store
            .insert_symbol(&create_test_symbol(
                "Handler",
                SymbolKind::Function,
                "src/b.rs",
            ))
            .unwrap();
        store
            .insert_symbol(&create_test_symbol("Handler", SymbolKind::Type, "src/c.rs"))
            .unwrap();

        let results = query_service
            .find_symbol_definition(&tenant, "Handler", None)
            .unwrap();

        assert_eq!(results.len(), 3);
        // Should be sorted by priority: Function > Class > Type
        assert_eq!(results[0].kind, SymbolKind::Function);
        assert_eq!(results[1].kind, SymbolKind::Class);
        assert_eq!(results[2].kind, SymbolKind::Type);
    }

    #[test]
    fn test_find_callers_single_hop() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let query_service = SymbolQueryService::new(store.clone());
        let tenant = test_tenant();

        // Insert caller symbol
        let caller_id = store
            .insert_symbol(&create_test_symbol(
                "main",
                SymbolKind::Function,
                "src/main.rs",
            ))
            .unwrap();

        // Insert call edge
        let edge = CallEdgeRecord {
            edge_id: None,
            tenant_id: tenant.clone(),
            caller_symbol_id: caller_id,
            callee_name: "process_data".to_string(),
            callee_symbol_id: None,
            call_file: "src/main.rs".to_string(),
            call_line: 15,
            call_col: 4,
            call_type: CallType::Direct,
        };
        store.insert_call_edge(&edge).unwrap();

        let callers = query_service
            .find_callers(&tenant, "process_data", 1, None)
            .unwrap();

        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller_file, "src/main.rs");
        assert_eq!(callers[0].call_line, 15);
        assert_eq!(callers[0].depth, 1);
    }

    #[test]
    fn test_find_callers_multi_hop() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let query_service = SymbolQueryService::new(store.clone());
        let tenant = test_tenant();

        // Insert symbols for chain: entry -> middleware -> handler
        let entry_id = store
            .insert_symbol(&create_test_symbol(
                "entry",
                SymbolKind::Function,
                "src/main.rs",
            ))
            .unwrap();
        let middleware_id = store
            .insert_symbol(&create_test_symbol(
                "middleware",
                SymbolKind::Function,
                "src/mid.rs",
            ))
            .unwrap();

        // entry calls middleware
        store
            .insert_call_edge(&CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: entry_id,
                callee_name: "middleware".to_string(),
                callee_symbol_id: Some(middleware_id),
                call_file: "src/main.rs".to_string(),
                call_line: 10,
                call_col: 4,
                call_type: CallType::Direct,
            })
            .unwrap();

        // middleware calls handler
        store
            .insert_call_edge(&CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: middleware_id,
                callee_name: "handler".to_string(),
                callee_symbol_id: None,
                call_file: "src/mid.rs".to_string(),
                call_line: 20,
                call_col: 8,
                call_type: CallType::Direct,
            })
            .unwrap();

        // Find callers of handler with depth 2
        let callers = query_service
            .find_callers(&tenant, "handler", 2, None)
            .unwrap();

        // Should find middleware as direct caller
        assert!(!callers.is_empty());
        assert!(callers.iter().any(|c| c.caller_file == "src/mid.rs"));
    }

    #[test]
    fn test_find_imports() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let query_service = SymbolQueryService::new(store.clone());
        let tenant = test_tenant();

        // Insert imports
        store
            .insert_import(&ImportRecord {
                import_id: None,
                tenant_id: tenant.clone(),
                source_file: "src/main.py".to_string(),
                imported_module: "json".to_string(),
                imported_name: None,
                alias: None,
                import_line: 1,
                is_relative: false,
            })
            .unwrap();

        store
            .insert_import(&ImportRecord {
                import_id: None,
                tenant_id: tenant.clone(),
                source_file: "src/utils.py".to_string(),
                imported_module: "json".to_string(),
                imported_name: Some("dumps".to_string()),
                alias: Some("j".to_string()),
                import_line: 2,
                is_relative: false,
            })
            .unwrap();

        let imports = query_service.find_imports(&tenant, "json", None).unwrap();

        assert_eq!(imports.len(), 2);
        assert!(imports.iter().any(|i| i.importing_file == "src/main.py"));
        assert!(imports.iter().any(|i| i.importing_file == "src/utils.py"));
        assert!(imports.iter().any(|i| i.alias == Some("j".to_string())));
    }

    #[test]
    fn test_find_references_combines_definitions_and_usages() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let query_service = SymbolQueryService::new(store.clone());
        let tenant = test_tenant();

        // Insert function definition
        store
            .insert_symbol(&create_test_symbol(
                "process",
                SymbolKind::Function,
                "src/lib.rs",
            ))
            .unwrap();

        // Insert caller
        let caller_id = store
            .insert_symbol(&create_test_symbol(
                "main",
                SymbolKind::Function,
                "src/main.rs",
            ))
            .unwrap();

        // Insert call edge
        store
            .insert_call_edge(&CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: caller_id,
                callee_name: "process".to_string(),
                callee_symbol_id: None,
                call_file: "src/main.rs".to_string(),
                call_line: 15,
                call_col: 4,
                call_type: CallType::Direct,
            })
            .unwrap();

        let refs = query_service
            .find_references(&tenant, "process", None)
            .unwrap();

        // Should have both definition and usage
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.file_path == "src/lib.rs")); // definition
        assert!(refs.iter().any(|r| r.file_path == "src/main.rs")); // usage
    }

    // ========================================================================
    // Trace Query Tests
    // ========================================================================

    use crate::structural::{StackFrameRecord, StackTraceRecord, ToolTraceRecord};

    #[test]
    fn test_format_timestamp() {
        let ts = 1704067200000; // 2024-01-01 00:00:00 UTC
        let formatted = format_timestamp(ts);
        assert!(formatted.starts_with("2024-01-01"));
        assert!(formatted.contains("00:00:00"));
    }

    #[test]
    fn test_parse_iso_datetime() {
        let dt = "2024-01-01T00:00:00Z";
        let ms = parse_iso_datetime(dt).unwrap();
        assert_eq!(ms, 1704067200000);
    }

    #[test]
    fn test_parse_iso_datetime_with_offset() {
        let dt = "2024-01-01T01:00:00+01:00";
        let ms = parse_iso_datetime(dt).unwrap();
        assert_eq!(ms, 1704067200000); // Same as midnight UTC
    }

    #[test]
    fn test_find_tool_calls_by_name() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let tenant = test_tenant();
        let service = TraceQueryService::new(store.clone());

        // Insert test traces
        let mut trace1 = ToolTraceRecord::new(tenant.clone(), "read_file", 1000);
        trace1.input_json = Some(r#"{"path": "/test.rs"}"#.to_string());
        store.insert_tool_trace(&trace1).unwrap();

        let mut trace2 = ToolTraceRecord::new(tenant.clone(), "write_file", 2000);
        trace2.input_json = Some(r#"{"path": "/out.txt"}"#.to_string());
        store.insert_tool_trace(&trace2).unwrap();

        // Find only read_file calls
        let results = service
            .find_tool_calls(&tenant, Some("read_file"), None, None, 100)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_name, "read_file");
        assert!(results[0].input.is_some());
    }

    #[test]
    fn test_find_tool_calls_by_time_range() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let tenant = test_tenant();
        let service = TraceQueryService::new(store.clone());

        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 1000))
            .unwrap();
        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 2000))
            .unwrap();
        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 3000))
            .unwrap();

        // Find in range 1500-2500
        let results = service
            .find_tool_calls(
                &tenant,
                None,
                Some(TimeRange::between(1500, 2500)),
                None,
                100,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].timestamp_ms, 2000);
    }

    #[test]
    fn test_find_errors_by_signature() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let tenant = test_tenant();
        let service = TraceQueryService::new(store.clone());

        let trace1 = StackTraceRecord::new(
            tenant.clone(),
            1000,
            "TypeError",
            "null is not a function",
            "trace1",
        );
        store.insert_stack_trace(&trace1, &[]).unwrap();

        let trace2 = StackTraceRecord::new(
            tenant.clone(),
            2000,
            "ReferenceError",
            "x is not defined",
            "trace2",
        );
        store.insert_stack_trace(&trace2, &[]).unwrap();

        let results = service
            .find_errors(&tenant, Some("TypeError"), None, None, None, 100)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].error_signature, "TypeError");
    }

    #[test]
    fn test_find_errors_in_function() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let tenant = test_tenant();
        let service = TraceQueryService::new(store.clone());

        let trace = StackTraceRecord::new(
            tenant.clone(),
            1000,
            "Error",
            "Something went wrong",
            "trace",
        );
        let frames = vec![
            StackFrameRecord {
                frame_id: None,
                trace_id: 0,
                frame_idx: 0,
                file_path: Some("src/main.rs".to_string()),
                function_name: Some("process_data".to_string()),
                line_number: Some(42),
                col_number: None,
                context: None,
            },
            StackFrameRecord {
                frame_id: None,
                trace_id: 0,
                frame_idx: 1,
                file_path: Some("src/lib.rs".to_string()),
                function_name: Some("handle_request".to_string()),
                line_number: Some(100),
                col_number: None,
                context: None,
            },
        ];
        store.insert_stack_trace(&trace, &frames).unwrap();

        // Find by function name
        let results = service
            .find_errors_in_function(&tenant, "process_data")
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].frames.len(), 2);
        assert_eq!(
            results[0].frames[0].function_name.as_deref(),
            Some("process_data")
        );
    }

    #[test]
    fn test_error_summary() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let tenant = test_tenant();
        let service = TraceQueryService::new(store.clone());

        // Insert multiple errors with same signature
        store
            .insert_stack_trace(
                &StackTraceRecord::new(tenant.clone(), 1000, "TypeError", "msg1", "t1"),
                &[],
            )
            .unwrap();
        store
            .insert_stack_trace(
                &StackTraceRecord::new(tenant.clone(), 2000, "TypeError", "msg2", "t2"),
                &[],
            )
            .unwrap();
        store
            .insert_stack_trace(
                &StackTraceRecord::new(tenant.clone(), 3000, "TypeError", "msg3", "t3"),
                &[],
            )
            .unwrap();
        store
            .insert_stack_trace(
                &StackTraceRecord::new(tenant.clone(), 4000, "ReferenceError", "msg4", "t4"),
                &[],
            )
            .unwrap();

        let summaries = service.get_error_summary(&tenant, None).unwrap();
        assert_eq!(summaries.len(), 2);

        // Should be sorted by count descending
        assert_eq!(summaries[0].error_signature, "TypeError");
        assert_eq!(summaries[0].count, 3);
        assert_eq!(summaries[0].first_seen_ms, 1000);
        assert_eq!(summaries[0].last_seen_ms, 3000);

        assert_eq!(summaries[1].error_signature, "ReferenceError");
        assert_eq!(summaries[1].count, 1);
    }

    #[test]
    fn test_find_tool_calls_with_errors() {
        let store = Arc::new(StructuralStore::in_memory().unwrap());
        let tenant = test_tenant();
        let service = TraceQueryService::new(store.clone());

        let mut success = ToolTraceRecord::new(tenant.clone(), "tool", 1000);
        success.output_json = Some(r#"{"result": "ok"}"#.to_string());
        store.insert_tool_trace(&success).unwrap();

        let mut error = ToolTraceRecord::new(tenant.clone(), "tool", 2000);
        error.error_json = Some(r#"{"type": "NotFound", "message": "File not found"}"#.to_string());
        store.insert_tool_trace(&error).unwrap();

        let results = service.find_tool_calls_with_errors(&tenant, None).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].error.is_some());
    }
}
