//! `memd session-start` — invoked from the Claude Code / Codex
//! SessionStart hook (Phase 2).
//!
//! Reconciles stale consolidation runs, refreshes `memory.md`
//! synchronously, then starts detached background consolidation when
//! enough chunks have accumulated. It does not block on an LLM call.
//!
//! When the project has no `.memd/project_scope.json`, the command
//! auto-creates one using sensible defaults (USER as tenant, repo
//! basename as project) so automatic startup does not require a per-repo
//! `memd init`. Opt out by setting `MEMD_AUTO_SCOPE=0` or
//! dropping a `.memd-skip` file in the repo root.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use super::args::ProjectScopeConfig;
use super::consolidate::{dirty_region_size, run_consolidate, ConsolidateOptions, MIN_REGION};
use super::memory_md::{read_project_scope, refresh_memory_md_with_health, MemoryMdOptions};
use super::paths::absolutize_project_dir;
use crate::error::Result;
use crate::store::{Store, TenantManager};
use crate::types::{ProjectId, TenantId};

/// Maximum length of an auto-derived `tenant_id` or `project_id`. Both
/// are written into directory names downstream, so we keep them short.
const MAX_AUTO_ID_LEN: usize = 64;

/// Options for the `session-start` subcommand.
#[derive(Debug, Clone)]
pub(super) struct SessionStartOptions {
    pub(super) project_dir: PathBuf,
}

/// Resolved inputs for the auto-scope decision. Extracted so unit
/// tests can drive the logic without mutating process env.
#[derive(Debug, Clone)]
pub(super) struct AutoScopeInputs {
    /// Default tenant id to assign when scope is auto-created. Comes
    /// from `MEMD_DEFAULT_TENANT`, then `USER`, then "default".
    pub(super) default_tenant: String,
    /// When false, auto-scope is suppressed and the caller should
    /// behave as before (no-op on missing scope).
    pub(super) enabled: bool,
}

impl AutoScopeInputs {
    /// Read inputs from the process environment.
    ///
    /// `default_tenant` precedence: `MEMD_DEFAULT_TENANT` → `USER` →
    /// `"default"`. Each raw value is trimmed and sanitised (see
    /// `sanitize_id`) so a username with spaces or punctuation
    /// (`"felix-schulz"`, `"  alice  "`) yields a tenant id that
    /// `TenantId::validate` will accept. If sanitisation produces an
    /// id the validator still rejects, falls back to `"default"`.
    pub(super) fn from_env() -> Self {
        let raw_tenant = std::env::var("MEMD_DEFAULT_TENANT")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("USER")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "default".to_string());
        let sanitized = sanitize_id(&raw_tenant, "default");
        // `sanitize_id` only produces alnum+`_`, which `TenantId`
        // always accepts. Guarded anyway so a future validator change
        // (e.g. min-length) doesn't silently write an invalid id.
        let default_tenant = if TenantId::validate(&sanitized).is_ok() {
            sanitized
        } else {
            "default".to_string()
        };
        let enabled = std::env::var("MEMD_AUTO_SCOPE")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        Self {
            default_tenant,
            enabled,
        }
    }
}

/// Run the session-start routine. When `.memd/project_scope.json` is
/// missing, this auto-creates one using `inputs.default_tenant` and
/// the basename of `project_dir` (unless suppressed by env or a
/// `.memd-skip` file). After scope exists, refreshes `memory.md` and
/// optionally spawns a background consolidation.
pub(super) async fn run_session_start<S: Store>(
    store: &S,
    options: SessionStartOptions,
) -> Result<Value> {
    run_session_start_inner(store, options, AutoScopeInputs::from_env()).await
}

pub(super) async fn run_session_start_inner<S: Store>(
    store: &S,
    options: SessionStartOptions,
    auto_inputs: AutoScopeInputs,
) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;

    // The marker disables all startup work, including refresh and recovery,
    // even when this repository was initialized previously.
    if project_dir.join(".memd-skip").exists() {
        return Ok(json!({ "skipped": "memd_skip_present" }));
    }

    let scope_path = project_dir.join(".memd").join("project_scope.json");
    let mut resolved_scope = match validated_project_scope(&project_dir) {
        Ok(scope) => scope.map(|scope| (scope.tenant_id, scope.project_id)),
        Err(error) => return Ok(invalid_scope_response(&scope_path, &error)),
    };
    // Preserve the legacy `.memd/config.json` fallback when there is no
    // project_scope.json. Auto-scope only applies when neither source resolves.
    if resolved_scope.is_none() {
        let legacy_path = project_dir.join(".memd/config.json");
        match read_legacy_scope(&legacy_path) {
            Ok(scope) => resolved_scope = scope,
            Err(error) => return Ok(invalid_scope_response(&legacy_path, &error)),
        }
    }

    // Auto-create only a truly absent scope. If another startup creates it
    // after our read, create_new leaves that file untouched and we validate
    // the winner before proceeding.
    let mut auto_scoped = false;
    if resolved_scope.is_none() {
        match maybe_auto_create_scope(&project_dir, &auto_inputs)? {
            AutoScopeOutcome::Created => {
                auto_scoped = true;
            }
            AutoScopeOutcome::AlreadyExists => {}
            AutoScopeOutcome::SkippedDisabled => {
                return Ok(json!({ "skipped": "auto_scope_disabled" }));
            }
            AutoScopeOutcome::SkippedMarker => {
                return Ok(json!({ "skipped": "memd_skip_present" }));
            }
        }
        resolved_scope = match validated_project_scope(&project_dir) {
            Ok(scope) => scope.map(|scope| (scope.tenant_id, scope.project_id)),
            Err(error) => return Ok(invalid_scope_response(&scope_path, &error)),
        };
    }

    let (tenant_id, project_id) = match resolved_scope {
        Some(scope) => scope,
        None => return Ok(json!({ "skipped": "no_scope" })),
    };

    // Reconcile staged candidates before generating agent-facing context.
    // Recovery is intentionally a session-start stage rather than a store-open
    // side effect. Session-start itself opens a short-lived writer when its
    // main store handle is read-only, preserving the writer-lock invariant.
    let consolidation_recovery = if let Some(persistent) = store.as_persistent() {
        let recovery_result = if persistent.is_read_only() {
            match persistent.open_consolidation_recovery_writer() {
                Ok(writer) => {
                    crate::consolidate::service::recover_consolidation_runs(&writer, 100).await
                }
                Err(error) => Err(error),
            }
        } else {
            crate::consolidate::service::recover_consolidation_runs(persistent, 100).await
        };
        match recovery_result {
            Ok(recovery) => {
                let mut index_refresh_errors = Vec::new();
                if persistent.is_read_only() {
                    let mut by_tenant = BTreeMap::<String, (TenantId, Vec<_>)>::new();
                    for (promoted_tenant, chunk_id) in &recovery.promoted_chunks {
                        by_tenant
                            .entry(promoted_tenant.to_string())
                            .or_insert_with(|| (promoted_tenant.clone(), Vec::new()))
                            .1
                            .push(chunk_id.clone());
                    }
                    for (_, (promoted_tenant, chunk_ids)) in by_tenant {
                        if let Err(error) = persistent
                            .refresh_promoted_chunks_in_memory(&promoted_tenant, &chunk_ids)
                            .await
                        {
                            index_refresh_errors.push(error.to_string());
                        }
                    }
                }
                json!({
                    "inspected": recovery.inspected,
                    "committed": recovery.committed,
                    "rolled_back": recovery.rolled_back,
                    "rejected": recovery.rejected,
                    "failed_recoverable": recovery.failed_recoverable,
                    "index_refresh_errors": index_refresh_errors,
                })
            }
            Err(error) => json!({ "error": error.to_string() }),
        }
    } else {
        json!({ "skipped": "non_persistent_store" })
    };

    // 1. Refresh memory.md synchronously.
    // The persistent store already carries the resolved data directory. Build
    // the lightweight manager from that exact path so session-start sees the
    // same central health and exposure ledgers as `memd memory-md`.
    let tenant_manager = store
        .as_persistent()
        .map(|persistent| TenantManager::new(persistent.data_dir().to_path_buf()));
    let memory_md = refresh_memory_md_with_health(
        store,
        tenant_manager.as_ref(),
        MemoryMdOptions {
            tenant_id: Some(tenant_id.clone()),
            project_id: project_id.clone(),
            project_dir: project_dir.clone(),
            output: PathBuf::from("memory.md"),
            project_limit: 10,
            // Machine-wide takeaways are capped by default so they
            // cannot dominate project startup context.
            global_limit: 2,
            candidate_k: 40,
            explain_output: None,
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
                        promote: false,
                        legacy_immediate: false,
                    },
                )
                .await;
                // A contended scope returns Ok with a "skipped" reason and no
                // child, so success alone does not mean one was started.
                consolidation_spawned = spawn
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("spawned_background").and_then(Value::as_bool))
                    .unwrap_or(false);
                if !consolidation_spawned {
                    skip_reason = match spawn.as_ref() {
                        Ok(value) => value
                            .get("skipped")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        // Reported rather than dropped: the spawn now resolves
                        // scope and takes locks in the parent, so a failure here
                        // is a real condition and used to surface as a silent
                        // "nothing happened".
                        Err(e) => Some(format!("spawn failed: {e}")),
                    };
                }
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
        "auto_scoped": auto_scoped,
        "consolidation_recovery": consolidation_recovery,
    }))
}

fn validated_project_scope(project_dir: &Path) -> Result<Option<ProjectScopeConfig>> {
    let scope = read_project_scope(project_dir)?;
    if let Some(scope) = &scope {
        TenantId::validate(&scope.tenant_id)?;
        ProjectId::validate_opt(scope.project_id.as_deref())?;
    }
    Ok(scope)
}

#[derive(Deserialize)]
struct LegacyScopeConfig {
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
}

fn read_legacy_scope(path: &Path) -> Result<Option<(String, Option<String>)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(path) {
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None)
                }
                Ok(_) => {
                    return Err(crate::error::MemdError::ValidationError(format!(
                        "unable to read {}: {error}",
                        path.display()
                    )))
                }
                Err(metadata_error) => {
                    return Err(crate::error::MemdError::ValidationError(format!(
                        "unable to inspect {} after read failure ({error}): {metadata_error}",
                        path.display()
                    )))
                }
            }
        }
        Err(error) => {
            return Err(crate::error::MemdError::ValidationError(format!(
                "unable to read {}: {error}",
                path.display()
            )))
        }
    };
    let scope: LegacyScopeConfig = serde_json::from_str(&text).map_err(|error| {
        crate::error::MemdError::ValidationError(format!(
            "failed to parse {}: {error}",
            path.display()
        ))
    })?;
    let tenant_id = scope.tenant_id.ok_or_else(|| {
        crate::error::MemdError::ValidationError(format!("{} is missing tenant_id", path.display()))
    })?;
    TenantId::validate(&tenant_id)?;
    ProjectId::validate_opt(scope.project_id.as_deref())?;
    Ok(Some((tenant_id, scope.project_id)))
}

fn invalid_scope_response(path: &Path, error: &crate::error::MemdError) -> Value {
    json!({
        "skipped": "invalid_project_scope",
        "project_scope": path,
        "diagnostic": format!(
            "existing scope file was preserved because it is invalid or unreadable: {error}"
        ),
    })
}

/// Outcome of the auto-scope attempt when no scope file is present.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AutoScopeOutcome {
    Created,
    AlreadyExists,
    SkippedDisabled,
    SkippedMarker,
}

/// Create a minimal `.memd/project_scope.json` for a repo that has no
/// scope yet. Honors `MEMD_AUTO_SCOPE=0` and a `.memd-skip` opt-out
/// marker. Writes ONLY `project_scope.json` (not the full `memd init`
/// payload) — implicit hooks should never touch `AGENTS.md` /
/// `CLAUDE.md` or write tenant guardrails on the user's behalf.
pub(super) fn maybe_auto_create_scope(
    project_dir: &Path,
    inputs: &AutoScopeInputs,
) -> Result<AutoScopeOutcome> {
    if !inputs.enabled {
        return Ok(AutoScopeOutcome::SkippedDisabled);
    }
    if project_dir.join(".memd-skip").exists() {
        return Ok(AutoScopeOutcome::SkippedMarker);
    }

    let project_id = derive_project_id(project_dir);
    let scope = ProjectScopeConfig {
        tenant_id: inputs.default_tenant.clone(),
        project_id: Some(project_id),
        interface: "cli".to_string(),
        cli_command: "memd".to_string(),
        agent_context_output: ".memd/context.md".to_string(),
        project_dir: project_dir.display().to_string(),
    };

    let memd_dir = project_dir.join(".memd");
    std::fs::create_dir_all(&memd_dir)?;
    let path = memd_dir.join("project_scope.json");
    let serialized = format!("{}\n", serde_json::to_string_pretty(&scope)?);
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(AutoScopeOutcome::AlreadyExists);
        }
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = file.write_all(serialized.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error.into());
    }
    Ok(AutoScopeOutcome::Created)
}

/// Derive a `project_id` from the repo basename via `sanitize_id`.
/// Pathological inputs (`/`, `.`, missing basename) fall back to
/// `"project"`.
fn derive_project_id(project_dir: &Path) -> String {
    let raw = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    sanitize_id(raw, "project")
}

/// Sanitise a raw string into an id safe for `TenantId::validate`
/// (ASCII alphanumeric + `_`, lowercased) and bounded by
/// `MAX_AUTO_ID_LEN` so the resulting directory names stay sane.
///
/// Non-ASCII characters are dropped (replaced with a single `_` run).
/// This is lossy for repo names like `café`, which becomes `caf`. We
/// accept the loss in v1 — anyone with a non-ASCII repo name can
/// override with `MEMD_DEFAULT_TENANT` / `memd init`.
fn sanitize_id(raw: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_AUTO_ID_LEN));
    let mut last_underscore = false;
    for ch in raw.chars() {
        if out.len() >= MAX_AUTO_ID_LEN {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore && !out.is_empty() {
            // Skip leading separators (out empty) and collapse runs.
            out.push('_');
            last_underscore = true;
        }
    }
    let trimmed = out.trim_end_matches('_').to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit_stats::{record_hits_to_data_dir, HitRecord};
    use crate::store::{MemoryStore, PersistentStore, PersistentStoreConfig};
    use crate::types::{ChunkType, MemoryChunk};
    use tempfile::tempdir;

    fn disabled_inputs() -> AutoScopeInputs {
        AutoScopeInputs {
            default_tenant: "ignored".to_string(),
            enabled: false,
        }
    }

    fn enabled_inputs(tenant: &str) -> AutoScopeInputs {
        AutoScopeInputs {
            default_tenant: tenant.to_string(),
            enabled: true,
        }
    }

    #[tokio::test]
    async fn missing_scope_with_auto_disabled_is_a_clean_noop() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let result = run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: dir.path().to_path_buf(),
            },
            disabled_inputs(),
        )
        .await
        .unwrap();
        assert_eq!(result["skipped"], "auto_scope_disabled");
        assert!(!dir.path().join(".memd/project_scope.json").exists());
    }

    #[tokio::test]
    async fn memd_skip_marker_suppresses_auto_scope() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".memd-skip"), "").unwrap();
        let result = run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: dir.path().to_path_buf(),
            },
            enabled_inputs("alice"),
        )
        .await
        .unwrap();
        assert_eq!(result["skipped"], "memd_skip_present");
        assert!(!dir.path().join(".memd/project_scope.json").exists());
    }

    #[tokio::test]
    async fn memd_skip_marker_suppresses_initialized_repository() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let memd_dir = dir.path().join(".memd");
        std::fs::create_dir_all(&memd_dir).unwrap();
        let existing = ProjectScopeConfig {
            tenant_id: "preexisting".to_string(),
            project_id: Some("keep_me".to_string()),
            interface: "cli".to_string(),
            cli_command: "memd".to_string(),
            agent_context_output: ".memd/context.md".to_string(),
            project_dir: dir.path().display().to_string(),
        };
        let scope_bytes = format!("{}\n", serde_json::to_string_pretty(&existing).unwrap());
        std::fs::write(memd_dir.join("project_scope.json"), &scope_bytes).unwrap();
        std::fs::write(dir.path().join("memory.md"), "keep existing memory\n").unwrap();
        std::fs::write(dir.path().join(".memd-skip"), "").unwrap();

        let result = run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: dir.path().to_path_buf(),
            },
            enabled_inputs("ignored"),
        )
        .await
        .unwrap();

        assert_eq!(result["skipped"], "memd_skip_present");
        assert_eq!(
            std::fs::read(memd_dir.join("project_scope.json")).unwrap(),
            scope_bytes.as_bytes()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("memory.md")).unwrap(),
            "keep existing memory\n"
        );
    }

    #[tokio::test]
    async fn auto_scope_writes_minimal_project_scope_and_runs_session() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        // Give the dir a stable basename so we can assert on project_id.
        let project_dir = dir.path().join("my-cool-repo");
        std::fs::create_dir_all(&project_dir).unwrap();

        let result = run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: project_dir.clone(),
            },
            enabled_inputs("alice"),
        )
        .await
        .unwrap();

        assert_eq!(result["auto_scoped"], true);
        assert!(result.get("memory_md").is_some());

        let scope: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project_dir.join(".memd/project_scope.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(scope["tenant_id"], "alice");
        assert_eq!(scope["project_id"], "my_cool_repo");
        assert_eq!(scope["interface"], "cli");
        // Auto-scope must NOT have touched AGENTS.md / CLAUDE.md or
        // written tenant_scope.json — implicit hooks stay minimal.
        assert!(!project_dir.join("AGENTS.md").exists());
        assert!(!project_dir.join("CLAUDE.md").exists());
        assert!(!project_dir.join(".memd/tenant_scope.json").exists());
        assert!(!project_dir.join(".memd/memory_guardrails.md").exists());
        assert!(project_dir.join("memory.md").exists());
    }

    #[tokio::test]
    async fn existing_scope_is_left_alone() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        // Write the full ProjectScopeConfig schema — that's what
        // `memd init` and our own auto-scope path emit, and what
        // downstream readers (memory_md::read_project_scope) require.
        let existing = ProjectScopeConfig {
            tenant_id: "preexisting".to_string(),
            project_id: Some("keep_me".to_string()),
            interface: "cli".to_string(),
            cli_command: "memd".to_string(),
            agent_context_output: ".memd/context.md".to_string(),
            project_dir: dir.path().display().to_string(),
        };
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            format!("{}\n", serde_json::to_string_pretty(&existing).unwrap()),
        )
        .unwrap();
        let before = std::fs::read_to_string(dir.path().join(".memd/project_scope.json")).unwrap();

        let result = run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: dir.path().to_path_buf(),
            },
            enabled_inputs("would_overwrite"),
        )
        .await
        .unwrap();

        assert_eq!(result["auto_scoped"], false);
        let after = std::fs::read_to_string(dir.path().join(".memd/project_scope.json")).unwrap();
        assert_eq!(
            before, after,
            "auto-scope must not overwrite existing scope"
        );
    }

    #[test]
    fn derive_project_id_handles_punctuation_and_edge_cases() {
        assert_eq!(
            derive_project_id(Path::new("/tmp/Foo-Bar.v2")),
            "foo_bar_v2"
        );
        assert_eq!(derive_project_id(Path::new("/tmp/__weird__")), "weird");
        assert_eq!(derive_project_id(Path::new("/")), "project");
        // `.` resolves to file_name() == None — must fall back, not panic.
        assert_eq!(derive_project_id(Path::new(".")), "project");
        // Non-ASCII chars are dropped (lossy by design, documented).
        assert_eq!(derive_project_id(Path::new("/tmp/café")), "caf");
    }

    #[test]
    fn sanitize_id_caps_length_and_validates_against_tenant_rules() {
        let huge = "a".repeat(500);
        let out = sanitize_id(&huge, "fallback");
        assert!(
            out.len() <= MAX_AUTO_ID_LEN,
            "len {} should be capped at {}",
            out.len(),
            MAX_AUTO_ID_LEN
        );
        // Output of sanitize_id must always satisfy TenantId rules so
        // it can safely be used as both tenant_id and project_id.
        assert!(TenantId::validate(&out).is_ok());
        // Hyphens / dots / spaces collapse to single underscore.
        assert_eq!(sanitize_id("Felix-Schulz", "x"), "felix_schulz");
        assert_eq!(sanitize_id("   ", "fallback"), "fallback");
    }

    #[test]
    fn from_env_falls_back_to_user_then_default() {
        // Pure smoke check on the env precedence chain — we don't
        // mutate process env (parallel tests share it). Call
        // and assert the result is non-empty and valid as a tenant.
        let inputs = AutoScopeInputs::from_env();
        assert!(!inputs.default_tenant.is_empty());
        assert!(
            TenantId::validate(&inputs.default_tenant).is_ok(),
            "from_env produced an invalid tenant_id: {:?}",
            inputs.default_tenant
        );
    }

    #[tokio::test]
    async fn invalid_existing_scope_is_preserved_with_diagnostic() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("recover-me");
        std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
        let scope_path = project_dir.join(".memd/project_scope.json");
        let original = b"{not json";
        std::fs::write(&scope_path, original).unwrap();

        let result = run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: project_dir.clone(),
            },
            enabled_inputs("alice"),
        )
        .await
        .unwrap();

        assert_eq!(result["skipped"], "invalid_project_scope");
        assert_eq!(result["project_scope"], scope_path.display().to_string());
        assert!(result["diagnostic"]
            .as_str()
            .unwrap()
            .contains("was preserved"));
        assert_eq!(std::fs::read(&scope_path).unwrap(), original);
        assert!(!project_dir.join("memory.md").exists());
    }

    #[tokio::test]
    async fn invalid_legacy_scope_is_preserved_without_creating_project_scope() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("legacy-repo");
        std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
        let legacy_path = project_dir.join(".memd/config.json");
        let project_scope_path = project_dir.join(".memd/project_scope.json");
        let original = b"{not legacy json";
        std::fs::write(&legacy_path, original).unwrap();

        let result = run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: project_dir.clone(),
            },
            enabled_inputs("would_mask_routing"),
        )
        .await
        .unwrap();

        assert_eq!(result["skipped"], "invalid_project_scope");
        assert_eq!(result["project_scope"], legacy_path.display().to_string());
        assert!(result["diagnostic"]
            .as_str()
            .unwrap()
            .contains("was preserved"));
        assert_eq!(std::fs::read(legacy_path).unwrap(), original);
        assert!(!project_scope_path.exists());
        assert!(!project_dir.join("memory.md").exists());
    }

    #[test]
    fn auto_scope_create_new_does_not_truncate_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        let scope_path = dir.path().join(".memd/project_scope.json");
        let original = b"created by a concurrent startup\n";
        std::fs::write(&scope_path, original).unwrap();

        let outcome = maybe_auto_create_scope(dir.path(), &enabled_inputs("alice")).unwrap();

        assert_eq!(outcome, AutoScopeOutcome::AlreadyExists);
        assert_eq!(std::fs::read(scope_path).unwrap(), original);
    }

    #[tokio::test]
    async fn session_start_uses_central_exposure_ledger() {
        let data = tempdir().unwrap();
        let project = tempdir().unwrap();
        let store = PersistentStore::open(PersistentStoreConfig {
            data_dir: data.path().to_path_buf(),
            enable_dense_search: false,
            enable_hybrid_search: false,
            enable_tiered_search: false,
            backfill_hnsw_on_startup: false,
            backfill_canonical_text_on_startup: false,
            ..Default::default()
        })
        .unwrap();
        let tenant = TenantId::new("central_tenant").unwrap();
        let project_id = "central_project";
        let chunk_id = store
            .add(
                MemoryChunk::new(
                    tenant.clone(),
                    "Validation: central-only exposure keeps an old startup fact active. \
                     Agent action: Verify session-start reads the store exposure ledger.",
                    ChunkType::Summary,
                )
                .with_project(ProjectId::from(project_id))
                .with_tags(vec!["kind:evidence".to_string()]),
            )
            .await
            .unwrap();
        store
            .metadata()
            .force_timestamp_created(&chunk_id, 1)
            .unwrap();
        record_hits_to_data_dir(
            data.path(),
            &[HitRecord {
                ts_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64,
                chunk_id: chunk_id.to_string(),
                tenant_id: tenant.to_string(),
                project_id: Some(project_id.to_string()),
                query_mode: "generic".to_string(),
                rank: 1,
                score: 1.0,
                rendered: true,
            }],
        );
        std::fs::create_dir_all(project.path().join(".memd")).unwrap();
        let scope = ProjectScopeConfig {
            tenant_id: tenant.to_string(),
            project_id: Some(project_id.to_string()),
            interface: "cli".to_string(),
            cli_command: "memd".to_string(),
            agent_context_output: ".memd/context.md".to_string(),
            project_dir: project.path().display().to_string(),
        };
        std::fs::write(
            project.path().join(".memd/project_scope.json"),
            format!("{}\n", serde_json::to_string_pretty(&scope).unwrap()),
        )
        .unwrap();

        run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: project.path().to_path_buf(),
            },
            enabled_inputs("ignored"),
        )
        .await
        .unwrap();

        let rendered = std::fs::read_to_string(project.path().join("memory.md")).unwrap();
        assert!(rendered.contains(&chunk_id.to_string()), "{rendered}");
        assert!(
            rendered.contains("priority: `14.0`"),
            "central exposure should prevent the old-chunk penalty:\n{rendered}"
        );
    }
}
