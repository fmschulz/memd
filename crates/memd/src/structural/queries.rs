//! High-level symbol and trace query API.
//!
//! Provides convenient methods for finding symbol definitions, references,
//! callers, imports, tool call traces, and stack trace errors using the
//! underlying StructuralStore.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{TimeZone, Utc};
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
                let caller_symbols =
                    self.store.find_symbols_by_name(tenant_id, &edge.callee_name);

                // Get caller info from symbol if available
                let (caller_name, caller_kind) = if let Ok(symbols) = &caller_symbols {
                    if let Some(sym) = symbols.first() {
                        (sym.name.clone(), sym.kind)
                    } else {
                        // Fallback: use edge info
                        (format!("caller_{}", edge.caller_symbol_id), SymbolKind::Function)
                    }
                } else {
                    (format!("caller_{}", edge.caller_symbol_id), SymbolKind::Function)
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
            .insert_symbol(&create_test_symbol("process_data", SymbolKind::Function, "src/lib.rs"))
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
            .insert_symbol(&create_test_symbol("Handler", SymbolKind::Class, "src/a.rs"))
            .unwrap();
        store
            .insert_symbol(&create_test_symbol("Handler", SymbolKind::Function, "src/b.rs"))
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
            .insert_symbol(&create_test_symbol("main", SymbolKind::Function, "src/main.rs"))
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
            .insert_symbol(&create_test_symbol("entry", SymbolKind::Function, "src/main.rs"))
            .unwrap();
        let middleware_id = store
            .insert_symbol(&create_test_symbol("middleware", SymbolKind::Function, "src/mid.rs"))
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
            .insert_symbol(&create_test_symbol("process", SymbolKind::Function, "src/lib.rs"))
            .unwrap();

        // Insert caller
        let caller_id = store
            .insert_symbol(&create_test_symbol("main", SymbolKind::Function, "src/main.rs"))
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
}
