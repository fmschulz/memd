use super::*;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct ProjectState {
    pub(super) generated_unix_ms: u128,
    pub(super) tenant_id: String,
    pub(super) project_id: Option<String>,
    pub(super) configured_project_dir: Option<String>,
    pub(super) resolved_project_dir: String,
    pub(super) scope_warnings: Vec<String>,
    pub(super) git: GitState,
    pub(super) latest_task: Option<StateSignal>,
    pub(super) latest_handoff: Option<StateSignal>,
    pub(super) latest_vcs: Option<StateSignal>,
    pub(super) next_actions: Vec<NextAction>,
    pub(super) task_source_state: TaskSourceState,
    pub(super) memory: MemoryState,
    pub(super) collection_warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum TaskSourceState {
    #[default]
    Missing,
    ParsedNoOpenTasks,
    ParsedOpenTasks,
    ParseFailed,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct GitState {
    pub(super) available: bool,
    pub(super) not_git_repo: bool,
    pub(super) branch: Option<String>,
    pub(super) clean: Option<bool>,
    pub(super) changed_entries: usize,
    pub(super) summary: String,
    pub(super) warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct StateSignal {
    pub(super) source_path: String,
    pub(super) line: Option<usize>,
    pub(super) heading: Option<String>,
    pub(super) text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct NextAction {
    pub(super) source_path: String,
    pub(super) line: usize,
    pub(super) heading: Option<String>,
    pub(super) text: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub(super) struct MemoryState {
    pub(super) metadata_active_chunks: Option<usize>,
    pub(super) readable_active_chunks: Option<usize>,
    pub(super) unreadable_active_chunks: Option<usize>,
    pub(super) scan_warning: Option<String>,
}
