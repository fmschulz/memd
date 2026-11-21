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
//! `memd init`. Inside a repository, a subdirectory uses the nearest scope up
//! to the repository root, and a missing scope is created at that root. Opt
//! out by setting `MEMD_AUTO_SCOPE=0` or dropping a `.memd-skip` file in the
//! repo root.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use super::args::{NativeHookHarness, ProjectScopeConfig};
use super::consolidate::{dirty_region_size, run_consolidate, ConsolidateOptions, MIN_REGION};
use super::memory_md::{read_project_scope, refresh_memory_md_with_health, MemoryMdOptions};
use super::paths::absolutize_project_dir;
use super::provenance::capture_execution_context;
use crate::store::metadata::{DigestScope, MetadataStore};

use super::scope::{
    canonical_home, enclosing_repository, is_unsafe_scope_root, main_checkout_of_linked_worktree,
    Repository,
};
use crate::error::{MemdError, Result};
use crate::store::{Store, TenantManager};
use crate::task_memory::ExecutionContext;
use crate::types::{ProjectId, TenantId};

/// Maximum length of an auto-derived `tenant_id` or `project_id`. Both
/// are written into directory names downstream, so we keep them short.
const MAX_AUTO_ID_LEN: usize = 64;
const MAX_NATIVE_HOOK_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
struct NativeHookPayload {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    parent_agent_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

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
    /// When false, session-start counts dirty chunks but never starts a
    /// background consolidation. Tests set this so they cannot spawn a
    /// model process.
    pub(super) consolidate: bool,
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
            consolidate: true,
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
    execution_context: Option<ExecutionContext>,
) -> Result<Value> {
    let mut result = run_session_start_inner(store, options, AutoScopeInputs::from_env()).await?;
    if let Some(execution_context) = execution_context {
        let result_object = result.as_object_mut().ok_or_else(|| {
            MemdError::ProtocolError("session-start result must be a JSON object".to_string())
        })?;
        result_object.insert(
            "execution_context".to_string(),
            serde_json::to_value(execution_context)?,
        );
    }
    Ok(result)
}

pub(super) fn read_native_hook_context(
    harness: NativeHookHarness,
    project_dir: &Path,
) -> Result<ExecutionContext> {
    let stdin = std::io::stdin();
    parse_native_hook_context(harness, stdin.lock(), project_dir)
}

pub(super) fn native_hook_project_dir(requested: &Path, context: &ExecutionContext) -> PathBuf {
    if requested != Path::new(".") {
        return requested.to_path_buf();
    }
    context
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| requested.to_path_buf())
}

fn parse_native_hook_context(
    harness: NativeHookHarness,
    reader: impl Read,
    project_dir: &Path,
) -> Result<ExecutionContext> {
    let payload = read_bounded_hook_payload(reader)?;
    let payload: NativeHookPayload = serde_json::from_slice(&payload)?;
    let cwd = nonempty(payload.cwd)
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                project_dir.join(path)
            }
        })
        .unwrap_or_else(|| project_dir.to_path_buf());
    let mut context = capture_execution_context(&cwd);
    let harness_name = match harness {
        NativeHookHarness::Claude => "claude",
        NativeHookHarness::Codex => "codex",
    };
    if context.harness.as_deref() != Some(harness_name) {
        context.native_session_id = None;
        context.native_thread_id = None;
    }
    context.harness = Some(harness_name.to_string());
    if let Some(model) = nonempty(payload.model) {
        context.model = Some(model);
    }
    if let Some(agent_id) = nonempty(payload.agent_id) {
        context.native_agent_id = Some(agent_id);
    }
    if let Some(parent_id) = nonempty(payload.parent_agent_id) {
        context.native_parent_id = Some(parent_id);
    }
    if let Some(session_id) = nonempty(payload.session_id) {
        context.native_session_id = Some(session_id);
    }
    if let Some(thread_id) = nonempty(payload.thread_id) {
        context.native_thread_id = Some(thread_id);
    }
    Ok(context)
}

fn read_bounded_hook_payload(reader: impl Read) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    reader
        .take(MAX_NATIVE_HOOK_BYTES + 1)
        .read_to_end(&mut payload)?;
    if payload.len() as u64 > MAX_NATIVE_HOOK_BYTES {
        return Err(MemdError::ValidationError(format!(
            "native hook payload exceeds {MAX_NATIVE_HOOK_BYTES} bytes"
        )));
    }
    Ok(payload)
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

    // A `.memd-skip` marker at the repository root opts out the whole
    // repository, whatever scope files its subdirectories hold.
    let repository = locate_repository(&project_dir, canonical_home().as_deref());
    if repository.as_ref().is_some_and(Repository::opted_out) {
        return Ok(json!({ "skipped": "memd_skip_present" }));
    }

    // Inside a repository, its scope covers this directory, as it does for
    // every other command. A repository without a scope gets one at its root,
    // never one per subdirectory. `scope_dir` is the directory that owns the
    // scope; `id_source` names the project when a scope has to be created.
    //
    // Nothing is written yet. A missing scope is only planned: the tenant and
    // project that would be created are derived, and the files follow once the
    // project has stored content.
    let mut scope_dir: PathBuf;
    let (tenant_id, project_id, new_scope) = match look_up_scope(&project_dir, repository.as_ref())
    {
        ScopeLookup::Found {
            scope_dir: found_dir,
            ids: (tenant_id, project_id),
        } => {
            scope_dir = found_dir;
            (tenant_id, project_id, None)
        }
        ScopeLookup::Missing {
            scope_dir: new_dir,
            id_source,
        } => match plan_new_scope(&new_dir, &id_source, &auto_inputs)? {
            Ok(plan) => {
                scope_dir = new_dir;
                (
                    plan.tenant_id.clone(),
                    Some(plan.project_id.clone()),
                    Some(plan),
                )
            }
            Err(skipped) => return Ok(skipped_response(&skipped)),
        },
        ScopeLookup::Invalid { path, error } => {
            return Ok(invalid_scope_response(&path, &error));
        }
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

    // Files exist only for a project that has content. An empty project gets
    // no scope, no `.memd` directory and no `memory.md`; an existing
    // `memory.md` stays as it is.
    if !project_has_content(store, &tenant_id, project_id.as_deref()).await? {
        let mut skipped = json!({
            "skipped": "no_stored_facts",
            "tenant_id": tenant_id,
            "project_id": project_id,
            "consolidation_recovery": consolidation_recovery,
        });
        if new_scope.is_none() {
            skipped["scope_dir"] = json!(scope_dir);
        }
        return Ok(skipped);
    }

    // Create a planned scope now. If another startup published it after our
    // read, its file stays untouched and we validate the winner. A repository
    // root that cannot be written must not fail the session: the scope then
    // goes to the session directory, still named after the repository.
    let mut auto_scoped = false;
    let (tenant_id, project_id) = match new_scope {
        None => (tenant_id, project_id),
        Some(plan) => {
            let (outcome, written_dir) =
                write_scope_with_fallback(&plan, &scope_dir, &project_dir)?;
            scope_dir = written_dir;
            auto_scoped = outcome == AutoScopeOutcome::Created;
            match read_scope_at(&scope_dir) {
                Ok(Some(ids)) => ids,
                Ok(None) => return Ok(json!({ "skipped": "no_scope" })),
                Err(invalid) => return Ok(invalid_scope_response(&invalid.0, &invalid.1)),
            }
        }
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
            lesson_limit: 3,
            candidate_k: 40,
            explain_output: None,
            // A session below its repository's root shows the repository's
            // facts, so the repo-novelty gate reads that checkout's docs.
            repo_dir: repo_dir_for(&project_dir, repository.as_ref()),
        },
    )
    .await?;

    // 2. If enough dirty chunks have accumulated, spawn a detached
    //    background consolidation. The scope directory owns the consolidation
    //    watermark, so one repository has one watermark whichever directory
    //    starts the session. Counting failures are non-fatal: session-start
    //    must never block or fail the session.
    let dirty = dirty_region_size(
        store,
        &scope_dir,
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
    if dirty >= MIN_REGION && auto_inputs.consolidate {
        match crate::consolidate::select::select_consolidator() {
            Ok(_) => {
                let spawn = run_consolidate(
                    store,
                    ConsolidateOptions {
                        tenant_id: Some(tenant_id),
                        project_id,
                        project_dir: scope_dir.clone(),
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

    let mut result = json!({
        "memory_md": memory_md,
        "dirty_count": dirty,
        "consolidation_spawned": consolidation_spawned,
        "consolidation_skipped": skip_reason,
        "auto_scoped": auto_scoped,
        "consolidation_recovery": consolidation_recovery,
    });
    if scope_dir != project_dir {
        result["scope_dir"] = json!(scope_dir);
    }
    Ok(result)
}

/// Tenant and project of a scope.
type ScopeIds = (String, Option<String>);

/// Read the scope that `dir` holds: `.memd/project_scope.json` first, then a
/// valid legacy `.memd/config.json`. An invalid or unreadable file is an error
/// that carries its path (boxed to keep the `Result` small).
fn read_scope_at(dir: &Path) -> std::result::Result<Option<ScopeIds>, Box<(PathBuf, MemdError)>> {
    let scope_path = dir.join(".memd/project_scope.json");
    if let Some(scope) =
        validated_project_scope(dir).map_err(|error| Box::new((scope_path, error)))?
    {
        return Ok(Some((scope.tenant_id, scope.project_id)));
    }
    let legacy_path = dir.join(".memd/config.json");
    read_legacy_scope(&legacy_path).map_err(|error| Box::new((legacy_path, error)))
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
    if scope.tenant_id.is_none() && scope.project_id.is_none() {
        return Ok(None);
    }
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
    SkippedUnsafeRoot,
}

/// Create a minimal `.memd/project_scope.json` in `scope_dir` for a repo that
/// has no scope yet. The project is named after `id_source`, which is
/// `scope_dir` itself unless the scope has to live elsewhere than the
/// repository it names. Honors `MEMD_AUTO_SCOPE=0` and a `.memd-skip` opt-out
/// marker. Writes ONLY `project_scope.json` (not the full `memd init`
/// payload) — implicit hooks should never touch `AGENTS.md` /
/// `CLAUDE.md` or write tenant guardrails on the user's behalf.
///
/// The file appears complete or not at all: several sessions can start in one
/// repository at once, and a reader must never see a half-written scope.
#[cfg(test)]
fn maybe_auto_create_scope(
    scope_dir: &Path,
    id_source: &Path,
    inputs: &AutoScopeInputs,
) -> Result<AutoScopeOutcome> {
    match plan_new_scope(scope_dir, id_source, inputs)? {
        Ok(plan) => plan.write(scope_dir),
        Err(skipped) => Ok(skipped),
    }
}

/// A scope that session-start or a first write may create: nothing is on disk
/// yet.
#[derive(Debug)]
struct NewScope {
    tenant_id: String,
    project_id: String,
}

impl NewScope {
    /// Publish the scope file in `dir/.memd`, whole or not at all.
    fn write(&self, dir: &Path) -> Result<AutoScopeOutcome> {
        let scope = ProjectScopeConfig {
            tenant_id: self.tenant_id.clone(),
            project_id: Some(self.project_id.clone()),
            interface: "cli".to_string(),
            cli_command: "memd".to_string(),
            agent_context_output: ".memd/context.md".to_string(),
            project_dir: dir.display().to_string(),
        };
        let memd_dir = dir.join(".memd");
        std::fs::create_dir_all(&memd_dir)?;
        let serialized = format!("{}\n", serde_json::to_string_pretty(&scope)?);
        publish_new_file(&memd_dir, "project_scope.json", serialized.as_bytes())
    }
}

/// Decide whether a scope may be created in `scope_dir`, and derive it, without
/// writing anything. The refusal is a `Skipped*` outcome: auto-scope is off,
/// a `.memd-skip` marker is present, or `scope_dir` is an unsafe root.
fn plan_new_scope(
    scope_dir: &Path,
    id_source: &Path,
    inputs: &AutoScopeInputs,
) -> Result<std::result::Result<NewScope, AutoScopeOutcome>> {
    if !inputs.enabled {
        return Ok(Err(AutoScopeOutcome::SkippedDisabled));
    }
    if scope_dir.join(".memd-skip").exists() {
        return Ok(Err(AutoScopeOutcome::SkippedMarker));
    }

    // Compare canonical paths so aliases such as ~/repo/.. and symlinks
    // cannot create an implicit scope for every descendant of home or /.
    let canonical = scope_dir.canonicalize()?;
    if is_unsafe_scope_root(&canonical, canonical_home().as_deref()) {
        return Ok(Err(AutoScopeOutcome::SkippedUnsafeRoot));
    }

    Ok(Ok(NewScope {
        tenant_id: inputs.default_tenant.clone(),
        project_id: derive_project_id(id_source),
    }))
}

/// The JSON result for a refused scope creation.
fn skipped_response(outcome: &AutoScopeOutcome) -> Value {
    match outcome {
        AutoScopeOutcome::SkippedDisabled => json!({ "skipped": "auto_scope_disabled" }),
        AutoScopeOutcome::SkippedUnsafeRoot => json!({ "skipped": "unsafe_auto_scope_root" }),
        AutoScopeOutcome::SkippedMarker => json!({ "skipped": "memd_skip_present" }),
        AutoScopeOutcome::Created | AutoScopeOutcome::AlreadyExists => {
            json!({ "skipped": "no_scope" })
        }
    }
}

/// True when the project has at least one visible fact or one experience case
/// row. A persistent store answers from its metadata counts, the ones the
/// `memory.md` digest uses; any other store answers from a listing.
async fn project_has_content<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: Option<&str>,
) -> Result<bool> {
    let tenant = TenantId::new(tenant_id)?;
    let Some(persistent) = store.as_persistent() else {
        let rows = store
            .list_chunks_for_project(&tenant, project_id, 1, 0)
            .await?;
        return Ok(!rows.is_empty());
    };
    let mut project_ids = Vec::new();
    if let Some(project) = project_id {
        project_ids.push(project.to_string());
        project_ids.extend(
            crate::ops::configured_project_aliases(&tenant, project)
                .into_iter()
                .filter(|alias| alias.origin_tenant_id == tenant.as_str())
                .filter_map(|alias| alias.origin_project_id),
        );
    }
    let scope = DigestScope {
        project_ids,
        include_unscoped: project_id.is_none(),
    };
    let counts = persistent.metadata().count_digest_rows(&tenant, &scope)?;
    Ok(counts.inside.visible + counts.inside.experience_projections > 0)
}

/// A scope that a first write publishes once the write has succeeded. Nothing
/// is on disk while it is pending, so a command that fails (bad input, a
/// rejected write, a store that will not open) leaves no scope behind, and a
/// project without a stored fact has no `.memd` directory.
#[derive(Debug)]
pub struct PendingScope {
    plan: NewScope,
    scope_dir: PathBuf,
    project_dir: PathBuf,
}

impl PendingScope {
    /// The directory that would hold `.memd/project_scope.json`.
    pub(super) fn scope_dir(&self) -> &Path {
        &self.scope_dir
    }

    /// Publish the scope file. Returns true when this call created it, false
    /// when a concurrent first write published one first (that file stays).
    pub fn publish(self) -> Result<bool> {
        let (outcome, _dir) =
            write_scope_with_fallback(&self.plan, &self.scope_dir, &self.project_dir)?;
        Ok(outcome == AutoScopeOutcome::Created)
    }
}

/// Publish `plan` in `scope_dir`. A directory that cannot be written must not
/// fail the session: the scope then goes to `project_dir`, still named after
/// the repository. Returns the outcome and the directory that holds the scope.
fn write_scope_with_fallback(
    plan: &NewScope,
    scope_dir: &Path,
    project_dir: &Path,
) -> Result<(AutoScopeOutcome, PathBuf)> {
    match plan.write(scope_dir) {
        Err(MemdError::IoError(error)) if scope_dir != project_dir && is_unwritable(&error) => {
            Ok((plan.write(project_dir)?, project_dir.to_path_buf()))
        }
        other => Ok((other?, scope_dir.to_path_buf())),
    }
}

/// A scope planned for a directory with none of its own.
pub(super) type PlannedScope = (ScopeIds, Option<PendingScope>);

/// Plan the scope of a directory that has none, the way session-start does,
/// without writing anything. Commands that start a project (the first write)
/// and commands that only read share this plan, so both use the same tenant
/// and project: the repository root or main checkout, else the directory
/// itself; the tenant from `MEMD_DEFAULT_TENANT`, then `USER`; the project from
/// the repository name.
///
/// The plan carries the tenant and project, and the pending scope to publish
/// after a write succeeded (`None` when a scope, including a legacy
/// `.memd/config.json`, already exists). A refusal (`Err`) is a `Skipped*`
/// outcome: auto-scope is off, a `.memd-skip` marker is present, or the root
/// is unsafe (home or `/`).
pub(super) fn plan_scope_for_first_write(
    start: &Path,
    inputs: &AutoScopeInputs,
) -> Result<std::result::Result<PlannedScope, AutoScopeOutcome>> {
    let project_dir = absolutize_project_dir(start)?;
    if project_dir.join(".memd-skip").exists() {
        return Ok(Err(AutoScopeOutcome::SkippedMarker));
    }
    let repository = locate_repository(&project_dir, canonical_home().as_deref());
    if repository.as_ref().is_some_and(Repository::opted_out) {
        return Ok(Err(AutoScopeOutcome::SkippedMarker));
    }
    let (scope_dir, id_source) = match look_up_scope(&project_dir, repository.as_ref()) {
        ScopeLookup::Found { ids, .. } => return Ok(Ok((ids, None))),
        ScopeLookup::Invalid { path, error } => {
            return Err(MemdError::ValidationError(format!(
                "invalid scope file {}: {error}",
                path.display()
            )))
        }
        ScopeLookup::Missing {
            scope_dir,
            id_source,
        } => (scope_dir, id_source),
    };
    let plan = match plan_new_scope(&scope_dir, &id_source, inputs)? {
        Ok(plan) => plan,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let ids = (plan.tenant_id.clone(), Some(plan.project_id.clone()));
    let pending = PendingScope {
        plan,
        scope_dir,
        project_dir,
    };
    Ok(Ok((ids, Some(pending))))
}

/// Why a scope cannot be planned, for an error message.
pub(super) fn refusal_reason(outcome: &AutoScopeOutcome) -> &'static str {
    match outcome {
        AutoScopeOutcome::SkippedDisabled => "automatic scope creation is off (MEMD_AUTO_SCOPE=0)",
        AutoScopeOutcome::SkippedMarker => "a .memd-skip marker opts this project out",
        AutoScopeOutcome::SkippedUnsafeRoot => {
            "the project root is the home directory or /, which never gets an automatic scope"
        }
        AutoScopeOutcome::Created | AutoScopeOutcome::AlreadyExists => "no scope is planned",
    }
}

/// Write `contents` to a temporary file in `dir`, then hard-link it to `name`.
/// The link fails if `name` exists, so the first publisher wins and no reader
/// sees a partial file. The temporary file is always removed.
///
/// A filesystem without hard links (FAT, some network mounts) rejects the
/// link. The temporary file is then renamed to `name` if `name` does not exist
/// yet. Two racers that both get that far write the same bytes, so the loser's
/// rename replaces the file with identical content. If `name` exists,
/// `AlreadyExists` is returned and the file is left alone. No path writes
/// `name` in place, so a reader never sees a partial file, and a crash leaves
/// at most a stray temporary file. The file and the directory are synced.
fn publish_new_file(dir: &Path, name: &str, contents: &[u8]) -> Result<AutoScopeOutcome> {
    publish_new_file_with(dir, name, contents, |from, to| std::fs::hard_link(from, to))
}

/// [`publish_new_file`] with the link step as a parameter, so tests can make
/// it fail the way a filesystem without hard links does.
fn publish_new_file_with(
    dir: &Path,
    name: &str,
    contents: &[u8],
    link: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<AutoScopeOutcome> {
    static NEXT_TEMPORARY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temporary = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let target = dir.join(name);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(contents).and_then(|()| file.sync_all()))
        .and_then(|()| match link(&temporary, &target) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                rename_unless_exists(dir, name, &temporary, &target)
            }
            linked => linked,
        })
        .and_then(|()| std::fs::File::open(dir).and_then(|directory| directory.sync_all()));
    let _ = std::fs::remove_file(&temporary);
    match written {
        Ok(()) => Ok(AutoScopeOutcome::Created),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(AutoScopeOutcome::AlreadyExists)
        }
        Err(error) => Err(error.into()),
    }
}

/// How long the rename fallback waits for another publisher's lock.
const PUBLISH_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Rename `temporary` to `target` unless `target` exists, with the existence
/// check and the rename under an exclusive lock file `.<name>.lock` in `dir`.
/// Another memd that publishes the same file without a hard link takes the same
/// lock, so it cannot slip a file in between our check and our rename. The
/// wait is bounded; a lock still held after it is an error that names the lock
/// file. A lock this call created is removed on every path.
///
/// Remaining window: a process that does not use this function (an editor,
/// `memd init`) can still write the file between the check and the rename. A
/// crash while holding the lock leaves the lock file; remove it by hand.
fn rename_unless_exists(
    dir: &Path,
    name: &str,
    temporary: &Path,
    target: &Path,
) -> std::io::Result<()> {
    struct Lock(PathBuf);
    impl Drop for Lock {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    let lock_path = dir.join(format!(".{name}.lock"));
    let deadline = std::time::Instant::now() + PUBLISH_LOCK_WAIT;
    let _lock = loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => break Lock(lock_path.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "another process holds {}; remove it if no memd is running",
                            lock_path.display()
                        ),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    };
    if target.symlink_metadata().is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(temporary, target)
}

/// The repository root that the facts' docs are checked against: the
/// checkout the session runs in (the worktree root for a linked worktree),
/// when the session directory lies below it. `None` at the root itself, and
/// outside any repository.
fn repo_dir_for(project_dir: &Path, repository: Option<&Repository>) -> Option<PathBuf> {
    repository
        .map(|repository| repository.repo_root.as_path())
        .filter(|root| *root != project_dir)
        .map(Path::to_path_buf)
}

/// True for the errors of a directory that session-start may not write.
fn is_unwritable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    )
}

/// The repository whose scope covers the session directory. A directory that
/// is itself a root is covered only by its main checkout, when it is a linked
/// worktree. A nested clone or a submodule is its own repository (`None`).
fn locate_repository(project_dir: &Path, home: Option<&Path>) -> Option<Repository> {
    enclosing_repository(project_dir, home).filter(|repository| {
        repository.repo_root != project_dir || repository.main_checkout.is_some()
    })
}

/// Where the scope of a session directory is, or would be.
#[derive(Debug)]
enum ScopeLookup {
    /// A scope exists: the nearest one between the directory and the
    /// repository root, or the main checkout's.
    Found { scope_dir: PathBuf, ids: ScopeIds },
    /// No scope exists. One would go in `scope_dir` and be named after
    /// `id_source`.
    Missing {
        scope_dir: PathBuf,
        id_source: PathBuf,
    },
    /// A scope file exists but is invalid or unreadable.
    Invalid { path: PathBuf, error: MemdError },
}

/// Look up the scope of `project_dir`: its own scope first (project scope,
/// then legacy scope), then, inside a repository, the nearest one between its
/// parent and the repository root, then the main checkout's. The nearest valid
/// scope wins; an invalid one stops the walk.
fn look_up_scope(project_dir: &Path, repository: Option<&Repository>) -> ScopeLookup {
    let directories = std::iter::once(project_dir).chain(repository.into_iter().flat_map(|r| {
        project_dir
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(&r.repo_root))
            .chain(r.main_checkout.as_deref())
    }));
    for dir in directories {
        match read_scope_at(dir) {
            Ok(Some(ids)) => {
                return ScopeLookup::Found {
                    scope_dir: dir.to_path_buf(),
                    ids,
                }
            }
            Ok(None) => {}
            Err(invalid) => {
                let (path, error) = *invalid;
                return ScopeLookup::Invalid { path, error };
            }
        }
    }
    let root = repository.map_or(project_dir, Repository::root);
    ScopeLookup::Missing {
        scope_dir: root.to_path_buf(),
        id_source: root.to_path_buf(),
    }
}

/// Derive a `project_id` from the repo basename via `sanitize_id`.
/// A linked git worktree uses the basename of its main checkout, so every
/// worktree of one repo shares one project. Pathological inputs (`/`, `.`,
/// missing basename) fall back to `"project"`.
fn derive_project_id(project_dir: &Path) -> String {
    let main_checkout = main_checkout_of_linked_worktree(project_dir);
    let raw = main_checkout
        .as_deref()
        .unwrap_or(project_dir)
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
            consolidate: false,
        }
    }

    fn enabled_inputs(tenant: &str) -> AutoScopeInputs {
        AutoScopeInputs {
            default_tenant: tenant.to_string(),
            enabled: true,
            consolidate: false,
        }
    }

    #[test]
    fn native_hook_payload_keeps_only_allowlisted_context() {
        let dir = tempdir().unwrap();
        let payload = json!({
            "session_id": "claude-session",
            "thread_id": "claude-thread",
            "agent_id": "worker-2",
            "parent_agent_id": "lead-1",
            "model": "claude-fable-5",
            "cwd": dir.path(),
            "prompt": "do not retain this prompt",
            "transcript_path": "/private/native-transcript.jsonl",
            "tool_input": {"secret": "do not retain this tool input"}
        });

        let context = parse_native_hook_context(
            NativeHookHarness::Claude,
            std::io::Cursor::new(serde_json::to_vec(&payload).unwrap()),
            Path::new("/unused"),
        )
        .unwrap();
        let serialized = serde_json::to_string(&context).unwrap();

        assert_eq!(context.harness.as_deref(), Some("claude"));
        assert_eq!(context.native_session_id.as_deref(), Some("claude-session"));
        assert_eq!(context.native_thread_id.as_deref(), Some("claude-thread"));
        assert_eq!(context.native_agent_id.as_deref(), Some("worker-2"));
        assert_eq!(context.native_parent_id.as_deref(), Some("lead-1"));
        assert_eq!(context.model.as_deref(), Some("claude-fable-5"));
        assert_eq!(context.cwd.as_deref(), dir.path().to_str());
        assert_eq!(
            native_hook_project_dir(Path::new("."), &context),
            dir.path()
        );
        assert_eq!(
            native_hook_project_dir(Path::new("/explicit/project"), &context),
            Path::new("/explicit/project")
        );
        assert!(!serialized.contains("prompt"));
        assert!(!serialized.contains("transcript"));
        assert!(!serialized.contains("tool_input"));
        assert!(!serialized.contains("do not retain"));
    }

    #[test]
    fn native_hook_payload_is_bounded() {
        let payload = vec![b' '; MAX_NATIVE_HOOK_BYTES as usize + 1];
        let error = parse_native_hook_context(
            NativeHookHarness::Codex,
            std::io::Cursor::new(payload),
            Path::new("."),
        )
        .unwrap_err();

        assert!(error.to_string().contains("exceeds 65536 bytes"));
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
        seed_content(&store, &project_dir).await;

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
        seed_content(&store, dir.path()).await;

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

    /// Lay out a main checkout and one linked worktree the way `git worktree
    /// add` does, without running git. Returns (main checkout, worktree).
    fn linked_worktree_layout(root: &Path, relative_gitdir: bool) -> (PathBuf, PathBuf) {
        let main = root.join("My-Repo");
        let git_dir = main.join(".git");
        let admin = git_dir.join("worktrees/cc-wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let worktree = main.join(".claude/worktrees/cc-20261017-074438-555a");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            admin.join("gitdir"),
            format!("{}/.git\n", worktree.display()),
        )
        .unwrap();
        let target = if relative_gitdir {
            "../../../.git/worktrees/cc-wt".to_string()
        } else {
            admin.display().to_string()
        };
        std::fs::write(worktree.join(".git"), format!("gitdir: {target}\n")).unwrap();
        (main, worktree)
    }

    #[test]
    fn derive_project_id_resolves_a_linked_worktree_to_its_main_checkout() {
        for relative in [false, true] {
            let tmp = tempdir().unwrap();
            let (main, worktree) = linked_worktree_layout(tmp.path(), relative);
            assert_eq!(derive_project_id(&main), "my_repo");
            assert_eq!(
                derive_project_id(&worktree),
                "my_repo",
                "relative={relative}"
            );
        }
    }

    #[test]
    fn derive_project_id_keeps_the_basename_for_plain_repos_and_submodules() {
        let tmp = tempdir().unwrap();
        let plain = tmp.path().join("Plain-Repo");
        std::fs::create_dir_all(plain.join(".git")).unwrap();
        assert_eq!(derive_project_id(&plain), "plain_repo");

        let non_git = tmp.path().join("scratch");
        std::fs::create_dir_all(&non_git).unwrap();
        assert_eq!(derive_project_id(&non_git), "scratch");

        // A submodule has a `.git` file but no `commondir`: it is its own project.
        let super_repo = tmp.path().join("super");
        let module_git = super_repo.join(".git/modules/sub");
        std::fs::create_dir_all(&module_git).unwrap();
        let submodule = super_repo.join("Sub-Module");
        std::fs::create_dir_all(&submodule).unwrap();
        std::fs::write(
            submodule.join(".git"),
            format!("gitdir: {}\n", module_git.display()),
        )
        .unwrap();
        assert_eq!(derive_project_id(&submodule), "sub_module");

        // A dangling or malformed `.git` file falls back to the basename.
        let broken = tmp.path().join("Broken-Tree");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join(".git"), "gitdir: /does/not/exist\n").unwrap();
        assert_eq!(derive_project_id(&broken), "broken_tree");
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

    #[tokio::test]
    async fn legacy_config_without_scope_is_preserved_while_auto_scoping() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("wiki-repo");
        std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
        let legacy_path = project_dir.join(".memd/config.json");
        let original = b"{\n  \"wiki\": {\"outdir\": \"wiki\"}\n}\n";
        std::fs::write(&legacy_path, original).unwrap();
        seed_content(&store, &project_dir).await;

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
        assert_eq!(std::fs::read(&legacy_path).unwrap(), original);
        let scope: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project_dir.join(".memd/project_scope.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(scope["tenant_id"], "alice");
        assert_eq!(scope["project_id"], "wiki_repo");
        assert!(project_dir.join("memory.md").exists());
    }

    #[tokio::test]
    async fn valid_legacy_scope_is_preserved_as_fallback() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("legacy-repo");
        std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
        let legacy_path = project_dir.join(".memd/config.json");
        let original = b"{\n  \"tenant_id\": \"legacy_tenant\",\n  \"project_id\": \"legacy_project\",\n  \"wiki\": {\"outdir\": \"wiki\"}\n}\n";
        std::fs::write(&legacy_path, original).unwrap();
        seed_content(&store, &project_dir).await;

        let result = run_session_start_inner(
            &store,
            SessionStartOptions {
                project_dir: project_dir.clone(),
            },
            enabled_inputs("would_replace_legacy"),
        )
        .await
        .unwrap();

        assert_eq!(result["auto_scoped"], false);
        assert_eq!(result["memory_md"]["tenant_id"], "legacy_tenant");
        assert_eq!(result["memory_md"]["project_id"], "legacy_project");
        assert_eq!(std::fs::read(&legacy_path).unwrap(), original);
        assert!(!project_dir.join(".memd/project_scope.json").exists());
        assert!(project_dir.join("memory.md").exists());
    }

    #[tokio::test]
    async fn legacy_project_without_tenant_remains_invalid() {
        let store = MemoryStore::new();
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("legacy-repo");
        std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
        let legacy_path = project_dir.join(".memd/config.json");
        let original = b"{\"project_id\":\"must_not_be_replaced\"}\n";
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
            .contains("missing tenant_id"));
        assert_eq!(std::fs::read(&legacy_path).unwrap(), original);
        assert!(!project_dir.join(".memd/project_scope.json").exists());
        assert!(!project_dir.join("memory.md").exists());
    }

    #[test]
    fn auto_scope_create_new_does_not_truncate_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        let scope_path = dir.path().join(".memd/project_scope.json");
        let original = b"created by a concurrent startup\n";
        std::fs::write(&scope_path, original).unwrap();

        let outcome =
            maybe_auto_create_scope(dir.path(), dir.path(), &enabled_inputs("alice")).unwrap();

        assert_eq!(outcome, AutoScopeOutcome::AlreadyExists);
        assert_eq!(std::fs::read(scope_path).unwrap(), original);
        let names: Vec<_> = std::fs::read_dir(dir.path().join(".memd"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["project_scope.json"], "no temporary file remains");
    }

    fn write_scope(dir: &Path, tenant: &str, project: &str) {
        std::fs::create_dir_all(dir.join(".memd")).unwrap();
        let scope = ProjectScopeConfig {
            tenant_id: tenant.to_string(),
            project_id: Some(project.to_string()),
            interface: "cli".to_string(),
            cli_command: "memd".to_string(),
            agent_context_output: ".memd/context.md".to_string(),
            project_dir: dir.display().to_string(),
        };
        std::fs::write(
            dir.join(".memd/project_scope.json"),
            format!("{}\n", serde_json::to_string_pretty(&scope).unwrap()),
        )
        .unwrap();
    }

    /// Run session-start against a persistent store. An in-memory store makes
    /// memory.md fall back to a hit-stats cache under the project's `.memd`,
    /// which would hide whether session-start itself created that directory.
    async fn start(project_dir: &Path) -> Value {
        let data = tempdir().unwrap();
        let store = open_test_store(data.path());
        seed_content(&store, project_dir).await;
        start_in(&store, project_dir).await
    }

    /// Start against an empty store: no project has any stored fact.
    async fn start_empty(project_dir: &Path) -> Value {
        let data = tempdir().unwrap();
        let store = open_test_store(data.path());
        start_in(&store, project_dir).await
    }

    /// The tenant and project a start in `dir` would use.
    fn ids_a_start_would_use(dir: &Path) -> Option<ScopeIds> {
        let dir = absolutize_project_dir(dir).unwrap();
        let repository = locate_repository(&dir, canonical_home().as_deref());
        match look_up_scope(&dir, repository.as_ref()) {
            ScopeLookup::Found { ids, .. } => Some(ids),
            ScopeLookup::Missing { id_source, .. } => {
                Some(("alice".to_string(), Some(derive_project_id(&id_source))))
            }
            ScopeLookup::Invalid { .. } => None,
        }
    }

    /// Store one fact in the project a start in `dir` would use, so the
    /// project has content and session-start writes its files.
    async fn seed_content<S: Store>(store: &S, dir: &Path) {
        let Some((tenant, project)) = ids_a_start_would_use(dir) else {
            return; // an invalid scope stops the start before any content check
        };
        let mut chunk = MemoryChunk::new(
            TenantId::new(&tenant).unwrap(),
            "Seeded fact: this project has stored content.",
            ChunkType::Summary,
        );
        if let Some(project) = project {
            chunk = chunk.with_project(ProjectId::from(project.as_str()));
        }
        store.add(chunk).await.unwrap();
    }

    fn open_test_store(data_dir: &Path) -> PersistentStore {
        PersistentStore::open(PersistentStoreConfig {
            data_dir: data_dir.to_path_buf(),
            enable_dense_search: false,
            enable_hybrid_search: false,
            backfill_hnsw_on_startup: false,
            backfill_canonical_text_on_startup: false,
            ..Default::default()
        })
        .unwrap()
    }

    async fn start_in(store: &PersistentStore, project_dir: &Path) -> Value {
        run_session_start_inner(
            store,
            SessionStartOptions {
                project_dir: project_dir.to_path_buf(),
            },
            enabled_inputs("alice"),
        )
        .await
        .unwrap()
    }

    async fn add_unconsolidated_chunks(store: &PersistentStore, project: &str, count: usize) {
        for index in 0..count {
            store
                .add(
                    MemoryChunk::new(
                        TenantId::new("alice").unwrap(),
                        format!(
                            "Observation {index}: the {project} project uses fact number {index}."
                        ),
                        ChunkType::Summary,
                    )
                    .with_project(ProjectId::from(project)),
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn subdirectory_start_shares_the_repository_dirty_count_and_creates_nothing() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_scope(&root, "alice", "repo");
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();
        let data = tempdir().unwrap();
        let store = open_test_store(data.path());
        add_unconsolidated_chunks(&store, "repo", MIN_REGION + 2).await;

        let at_root = start_in(&store, &root).await;
        let in_subdirectory = start_in(&store, &review).await;

        assert_eq!(at_root["dirty_count"], MIN_REGION + 2);
        assert_eq!(in_subdirectory["dirty_count"], at_root["dirty_count"]);
        let created: Vec<_> = std::fs::read_dir(&review)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(created, ["memory.md"]);
    }

    #[tokio::test]
    async fn subdirectory_start_honours_the_repository_consolidation_watermark() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_scope(&root, "alice", "repo");
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();
        let data = tempdir().unwrap();
        let store = open_test_store(data.path());
        add_unconsolidated_chunks(&store, "repo", MIN_REGION + 2).await;
        std::fs::create_dir_all(root.join(".memd/data")).unwrap();
        std::fs::write(
            root.join(".memd/data/consolidate.state.json"),
            format!("{{\"last_consolidation_ms\": {}}}", i64::MAX / 2),
        )
        .unwrap();

        let result = start_in(&store, &review).await;

        assert_eq!(result["dirty_count"], 0);
        assert!(!review.join(".memd").exists());
    }

    #[tokio::test]
    async fn forged_worktree_pointer_makes_a_directory_its_own_root() {
        let tmp = tempdir().unwrap();
        let victim = tmp.path().join("victim");
        let admin = victim.join(".git/worktrees/x");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let real_worktree = tmp.path().join("real-worktree");
        std::fs::write(
            admin.join("gitdir"),
            format!("{}/.git\n", real_worktree.display()),
        )
        .unwrap();
        write_scope(&victim, "alice", "victim_project");
        let attacker = tmp.path().join("attacker");
        std::fs::create_dir_all(&attacker).unwrap();
        std::fs::write(
            attacker.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();

        assert_eq!(main_checkout_of_linked_worktree(&attacker), None);
        let result = start(&attacker).await;

        assert_eq!(result["auto_scoped"], true);
        assert!(result.get("scope_dir").is_none());
        assert_eq!(read_scope_json(&attacker)["project_id"], "attacker");
        assert_eq!(read_scope_json(&victim)["project_id"], "victim_project");
    }

    #[tokio::test]
    async fn worktree_without_a_back_pointer_is_not_linked() {
        let tmp = tempdir().unwrap();
        let (main, worktree) = linked_worktree_at(tmp.path(), false);
        assert_eq!(
            main_checkout_of_linked_worktree(&worktree).as_deref(),
            Some(main.as_path())
        );
        let admin = main.join(".git/worktrees/sibling-wt");
        std::fs::remove_file(admin.join("gitdir")).unwrap();

        assert_eq!(main_checkout_of_linked_worktree(&worktree), None);
    }

    #[tokio::test]
    async fn parent_components_do_not_pull_a_directory_into_the_wrong_repository() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("b");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        write_scope(&repo, "alice", "repo_b");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();

        let result = start(&repo.join("../other")).await;

        assert_eq!(result["auto_scoped"], true);
        assert!(result.get("scope_dir").is_none());
        assert_eq!(read_scope_json(&other)["project_id"], "other");
        assert!(other.join("memory.md").exists());
        assert!(!repo.join("memory.md").exists());
    }

    #[tokio::test]
    async fn skip_marker_at_the_repository_root_wins_over_a_subdirectory_scope() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".memd-skip"), "").unwrap();
        let sub = root.join("sub");
        write_scope(&sub, "alice", "sub_project");

        let result = start(&sub).await;

        assert_eq!(result["skipped"], "memd_skip_present");
        assert!(!sub.join("memory.md").exists());
    }

    #[test]
    fn auto_scope_publishes_the_complete_file_and_leaves_no_temporary() {
        let dir = tempdir().unwrap();

        let outcome =
            maybe_auto_create_scope(dir.path(), dir.path(), &enabled_inputs("alice")).unwrap();

        assert_eq!(outcome, AutoScopeOutcome::Created);
        let names: Vec<_> = std::fs::read_dir(dir.path().join(".memd"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["project_scope.json"]);
        assert_eq!(read_scope_json(dir.path())["tenant_id"], "alice");
    }

    #[tokio::test]
    async fn unwritable_repository_root_falls_back_to_the_session_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let root = tmp.path().join("Locked-Repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable_anyway = std::fs::create_dir(root.join("probe")).is_ok();

        let result = start(&review).await;

        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        if writable_anyway {
            return; // running as a user that ignores directory permissions
        }
        assert_eq!(result["auto_scoped"], true);
        assert!(result.get("scope_dir").is_none());
        assert_eq!(read_scope_json(&review)["project_id"], "locked_repo");
        assert!(!root.join(".memd").exists());
        assert!(review.join("memory.md").exists());
    }

    #[tokio::test]
    async fn subdirectory_inherits_the_repository_scope() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_scope(&root, "alice", "repo");
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();

        let result = start(&review).await;

        assert_eq!(result["auto_scoped"], false);
        assert_eq!(result["scope_dir"], root.display().to_string());
        assert_eq!(result["memory_md"]["tenant_id"], "alice");
        assert_eq!(result["memory_md"]["project_id"], "repo");
        assert!(!review.join(".memd").exists());
        assert!(review.join("memory.md").exists());
    }

    #[tokio::test]
    async fn nearest_ancestor_scope_wins_over_the_repository_root() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_scope(&root, "alice", "repo");
        let sub = root.join("sub");
        write_scope(&sub, "alice", "sub_project");
        let deep = sub.join("deep");
        std::fs::create_dir_all(&deep).unwrap();

        let result = start(&deep).await;

        assert_eq!(result["scope_dir"], sub.display().to_string());
        assert_eq!(result["memory_md"]["project_id"], "sub_project");
        assert!(!deep.join(".memd").exists());
    }

    #[tokio::test]
    async fn subdirectory_of_unscoped_repository_creates_the_scope_at_the_root() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("Fresh-Repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("review/deep");
        std::fs::create_dir_all(&deep).unwrap();

        let result = start(&deep).await;

        assert_eq!(result["auto_scoped"], true);
        assert_eq!(result["scope_dir"], root.display().to_string());
        let scope: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".memd/project_scope.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(scope["tenant_id"], "alice");
        assert_eq!(scope["project_id"], "fresh_repo");
        assert!(!root.join("review/.memd").exists());
        assert!(!deep.join(".memd").exists());
        assert!(deep.join("memory.md").exists());
    }

    #[tokio::test]
    async fn invalid_repository_scope_stops_the_subdirectory_start() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".memd")).unwrap();
        let scope_path = root.join(".memd/project_scope.json");
        std::fs::write(&scope_path, b"{not json").unwrap();
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();

        let result = start(&review).await;

        assert_eq!(result["skipped"], "invalid_project_scope");
        assert_eq!(result["project_scope"], scope_path.display().to_string());
        assert_eq!(std::fs::read(&scope_path).unwrap(), b"{not json");
        assert!(!review.join(".memd").exists());
        assert!(!review.join("memory.md").exists());
    }

    #[tokio::test]
    async fn skip_marker_at_the_repository_root_suppresses_scope_creation() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".memd-skip"), "").unwrap();
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();

        let result = start(&review).await;

        assert_eq!(result["skipped"], "memd_skip_present");
        assert!(!root.join(".memd").exists());
        assert!(!review.join(".memd").exists());
    }

    #[test]
    fn unsafe_scope_root_is_the_filesystem_root_or_home() {
        let home = Path::new("/home/someone");
        assert!(is_unsafe_scope_root(Path::new("/"), Some(home)));
        assert!(is_unsafe_scope_root(home, Some(home)));
        assert!(!is_unsafe_scope_root(
            Path::new("/home/someone/repo"),
            Some(home)
        ));
        assert!(!is_unsafe_scope_root(Path::new("/srv/repo"), None));
    }

    #[test]
    fn a_repository_at_home_is_ignored_for_subdirectories() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".git")).unwrap();
        write_scope(&home, "alice", "home");
        let sub = home.join("notes");
        std::fs::create_dir_all(&sub).unwrap();

        assert!(locate_repository(&sub, Some(&home)).is_none());
        let repository = locate_repository(&sub, None).unwrap();
        assert!(matches!(
            look_up_scope(&sub, Some(&repository)),
            ScopeLookup::Found { .. }
        ));
    }

    #[tokio::test]
    async fn directory_outside_any_repository_gets_its_own_scope() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path().join("plain-dir/inner");
        std::fs::create_dir_all(&project_dir).unwrap();

        let result = start(&project_dir).await;

        assert_eq!(result["auto_scoped"], true);
        assert!(result.get("scope_dir").is_none());
        assert!(project_dir.join(".memd/project_scope.json").exists());
        assert!(!tmp.path().join("plain-dir/.memd").exists());
    }

    #[tokio::test]
    async fn subdirectory_of_a_linked_worktree_inherits_the_worktree_scope() {
        let tmp = tempdir().unwrap();
        let (main, worktree) = linked_worktree_layout(tmp.path(), false);
        write_scope(&main, "alice", "main_project");
        write_scope(&worktree, "alice", "worktree_project");
        let sub = worktree.join("review");
        std::fs::create_dir_all(&sub).unwrap();

        let result = start(&sub).await;

        assert_eq!(result["scope_dir"], worktree.display().to_string());
        assert_eq!(result["memory_md"]["project_id"], "worktree_project");
        assert!(!sub.join(".memd").exists());
    }

    /// A linked worktree of `Main-Repo`, either inside the main checkout (the
    /// `cc` layout) or next to it. Returns (main checkout, worktree).
    fn linked_worktree_at(root: &Path, inside_main: bool) -> (PathBuf, PathBuf) {
        if inside_main {
            return linked_worktree_layout(root, false);
        }
        let main = root.join("Main-Repo");
        let admin = main.join(".git/worktrees/sibling-wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let worktree = root.join("sibling-wt");
        std::fs::create_dir_all(&worktree).unwrap();
        // Real git records the worktree's `.git` file in the admin directory.
        std::fs::write(
            admin.join("gitdir"),
            format!("{}/.git\n", worktree.display()),
        )
        .unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        (main, worktree)
    }

    fn read_scope_json(dir: &Path) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(dir.join(".memd/project_scope.json")).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn nested_independent_repository_keeps_its_own_scope() {
        let tmp = tempdir().unwrap();
        let parent = tmp.path().join("parent");
        std::fs::create_dir_all(parent.join(".git")).unwrap();
        write_scope(&parent, "alice", "parent_project");
        let child = parent.join("vendor/child");
        std::fs::create_dir_all(child.join(".git")).unwrap();

        let result = start(&child).await;

        assert_eq!(result["auto_scoped"], true);
        assert!(result.get("scope_dir").is_none());
        assert_eq!(read_scope_json(&child)["project_id"], "child");
        assert_eq!(read_scope_json(&parent)["project_id"], "parent_project");
    }

    #[tokio::test]
    async fn submodule_keeps_its_own_scope() {
        let tmp = tempdir().unwrap();
        let parent = tmp.path().join("parent");
        let module_git = parent.join(".git/modules/sub");
        std::fs::create_dir_all(&module_git).unwrap();
        write_scope(&parent, "alice", "parent_project");
        let submodule = parent.join("Sub-Module");
        std::fs::create_dir_all(&submodule).unwrap();
        std::fs::write(
            submodule.join(".git"),
            format!("gitdir: {}\n", module_git.display()),
        )
        .unwrap();

        let result = start(&submodule).await;

        assert_eq!(result["auto_scoped"], true);
        assert!(result.get("scope_dir").is_none());
        assert_eq!(read_scope_json(&submodule)["project_id"], "sub_module");
    }

    #[tokio::test]
    async fn linked_worktree_root_inherits_the_main_checkout_scope() {
        for inside_main in [true, false] {
            let tmp = tempdir().unwrap();
            let (main, worktree) = linked_worktree_at(tmp.path(), inside_main);
            write_scope(&main, "custom_tenant", "custom_project");

            let result = start(&worktree).await;

            assert_eq!(result["auto_scoped"], false, "inside_main={inside_main}");
            assert_eq!(result["scope_dir"], main.display().to_string());
            assert_eq!(result["memory_md"]["tenant_id"], "custom_tenant");
            assert_eq!(result["memory_md"]["project_id"], "custom_project");
            assert!(!worktree.join(".memd").exists());
            assert!(worktree.join("memory.md").exists());
        }
    }

    #[tokio::test]
    async fn linked_worktree_with_unscoped_main_creates_the_scope_at_the_main_checkout() {
        for inside_main in [true, false] {
            let tmp = tempdir().unwrap();
            let (main, worktree) = linked_worktree_at(tmp.path(), inside_main);
            let sub = worktree.join("review");
            std::fs::create_dir_all(&sub).unwrap();

            let expected_project = if inside_main { "my_repo" } else { "main_repo" };
            for dir in [&worktree, &sub] {
                let result = start(dir).await;
                assert_eq!(result["scope_dir"], main.display().to_string());
                assert_eq!(read_scope_json(&main)["project_id"], expected_project);
                assert!(
                    !worktree.join(".memd").exists(),
                    "inside_main={inside_main}"
                );
                assert!(!sub.join(".memd").exists());
            }
        }
    }

    #[tokio::test]
    async fn subdirectory_of_an_unscoped_worktree_inherits_the_main_checkout_scope() {
        let tmp = tempdir().unwrap();
        let (main, worktree) = linked_worktree_at(tmp.path(), false);
        write_scope(&main, "custom_tenant", "custom_project");
        let sub = worktree.join("review");
        std::fs::create_dir_all(&sub).unwrap();

        let result = start(&sub).await;

        assert_eq!(result["scope_dir"], main.display().to_string());
        assert_eq!(result["memory_md"]["project_id"], "custom_project");
        assert!(!sub.join(".memd").exists());
        assert!(!worktree.join(".memd").exists());
    }

    #[tokio::test]
    async fn legacy_scope_on_the_walk_is_inherited_without_a_shadowing_scope() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".memd")).unwrap();
        std::fs::write(
            root.join(".memd/config.json"),
            b"{\"tenant_id\":\"legacy_tenant\",\"project_id\":\"legacy_project\"}\n",
        )
        .unwrap();
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();

        let result = start(&review).await;

        assert_eq!(result["auto_scoped"], false);
        assert_eq!(result["scope_dir"], root.display().to_string());
        assert_eq!(result["memory_md"]["tenant_id"], "legacy_tenant");
        assert_eq!(result["memory_md"]["project_id"], "legacy_project");
        assert!(!root.join(".memd/project_scope.json").exists());
        assert!(!review.join(".memd").exists());
    }

    #[tokio::test]
    async fn invalid_legacy_scope_on_the_walk_stops_the_start() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".memd")).unwrap();
        let legacy_path = root.join(".memd/config.json");
        std::fs::write(&legacy_path, b"{not legacy json").unwrap();
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();

        let result = start(&review).await;

        assert_eq!(result["skipped"], "invalid_project_scope");
        assert_eq!(result["project_scope"], legacy_path.display().to_string());
        assert!(!root.join(".memd/project_scope.json").exists());
        assert!(!review.join(".memd").exists());
    }

    #[tokio::test]
    async fn skip_marker_opts_out_the_whole_repository_even_with_a_scope() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_scope(&root, "alice", "repo");
        std::fs::write(root.join(".memd-skip"), "").unwrap();
        let review = root.join("review");
        std::fs::create_dir_all(&review).unwrap();

        let result = start(&review).await;

        assert_eq!(result["skipped"], "memd_skip_present");
        assert!(!review.join("memory.md").exists());
    }

    #[tokio::test]
    async fn skip_marker_in_the_main_checkout_opts_out_its_linked_worktrees() {
        let tmp = tempdir().unwrap();
        let (main, worktree) = linked_worktree_at(tmp.path(), false);
        write_scope(&main, "alice", "repo");
        std::fs::write(main.join(".memd-skip"), "").unwrap();

        let result = start(&worktree).await;

        assert_eq!(result["skipped"], "memd_skip_present");
        assert!(!worktree.join("memory.md").exists());
        assert!(!worktree.join(".memd").exists());
    }

    #[tokio::test]
    async fn symlink_in_the_project_dir_resolves_like_the_kernel_does() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("b/repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        write_scope(&repo, "alice", "repo_b");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::os::unix::fs::symlink(repo.join("sub"), root.join("a/link")).unwrap();

        // `a/link/../src` is `b/repo/src`. Lexically it would be `a/src`.
        let result = start(&root.join("a/link/../src")).await;

        assert_eq!(result["scope_dir"], repo.display().to_string());
        assert_eq!(result["memory_md"]["project_id"], "repo_b");
        assert!(repo.join("src/memory.md").exists());
        assert!(!root.join("a/src").exists());
    }

    #[test]
    fn publish_falls_back_to_rename_when_hard_links_are_unsupported() {
        let dir = tempdir().unwrap();
        let unsupported = |from: &Path, to: &Path| {
            // The target must not exist, and the temporary holds every byte.
            assert!(!to.exists(), "target visible before publish");
            assert_eq!(std::fs::read(from).unwrap(), b"complete contents\n");
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no hard links",
            ))
        };

        let outcome = publish_new_file_with(
            dir.path(),
            "scope.json",
            b"complete contents\n",
            unsupported,
        )
        .unwrap();

        assert_eq!(outcome, AutoScopeOutcome::Created);
        assert_eq!(
            std::fs::read(dir.path().join("scope.json")).unwrap(),
            b"complete contents\n"
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["scope.json"], "no temporary file remains");
    }

    #[test]
    fn publish_fallback_never_replaces_an_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("scope.json"), b"first publisher\n").unwrap();
        let unsupported = |_: &Path, _: &Path| {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no hard links",
            ))
        };

        let outcome =
            publish_new_file_with(dir.path(), "scope.json", b"second publisher\n", unsupported)
                .unwrap();

        assert_eq!(outcome, AutoScopeOutcome::AlreadyExists);
        assert_eq!(
            std::fs::read(dir.path().join("scope.json")).unwrap(),
            b"first publisher\n"
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["scope.json"]);
    }

    #[test]
    fn rename_fallback_never_replaces_a_file_published_between_check_and_rename() {
        let dir = tempdir().unwrap();
        let lock = dir.path().join(".scope.json.lock");
        // Another process holds the publication lock, then publishes its own
        // scope and releases the lock. Our check and rename run under the lock,
        // so they must see that file.
        std::fs::write(&lock, b"").unwrap();
        let other = std::thread::spawn({
            let dir = dir.path().to_path_buf();
            let lock = lock.clone();
            move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::fs::write(dir.join("scope.json"), b"other tenant\n").unwrap();
                std::fs::remove_file(lock).unwrap();
            }
        });
        let unsupported = |_: &Path, _: &Path| {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no hard links",
            ))
        };

        let outcome =
            publish_new_file_with(dir.path(), "scope.json", b"our tenant\n", unsupported).unwrap();
        other.join().unwrap();

        assert_eq!(outcome, AutoScopeOutcome::AlreadyExists);
        assert_eq!(
            std::fs::read(dir.path().join("scope.json")).unwrap(),
            b"other tenant\n"
        );
        assert_eq!(
            entries(dir.path()),
            ["scope.json"],
            "no lock or temporary file"
        );
    }

    #[test]
    fn rename_fallback_gives_up_on_a_lock_that_is_never_released() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".scope.json.lock"), b"").unwrap();
        let unsupported = |_: &Path, _: &Path| {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no hard links",
            ))
        };

        let started = std::time::Instant::now();
        let error =
            publish_new_file_with(dir.path(), "scope.json", b"x\n", unsupported).unwrap_err();

        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(error.to_string().contains(".scope.json.lock"), "{error}");
        assert!(!dir.path().join("scope.json").exists());
        // A lock this call did not create is left for its owner.
        assert!(dir.path().join(".scope.json.lock").exists());
    }

    #[test]
    fn repo_dir_is_the_active_checkout_below_its_root_only() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (main, worktree) = linked_worktree_at(&root, false);
        let sub = worktree.join("review");
        std::fs::create_dir_all(&sub).unwrap();
        let plain = root.join("plain-repo");
        std::fs::create_dir_all(plain.join(".git")).unwrap();
        std::fs::create_dir_all(plain.join("sub")).unwrap();

        // A linked worktree uses its own root, not the main checkout.
        let repository = locate_repository(&sub, None);
        assert_eq!(
            repo_dir_for(&sub, repository.as_ref()).as_deref(),
            Some(worktree.as_path())
        );
        assert_ne!(
            Some(main.as_path()),
            repo_dir_for(&sub, repository.as_ref()).as_deref()
        );
        // At the worktree root itself there is no separate repository dir.
        let at_root = locate_repository(&worktree, None);
        assert_eq!(repo_dir_for(&worktree, at_root.as_ref()), None);
        // A plain repository below its root, and outside any repository.
        let sub_plain = plain.join("sub");
        let repository = locate_repository(&sub_plain, None);
        assert_eq!(
            repo_dir_for(&sub_plain, repository.as_ref()).as_deref(),
            Some(plain.as_path())
        );
        assert_eq!(repo_dir_for(&root, None), None);
    }

    fn repository_with_subdirectory(root: &Path) -> (PathBuf, PathBuf) {
        let repo = root.join("Fresh-Repo");
        let sub = repo.join("review");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        (repo, sub)
    }

    fn entries(dir: &Path) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn unscoped_repository_without_stored_facts_gets_no_files() {
        let tmp = tempdir().unwrap();
        let (repo, sub) = repository_with_subdirectory(tmp.path());

        let result = start_empty(&sub).await;

        assert_eq!(result["skipped"], "no_stored_facts");
        assert_eq!(result["tenant_id"], "alice");
        assert_eq!(result["project_id"], "fresh_repo");
        assert!(result.get("memory_md").is_none());
        assert_eq!(entries(&repo), [".git", "review"]);
        assert_eq!(entries(&sub), Vec::<std::ffi::OsString>::new());
    }

    #[tokio::test]
    async fn first_stored_fact_makes_session_start_create_the_scope_and_memory_md() {
        let tmp = tempdir().unwrap();
        let (repo, sub) = repository_with_subdirectory(tmp.path());
        let data = tempdir().unwrap();
        let store = open_test_store(data.path());

        let before = start_in(&store, &sub).await;
        assert_eq!(before["skipped"], "no_stored_facts");
        add_unconsolidated_chunks(&store, "fresh_repo", 1).await;
        let after = start_in(&store, &sub).await;

        assert_eq!(after["auto_scoped"], true);
        assert_eq!(after["scope_dir"], repo.display().to_string());
        assert_eq!(read_scope_json(&repo)["project_id"], "fresh_repo");
        assert!(sub.join("memory.md").exists());
        assert!(!sub.join(".memd").exists());
    }

    #[tokio::test]
    async fn existing_scope_with_an_empty_project_writes_no_memory_md() {
        let tmp = tempdir().unwrap();
        let (repo, sub) = repository_with_subdirectory(tmp.path());
        write_scope(&repo, "alice", "empty_project");
        std::fs::write(sub.join("memory.md"), "keep existing memory\n").unwrap();

        let result = start_empty(&sub).await;

        assert_eq!(result["skipped"], "no_stored_facts");
        assert_eq!(result["tenant_id"], "alice");
        assert_eq!(result["project_id"], "empty_project");
        assert_eq!(result["scope_dir"], repo.display().to_string());
        assert_eq!(
            std::fs::read_to_string(sub.join("memory.md")).unwrap(),
            "keep existing memory\n"
        );
        let fresh = repo.join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        let again = start_empty(&fresh).await;
        assert_eq!(again["skipped"], "no_stored_facts");
        assert!(!fresh.join("memory.md").exists());
    }

    #[tokio::test]
    async fn an_experience_case_counts_as_stored_content() {
        assert_case_counts_as_content(false).await;
    }

    #[tokio::test]
    async fn a_compact_experience_case_counts_as_stored_content() {
        assert_case_counts_as_content(true).await;
    }

    async fn assert_case_counts_as_content(compact: bool) {
        use crate::experience::{record_case, CaseFile, ExperienceContext};

        let tmp = tempdir().unwrap();
        let (repo, sub) = repository_with_subdirectory(tmp.path());
        write_scope(&repo, "alice", "case_project");
        let data = tempdir().unwrap();
        let store = open_test_store(data.path());
        let case = CaseFile::parse(
            json!({
                "problem": "startup is slow",
                "symptoms": ["slow startup"],
                "failed": ["restarted the daemon"],
                "fix": "Apply the case_project workaround before launching.",
                "check": {"method": "observed", "result": "startup took under a second"},
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        record_case(
            &store,
            ExperienceContext {
                tenant_id: TenantId::new("alice").unwrap(),
                project_id: ProjectId::from("case_project"),
                agent_id: Some("session-start-test".to_string()),
                session_id: None,
                observed_at_ms: Some(1_800_000_000_000),
                provenance: Default::default(),
            },
            case,
            None,
            compact,
        )
        .await
        .unwrap();

        let result = start_in(&store, &sub).await;

        assert!(result.get("skipped").is_none(), "{result}");
        assert!(sub.join("memory.md").exists());
    }

    #[tokio::test]
    async fn session_start_uses_central_exposure_ledger() {
        let data = tempdir().unwrap();
        let project = tempdir().unwrap();
        let mut config = PersistentStoreConfig {
            data_dir: data.path().to_path_buf(),
            enable_dense_search: false,
            enable_hybrid_search: false,
            backfill_hnsw_on_startup: false,
            backfill_canonical_text_on_startup: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config.clone()).unwrap();
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
        drop(store);
        config.read_only = true;
        let store = PersistentStore::open(config).unwrap();

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
