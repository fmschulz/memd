//! `memd consolidate` — LLM-backed memory consolidation (Phase 2).
//!
//! Builds a working region from chunks written (and retrieved) since
//! the last run, asks the configured [`Consolidator`] to rewrite them
//! into a smaller deduplicated set of `kind:consolidated` lessons,
//! and persists those lessons. Project-scoped runs soft-tombstone
//! superseded sources; tenant-wide runs retain project sources and
//! record non-destructive `derives_from` lineage. Nothing is deleted.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use super::paths::absolutize_project_dir;
use crate::consolidate::journal::LineageRelation;
use crate::consolidate::prompt::{
    build_consolidation_prompt, parse_consolidation_response, RegionChunk,
};
use crate::consolidate::select::select_consolidator;
use crate::consolidate::service::execute_consolidation_with_identity;
use crate::consolidate::Consolidator;
use crate::error::{MemdError, Result};
use crate::store::Store;
use crate::types::lifecycle::VisibilityPolicy;
use crate::types::{ChunkId, ChunkStatus, MemoryChunk, TenantId};

/// Minimum region size below which consolidation is skipped unless
/// `--force` is passed. Also the dirty-chunk threshold at which
/// `memd session-start` spawns a background consolidation.
pub(super) const MIN_REGION: usize = 10;

/// Hard ceiling on chunks scanned while building the region.
const SCAN_LIMIT: usize = 500;

/// Options for the `consolidate` subcommand.
#[derive(Debug, Clone)]
pub(super) struct ConsolidateOptions {
    pub(super) tenant_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) max_region: usize,
    pub(super) dry_run: bool,
    pub(super) background: bool,
    pub(super) force: bool,
    pub(super) promote: bool,
    pub(super) legacy_immediate: bool,
}

/// Entry point: resolves the consolidator backend from the
/// environment, then runs the consolidation.
pub(super) async fn run_consolidate<S: Store>(
    store: &S,
    options: ConsolidateOptions,
) -> Result<Value> {
    if options.legacy_immediate {
        eprintln!(
            "warning: --legacy-immediate is deprecated and will be removed in the next release; use --promote"
        );
    }
    if options.background {
        return spawn_background(store.as_persistent().map(|s| s.data_dir()), &options);
    }
    let consolidator = select_consolidator()?;
    consolidate_core(store, options, consolidator.as_ref()).await
}

pub(super) async fn run_consolidate_review<S: Store>(
    store: &S,
    run_id: Option<&str>,
    list: bool,
    limit: usize,
    accept: bool,
    reject: bool,
) -> Result<Value> {
    let persistent = store.as_persistent().ok_or_else(|| {
        MemdError::ValidationError("consolidate-review requires a persistent store".to_string())
    })?;
    if list {
        if run_id.is_some() || accept || reject {
            return Err(MemdError::ValidationError(
                "consolidate-review --list does not accept a run id or decision".to_string(),
            ));
        }
        let mut staged = Vec::new();
        for run in persistent
            .metadata()
            .list_staged_consolidation_runs(limit.clamp(1, 1_000))?
        {
            let candidate_count = persistent
                .metadata()
                .get_consolidation_entries(&run.run_id)?
                .len();
            let source_count = persistent
                .metadata()
                .get_memory_lineage(&run.run_id)?
                .into_iter()
                .map(|edge| edge.source_chunk_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            staged.push(json!({
                "run_id": run.run_id.to_string(),
                "tenant_id": run.tenant_id.to_string(),
                "project_id": run.project_id,
                "state": run.state.as_str(),
                "candidate_count": candidate_count,
                "source_count": source_count,
                "consolidator": {
                    "adapter": run.consolidator,
                    "command": run.consolidator_command,
                    "model": run.consolidator_model,
                    "version": run.consolidator_version,
                },
                "created_at_ms": run.created_at_ms,
                "updated_at_ms": run.updated_at_ms,
            }));
        }
        let count = staged.len();
        return Ok(json!({
            "staged_runs": staged,
            "count": count,
        }));
    }
    if accept == reject {
        return Err(MemdError::ValidationError(
            "consolidate-review requires exactly one of --accept or --reject".to_string(),
        ));
    }
    let run_id = run_id.ok_or_else(|| {
        MemdError::ValidationError(
            "consolidate-review requires a run id unless --list is used".to_string(),
        )
    })?;
    let run_id = crate::consolidate::journal::ConsolidationRunId::parse(run_id)?;
    let decision = if accept {
        crate::consolidate::service::ConsolidationReviewDecision::Accept
    } else {
        crate::consolidate::service::ConsolidationReviewDecision::Reject
    };
    let execution =
        crate::consolidate::service::review_consolidation_run(persistent, &run_id, decision)
            .await?;
    Ok(json!({
        "run_id": execution.run_id.to_string(),
        "state": execution.state.as_str(),
        "candidate_chunk_ids": execution.candidate_chunk_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "source_count": execution.source_count,
    }))
}

/// Core consolidation flow, parameterised over the [`Consolidator`] so
/// tests can inject a deterministic backend.
async fn consolidate_core<S: Store>(
    store: &S,
    options: ConsolidateOptions,
    consolidator: &dyn Consolidator,
) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let (tenant_id, project_id) = resolve_scope(
        &project_dir,
        options.tenant_id.clone(),
        options.project_id.clone(),
    )?;
    let tenant = TenantId::new(&tenant_id)?;
    let max_region = options.max_region.clamp(MIN_REGION, SCAN_LIMIT);

    // Reconcile stale journaled work before selecting a new region or paying
    // for another model call. The recovery service has an in-flight grace
    // period, so this does not claim a run that is still writing candidates.
    if !options.dry_run {
        if let Some(persistent) = store.as_persistent() {
            crate::consolidate::service::recover_consolidation_runs(persistent, 100).await?;
        }
    }

    let state_path = consolidate_state_path(&project_dir);
    let since_ms = read_last_consolidation_ms(&state_path);

    let region = collect_region(
        store,
        &tenant,
        project_id.as_deref(),
        since_ms,
        &project_dir,
        max_region,
    )
    .await?;

    if region.len() < MIN_REGION && !options.force {
        return Ok(json!({
            "skipped": "below_threshold",
            "region_size": region.len(),
            "min_region": MIN_REGION,
        }));
    }

    let prompt = build_consolidation_prompt(&region);
    if options.dry_run {
        return Ok(json!({
            "dry_run": true,
            "region_size": region.len(),
            "prompt": prompt,
        }));
    }

    let identity = consolidator.identity().await?;
    let raw = consolidator.consolidate(&prompt).await?;
    let entries = parse_consolidation_response(&raw, &region)?;

    let inherited_ctx = most_common_ctx_tags(&region, 3);
    let persistent = store.as_persistent().ok_or_else(|| {
        MemdError::ValidationError("consolidate requires a persistent store".to_string())
    })?;
    let promotion_requested = options.promote || options.legacy_immediate;
    let execution = execute_consolidation_with_identity(
        persistent,
        &tenant,
        project_id.as_deref(),
        &entries,
        if project_id.is_some() {
            LineageRelation::Supersedes
        } else {
            LineageRelation::DerivesFrom
        },
        &identity,
        &inherited_ctx,
        &prompt,
        &raw,
        promotion_requested,
    )
    .await?;
    if !matches!(
        execution.state,
        crate::consolidate::journal::ConsolidationState::Validated
            | crate::consolidate::journal::ConsolidationState::Committed
    ) {
        return Err(MemdError::StorageError(format!(
            "consolidation run {} stopped before validation in state {}",
            execution.run_id, execution.state
        )));
    }
    let written = execution
        .candidate_chunk_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let committed = execution.state == crate::consolidate::journal::ConsolidationState::Committed;
    let tombstoned = if committed && project_id.is_some() {
        execution.source_count
    } else {
        0
    };
    let now = now_ms();

    write_last_consolidation_ms(&state_path, now)?;

    // Tenant-wide runs persist lessons with no project_id; flag that
    // project-scoped searches will not surface them.
    let tenant_wide_write = committed && project_id.is_none() && !written.is_empty();
    let mut summary = json!({
        "tenant_id": tenant_id,
        "project_id": project_id,
        "region_size": region.len(),
        "consolidated": if committed { written.len() } else { 0 },
        "staged": if committed { 0 } else { written.len() },
        "tombstoned": tombstoned,
        "state": execution.state.as_str(),
        "promotion_requested": promotion_requested,
        "consolidator": identity,
        "candidate_chunk_ids": written,
        "new_chunk_ids": if committed { execution.candidate_chunk_ids.iter().map(ToString::to_string).collect::<Vec<_>>() } else { Vec::new() },
        "run_id": execution.run_id.to_string(),
        "reused_existing_run": execution.reused_existing_run,
    });
    if tenant_wide_write {
        summary["warning"] = json!(
            "consolidated lessons written without project_id; project-scoped searches \
             will not see them directly (they surface via tenant-wide search and \
             memory-md Machine-Wide Fact Library); project-scoped sources were retained"
        );
    }
    Ok(summary)
}

/// Count the chunks that would form the consolidation region for the
/// given scope. Used by `memd session-start` to decide whether a
/// background consolidation is worth spawning.
pub(super) async fn dirty_region_size<S: Store>(
    store: &S,
    project_dir: &Path,
    tenant_arg: Option<String>,
    project_arg: Option<String>,
) -> Result<usize> {
    let (tenant_id, project_id) = resolve_scope(project_dir, tenant_arg, project_arg)?;
    let tenant = TenantId::new(&tenant_id)?;
    let since_ms = read_last_consolidation_ms(&consolidate_state_path(project_dir));
    let region = collect_region(
        store,
        &tenant,
        project_id.as_deref(),
        since_ms,
        project_dir,
        SCAN_LIMIT,
    )
    .await?;
    Ok(region.len())
}

/// Resolve `(tenant_id, project_id)` from explicit args, falling back
/// to `.memd/project_scope.json` then `.memd/config.json`. Mirrors
/// `scope::resolve_required`: an explicit `--tenant-id` suppresses
/// scope-file inheritance entirely, so a tenant-wide run is never
/// silently narrowed to the cwd scope file's project_id.
pub(super) fn resolve_scope(
    project_dir: &Path,
    tenant_arg: Option<String>,
    project_arg: Option<String>,
) -> Result<(String, Option<String>)> {
    if let Some(tenant_id) = tenant_arg {
        return Ok((tenant_id, project_arg));
    }
    let scope = read_scope_file(project_dir);
    let tenant_id = scope
        .as_ref()
        .and_then(|s| s.tenant_id.clone())
        .ok_or_else(|| {
            MemdError::ValidationError(
                "consolidate requires --tenant-id or .memd/project_scope.json".to_string(),
            )
        })?;
    let project_id = project_arg.or_else(|| scope.and_then(|s| s.project_id));
    Ok((tenant_id, project_id))
}

#[derive(Debug, Deserialize)]
struct ScopeFile {
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
}

/// Read `{tenant_id, project_id}` from `.memd/project_scope.json` or
/// `.memd/config.json`, ignoring any other fields.
fn read_scope_file(project_dir: &Path) -> Option<ScopeFile> {
    for name in [".memd/project_scope.json", ".memd/config.json"] {
        let path = project_dir.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(scope) = serde_json::from_str::<ScopeFile>(&text) {
            return Some(scope);
        }
    }
    None
}

fn consolidate_state_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".memd/data/consolidate.state.json")
}

#[derive(Debug, Default, Deserialize, serde::Serialize)]
struct ConsolidateState {
    #[serde(default)]
    last_consolidation_ms: i64,
}

fn read_last_consolidation_ms(state_path: &Path) -> i64 {
    std::fs::read_to_string(state_path)
        .ok()
        .and_then(|text| serde_json::from_str::<ConsolidateState>(&text).ok())
        .map(|state| state.last_consolidation_ms)
        .unwrap_or(0)
}

fn write_last_consolidation_ms(state_path: &Path, ms: i64) -> Result<()> {
    if let Some(parent) = state_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let state = ConsolidateState {
        last_consolidation_ms: ms,
    };
    std::fs::write(state_path, serde_json::to_string_pretty(&state)? + "\n")?;
    Ok(())
}

/// Build the consolidation working region: chunks written since
/// `since_ms`, unioned with chunk ids seen in recent search logs.
/// `kind:consolidated` / `kind:superseded` chunks are excluded so the
/// region is always raw material, never prior output.
async fn collect_region<S: Store>(
    store: &S,
    tenant: &TenantId,
    project_id: Option<&str>,
    since_ms: i64,
    project_dir: &Path,
    max_region: usize,
) -> Result<Vec<RegionChunk>> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut region: Vec<RegionChunk> = Vec::new();
    let visibility = VisibilityPolicy::default();
    let visibility_now_ms = now_ms();

    let recent = store
        .list_chunks_for_project(tenant, project_id, SCAN_LIMIT, 0)
        .await?;
    for chunk in recent {
        let Some(resolved) = store.get_with_lifecycle(tenant, &chunk.chunk_id).await? else {
            continue;
        };
        if !visibility.is_visible_at(resolved.status, &resolved.lifecycle, visibility_now_ms) {
            continue;
        }
        consider_chunk(
            resolved.chunk,
            RegionReason::NewWrite,
            since_ms,
            project_id,
            &mut seen,
            &mut region,
        );
    }

    for chunk_id in search_log_chunk_ids(project_dir, since_ms) {
        if seen.contains(&chunk_id) {
            continue;
        }
        let Ok(parsed) = ChunkId::parse(&chunk_id) else {
            continue;
        };
        // Lifecycle can change after a search log is written. Resolve its
        // overlay now so an intervening correction cannot revive a stale
        // source and fork its supersession lineage. The lookup remains
        // tenant-scoped, and `consider_chunk` re-checks `project_id`.
        if let Ok(Some(resolved)) = store.get_with_lifecycle(tenant, &parsed).await {
            if !visibility.is_visible_at(resolved.status, &resolved.lifecycle, visibility_now_ms) {
                continue;
            }
            consider_chunk(
                resolved.chunk,
                RegionReason::RecentHit,
                since_ms,
                project_id,
                &mut seen,
                &mut region,
            );
        }
    }

    // Newest first, then id ascending for deterministic ties.
    region.sort_by(|a, b| {
        b.timestamp_created
            .cmp(&a.timestamp_created)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    region.truncate(max_region);
    Ok(region)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionReason {
    NewWrite,
    RecentHit,
}

/// Apply region-membership filters to one chunk and push it if it
/// qualifies. When `project_id` is `Some`, chunks outside that
/// project are rejected — this guards the search-log union path,
/// where chunk ids are resolved tenant-wide.
fn consider_chunk(
    chunk: MemoryChunk,
    reason: RegionReason,
    since_ms: i64,
    project_id: Option<&str>,
    seen: &mut std::collections::HashSet<String>,
    region: &mut Vec<RegionChunk>,
) {
    if let Some(project_id) = project_id {
        if chunk.project_id.as_option() != Some(project_id) {
            return;
        }
    }
    if reason == RegionReason::NewWrite && chunk.timestamp_created <= since_ms {
        return;
    }
    if matches!(
        chunk.status,
        ChunkStatus::Candidate
            | ChunkStatus::Superseded
            | ChunkStatus::Deleted
            | ChunkStatus::Expired
            | ChunkStatus::Error
    ) {
        return;
    }
    if chunk
        .tags
        .iter()
        .any(|t| t.starts_with("kind:consolidated") || t.starts_with("kind:superseded"))
    {
        return;
    }
    let id = chunk.chunk_id.to_string();
    if !seen.insert(id.clone()) {
        return;
    }
    region.push(RegionChunk {
        chunk_id: id,
        chunk_type: chunk.chunk_type.to_string(),
        tags: chunk.tags.clone(),
        timestamp_created: chunk.timestamp_created,
        text: chunk.text,
        project_id: chunk.project_id.as_option().map(str::to_string),
    });
}

/// Scan `.memd/search-logs/*.json` for chunk ids retrieved since
/// `since_ms`. The unix-ms stamp in each filename
/// (`memd_search_<stamp>.json`) bounds the scan. Best-effort: any
/// unreadable or unparseable file is silently skipped.
fn search_log_chunk_ids(project_dir: &Path, since_ms: i64) -> Vec<String> {
    let log_dir = project_dir.join(".memd/search-logs");
    let Ok(entries) = std::fs::read_dir(&log_dir) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") || name.ends_with("_log.jsonl") {
            continue;
        }
        let Some(stamp) = name
            .rsplit_once('_')
            .and_then(|(_, tail)| tail.strip_suffix(".json"))
            .and_then(|s| s.parse::<i64>().ok())
        else {
            continue;
        };
        if stamp <= since_ms {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(results) = payload.get("results").and_then(Value::as_array) {
            for result in results {
                if let Some(id) = result.get("chunk_id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    }
    ids
}

/// The `ctx:*` tags most common across the region, up to `limit`.
/// Consolidated lessons inherit these so subsystem/file context is
/// not lost when raw chunks are tombstoned.
fn most_common_ctx_tags(region: &[RegionChunk], limit: usize) -> Vec<String> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for chunk in region {
        for tag in &chunk.tags {
            if tag.starts_with("ctx:") {
                *counts.entry(tag.as_str()).or_insert(0) += 1;
            }
        }
    }
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    // Frequency desc, then tag asc for deterministic ties.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ranked
        .into_iter()
        .take(limit)
        .map(|(tag, _)| tag.to_string())
        .collect()
}

/// Path of the single-flight lock guarding background consolidation for
/// one scope. The scope is hashed because tenant and project ids are free
/// text and must not shape a filename.
fn spawn_lock_path(data_dir: &Path, tenant_id: &str, project_id: Option<&str>) -> PathBuf {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(tenant_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(project_id.unwrap_or("").as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    data_dir.join(format!(".consolidate-{}.lock", &digest[..16]))
}

/// Outcome of trying to claim the single-flight lock for one scope.
enum SpawnClaim {
    /// Free to spawn. Carries the parent's descriptor when a lock was taken,
    /// and `None` on platforms without `flock`, where no guard is available
    /// and the previous unguarded behavior stands.
    Free(Option<std::fs::File>),
    /// Another background consolidation already owns this scope.
    Busy,
}

/// Try to claim the spawn lock for one scope.
///
/// The returned descriptor keeps `FD_CLOEXEC`; [`inherit_lock_fd`] is what
/// hands it to a single child. The parent closes its own copy right after
/// spawning, so the lock lives as long as the child and the kernel releases it
/// even if the child is killed.
#[cfg(unix)]
fn try_claim_spawn_lock(path: &Path) -> Result<SpawnClaim> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let opened = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    // Keep the lock off fds 0-2. If stdin/stdout/stderr were closed, open()
    // hands back one of them and the child's Stdio::null() setup would later
    // overwrite it, silently dropping the guard and restoring the herd.
    let file = if opened.as_raw_fd() < 3 {
        let raw = unsafe { libc::fcntl(opened.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if raw < 0 {
            return Err(MemdError::IoError(std::io::Error::last_os_error()));
        }
        drop(opened);
        std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) })
    } else {
        opened
    };
    let fd = file.as_raw_fd();
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        // EWOULDBLOCK and EAGAIN are the same value on Linux; compare rather
        // than match so the duplicate arm does not read as unreachable.
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.raw_os_error() == Some(libc::EAGAIN)
        {
            return Ok(SpawnClaim::Busy);
        }
        return Err(MemdError::IoError(err));
    }
    Ok(SpawnClaim::Free(Some(file)))
}

/// Hand the claimed lock to one specific child.
///
/// `FD_CLOEXEC` is cleared after fork and before exec, so the descriptor
/// reaches this child and nothing else. Clearing it on the parent's own
/// descriptor instead would leak the lock into every process spawned by any
/// thread while the claim is open, and those processes would hold the scope
/// blocked for as long as they live.
#[cfg(unix)]
fn inherit_lock_fd(command: &mut std::process::Command, lock: Option<&std::fs::File>) {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let Some(file) = lock else { return };
    let fd = file.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            // `fork` copies the whole descriptor table, so this child starts out
            // holding every descriptor the parent had, including the store's
            // SQLite and Tantivy locks. Only `FD_CLOEXEC` drops them at `exec`,
            // and this code cannot guarantee every descriptor in the process
            // carries it. Mark the whole range first, then clear the flag on the
            // one descriptor the child is meant to keep, so inheritance is
            // stated here rather than inferred from every other open file.
            //
            // CLOSE_RANGE_CLOEXEC sets the flag instead of closing, so a
            // descriptor std still needs for stdio setup is not pulled out from
            // under it. Kernels without the call (pre-5.9) fall back to the
            // previous behavior.
            if libc::close_range(3, libc::c_uint::MAX, libc::CLOSE_RANGE_CLOEXEC as i32) != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ENOSYS) {
                    return Err(err);
                }
            }
            // fcntl is async-signal-safe, which pre_exec requires.
            if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Without `flock` there is no guard, so spawning proceeds as it did before.
#[cfg(not(unix))]
fn try_claim_spawn_lock(_path: &Path) -> Result<SpawnClaim> {
    Ok(SpawnClaim::Free(None))
}

/// Re-exec `memd consolidate` as a detached background child and
/// return immediately. The child runs the same scope without
/// `--background`.
///
/// `lock_root` enables single-flight: without it every `memd session-start`
/// stacks another detached child, and because those children cannot finish
/// while they queue behind each other the dirty region never shrinks below
/// [`MIN_REGION`], so each new session spawns one more. The guard has to sit
/// here in the parent rather than in the child's CLI handler, because the
/// child opens the store (contacting the warm worker) before any handler
/// runs, which is the contention being avoided.
fn spawn_background(lock_root: Option<&Path>, options: &ConsolidateOptions) -> Result<Value> {
    let mut lock = None;
    let mut options = options.clone();
    if let Some(data_dir) = lock_root {
        let (tenant_id, project_id) = resolve_scope(
            &options.project_dir,
            options.tenant_id.clone(),
            options.project_id.clone(),
        )?;
        let path = spawn_lock_path(data_dir, &tenant_id, project_id.as_deref());
        match try_claim_spawn_lock(&path)? {
            SpawnClaim::Free(guard) => lock = guard,
            SpawnClaim::Busy => return Ok(json!({ "skipped": "already_running" })),
        }
        // Pin the child to the scope the lock was keyed on. Left implicit, the
        // child re-resolves the scope file after the spawn, and a scope edit in
        // that window would have it consolidate one scope while holding
        // another's lock, letting a second child claim the free one.
        options.tenant_id = Some(tenant_id);
        options.project_id = project_id;
    }
    let result = spawn_background_inner(&options, lock.as_ref());
    // Explicit: closes only the parent's descriptor, after the child has been
    // spawned and inherited its own copy, which is what keeps the flock held.
    drop(lock);
    result
}

fn spawn_background_inner(
    options: &ConsolidateOptions,
    #[cfg_attr(not(unix), allow(unused_variables))] lock: Option<&std::fs::File>,
) -> Result<Value> {
    let exe = std::env::current_exe()
        .map_err(|e| MemdError::ProtocolError(format!("cannot resolve current executable: {e}")))?;
    let mut command = std::process::Command::new(exe);
    command.arg("consolidate");
    if let Some(tenant) = &options.tenant_id {
        command.arg("--tenant-id").arg(tenant);
    }
    if let Some(project) = &options.project_id {
        command.arg("--project-id").arg(project);
    }
    command
        .arg("--project-dir")
        .arg(&options.project_dir)
        .arg("--max-region")
        .arg(options.max_region.to_string());
    if options.force {
        command.arg("--force");
    }
    if options.promote {
        command.arg("--promote");
    }
    if options.legacy_immediate {
        command.arg("--legacy-immediate");
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        inherit_lock_fd(&mut command, lock);
    }
    let child = command.spawn().map_err(|e| {
        MemdError::ProtocolError(format!("failed to spawn background consolidate: {e}"))
    })?;
    Ok(json!({ "spawned_background": true, "pid": child.id() }))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consolidate::MockEnvConsolidator;
    use crate::mcp::handlers::{handle_memory_search, SearchParams};
    use crate::store::metadata::MetadataStore;
    use crate::store::persistent::{PersistentStore, PersistentStoreConfig};
    use crate::store::Store;
    use crate::types::lifecycle::LifecycleDelta;
    use crate::types::{ChunkStatus, ChunkType, ProjectId};
    use tempfile::tempdir;

    /// Serialises tests that set the process-global `MOCK_RESPONSE_ENV`.
    static MOCK_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn spawn_lock_is_scope_keyed() {
        let dir = tempdir().unwrap();
        let a = spawn_lock_path(dir.path(), "default", Some("virosync"));
        assert_eq!(a, spawn_lock_path(dir.path(), "default", Some("virosync")));
        assert_ne!(a, spawn_lock_path(dir.path(), "default", Some("memd")));
        assert_ne!(a, spawn_lock_path(dir.path(), "other", Some("virosync")));
        // A free-text scope must not shape the filename.
        let hostile = spawn_lock_path(dir.path(), "../../etc", Some("a/b"));
        assert_eq!(hostile.parent(), Some(dir.path()));
    }

    /// The guard is only useful if the lock outlives the parent that took it.
    /// Hold it across a real `exec`, then prove a second claim is refused while
    /// that child lives and succeeds once it is gone.
    #[cfg(unix)]
    #[test]
    fn spawn_lock_survives_exec_and_releases_with_the_child() {
        let dir = tempdir().unwrap();
        let path = spawn_lock_path(dir.path(), "default", Some("virosync"));

        let held = match try_claim_spawn_lock(&path).unwrap() {
            SpawnClaim::Free(guard) => guard.expect("unix claim holds a descriptor"),
            SpawnClaim::Busy => panic!("first claim must succeed"),
        };
        // Hand the lock over exactly as spawn_background_inner does.
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        inherit_lock_fd(&mut command, Some(&held));
        let mut child = command.spawn().expect("spawn holder");
        // The parent's descriptor goes away exactly as it does after a real
        // background spawn; only the child's inherited copy remains.
        drop(held);

        assert!(
            matches!(try_claim_spawn_lock(&path).unwrap(), SpawnClaim::Busy),
            "second claim must be refused while the child holds the inherited lock"
        );

        child.kill().unwrap();
        child.wait().unwrap();
        // `FD_CLOEXEC` bounds `exec`, not `fork`: any of the ~1000 tests in this
        // binary that spawns a process during the claim window inherits the
        // descriptor until its own exec runs, and holds the lock for that
        // instant. Allow those transient holders to clear before concluding the
        // kernel failed to release.
        let released = (0..200).any(|_| {
            if matches!(try_claim_spawn_lock(&path).unwrap(), SpawnClaim::Free(_)) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        });
        assert!(
            released,
            "kernel must release the lock when the child dies, leaving no stale state"
        );
    }

    fn persistent_store(dir: &Path) -> PersistentStore {
        let cfg = PersistentStoreConfig {
            data_dir: dir.to_path_buf(),
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        PersistentStore::open(cfg).expect("open store")
    }

    fn region_chunk(id: &str, ctx: &[&str]) -> RegionChunk {
        RegionChunk {
            chunk_id: id.to_string(),
            chunk_type: "summary".to_string(),
            tags: ctx.iter().map(|t| t.to_string()).collect(),
            timestamp_created: 1,
            text: "t".to_string(),
            project_id: None,
        }
    }

    #[test]
    fn most_common_ctx_prefers_frequent_tags() {
        let region = vec![
            region_chunk("a", &["ctx:file:x", "ctx:subsystem:s"]),
            region_chunk("b", &["ctx:file:x"]),
            region_chunk("c", &["ctx:file:y"]),
        ];
        let tags = most_common_ctx_tags(&region, 2);
        assert_eq!(tags[0], "ctx:file:x");
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn state_roundtrip() {
        let dir = tempdir().unwrap();
        let path = consolidate_state_path(dir.path());
        assert_eq!(read_last_consolidation_ms(&path), 0);
        write_last_consolidation_ms(&path, 4242).unwrap();
        assert_eq!(read_last_consolidation_ms(&path), 4242);
    }

    #[tokio::test]
    async fn recent_search_hit_includes_chunk_created_before_watermark() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        let mut old_chunk = MemoryChunk::new(
            tenant.clone(),
            "old lesson retrieved after the last consolidation",
            ChunkType::Summary,
        )
        .with_project(ProjectId::from("p"));
        old_chunk.timestamp_created = 100;
        let old_id = store.add(old_chunk).await.unwrap();

        let log_dir = dir.path().join(".memd/search-logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("memd_search_2000.json"),
            serde_json::to_string(&json!({
                "results": [{"chunk_id": old_id.to_string()}]
            }))
            .unwrap(),
        )
        .unwrap();

        let region = collect_region(&store, &tenant, Some("p"), 1000, dir.path(), 50)
            .await
            .unwrap();

        assert_eq!(region.len(), 1, "a recent hit must revive an old chunk");
        assert_eq!(region[0].chunk_id, old_id.to_string());
    }

    #[tokio::test]
    async fn recent_search_hit_excludes_chunk_superseded_after_search() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        let mut old_chunk = MemoryChunk::new(
            tenant.clone(),
            "stale lesson retrieved before a later correction",
            ChunkType::Summary,
        )
        .with_project(ProjectId::from("p"));
        old_chunk.timestamp_created = 100;
        let old_id = store.add(old_chunk).await.unwrap();

        let log_dir = dir.path().join(".memd/search-logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("memd_search_2000.json"),
            serde_json::to_string(&json!({
                "results": [{"chunk_id": old_id.to_string()}]
            }))
            .unwrap(),
        )
        .unwrap();

        store
            .update_lifecycle_if_exists(
                &tenant,
                &old_id,
                &LifecycleDelta {
                    status: Some(ChunkStatus::Superseded),
                    superseded_by: Some(ChunkId::new()),
                    lifecycle_updated_at_ms: Some(2500),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let region = collect_region(&store, &tenant, Some("p"), 1000, dir.path(), 50)
            .await
            .unwrap();

        assert!(
            region.is_empty(),
            "a recent hit superseded after search must not re-enter consolidation"
        );
    }

    #[tokio::test]
    async fn new_write_excludes_chunk_superseded_before_consolidation() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        let mut stale_chunk = MemoryChunk::new(
            tenant.clone(),
            "new lesson corrected before consolidation",
            ChunkType::Summary,
        )
        .with_project(ProjectId::from("p"));
        stale_chunk.timestamp_created = 2000;
        let stale_id = store.add(stale_chunk).await.unwrap();
        store
            .update_lifecycle_if_exists(
                &tenant,
                &stale_id,
                &LifecycleDelta {
                    status: Some(ChunkStatus::Superseded),
                    superseded_by: Some(ChunkId::new()),
                    lifecycle_updated_at_ms: Some(2500),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let region = collect_region(&store, &tenant, Some("p"), 1000, dir.path(), 50)
            .await
            .unwrap();

        assert!(
            region.is_empty(),
            "a new write superseded before consolidation must not enter the region"
        );
    }

    #[tokio::test]
    async fn recent_search_hits_reject_foreign_project_chunks() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        let own_id = store
            .add(
                MemoryChunk::new(tenant.clone(), "own project lesson", ChunkType::Summary)
                    .with_project(ProjectId::from("p")),
            )
            .await
            .unwrap();
        let foreign_id = store
            .add(
                MemoryChunk::new(tenant.clone(), "foreign project lesson", ChunkType::Summary)
                    .with_project(ProjectId::from("q")),
            )
            .await
            .unwrap();

        let log_dir = dir.path().join(".memd/search-logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("memd_search_2000.json"),
            serde_json::to_string(&json!({
                "results": [
                    {"chunk_id": own_id.to_string()},
                    {"chunk_id": foreign_id.to_string()}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let region = collect_region(&store, &tenant, Some("p"), 0, dir.path(), 50)
            .await
            .unwrap();

        let ids = region
            .iter()
            .map(|chunk| chunk.chunk_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert!(ids.contains(own_id.to_string().as_str()));
        assert!(!ids.contains(foreign_id.to_string().as_str()));
    }

    #[tokio::test]
    async fn region_scan_boundary_is_deterministic_for_equal_timestamps() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        let mut expected = Vec::new();
        for i in 0..=SCAN_LIMIT {
            let mut chunk = MemoryChunk::new(
                tenant.clone(),
                format!("equal timestamp lesson {i}"),
                ChunkType::Summary,
            )
            .with_project(ProjectId::from("p"));
            chunk.timestamp_created = 2000;
            expected.push(store.add(chunk).await.unwrap().to_string());
        }
        expected.sort();
        expected.truncate(SCAN_LIMIT);

        let region = collect_region(&store, &tenant, Some("p"), 1000, dir.path(), SCAN_LIMIT)
            .await
            .unwrap();
        let actual = region
            .iter()
            .map(|chunk| chunk.chunk_id.clone())
            .collect::<Vec<_>>();

        assert_eq!(actual, expected);
    }

    #[test]
    fn unstamped_search_log_is_ignored() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path().join(".memd/search-logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("manual.json"),
            serde_json::to_string(&json!({
                "results": [{"chunk_id": ChunkId::new().to_string()}]
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(search_log_chunk_ids(dir.path(), 1000).is_empty());
    }

    #[test]
    fn resolve_scope_prefers_explicit_args() {
        let dir = tempdir().unwrap();
        let (tenant, project) =
            resolve_scope(dir.path(), Some("t".to_string()), Some("p".to_string())).unwrap();
        assert_eq!(tenant, "t");
        assert_eq!(project.as_deref(), Some("p"));
    }

    #[test]
    fn resolve_scope_reads_config_json() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/config.json"),
            r#"{"tenant_id":"ct","project_id":"cp"}"#,
        )
        .unwrap();
        let (tenant, project) = resolve_scope(dir.path(), None, None).unwrap();
        assert_eq!(tenant, "ct");
        assert_eq!(project.as_deref(), Some("cp"));
    }

    #[tokio::test]
    async fn below_threshold_skips_without_force() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        for i in 0..3 {
            store
                .add(
                    MemoryChunk::new(tenant.clone(), format!("chunk {i}"), ChunkType::Summary)
                        .with_project(ProjectId::from("p")),
                )
                .await
                .unwrap();
        }
        let opts = ConsolidateOptions {
            tenant_id: Some("t".to_string()),
            project_id: Some("p".to_string()),
            project_dir: dir.path().to_path_buf(),
            max_region: 50,
            dry_run: false,
            background: false,
            force: false,
            promote: false,
            legacy_immediate: false,
        };
        let consolidator = MockEnvConsolidator;
        let result = consolidate_core(&store, opts, &consolidator).await.unwrap();
        assert_eq!(result["skipped"], "below_threshold");
    }

    #[tokio::test]
    async fn consolidate_writes_and_tombstones() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        let mut ids = Vec::new();
        for i in 0..12 {
            let id = store
                .add(
                    MemoryChunk::new(
                        tenant.clone(),
                        format!("duplicate lesson {i}: cache keys must be tenant-scoped"),
                        ChunkType::Summary,
                    )
                    .with_project(ProjectId::from("p"))
                    .with_tags(vec!["ctx:subsystem:cache".to_string()]),
                )
                .await
                .unwrap();
            ids.push(id.to_string());
        }
        let ids_json = serde_json::to_string(&ids).unwrap();
        let response = format!(
            r#"[{{"text":"Cache keys must be tenant-scoped.","agent_action":"Reuse tenant-scoped keys when repairing this cache failure.","evidence":{ids_json},"confidence":0.9,"supersedes":{ids_json},"priority":8}}]"#,
        );
        let _guard = MOCK_ENV_LOCK.lock().await;
        std::env::set_var(crate::consolidate::MOCK_RESPONSE_ENV, &response);

        let opts = ConsolidateOptions {
            tenant_id: Some("t".to_string()),
            project_id: Some("p".to_string()),
            project_dir: dir.path().to_path_buf(),
            max_region: 50,
            dry_run: false,
            background: false,
            force: false,
            promote: true,
            legacy_immediate: false,
        };
        let consolidator = MockEnvConsolidator;
        let result = consolidate_core(&store, opts, &consolidator).await.unwrap();
        std::env::remove_var(crate::consolidate::MOCK_RESPONSE_ENV);

        assert_eq!(result["consolidated"], 1);
        assert_eq!(result["tombstoned"], 12);
        // Project-scoped runs must not warn about tenant-wide writes.
        assert!(result.get("warning").is_none());

        // Sources are now superseded; the consolidated chunk carries
        // the provenance tag.
        let new_id = result["new_chunk_ids"][0].as_str().unwrap();
        let consolidated = store
            .get(&tenant, &ChunkId::parse(new_id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(consolidated.tags.iter().any(|t| t == "kind:consolidated"));
        assert!(consolidated
            .tags
            .iter()
            .any(|t| t.starts_with("ctx:subsystem:cache")));
    }

    #[tokio::test]
    async fn consolidate_stages_by_default_without_hiding_sources() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        let mut ids = Vec::new();
        for i in 0..12 {
            ids.push(
                store
                    .add(
                        MemoryChunk::new(
                            tenant.clone(),
                            format!("staged lesson {i}: preserve the source until review"),
                            ChunkType::Summary,
                        )
                        .with_project(ProjectId::from("p")),
                    )
                    .await
                    .unwrap()
                    .to_string(),
            );
        }
        let ids_json = serde_json::to_string(&ids).unwrap();
        let response = format!(
            r#"[{{"text":"Sources remain visible until explicit review.","agent_action":"Verify the staged candidate before accepting its replacement.","evidence":{ids_json},"confidence":0.9,"supersedes":{ids_json},"priority":7}}]"#,
        );
        let _guard = MOCK_ENV_LOCK.lock().await;
        std::env::set_var(crate::consolidate::MOCK_RESPONSE_ENV, &response);
        let result = consolidate_core(
            &store,
            ConsolidateOptions {
                tenant_id: Some("t".to_string()),
                project_id: Some("p".to_string()),
                project_dir: dir.path().to_path_buf(),
                max_region: 50,
                dry_run: false,
                background: false,
                force: false,
                promote: false,
                legacy_immediate: false,
            },
            &MockEnvConsolidator,
        )
        .await
        .unwrap();
        std::env::remove_var(crate::consolidate::MOCK_RESPONSE_ENV);

        assert_eq!(result["state"], "validated");
        assert_eq!(result["staged"], 1);
        assert_eq!(result["consolidated"], 0);
        assert_eq!(result["tombstoned"], 0);
        for source_id in ids {
            let source = store
                .metadata()
                .get(&tenant, &ChunkId::parse(&source_id).unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(source.status, ChunkStatus::Final);
        }
        let candidate_id = ChunkId::parse(
            result["candidate_chunk_ids"][0]
                .as_str()
                .expect("candidate id"),
        )
        .unwrap();
        assert_eq!(
            store
                .metadata()
                .get(&tenant, &candidate_id)
                .unwrap()
                .unwrap()
                .status,
            ChunkStatus::Candidate
        );
        let pending = run_consolidate_review(&store, None, true, 100, false, false)
            .await
            .unwrap();
        assert_eq!(pending["count"], 1);
        assert_eq!(pending["staged_runs"][0]["run_id"], result["run_id"]);
    }

    #[tokio::test]
    async fn dry_run_emits_prompt_without_calling_llm() {
        let dir = tempdir().unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        for i in 0..12 {
            store
                .add(
                    MemoryChunk::new(tenant.clone(), format!("chunk {i}"), ChunkType::Summary)
                        .with_project(ProjectId::from("p")),
                )
                .await
                .unwrap();
        }
        let opts = ConsolidateOptions {
            tenant_id: Some("t".to_string()),
            project_id: Some("p".to_string()),
            project_dir: dir.path().to_path_buf(),
            max_region: 50,
            dry_run: true,
            background: false,
            force: false,
            promote: false,
            legacy_immediate: false,
        };
        // A mock with no response env set must NOT be invoked.
        let consolidator = MockEnvConsolidator;
        let result = consolidate_core(&store, opts, &consolidator).await.unwrap();
        assert_eq!(result["dry_run"], true);
        assert!(result["prompt"].as_str().unwrap().contains("CHUNKS:"));
    }

    #[tokio::test]
    async fn explicit_tenant_ignores_scope_file_project() {
        let dir = tempdir().unwrap();
        // cwd scope file names another tenant/project; an explicit
        // --tenant-id must not inherit its project_id, so the region
        // stays tenant-wide (the old code narrowed it to 0 chunks).
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{"tenant_id":"other_tenant","project_id":"other_project"}"#,
        )
        .unwrap();
        let store = persistent_store(&dir.path().join("store"));
        let tenant = TenantId::new("t").unwrap();
        let mut ids = Vec::new();
        for i in 0..12 {
            let id = store
                .add(
                    MemoryChunk::new(tenant.clone(), format!("lesson {i}"), ChunkType::Summary)
                        .with_project(ProjectId::from("p")),
                )
                .await
                .unwrap();
            ids.push(id.to_string());
        }
        let ids_json = serde_json::to_string(&ids).unwrap();
        let response = format!(
            r#"[{{"text":"One tenant-wide lesson.","agent_action":"Reuse this tenant-wide lesson only after checking its sources.","evidence":{ids_json},"confidence":0.9,"supersedes":{ids_json},"priority":8}}]"#,
        );
        let _guard = MOCK_ENV_LOCK.lock().await;
        std::env::set_var(crate::consolidate::MOCK_RESPONSE_ENV, &response);
        let opts = ConsolidateOptions {
            tenant_id: Some("t".to_string()),
            project_id: None,
            project_dir: dir.path().to_path_buf(),
            max_region: 50,
            dry_run: false,
            background: false,
            force: false,
            promote: true,
            legacy_immediate: false,
        };
        let result = consolidate_core(&store, opts.clone(), &MockEnvConsolidator)
            .await
            .unwrap();
        std::env::remove_var(crate::consolidate::MOCK_RESPONSE_ENV);

        // Old behavior borrowed "other_project" from the scope file:
        // region_size 0 -> skipped:below_threshold.
        assert_eq!(result["region_size"], 12);
        assert_eq!(result["consolidated"], 1);
        assert!(result["project_id"].is_null());
        assert!(result["warning"]
            .as_str()
            .unwrap()
            .contains("project-scoped searches"));
        assert_eq!(result["tombstoned"], 0);
        let new_id = ChunkId::parse(result["new_chunk_ids"][0].as_str().unwrap()).unwrap();
        let tenant_wide = store.get(&tenant, &new_id).await.unwrap().unwrap();
        assert!(tenant_wide
            .tags
            .iter()
            .any(|tag| tag.starts_with("derives_from:")));
        assert!(!tenant_wide
            .tags
            .iter()
            .any(|tag| tag.starts_with("supersedes:")));

        // A tenant-wide synthesis is additional machine-wide knowledge, not
        // a project-visible replacement. Its project-scoped sources must stay
        // active until a replacement exists in the same project scope.
        for id in &ids {
            let resolved = store
                .get_with_lifecycle(&tenant, &ChunkId::parse(id).unwrap())
                .await
                .unwrap()
                .expect("tenant-wide source remains stored");
            assert_eq!(
                resolved.status,
                ChunkStatus::Final,
                "tenant-wide consolidation must retain project source {id}"
            );
            assert!(resolved.lifecycle.superseded_by.is_none());
        }

        let response = handle_memory_search(
            &store,
            SearchParams {
                tenant_id: "t".to_string(),
                project_id: Some("p".to_string()),
                query: "lesson".to_string(),
                k: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let text = response["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        let hits = payload["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["chunk_id"].as_str())
            .collect::<std::collections::HashSet<_>>();
        for id in &ids {
            assert!(
                hits.contains(id.as_str()),
                "project search must retain tenant-wide source {id}"
            );
        }

        let repeated = consolidate_core(&store, opts, &MockEnvConsolidator)
            .await
            .unwrap();
        assert_eq!(repeated["skipped"], "below_threshold");
        assert_eq!(repeated["region_size"], 0);
    }
}
