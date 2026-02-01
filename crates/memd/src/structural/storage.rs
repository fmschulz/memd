//! SQLite storage for structural index data.
//!
//! Provides persistent storage for call graph edges and import relationships
//! extracted from source code AST.

use rusqlite::{params, Connection, Result as SqliteResult};
use std::path::Path;
use std::sync::Mutex;

use crate::types::TenantId;

/// Schema for structural index tables.
const STRUCTURAL_SCHEMA: &str = r#"
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
"#;

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

/// SQLite-backed structural index store.
pub struct StructuralStore {
    conn: Mutex<Connection>,
}

impl StructuralStore {
    /// Open or create a structural store at the given path.
    pub fn open(path: &Path) -> SqliteResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(STRUCTURAL_SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory store for testing.
    pub fn in_memory() -> SqliteResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(STRUCTURAL_SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
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
    pub fn find_callers_by_symbol(&self, callee_symbol_id: i64) -> SqliteResult<Vec<CallEdgeRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT edge_id, tenant_id, caller_symbol_id, callee_name, callee_symbol_id,
                    call_file, call_line, call_col, call_type
             FROM call_edges
             WHERE callee_symbol_id = ?1",
        )?;

        let rows = stmt.query_map(params![callee_symbol_id], |row| {
            self.row_to_call_edge(row)
        })?;

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

        let rows = stmt.query_map(params![caller_symbol_id], |row| {
            self.row_to_call_edge(row)
        })?;

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
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
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
    pub fn delete_file_imports(&self, tenant_id: &TenantId, file_path: &str) -> SqliteResult<usize> {
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
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
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

        let imports = store.find_imports_by_file(&import.tenant_id, &import.source_file).unwrap();
        assert_eq!(imports.len(), 1);
        assert!(imports[0].is_relative);
        assert_eq!(imports[0].alias.as_deref(), Some("h"));
    }
}
