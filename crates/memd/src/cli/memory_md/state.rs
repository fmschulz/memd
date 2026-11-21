use super::*;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct ProjectState {
    pub(super) generated_unix_ms: u128,
    pub(super) tenant_id: String,
    pub(super) project_id: Option<String>,
    pub(super) configured_project_dir: Option<String>,
    pub(super) resolved_project_dir: String,
    pub(super) scope_warnings: Vec<String>,
    pub(super) memory: MemoryState,
    pub(super) collection_warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub(super) struct MemoryState {
    /// Visible facts of the active project (SQL count).
    pub(super) visible_project_facts: usize,
    /// Visible facts of the tenant (SQL count).
    pub(super) visible_tenant_facts: usize,
    /// Candidate rows whose payload did not load.
    pub(super) unreadable_active_chunks: usize,
    pub(super) scan_warnings: Vec<String>,
}
