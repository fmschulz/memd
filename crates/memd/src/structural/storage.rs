//! SQLite storage for structural index data.
//!
//! Provides persistent storage for symbols, call graph edges, import
//! relationships, tool call traces, and stack traces.

use rusqlite::{params, Connection, Result as SqliteResult};
use std::path::Path;
use std::sync::Mutex;

use crate::error::Result;
use crate::types::TenantId;

/// Schema for structural index tables.
const STRUCTURAL_SCHEMA: &str = r#"
-- Symbols table: functions, classes, methods, variables
CREATE TABLE IF NOT EXISTS symbols (
    symbol_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT,
    file_path TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    col_start INTEGER NOT NULL,
    col_end INTEGER NOT NULL,
    parent_symbol_id INTEGER,
    signature TEXT,
    docstring TEXT,
    visibility TEXT,
    language TEXT NOT NULL,
    FOREIGN KEY (parent_symbol_id) REFERENCES symbols(symbol_id)
);

-- Indexes for efficient symbol lookup
CREATE INDEX IF NOT EXISTS idx_symbols_tenant_name
    ON symbols(tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_symbols_tenant_file
    ON symbols(tenant_id, file_path);
CREATE INDEX IF NOT EXISTS idx_symbols_kind
    ON symbols(tenant_id, kind);
CREATE INDEX IF NOT EXISTS idx_symbols_parent
    ON symbols(parent_symbol_id);

-- Call graph edges: caller -> callee
CREATE TABLE IF NOT EXISTS call_edges (
    edge_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    caller_symbol_id INTEGER NOT NULL,
    callee_name TEXT NOT NULL,
    callee_symbol_id INTEGER,
    call_file TEXT NOT NULL,
    call_line INTEGER NOT NULL,
    call_col INTEGER NOT NULL,
    call_type TEXT NOT NULL
);

-- Import graph: file -> module dependencies
CREATE TABLE IF NOT EXISTS imports (
    import_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    source_file TEXT NOT NULL,
    imported_module TEXT NOT NULL,
    imported_name TEXT,
    alias TEXT,
    import_line INTEGER NOT NULL,
    is_relative INTEGER DEFAULT 0
);

-- Indexes for call graph queries
CREATE INDEX IF NOT EXISTS idx_call_edges_caller
    ON call_edges(caller_symbol_id);
CREATE INDEX IF NOT EXISTS idx_call_edges_callee_name
    ON call_edges(tenant_id, callee_name);
CREATE INDEX IF NOT EXISTS idx_call_edges_callee_symbol
    ON call_edges(callee_symbol_id) WHERE callee_symbol_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_call_edges_file
    ON call_edges(tenant_id, call_file);

-- Indexes for import queries
CREATE INDEX IF NOT EXISTS idx_imports_source
    ON imports(tenant_id, source_file);
CREATE INDEX IF NOT EXISTS idx_imports_module
    ON imports(tenant_id, imported_module);

-- Tool call traces: captures agent tool invocations
CREATE TABLE IF NOT EXISTS tool_traces (
    trace_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT,
    session_id TEXT,
    tool_name TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    input_json TEXT,      -- Full input parameters
    output_json TEXT,     -- Full output (not truncated)
    error_json TEXT,      -- Error if any
    context_tags TEXT,    -- JSON array of tags
    duration_ms INTEGER   -- How long the call took
);

-- Stack traces from errors
CREATE TABLE IF NOT EXISTS stack_traces (
    trace_id INTEGER PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT,
    session_id TEXT,
    timestamp_ms INTEGER NOT NULL,
    error_signature TEXT,  -- Normalized error type/message
    error_message TEXT,    -- Full error message
    full_trace TEXT        -- Raw trace string
);

-- Individual stack frames for precise queries
CREATE TABLE IF NOT EXISTS stack_frames (
    frame_id INTEGER PRIMARY KEY,
    trace_id INTEGER NOT NULL,
    frame_idx INTEGER NOT NULL,  -- 0 = top of stack
    file_path TEXT,
    function_name TEXT,
    line_number INTEGER,
    col_number INTEGER,
    context TEXT,  -- Code snippet if available
    FOREIGN KEY (trace_id) REFERENCES stack_traces(trace_id) ON DELETE CASCADE
);

-- Indexes for trace queries
CREATE INDEX IF NOT EXISTS idx_tool_traces_tenant_tool
    ON tool_traces(tenant_id, tool_name, timestamp_ms DESC);
CREATE INDEX IF NOT EXISTS idx_tool_traces_session
    ON tool_traces(session_id, timestamp_ms DESC);
CREATE INDEX IF NOT EXISTS idx_tool_traces_time
    ON tool_traces(tenant_id, timestamp_ms DESC);

CREATE INDEX IF NOT EXISTS idx_stack_traces_tenant
    ON stack_traces(tenant_id, timestamp_ms DESC);
CREATE INDEX IF NOT EXISTS idx_stack_traces_error_sig
    ON stack_traces(tenant_id, error_signature);

CREATE INDEX IF NOT EXISTS idx_stack_frames_function
    ON stack_frames(function_name);
CREATE INDEX IF NOT EXISTS idx_stack_frames_file
    ON stack_frames(file_path);
CREATE INDEX IF NOT EXISTS idx_stack_frames_trace
    ON stack_frames(trace_id);
"#;

/// Symbol kinds extracted from source code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Variable,
    Type,
    Module,
    Interface,
    Enum,
    Constant,
}

impl SymbolKind {
    /// Convert to string for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Class => "class",
            SymbolKind::Method => "method",
            SymbolKind::Variable => "variable",
            SymbolKind::Type => "type",
            SymbolKind::Module => "module",
            SymbolKind::Interface => "interface",
            SymbolKind::Enum => "enum",
            SymbolKind::Constant => "constant",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(SymbolKind::Function),
            "class" => Some(SymbolKind::Class),
            "method" => Some(SymbolKind::Method),
            "variable" => Some(SymbolKind::Variable),
            "type" => Some(SymbolKind::Type),
            "module" => Some(SymbolKind::Module),
            "interface" => Some(SymbolKind::Interface),
            "enum" => Some(SymbolKind::Enum),
            "constant" => Some(SymbolKind::Constant),
            _ => None,
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A symbol record for storage.
#[derive(Debug, Clone)]
pub struct SymbolRecord {
    /// Database ID (None before insert).
    pub symbol_id: Option<i64>,
    /// Tenant isolation.
    pub tenant_id: TenantId,
    /// Optional project scope.
    pub project_id: Option<String>,
    /// File containing the symbol.
    pub file_path: String,
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
    /// Parent symbol ID for nested scopes.
    pub parent_symbol_id: Option<i64>,
    /// Function signature or type annotation.
    pub signature: Option<String>,
    /// Extracted documentation.
    pub docstring: Option<String>,
    /// Visibility (public, private, etc.).
    pub visibility: Option<String>,
    /// Source language.
    pub language: String,
}

/// Type of function/method call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallType {
    /// Direct function call: `foo()`
    Direct,
    /// Method call: `obj.method()`
    Method,
    /// Qualified/scoped call: `module::func()` or `module.func()`
    Qualified,
}

impl CallType {
    /// Convert to string for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            CallType::Direct => "direct",
            CallType::Method => "method",
            CallType::Qualified => "qualified",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "direct" => Some(CallType::Direct),
            "method" => Some(CallType::Method),
            "qualified" => Some(CallType::Qualified),
            _ => None,
        }
    }
}

/// A call edge record for storage.
#[derive(Debug, Clone)]
pub struct CallEdgeRecord {
    pub edge_id: Option<i64>,
    pub tenant_id: TenantId,
    pub caller_symbol_id: i64,
    pub callee_name: String,
    pub callee_symbol_id: Option<i64>,
    pub call_file: String,
    pub call_line: u32,
    pub call_col: u32,
    pub call_type: CallType,
}

/// An import record for storage.
#[derive(Debug, Clone)]
pub struct ImportRecord {
    pub import_id: Option<i64>,
    pub tenant_id: TenantId,
    pub source_file: String,
    pub imported_module: String,
    pub imported_name: Option<String>,
    pub alias: Option<String>,
    pub import_line: u32,
    pub is_relative: bool,
}

/// Time range for filtering trace queries.
#[derive(Debug, Clone, Default)]
pub struct TimeRange {
    /// Start of range (inclusive), Unix milliseconds.
    pub from_ms: Option<i64>,
    /// End of range (inclusive), Unix milliseconds.
    pub to_ms: Option<i64>,
}

impl TimeRange {
    /// Create an unbounded time range.
    pub fn unbounded() -> Self {
        Self::default()
    }

    /// Create a time range with a start bound.
    pub fn from(from_ms: i64) -> Self {
        Self {
            from_ms: Some(from_ms),
            to_ms: None,
        }
    }

    /// Create a time range with both bounds.
    pub fn between(from_ms: i64, to_ms: i64) -> Self {
        Self {
            from_ms: Some(from_ms),
            to_ms: Some(to_ms),
        }
    }
}

/// Record for a tool call trace.
#[derive(Debug, Clone)]
pub struct ToolTraceRecord {
    /// Database ID (None for new records).
    pub trace_id: Option<i64>,
    /// Tenant this trace belongs to.
    pub tenant_id: TenantId,
    /// Project context (optional).
    pub project_id: Option<String>,
    /// Session context (optional).
    pub session_id: Option<String>,
    /// Name of the tool called.
    pub tool_name: String,
    /// When the call was made, Unix milliseconds.
    pub timestamp_ms: i64,
    /// Serialized input parameters.
    pub input_json: Option<String>,
    /// Serialized output (not truncated).
    pub output_json: Option<String>,
    /// Serialized error if any.
    pub error_json: Option<String>,
    /// Context tags for filtering.
    pub context_tags: Vec<String>,
    /// How long the call took in milliseconds.
    pub duration_ms: Option<i64>,
}

impl ToolTraceRecord {
    /// Create a new tool trace record.
    pub fn new(tenant_id: TenantId, tool_name: impl Into<String>, timestamp_ms: i64) -> Self {
        Self {
            trace_id: None,
            tenant_id,
            project_id: None,
            session_id: None,
            tool_name: tool_name.into(),
            timestamp_ms,
            input_json: None,
            output_json: None,
            error_json: None,
            context_tags: Vec::new(),
            duration_ms: None,
        }
    }

    /// Check if this trace has an error.
    pub fn has_error(&self) -> bool {
        self.error_json.is_some()
    }
}

/// Record for a stack trace.
#[derive(Debug, Clone)]
pub struct StackTraceRecord {
    /// Database ID (None for new records).
    pub trace_id: Option<i64>,
    /// Tenant this trace belongs to.
    pub tenant_id: TenantId,
    /// Project context (optional).
    pub project_id: Option<String>,
    /// Session context (optional).
    pub session_id: Option<String>,
    /// When the error occurred, Unix milliseconds.
    pub timestamp_ms: i64,
    /// Normalized error signature for grouping.
    pub error_signature: String,
    /// Full error message.
    pub error_message: String,
    /// Raw stack trace string.
    pub full_trace: String,
}

impl StackTraceRecord {
    /// Create a new stack trace record.
    pub fn new(
        tenant_id: TenantId,
        timestamp_ms: i64,
        error_signature: impl Into<String>,
        error_message: impl Into<String>,
        full_trace: impl Into<String>,
    ) -> Self {
        Self {
            trace_id: None,
            tenant_id,
            project_id: None,
            session_id: None,
            timestamp_ms,
            error_signature: error_signature.into(),
            error_message: error_message.into(),
            full_trace: full_trace.into(),
        }
    }
}

/// Record for a single stack frame.
#[derive(Debug, Clone)]
pub struct StackFrameRecord {
    /// Database ID (None for new records).
    pub frame_id: Option<i64>,
    /// Parent trace ID.
    pub trace_id: i64,
    /// Frame index (0 = top of stack).
    pub frame_idx: u32,
    /// File path where the frame occurred.
    pub file_path: Option<String>,
    /// Function name.
    pub function_name: Option<String>,
    /// Line number.
    pub line_number: Option<u32>,
    /// Column number.
    pub col_number: Option<u32>,
    /// Code context snippet.
    pub context: Option<String>,
}

impl StackFrameRecord {
    /// Create a new stack frame record.
    pub fn new(trace_id: i64, frame_idx: u32) -> Self {
        Self {
            frame_id: None,
            trace_id,
            frame_idx,
            file_path: None,
            function_name: None,
            line_number: None,
            col_number: None,
            context: None,
        }
    }
}

/// SQLite-backed structural index store.
pub struct StructuralStore {
    conn: Mutex<Connection>,
}

impl StructuralStore {
    /// Open or create a structural store at the given path.
    pub fn open(path: &Path) -> SqliteResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(STRUCTURAL_SCHEMA)?;
        // Enable foreign key support for stack_frames -> stack_traces
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory store for testing.
    pub fn in_memory() -> SqliteResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(STRUCTURAL_SCHEMA)?;
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // --- Symbol operations ---

    /// Insert a single symbol and return its ID.
    pub fn insert_symbol(&self, symbol: &SymbolRecord) -> Result<i64> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO symbols (
                tenant_id, project_id, file_path, name, kind,
                line_start, line_end, col_start, col_end,
                parent_symbol_id, signature, docstring, visibility, language
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                symbol.tenant_id.as_str(),
                symbol.project_id.as_deref(),
                &symbol.file_path,
                &symbol.name,
                symbol.kind.as_str(),
                symbol.line_start as i64,
                symbol.line_end as i64,
                symbol.col_start as i64,
                symbol.col_end as i64,
                symbol.parent_symbol_id,
                symbol.signature.as_deref(),
                symbol.docstring.as_deref(),
                symbol.visibility.as_deref(),
                &symbol.language,
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Insert multiple symbols in a transaction for performance.
    pub fn insert_symbols_batch(&self, symbols: &[SymbolRecord]) -> Result<Vec<i64>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        let mut ids = Vec::with_capacity(symbols.len());

        {
            let mut stmt = tx.prepare(
                "INSERT INTO symbols (
                    tenant_id, project_id, file_path, name, kind,
                    line_start, line_end, col_start, col_end,
                    parent_symbol_id, signature, docstring, visibility, language
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;

            for symbol in symbols {
                stmt.execute(params![
                    symbol.tenant_id.as_str(),
                    symbol.project_id.as_deref(),
                    &symbol.file_path,
                    &symbol.name,
                    symbol.kind.as_str(),
                    symbol.line_start as i64,
                    symbol.line_end as i64,
                    symbol.col_start as i64,
                    symbol.col_end as i64,
                    symbol.parent_symbol_id,
                    symbol.signature.as_deref(),
                    symbol.docstring.as_deref(),
                    symbol.visibility.as_deref(),
                    &symbol.language,
                ])?;
                ids.push(tx.last_insert_rowid());
            }
        }

        tx.commit()?;
        Ok(ids)
    }

    /// Find symbols by exact name match.
    pub fn find_symbols_by_name(
        &self,
        tenant_id: &TenantId,
        name: &str,
    ) -> Result<Vec<SymbolRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT symbol_id, tenant_id, project_id, file_path, name, kind,
                    line_start, line_end, col_start, col_end,
                    parent_symbol_id, signature, docstring, visibility, language
             FROM symbols
             WHERE tenant_id = ?1 AND name = ?2",
        )?;

        let rows = stmt.query_map(params![tenant_id.as_str(), name], |row| {
            Self::row_to_symbol(row)
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// Find symbols by name prefix (for autocomplete).
    pub fn find_symbols_by_name_prefix(
        &self,
        tenant_id: &TenantId,
        prefix: &str,
    ) -> Result<Vec<SymbolRecord>> {
        let conn = self.conn.lock().unwrap();

        // Use LIKE with ESCAPE for safe prefix matching
        let pattern = format!("{}%", prefix.replace('%', "\\%").replace('_', "\\_"));

        let mut stmt = conn.prepare(
            "SELECT symbol_id, tenant_id, project_id, file_path, name, kind,
                    line_start, line_end, col_start, col_end,
                    parent_symbol_id, signature, docstring, visibility, language
             FROM symbols
             WHERE tenant_id = ?1 AND name LIKE ?2 ESCAPE '\\'
             ORDER BY name
             LIMIT 100",
        )?;

        let rows = stmt.query_map(params![tenant_id.as_str(), &pattern], |row| {
            Self::row_to_symbol(row)
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// Find all symbols in a file.
    pub fn find_symbols_by_file(
        &self,
        tenant_id: &TenantId,
        file_path: &str,
    ) -> Result<Vec<SymbolRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT symbol_id, tenant_id, project_id, file_path, name, kind,
                    line_start, line_end, col_start, col_end,
                    parent_symbol_id, signature, docstring, visibility, language
             FROM symbols
             WHERE tenant_id = ?1 AND file_path = ?2
             ORDER BY line_start",
        )?;

        let rows = stmt.query_map(params![tenant_id.as_str(), file_path], |row| {
            Self::row_to_symbol(row)
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// Delete all symbols for a file (for re-indexing).
    pub fn delete_file_symbols(&self, tenant_id: &TenantId, file_path: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();

        let rows_deleted = conn.execute(
            "DELETE FROM symbols WHERE tenant_id = ?1 AND file_path = ?2",
            params![tenant_id.as_str(), file_path],
        )?;

        Ok(rows_deleted)
    }

    /// Get child symbols of a parent (e.g., methods of a class).
    pub fn get_symbol_children(&self, symbol_id: i64) -> Result<Vec<SymbolRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT symbol_id, tenant_id, project_id, file_path, name, kind,
                    line_start, line_end, col_start, col_end,
                    parent_symbol_id, signature, docstring, visibility, language
             FROM symbols
             WHERE parent_symbol_id = ?1
             ORDER BY line_start",
        )?;

        let rows = stmt.query_map(params![symbol_id], |row| Self::row_to_symbol(row))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// Convert a database row to SymbolRecord.
    fn row_to_symbol(row: &rusqlite::Row) -> rusqlite::Result<SymbolRecord> {
        let symbol_id: i64 = row.get(0)?;
        let tenant_id_str: String = row.get(1)?;
        let project_id: Option<String> = row.get(2)?;
        let file_path: String = row.get(3)?;
        let name: String = row.get(4)?;
        let kind_str: String = row.get(5)?;
        let line_start: i64 = row.get(6)?;
        let line_end: i64 = row.get(7)?;
        let col_start: i64 = row.get(8)?;
        let col_end: i64 = row.get(9)?;
        let parent_symbol_id: Option<i64> = row.get(10)?;
        let signature: Option<String> = row.get(11)?;
        let docstring: Option<String> = row.get(12)?;
        let visibility: Option<String> = row.get(13)?;
        let language: String = row.get(14)?;

        // Parse tenant_id - this should not fail for valid stored data
        let tenant_id = TenantId::new(&tenant_id_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?;

        // Parse kind - default to Function if unknown
        let kind = SymbolKind::from_str(&kind_str).unwrap_or(SymbolKind::Function);

        Ok(SymbolRecord {
            symbol_id: Some(symbol_id),
            tenant_id,
            project_id,
            file_path,
            name,
            kind,
            line_start: line_start as u32,
            line_end: line_end as u32,
            col_start: col_start as u32,
            col_end: col_end as u32,
            parent_symbol_id,
            signature,
            docstring,
            visibility,
            language,
        })
    }

    // --- Call edge operations ---

    /// Insert a single call edge.
    pub fn insert_call_edge(&self, edge: &CallEdgeRecord) -> SqliteResult<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO call_edges (
                tenant_id, caller_symbol_id, callee_name, callee_symbol_id,
                call_file, call_line, call_col, call_type
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                edge.tenant_id.as_str(),
                edge.caller_symbol_id,
                edge.callee_name,
                edge.callee_symbol_id,
                edge.call_file,
                edge.call_line,
                edge.call_col,
                edge.call_type.as_str(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Insert multiple call edges in a batch.
    pub fn insert_call_edges_batch(&self, edges: &[CallEdgeRecord]) -> SqliteResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO call_edges (
                    tenant_id, caller_symbol_id, callee_name, callee_symbol_id,
                    call_file, call_line, call_col, call_type
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for edge in edges {
                stmt.execute(params![
                    edge.tenant_id.as_str(),
                    edge.caller_symbol_id,
                    edge.callee_name,
                    edge.callee_symbol_id,
                    edge.call_file,
                    edge.call_line,
                    edge.call_col,
                    edge.call_type.as_str(),
                ])?;
            }
        }
        tx.commit()
    }

    /// Find all callers of a function by name.
    pub fn find_callers(
        &self,
        tenant_id: &TenantId,
        callee_name: &str,
    ) -> SqliteResult<Vec<CallEdgeRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT edge_id, tenant_id, caller_symbol_id, callee_name, callee_symbol_id,
                    call_file, call_line, call_col, call_type
             FROM call_edges
             WHERE tenant_id = ?1 AND callee_name = ?2",
        )?;

        let rows = stmt.query_map(params![tenant_id.as_str(), callee_name], |row| {
            self.row_to_call_edge(row)
        })?;

        rows.collect()
    }

    /// Find all callers by resolved symbol ID.
    pub fn find_callers_by_symbol(
        &self,
        callee_symbol_id: i64,
    ) -> SqliteResult<Vec<CallEdgeRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT edge_id, tenant_id, caller_symbol_id, callee_name, callee_symbol_id,
                    call_file, call_line, call_col, call_type
             FROM call_edges
             WHERE callee_symbol_id = ?1",
        )?;

        let rows = stmt.query_map(params![callee_symbol_id], |row| self.row_to_call_edge(row))?;

        rows.collect()
    }

    /// Find all callees of a function by caller symbol ID.
    pub fn find_callees(&self, caller_symbol_id: i64) -> SqliteResult<Vec<CallEdgeRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT edge_id, tenant_id, caller_symbol_id, callee_name, callee_symbol_id,
                    call_file, call_line, call_col, call_type
             FROM call_edges
             WHERE caller_symbol_id = ?1",
        )?;

        let rows = stmt.query_map(params![caller_symbol_id], |row| self.row_to_call_edge(row))?;

        rows.collect()
    }

    /// Delete all call edges for a file (for re-indexing).
    pub fn delete_file_edges(&self, tenant_id: &TenantId, file_path: &str) -> SqliteResult<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM call_edges WHERE tenant_id = ?1 AND call_file = ?2",
            params![tenant_id.as_str(), file_path],
        )
    }

    fn row_to_call_edge(&self, row: &rusqlite::Row<'_>) -> SqliteResult<CallEdgeRecord> {
        let call_type_str: String = row.get(8)?;
        let tenant_str: String = row.get(1)?;

        Ok(CallEdgeRecord {
            edge_id: Some(row.get(0)?),
            tenant_id: TenantId::new(tenant_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e.to_string(),
                    )),
                )
            })?,
            caller_symbol_id: row.get(2)?,
            callee_name: row.get(3)?,
            callee_symbol_id: row.get(4)?,
            call_file: row.get(5)?,
            call_line: row.get(6)?,
            call_col: row.get(7)?,
            call_type: CallType::from_str(&call_type_str).unwrap_or(CallType::Direct),
        })
    }

    // --- Import operations ---

    /// Insert a single import record.
    pub fn insert_import(&self, import: &ImportRecord) -> SqliteResult<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO imports (
                tenant_id, source_file, imported_module, imported_name,
                alias, import_line, is_relative
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                import.tenant_id.as_str(),
                import.source_file,
                import.imported_module,
                import.imported_name,
                import.alias,
                import.import_line,
                import.is_relative as i32,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Insert multiple import records in a batch.
    pub fn insert_imports_batch(&self, imports: &[ImportRecord]) -> SqliteResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO imports (
                    tenant_id, source_file, imported_module, imported_name,
                    alias, import_line, is_relative
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for import in imports {
                stmt.execute(params![
                    import.tenant_id.as_str(),
                    import.source_file,
                    import.imported_module,
                    import.imported_name,
                    import.alias,
                    import.import_line,
                    import.is_relative as i32,
                ])?;
            }
        }
        tx.commit()
    }

    /// Find all imports in a file.
    pub fn find_imports_by_file(
        &self,
        tenant_id: &TenantId,
        file_path: &str,
    ) -> SqliteResult<Vec<ImportRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT import_id, tenant_id, source_file, imported_module, imported_name,
                    alias, import_line, is_relative
             FROM imports
             WHERE tenant_id = ?1 AND source_file = ?2",
        )?;

        let rows = stmt.query_map(params![tenant_id.as_str(), file_path], |row| {
            self.row_to_import(row)
        })?;

        rows.collect()
    }

    /// Find all files that import a module.
    pub fn find_importers(
        &self,
        tenant_id: &TenantId,
        module: &str,
    ) -> SqliteResult<Vec<ImportRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT import_id, tenant_id, source_file, imported_module, imported_name,
                    alias, import_line, is_relative
             FROM imports
             WHERE tenant_id = ?1 AND imported_module = ?2",
        )?;

        let rows = stmt.query_map(params![tenant_id.as_str(), module], |row| {
            self.row_to_import(row)
        })?;

        rows.collect()
    }

    /// Delete all imports for a file (for re-indexing).
    pub fn delete_file_imports(
        &self,
        tenant_id: &TenantId,
        file_path: &str,
    ) -> SqliteResult<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM imports WHERE tenant_id = ?1 AND source_file = ?2",
            params![tenant_id.as_str(), file_path],
        )
    }

    fn row_to_import(&self, row: &rusqlite::Row<'_>) -> SqliteResult<ImportRecord> {
        let tenant_str: String = row.get(1)?;
        let is_relative_int: i32 = row.get(7)?;

        Ok(ImportRecord {
            import_id: Some(row.get(0)?),
            tenant_id: TenantId::new(tenant_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e.to_string(),
                    )),
                )
            })?,
            source_file: row.get(2)?,
            imported_module: row.get(3)?,
            imported_name: row.get(4)?,
            alias: row.get(5)?,
            import_line: row.get(6)?,
            is_relative: is_relative_int != 0,
        })
    }

    // --- Tool trace operations ---

    /// Insert a tool trace record.
    ///
    /// Returns the assigned trace_id.
    pub fn insert_tool_trace(&self, trace: &ToolTraceRecord) -> SqliteResult<i64> {
        let conn = self.conn.lock().unwrap();
        let tags_json = serde_json::to_string(&trace.context_tags).unwrap_or_else(|_| "[]".into());

        conn.execute(
            r#"
            INSERT INTO tool_traces
                (tenant_id, project_id, session_id, tool_name, timestamp_ms,
                 input_json, output_json, error_json, context_tags, duration_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                trace.tenant_id.as_str(),
                trace.project_id.as_deref(),
                trace.session_id.as_deref(),
                trace.tool_name,
                trace.timestamp_ms,
                trace.input_json.as_deref(),
                trace.output_json.as_deref(),
                trace.error_json.as_deref(),
                tags_json,
                trace.duration_ms,
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Insert multiple tool traces in a batch.
    pub fn insert_tool_traces_batch(&self, traces: &[ToolTraceRecord]) -> SqliteResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO tool_traces
                    (tenant_id, project_id, session_id, tool_name, timestamp_ms,
                     input_json, output_json, error_json, context_tags, duration_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
            )?;

            for trace in traces {
                let tags_json =
                    serde_json::to_string(&trace.context_tags).unwrap_or_else(|_| "[]".into());
                stmt.execute(params![
                    trace.tenant_id.as_str(),
                    trace.project_id.as_deref(),
                    trace.session_id.as_deref(),
                    trace.tool_name,
                    trace.timestamp_ms,
                    trace.input_json.as_deref(),
                    trace.output_json.as_deref(),
                    trace.error_json.as_deref(),
                    tags_json,
                    trace.duration_ms,
                ])?;
            }
        }

        tx.commit()
    }

    /// Find tool traces by tenant, optionally filtered by tool name and time range.
    pub fn find_tool_traces(
        &self,
        tenant_id: &TenantId,
        tool_name: Option<&str>,
        time_range: Option<TimeRange>,
    ) -> SqliteResult<Vec<ToolTraceRecord>> {
        let conn = self.conn.lock().unwrap();
        let time_range = time_range.unwrap_or_default();

        let mut sql = String::from(
            r#"
            SELECT trace_id, tenant_id, project_id, session_id, tool_name, timestamp_ms,
                   input_json, output_json, error_json, context_tags, duration_ms
            FROM tool_traces
            WHERE tenant_id = ?
            "#,
        );

        let mut param_idx = 2;
        if tool_name.is_some() {
            sql.push_str(&format!(" AND tool_name = ?{}", param_idx));
            param_idx += 1;
        }
        if time_range.from_ms.is_some() {
            sql.push_str(&format!(" AND timestamp_ms >= ?{}", param_idx));
            param_idx += 1;
        }
        if time_range.to_ms.is_some() {
            sql.push_str(&format!(" AND timestamp_ms <= ?{}", param_idx));
        }
        sql.push_str(" ORDER BY timestamp_ms DESC");

        let mut stmt = conn.prepare(&sql)?;

        // Build parameter list dynamically
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(tenant_id.as_str().to_string())];
        if let Some(name) = tool_name {
            params_vec.push(Box::new(name.to_string()));
        }
        if let Some(from) = time_range.from_ms {
            params_vec.push(Box::new(from));
        }
        if let Some(to) = time_range.to_ms {
            params_vec.push(Box::new(to));
        }

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(params_refs.as_slice(), |row| self.row_to_tool_trace(row))?;

        rows.collect()
    }

    /// Find tool traces by session ID.
    pub fn find_tool_traces_by_session(
        &self,
        session_id: &str,
    ) -> SqliteResult<Vec<ToolTraceRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            r#"
            SELECT trace_id, tenant_id, project_id, session_id, tool_name, timestamp_ms,
                   input_json, output_json, error_json, context_tags, duration_ms
            FROM tool_traces
            WHERE session_id = ?1
            ORDER BY timestamp_ms DESC
            "#,
        )?;

        let rows = stmt.query_map([session_id], |row| self.row_to_tool_trace(row))?;
        rows.collect()
    }

    /// Find tool traces that have errors.
    pub fn find_tool_traces_with_error(
        &self,
        tenant_id: &TenantId,
    ) -> SqliteResult<Vec<ToolTraceRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            r#"
            SELECT trace_id, tenant_id, project_id, session_id, tool_name, timestamp_ms,
                   input_json, output_json, error_json, context_tags, duration_ms
            FROM tool_traces
            WHERE tenant_id = ?1 AND error_json IS NOT NULL
            ORDER BY timestamp_ms DESC
            "#,
        )?;

        let rows = stmt.query_map([tenant_id.as_str()], |row| self.row_to_tool_trace(row))?;
        rows.collect()
    }

    fn row_to_tool_trace(&self, row: &rusqlite::Row) -> SqliteResult<ToolTraceRecord> {
        let tenant_str: String = row.get(1)?;
        let tags_json: Option<String> = row.get(9)?;

        let context_tags: Vec<String> = tags_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        Ok(ToolTraceRecord {
            trace_id: Some(row.get(0)?),
            tenant_id: TenantId::new(&tenant_str).unwrap_or_else(|_| {
                // Fallback for corrupted data
                TenantId::new("unknown").unwrap()
            }),
            project_id: row.get(2)?,
            session_id: row.get(3)?,
            tool_name: row.get(4)?,
            timestamp_ms: row.get(5)?,
            input_json: row.get(6)?,
            output_json: row.get(7)?,
            error_json: row.get(8)?,
            context_tags,
            duration_ms: row.get(10)?,
        })
    }

    // --- Stack trace operations ---

    /// Insert a stack trace with its frames.
    ///
    /// Inserts both the trace and all frames in a single transaction.
    /// Returns the assigned trace_id.
    pub fn insert_stack_trace(
        &self,
        trace: &StackTraceRecord,
        frames: &[StackFrameRecord],
    ) -> SqliteResult<i64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        tx.execute(
            r#"
            INSERT INTO stack_traces
                (tenant_id, project_id, session_id, timestamp_ms,
                 error_signature, error_message, full_trace)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                trace.tenant_id.as_str(),
                trace.project_id.as_deref(),
                trace.session_id.as_deref(),
                trace.timestamp_ms,
                trace.error_signature,
                trace.error_message,
                trace.full_trace,
            ],
        )?;

        let trace_id = tx.last_insert_rowid();

        // Insert frames
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO stack_frames
                    (trace_id, frame_idx, file_path, function_name,
                     line_number, col_number, context)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
            )?;

            for frame in frames {
                stmt.execute(params![
                    trace_id,
                    frame.frame_idx,
                    frame.file_path.as_deref(),
                    frame.function_name.as_deref(),
                    frame.line_number,
                    frame.col_number,
                    frame.context.as_deref(),
                ])?;
            }
        }

        tx.commit()?;
        Ok(trace_id)
    }

    /// Find stack traces by tenant, optionally filtered by error signature and time range.
    pub fn find_stack_traces(
        &self,
        tenant_id: &TenantId,
        error_signature: Option<&str>,
        time_range: Option<TimeRange>,
    ) -> SqliteResult<Vec<StackTraceRecord>> {
        let conn = self.conn.lock().unwrap();
        let time_range = time_range.unwrap_or_default();

        let mut sql = String::from(
            r#"
            SELECT trace_id, tenant_id, project_id, session_id, timestamp_ms,
                   error_signature, error_message, full_trace
            FROM stack_traces
            WHERE tenant_id = ?
            "#,
        );

        let mut param_idx = 2;
        if error_signature.is_some() {
            sql.push_str(&format!(" AND error_signature = ?{}", param_idx));
            param_idx += 1;
        }
        if time_range.from_ms.is_some() {
            sql.push_str(&format!(" AND timestamp_ms >= ?{}", param_idx));
            param_idx += 1;
        }
        if time_range.to_ms.is_some() {
            sql.push_str(&format!(" AND timestamp_ms <= ?{}", param_idx));
        }
        sql.push_str(" ORDER BY timestamp_ms DESC");

        let mut stmt = conn.prepare(&sql)?;

        // Build parameter list dynamically
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(tenant_id.as_str().to_string())];
        if let Some(sig) = error_signature {
            params_vec.push(Box::new(sig.to_string()));
        }
        if let Some(from) = time_range.from_ms {
            params_vec.push(Box::new(from));
        }
        if let Some(to) = time_range.to_ms {
            params_vec.push(Box::new(to));
        }

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(params_refs.as_slice(), |row| self.row_to_stack_trace(row))?;

        rows.collect()
    }

    /// Find stack traces where a specific function appears in the stack.
    pub fn find_stack_traces_by_function(
        &self,
        tenant_id: &TenantId,
        function_name: &str,
    ) -> SqliteResult<Vec<StackTraceRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            r#"
            SELECT DISTINCT st.trace_id, st.tenant_id, st.project_id, st.session_id,
                   st.timestamp_ms, st.error_signature, st.error_message, st.full_trace
            FROM stack_traces st
            INNER JOIN stack_frames sf ON st.trace_id = sf.trace_id
            WHERE st.tenant_id = ?1 AND sf.function_name = ?2
            ORDER BY st.timestamp_ms DESC
            "#,
        )?;

        let rows = stmt.query_map(params![tenant_id.as_str(), function_name], |row| {
            self.row_to_stack_trace(row)
        })?;

        rows.collect()
    }

    /// Get all frames for a stack trace.
    pub fn get_stack_frames(&self, trace_id: i64) -> SqliteResult<Vec<StackFrameRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            r#"
            SELECT frame_id, trace_id, frame_idx, file_path, function_name,
                   line_number, col_number, context
            FROM stack_frames
            WHERE trace_id = ?1
            ORDER BY frame_idx ASC
            "#,
        )?;

        let rows = stmt.query_map([trace_id], |row| {
            Ok(StackFrameRecord {
                frame_id: Some(row.get(0)?),
                trace_id: row.get(1)?,
                frame_idx: row.get(2)?,
                file_path: row.get(3)?,
                function_name: row.get(4)?,
                line_number: row.get::<_, Option<i32>>(5)?.map(|n| n as u32),
                col_number: row.get::<_, Option<i32>>(6)?.map(|n| n as u32),
                context: row.get(7)?,
            })
        })?;

        rows.collect()
    }

    fn row_to_stack_trace(&self, row: &rusqlite::Row) -> SqliteResult<StackTraceRecord> {
        let tenant_str: String = row.get(1)?;

        Ok(StackTraceRecord {
            trace_id: Some(row.get(0)?),
            tenant_id: TenantId::new(&tenant_str)
                .unwrap_or_else(|_| TenantId::new("unknown").unwrap()),
            project_id: row.get(2)?,
            session_id: row.get(3)?,
            timestamp_ms: row.get(4)?,
            error_signature: row.get(5)?,
            error_message: row.get(6)?,
            full_trace: row.get(7)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    fn create_test_symbol(tenant: &str, name: &str, kind: SymbolKind) -> SymbolRecord {
        SymbolRecord {
            symbol_id: None,
            tenant_id: TenantId::new(tenant).unwrap(),
            project_id: None,
            file_path: "src/main.rs".to_string(),
            name: name.to_string(),
            kind,
            line_start: 10,
            line_end: 20,
            col_start: 0,
            col_end: 1,
            parent_symbol_id: None,
            signature: Some("fn main() -> ()".to_string()),
            docstring: Some("Entry point".to_string()),
            visibility: Some("public".to_string()),
            language: "rust".to_string(),
        }
    }

    #[test]
    fn test_insert_and_find_symbol() {
        let store = StructuralStore::in_memory().unwrap();

        let symbol = create_test_symbol("tenant_a", "main", SymbolKind::Function);
        let id = store.insert_symbol(&symbol).unwrap();
        assert!(id > 0);

        let tenant_id = TenantId::new("tenant_a").unwrap();
        let found = store.find_symbols_by_name(&tenant_id, "main").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "main");
        assert_eq!(found[0].kind, SymbolKind::Function);
        assert_eq!(found[0].signature, Some("fn main() -> ()".to_string()));
    }

    #[test]
    fn test_find_symbols_by_name_prefix() {
        let store = StructuralStore::in_memory().unwrap();

        // Insert symbols with similar names
        store
            .insert_symbol(&create_test_symbol(
                "tenant_a",
                "process_data",
                SymbolKind::Function,
            ))
            .unwrap();
        store
            .insert_symbol(&create_test_symbol(
                "tenant_a",
                "process_file",
                SymbolKind::Function,
            ))
            .unwrap();
        store
            .insert_symbol(&create_test_symbol(
                "tenant_a",
                "parse_input",
                SymbolKind::Function,
            ))
            .unwrap();

        let tenant_id = TenantId::new("tenant_a").unwrap();

        // Find by prefix "process"
        let found = store
            .find_symbols_by_name_prefix(&tenant_id, "process")
            .unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|s| s.name.starts_with("process")));

        // Find by prefix "p"
        let found = store.find_symbols_by_name_prefix(&tenant_id, "p").unwrap();
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn test_symbol_tenant_isolation() {
        let store = StructuralStore::in_memory().unwrap();

        // Insert symbol for tenant_a
        store
            .insert_symbol(&create_test_symbol(
                "tenant_a",
                "secret_fn",
                SymbolKind::Function,
            ))
            .unwrap();

        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();

        // Tenant A can find their symbol
        let found_a = store.find_symbols_by_name(&tenant_a, "secret_fn").unwrap();
        assert_eq!(found_a.len(), 1);

        // Tenant B cannot find tenant A's symbol
        let found_b = store.find_symbols_by_name(&tenant_b, "secret_fn").unwrap();
        assert_eq!(found_b.len(), 0);
    }

    #[test]
    fn test_delete_file_symbols() {
        let store = StructuralStore::in_memory().unwrap();

        // Insert symbols for different files
        let mut symbol1 = create_test_symbol("tenant_a", "fn1", SymbolKind::Function);
        symbol1.file_path = "src/lib.rs".to_string();
        store.insert_symbol(&symbol1).unwrap();

        let mut symbol2 = create_test_symbol("tenant_a", "fn2", SymbolKind::Function);
        symbol2.file_path = "src/lib.rs".to_string();
        store.insert_symbol(&symbol2).unwrap();

        let mut symbol3 = create_test_symbol("tenant_a", "fn3", SymbolKind::Function);
        symbol3.file_path = "src/other.rs".to_string();
        store.insert_symbol(&symbol3).unwrap();

        let tenant_id = TenantId::new("tenant_a").unwrap();

        // Verify all symbols exist
        let lib_symbols = store
            .find_symbols_by_file(&tenant_id, "src/lib.rs")
            .unwrap();
        assert_eq!(lib_symbols.len(), 2);

        // Delete symbols for src/lib.rs
        let deleted = store.delete_file_symbols(&tenant_id, "src/lib.rs").unwrap();
        assert_eq!(deleted, 2);

        // Verify lib.rs symbols are gone
        let lib_symbols = store
            .find_symbols_by_file(&tenant_id, "src/lib.rs")
            .unwrap();
        assert_eq!(lib_symbols.len(), 0);

        // Verify other.rs symbols still exist
        let other_symbols = store
            .find_symbols_by_file(&tenant_id, "src/other.rs")
            .unwrap();
        assert_eq!(other_symbols.len(), 1);
    }

    #[test]
    fn test_nested_symbols() {
        let store = StructuralStore::in_memory().unwrap();

        // Insert a class
        let class_symbol = SymbolRecord {
            symbol_id: None,
            tenant_id: TenantId::new("tenant_a").unwrap(),
            project_id: None,
            file_path: "src/user.rs".to_string(),
            name: "User".to_string(),
            kind: SymbolKind::Class,
            line_start: 1,
            line_end: 50,
            col_start: 0,
            col_end: 1,
            parent_symbol_id: None,
            signature: None,
            docstring: Some("User struct".to_string()),
            visibility: Some("public".to_string()),
            language: "rust".to_string(),
        };
        let class_id = store.insert_symbol(&class_symbol).unwrap();

        // Insert methods with class as parent
        let method1 = SymbolRecord {
            symbol_id: None,
            tenant_id: TenantId::new("tenant_a").unwrap(),
            project_id: None,
            file_path: "src/user.rs".to_string(),
            name: "new".to_string(),
            kind: SymbolKind::Method,
            line_start: 5,
            line_end: 10,
            col_start: 4,
            col_end: 5,
            parent_symbol_id: Some(class_id),
            signature: Some("fn new() -> Self".to_string()),
            docstring: None,
            visibility: Some("public".to_string()),
            language: "rust".to_string(),
        };
        store.insert_symbol(&method1).unwrap();

        let method2 = SymbolRecord {
            symbol_id: None,
            tenant_id: TenantId::new("tenant_a").unwrap(),
            project_id: None,
            file_path: "src/user.rs".to_string(),
            name: "get_name".to_string(),
            kind: SymbolKind::Method,
            line_start: 15,
            line_end: 20,
            col_start: 4,
            col_end: 5,
            parent_symbol_id: Some(class_id),
            signature: Some("fn get_name(&self) -> &str".to_string()),
            docstring: None,
            visibility: Some("public".to_string()),
            language: "rust".to_string(),
        };
        store.insert_symbol(&method2).unwrap();

        // Get children of the class
        let children = store.get_symbol_children(class_id).unwrap();
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|s| s.kind == SymbolKind::Method));
        assert!(children
            .iter()
            .all(|s| s.parent_symbol_id == Some(class_id)));

        // Verify ordering by line
        assert_eq!(children[0].name, "new");
        assert_eq!(children[1].name, "get_name");
    }

    #[test]
    fn test_batch_insert_symbols() {
        let store = StructuralStore::in_memory().unwrap();

        let symbols: Vec<SymbolRecord> = (0..100)
            .map(|i| SymbolRecord {
                symbol_id: None,
                tenant_id: TenantId::new("tenant_a").unwrap(),
                project_id: None,
                file_path: "src/big.rs".to_string(),
                name: format!("func_{}", i),
                kind: SymbolKind::Function,
                line_start: i * 10,
                line_end: i * 10 + 5,
                col_start: 0,
                col_end: 1,
                parent_symbol_id: None,
                signature: None,
                docstring: None,
                visibility: None,
                language: "rust".to_string(),
            })
            .collect();

        let ids = store.insert_symbols_batch(&symbols).unwrap();
        assert_eq!(ids.len(), 100);

        // Verify all are unique and positive
        let unique_ids: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique_ids.len(), 100);
        assert!(ids.iter().all(|&id| id > 0));

        // Verify we can find them
        let tenant_id = TenantId::new("tenant_a").unwrap();
        let found = store
            .find_symbols_by_file(&tenant_id, "src/big.rs")
            .unwrap();
        assert_eq!(found.len(), 100);
    }

    #[test]
    fn test_insert_call_edge() {
        let store = StructuralStore::in_memory().unwrap();
        let edge = CallEdgeRecord {
            edge_id: None,
            tenant_id: test_tenant(),
            caller_symbol_id: 1,
            callee_name: "helper".to_string(),
            callee_symbol_id: None,
            call_file: "src/main.rs".to_string(),
            call_line: 10,
            call_col: 5,
            call_type: CallType::Direct,
        };

        let id = store.insert_call_edge(&edge).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_find_callers_by_name() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        // Insert two edges calling the same function
        let edges = vec![
            CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: 1,
                callee_name: "shared_func".to_string(),
                callee_symbol_id: None,
                call_file: "src/a.rs".to_string(),
                call_line: 10,
                call_col: 5,
                call_type: CallType::Direct,
            },
            CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: 2,
                callee_name: "shared_func".to_string(),
                callee_symbol_id: None,
                call_file: "src/b.rs".to_string(),
                call_line: 20,
                call_col: 10,
                call_type: CallType::Method,
            },
        ];
        store.insert_call_edges_batch(&edges).unwrap();

        let callers = store.find_callers(&tenant, "shared_func").unwrap();
        assert_eq!(callers.len(), 2);
        assert!(callers.iter().any(|e| e.caller_symbol_id == 1));
        assert!(callers.iter().any(|e| e.caller_symbol_id == 2));
    }

    #[test]
    fn test_find_callees() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        // Insert edges from caller_symbol_id 1 to multiple callees
        let edges = vec![
            CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: 1,
                callee_name: "func_a".to_string(),
                callee_symbol_id: None,
                call_file: "src/main.rs".to_string(),
                call_line: 10,
                call_col: 5,
                call_type: CallType::Direct,
            },
            CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: 1,
                callee_name: "func_b".to_string(),
                callee_symbol_id: None,
                call_file: "src/main.rs".to_string(),
                call_line: 15,
                call_col: 5,
                call_type: CallType::Qualified,
            },
        ];
        store.insert_call_edges_batch(&edges).unwrap();

        let callees = store.find_callees(1).unwrap();
        assert_eq!(callees.len(), 2);
        assert!(callees.iter().any(|e| e.callee_name == "func_a"));
        assert!(callees.iter().any(|e| e.callee_name == "func_b"));
    }

    #[test]
    fn test_delete_file_edges() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let edges = vec![
            CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: 1,
                callee_name: "func_a".to_string(),
                callee_symbol_id: None,
                call_file: "src/main.rs".to_string(),
                call_line: 10,
                call_col: 5,
                call_type: CallType::Direct,
            },
            CallEdgeRecord {
                edge_id: None,
                tenant_id: tenant.clone(),
                caller_symbol_id: 2,
                callee_name: "func_b".to_string(),
                callee_symbol_id: None,
                call_file: "src/other.rs".to_string(),
                call_line: 20,
                call_col: 10,
                call_type: CallType::Direct,
            },
        ];
        store.insert_call_edges_batch(&edges).unwrap();

        let deleted = store.delete_file_edges(&tenant, "src/main.rs").unwrap();
        assert_eq!(deleted, 1);

        // Verify the other file's edges remain
        let remaining = store.find_callers(&tenant, "func_b").unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_insert_import() {
        let store = StructuralStore::in_memory().unwrap();
        let import = ImportRecord {
            import_id: None,
            tenant_id: test_tenant(),
            source_file: "src/main.py".to_string(),
            imported_module: "os".to_string(),
            imported_name: Some("path".to_string()),
            alias: None,
            import_line: 1,
            is_relative: false,
        };

        let id = store.insert_import(&import).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_find_importers() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let imports = vec![
            ImportRecord {
                import_id: None,
                tenant_id: tenant.clone(),
                source_file: "src/a.py".to_string(),
                imported_module: "json".to_string(),
                imported_name: None,
                alias: None,
                import_line: 1,
                is_relative: false,
            },
            ImportRecord {
                import_id: None,
                tenant_id: tenant.clone(),
                source_file: "src/b.py".to_string(),
                imported_module: "json".to_string(),
                imported_name: Some("dumps".to_string()),
                alias: None,
                import_line: 2,
                is_relative: false,
            },
        ];
        store.insert_imports_batch(&imports).unwrap();

        let importers = store.find_importers(&tenant, "json").unwrap();
        assert_eq!(importers.len(), 2);
        assert!(importers.iter().any(|i| i.source_file == "src/a.py"));
        assert!(importers.iter().any(|i| i.source_file == "src/b.py"));
    }

    #[test]
    fn test_find_imports_by_file() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let imports = vec![
            ImportRecord {
                import_id: None,
                tenant_id: tenant.clone(),
                source_file: "src/main.py".to_string(),
                imported_module: "os".to_string(),
                imported_name: None,
                alias: None,
                import_line: 1,
                is_relative: false,
            },
            ImportRecord {
                import_id: None,
                tenant_id: tenant.clone(),
                source_file: "src/main.py".to_string(),
                imported_module: "sys".to_string(),
                imported_name: None,
                alias: None,
                import_line: 2,
                is_relative: false,
            },
        ];
        store.insert_imports_batch(&imports).unwrap();

        let file_imports = store.find_imports_by_file(&tenant, "src/main.py").unwrap();
        assert_eq!(file_imports.len(), 2);
    }

    #[test]
    fn test_delete_file_imports() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let imports = vec![
            ImportRecord {
                import_id: None,
                tenant_id: tenant.clone(),
                source_file: "src/main.py".to_string(),
                imported_module: "os".to_string(),
                imported_name: None,
                alias: None,
                import_line: 1,
                is_relative: false,
            },
            ImportRecord {
                import_id: None,
                tenant_id: tenant.clone(),
                source_file: "src/other.py".to_string(),
                imported_module: "sys".to_string(),
                imported_name: None,
                alias: None,
                import_line: 1,
                is_relative: false,
            },
        ];
        store.insert_imports_batch(&imports).unwrap();

        let deleted = store.delete_file_imports(&tenant, "src/main.py").unwrap();
        assert_eq!(deleted, 1);

        // Verify other file's imports remain
        let remaining = store.find_imports_by_file(&tenant, "src/other.py").unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_relative_import() {
        let store = StructuralStore::in_memory().unwrap();
        let import = ImportRecord {
            import_id: None,
            tenant_id: test_tenant(),
            source_file: "src/submodule/main.py".to_string(),
            imported_module: ".utils".to_string(),
            imported_name: Some("helper".to_string()),
            alias: Some("h".to_string()),
            import_line: 3,
            is_relative: true,
        };

        let id = store.insert_import(&import).unwrap();
        assert!(id > 0);

        let imports = store
            .find_imports_by_file(&import.tenant_id, &import.source_file)
            .unwrap();
        assert_eq!(imports.len(), 1);
        assert!(imports[0].is_relative);
        assert_eq!(imports[0].alias.as_deref(), Some("h"));
    }

    // --- Tool trace tests ---

    #[test]
    fn test_insert_tool_trace() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let mut trace = ToolTraceRecord::new(tenant.clone(), "read_file", 1000);
        trace.input_json = Some(r#"{"path": "/test.rs"}"#.to_string());
        trace.output_json = Some(r#"{"content": "fn main() {}"}"#.to_string());
        trace.session_id = Some("session_1".to_string());
        trace.context_tags = vec!["rust".to_string(), "code".to_string()];
        trace.duration_ms = Some(50);

        let trace_id = store.insert_tool_trace(&trace).unwrap();
        assert!(trace_id > 0);

        // Retrieve and verify
        let traces = store
            .find_tool_traces(&tenant, Some("read_file"), None)
            .unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].tool_name, "read_file");
        assert_eq!(traces[0].context_tags, vec!["rust", "code"]);
    }

    #[test]
    fn test_find_tool_traces_by_name() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        // Insert traces for different tools
        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "read_file", 1000))
            .unwrap();
        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "read_file", 2000))
            .unwrap();
        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "write_file", 3000))
            .unwrap();

        let read_traces = store
            .find_tool_traces(&tenant, Some("read_file"), None)
            .unwrap();
        assert_eq!(read_traces.len(), 2);

        let write_traces = store
            .find_tool_traces(&tenant, Some("write_file"), None)
            .unwrap();
        assert_eq!(write_traces.len(), 1);
    }

    #[test]
    fn test_find_tool_traces_by_time_range() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 1000))
            .unwrap();
        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 2000))
            .unwrap();
        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant.clone(), "tool", 3000))
            .unwrap();

        // Range from 1500 to 2500 should only include the 2000 trace
        let traces = store
            .find_tool_traces(&tenant, None, Some(TimeRange::between(1500, 2500)))
            .unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].timestamp_ms, 2000);

        // From 1500 onwards should include 2000 and 3000
        let traces = store
            .find_tool_traces(&tenant, None, Some(TimeRange::from(1500)))
            .unwrap();
        assert_eq!(traces.len(), 2);
    }

    #[test]
    fn test_find_tool_traces_by_session() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let mut trace1 = ToolTraceRecord::new(tenant.clone(), "tool", 1000);
        trace1.session_id = Some("session_a".to_string());

        let mut trace2 = ToolTraceRecord::new(tenant.clone(), "tool", 2000);
        trace2.session_id = Some("session_b".to_string());

        let mut trace3 = ToolTraceRecord::new(tenant.clone(), "tool", 3000);
        trace3.session_id = Some("session_a".to_string());

        store.insert_tool_trace(&trace1).unwrap();
        store.insert_tool_trace(&trace2).unwrap();
        store.insert_tool_trace(&trace3).unwrap();

        let session_a_traces = store.find_tool_traces_by_session("session_a").unwrap();
        assert_eq!(session_a_traces.len(), 2);

        let session_b_traces = store.find_tool_traces_by_session("session_b").unwrap();
        assert_eq!(session_b_traces.len(), 1);
    }

    #[test]
    fn test_find_tool_traces_with_error() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let mut success = ToolTraceRecord::new(tenant.clone(), "tool", 1000);
        success.output_json = Some("result".to_string());

        let mut error = ToolTraceRecord::new(tenant.clone(), "tool", 2000);
        error.error_json = Some(r#"{"type": "NotFound"}"#.to_string());

        store.insert_tool_trace(&success).unwrap();
        store.insert_tool_trace(&error).unwrap();

        let error_traces = store.find_tool_traces_with_error(&tenant).unwrap();
        assert_eq!(error_traces.len(), 1);
        assert!(error_traces[0].has_error());
    }

    // --- Stack trace tests ---

    #[test]
    fn test_insert_stack_trace_with_frames() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let trace = StackTraceRecord::new(
            tenant.clone(),
            1000,
            "NullPointerException",
            "Cannot read property 'x' of null",
            "Error at foo.js:10\n  at bar.js:20\n  at main.js:5",
        );

        let frames = vec![
            StackFrameRecord {
                frame_id: None,
                trace_id: 0, // Will be set during insert
                frame_idx: 0,
                file_path: Some("foo.js".to_string()),
                function_name: Some("processData".to_string()),
                line_number: Some(10),
                col_number: Some(5),
                context: None,
            },
            StackFrameRecord {
                frame_id: None,
                trace_id: 0,
                frame_idx: 1,
                file_path: Some("bar.js".to_string()),
                function_name: Some("handleRequest".to_string()),
                line_number: Some(20),
                col_number: None,
                context: None,
            },
            StackFrameRecord {
                frame_id: None,
                trace_id: 0,
                frame_idx: 2,
                file_path: Some("main.js".to_string()),
                function_name: Some("main".to_string()),
                line_number: Some(5),
                col_number: None,
                context: None,
            },
        ];

        let trace_id = store.insert_stack_trace(&trace, &frames).unwrap();
        assert!(trace_id > 0);

        // Verify trace was stored
        let traces = store.find_stack_traces(&tenant, None, None).unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].error_signature, "NullPointerException");

        // Verify frames were stored
        let stored_frames = store.get_stack_frames(trace_id).unwrap();
        assert_eq!(stored_frames.len(), 3);
        assert_eq!(
            stored_frames[0].function_name.as_deref(),
            Some("processData")
        );
        assert_eq!(
            stored_frames[1].function_name.as_deref(),
            Some("handleRequest")
        );
        assert_eq!(stored_frames[2].function_name.as_deref(), Some("main"));
    }

    #[test]
    fn test_find_stack_traces_by_function() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        // Insert a trace with specific functions
        let trace = StackTraceRecord::new(
            tenant.clone(),
            1000,
            "Error",
            "Something went wrong",
            "trace",
        );

        let frames = vec![StackFrameRecord {
            frame_id: None,
            trace_id: 0,
            frame_idx: 0,
            file_path: None,
            function_name: Some("target_function".to_string()),
            line_number: None,
            col_number: None,
            context: None,
        }];

        store.insert_stack_trace(&trace, &frames).unwrap();

        // Another trace without the target function
        let trace2 = StackTraceRecord::new(tenant.clone(), 2000, "Error2", "Other error", "trace2");
        let frames2 = vec![StackFrameRecord {
            frame_id: None,
            trace_id: 0,
            frame_idx: 0,
            file_path: None,
            function_name: Some("other_function".to_string()),
            line_number: None,
            col_number: None,
            context: None,
        }];
        store.insert_stack_trace(&trace2, &frames2).unwrap();

        // Find by function name
        let traces = store
            .find_stack_traces_by_function(&tenant, "target_function")
            .unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].error_signature, "Error");

        // Should not find the other trace
        let traces = store
            .find_stack_traces_by_function(&tenant, "other_function")
            .unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].error_signature, "Error2");
    }

    #[test]
    fn test_find_stack_traces_by_error_signature() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant = test_tenant();

        let trace1 = StackTraceRecord::new(
            tenant.clone(),
            1000,
            "TypeError",
            "null is not a function",
            "t1",
        );
        let trace2 = StackTraceRecord::new(
            tenant.clone(),
            2000,
            "TypeError",
            "undefined is not a function",
            "t2",
        );
        let trace3 = StackTraceRecord::new(
            tenant.clone(),
            3000,
            "ReferenceError",
            "x is not defined",
            "t3",
        );

        store.insert_stack_trace(&trace1, &[]).unwrap();
        store.insert_stack_trace(&trace2, &[]).unwrap();
        store.insert_stack_trace(&trace3, &[]).unwrap();

        let type_errors = store
            .find_stack_traces(&tenant, Some("TypeError"), None)
            .unwrap();
        assert_eq!(type_errors.len(), 2);

        let ref_errors = store
            .find_stack_traces(&tenant, Some("ReferenceError"), None)
            .unwrap();
        assert_eq!(ref_errors.len(), 1);
    }

    #[test]
    fn test_trace_tenant_isolation() {
        let store = StructuralStore::in_memory().unwrap();
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();

        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant_a.clone(), "tool", 1000))
            .unwrap();
        store
            .insert_tool_trace(&ToolTraceRecord::new(tenant_b.clone(), "tool", 2000))
            .unwrap();

        let a_traces = store.find_tool_traces(&tenant_a, None, None).unwrap();
        let b_traces = store.find_tool_traces(&tenant_b, None, None).unwrap();

        assert_eq!(a_traces.len(), 1);
        assert_eq!(b_traces.len(), 1);
        assert_eq!(a_traces[0].timestamp_ms, 1000);
        assert_eq!(b_traces[0].timestamp_ms, 2000);
    }
}
