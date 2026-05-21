//! `memd session-start` — invoked from the Claude Code / Codex
//! SessionStart hook (Phase 2).
//!
//! Two jobs, both fast: refresh `memory.md` synchronously so the
//! agent has fresh context, then — if enough chunks have accumulated
//! since the last consolidation — kick off a detached background
//! `memd consolidate`. The command returns immediately; it never
//! blocks the session on an LLM call.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::consolidate::{dirty_region_size, resolve_scope, run_consolidate, ConsolidateOptions, MIN_REGION};
use super::memory_md::{refresh_memory_md, MemoryMdOptions};
use super::paths::absolutize_project_dir;
use crate::error::Result;
use crate::store::Store;

/// Options for the `session-start` subcommand.
#[derive(Debug, Clone)]
pub(super) struct SessionStartOptions {
    pub(super) project_dir: PathBuf,
}

/// Run the session-start routine. Missing scope is not an error — the
/// command simply no-ops so it is safe to wire unconditionally into a
/// SessionStart hook for every repo.
pub(super) async fn run_session_start<S: Store>(
    store: &S,
    options: SessionStartOptions,
) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;

    // No `.memd` scope file → nothing to do; exit cleanly.
    let (tenant_id, project_id) = match resolve_scope(&project_dir, None, None) {
        Ok(scope) => scope,
        Err(_) => {
            return Ok(json!({ "skipped": "no_scope" }));
        }
    };

    // 1. Refresh memory.md synchronously.
    let memory_md = refresh_memory_md(
        store,
        MemoryMdOptions {
            tenant_id: Some(tenant_id.clone()),
            project_id: project_id.clone(),
            project_dir: project_dir.clone(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            global_limit: 10,
            candidate_k: 40,
            cross_tenant: false,
        },
    )
    .await?;

    // 2. If enough dirty chunks have accumulated, spawn a detached
    //    background consolidation. Counting failures are non-fatal —
    //    session-start must never block or fail the session.
    let dirty = dirty_region_size(
        store,
        &project_dir,
        Some(tenant_id.clone()),
        project_id.clone(),
    )
    .await
    .unwrap_or(0);

    // The background child runs with stdio nulled, so a missing CLI
    // backend would fail invisibly. Preflight the selector here and
    // skip the spawn (with a reason) when no backend is available.
    let mut consolidation_spawned = false;
    let mut skip_reason: Option<String> = None;
    if dirty >= MIN_REGION {
        match crate::consolidate::select::select_consolidator() {
            Ok(_) => {
                let spawn = run_consolidate(
                    store,
                    ConsolidateOptions {
                        tenant_id: Some(tenant_id),
                        project_id,
                        project_dir: project_dir.clone(),
                        max_region: 50,
                        dry_run: false,
                        background: true,
                        force: false,
                        promote_to_shared: false,
                    },
                )
                .await;
                consolidation_spawned = spawn.is_ok();
            }
            Err(e) => {
                skip_reason = Some(format!("no consolidator backend: {e}"));
            }
        }
    }

    Ok(json!({
        "memory_md": memory_md,
        "dirty_count": dirty,
        "consolidation_spawned": consolidation_spawned,
        "consolidation_skipped": skip_reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use tempfile::tempdir;

    #[tokio::test]
    async fn missing_scope_is_a_clean_noop() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let result = run_session_start(
            &store,
            SessionStartOptions {
                project_dir: dir.path().to_path_buf(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result["skipped"], "no_scope");
    }

    #[tokio::test]
    async fn writes_memory_md_when_scope_present() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/config.json"),
            r#"{"tenant_id":"t","project_id":"p"}"#,
        )
        .unwrap();
        let result = run_session_start(
            &store,
            SessionStartOptions {
                project_dir: dir.path().to_path_buf(),
            },
        )
        .await
        .unwrap();
        assert!(result.get("memory_md").is_some());
        assert_eq!(result["consolidation_spawned"], false);
        assert!(dir.path().join("memory.md").exists());
    }
}
