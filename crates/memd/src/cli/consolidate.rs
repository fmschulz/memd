//! `memd consolidate` — LLM-backed memory consolidation (Phase 2).
//!
//! Builds a working region from chunks written (and retrieved) since
//! the last run, asks the configured [`Consolidator`] to rewrite them
//! into a smaller deduplicated set of `kind:consolidated` lessons,
//! persists those lessons, and soft-tombstones the superseded
//! sources. Nothing is ever deleted — superseded chunks stay on disk
//! and are merely hidden from retrieval.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use super::paths::absolutize_project_dir;
use crate::consolidate::prompt::{
    build_consolidation_prompt, parse_consolidation_response, ConsolidatedEntry, RegionChunk,
};
use crate::consolidate::select::select_consolidator;
use crate::consolidate::Consolidator;
use crate::error::{MemdError, Result};
use crate::store::Store;
use crate::types::lifecycle::LifecycleDelta;
use crate::types::{ChunkId, ChunkStatus, ChunkType, MemoryChunk, ProjectId, TenantId};

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
}

/// Entry point: resolves the consolidator backend from the
/// environment, then runs the consolidation.
pub(super) async fn run_consolidate<S: Store>(
    store: &S,
    options: ConsolidateOptions,
) -> Result<Value> {
    if options.background {
        return spawn_background(&options);
    }
    let consolidator = select_consolidator()?;
    consolidate_core(store, options, consolidator.as_ref()).await
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

    let raw = consolidator.consolidate(&prompt).await?;
    let entries = parse_consolidation_response(&raw, &region)?;

    let now = now_ms();
    let inherited_ctx = most_common_ctx_tags(&region, 3);
    let mut written = Vec::new();
    let mut tombstoned = 0usize;

    for entry in &entries {
        let new_id = persist_consolidated(
            store,
            &tenant,
            project_id.as_deref(),
            entry,
            consolidator.name(),
            &inherited_ctx,
        )
        .await?;
        tombstoned += tombstone_sources(store, &tenant, entry, &new_id, now).await?;
        written.push(new_id.to_string());
    }

    write_last_consolidation_ms(&state_path, now)?;

    // Tenant-wide runs persist lessons with no project_id; flag that
    // project-scoped searches will not surface them.
    let tenant_wide_write = project_id.is_none() && !written.is_empty();
    let mut summary = json!({
        "tenant_id": tenant_id,
        "project_id": project_id,
        "region_size": region.len(),
        "consolidated": written.len(),
        "tombstoned": tombstoned,
        "consolidator": consolidator.name(),
        "new_chunk_ids": written,
    });
    if tenant_wide_write {
        summary["warning"] = json!(
            "consolidated lessons written without project_id; project-scoped searches \
             will not see them (they surface via tenant-wide search and memory-md \
             Machine-Wide Fact Library)"
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

    let recent = store
        .list_chunks_for_project(tenant, project_id, SCAN_LIMIT, 0)
        .await?;
    for chunk in recent {
        consider_chunk(chunk, since_ms, project_id, &mut seen, &mut region);
    }

    for chunk_id in search_log_chunk_ids(project_dir, since_ms) {
        if seen.contains(&chunk_id) {
            continue;
        }
        let Ok(parsed) = ChunkId::parse(&chunk_id) else {
            continue;
        };
        // `store.get` is tenant-scoped only — `consider_chunk`
        // re-checks `project_id` so a project consolidation never
        // ingests (and later tombstones) another project's chunk.
        if let Ok(Some(chunk)) = store.get(tenant, &parsed).await {
            consider_chunk(chunk, since_ms, project_id, &mut seen, &mut region);
        }
    }

    // Newest first, then cap.
    region.sort_by(|a, b| b.timestamp_created.cmp(&a.timestamp_created));
    region.truncate(max_region);
    Ok(region)
}

/// Apply region-membership filters to one chunk and push it if it
/// qualifies. When `project_id` is `Some`, chunks outside that
/// project are rejected — this guards the search-log union path,
/// where chunk ids are resolved tenant-wide.
fn consider_chunk(
    chunk: MemoryChunk,
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
    if chunk.timestamp_created <= since_ms {
        return;
    }
    if matches!(
        chunk.status,
        ChunkStatus::Superseded | ChunkStatus::Deleted | ChunkStatus::Expired
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
        if let Some(stamp) = name
            .rsplit_once('_')
            .and_then(|(_, tail)| tail.strip_suffix(".json"))
            .and_then(|s| s.parse::<i64>().ok())
        {
            if stamp <= since_ms {
                continue;
            }
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

/// Persist one consolidated lesson and return its new chunk id.
async fn persist_consolidated<S: Store>(
    store: &S,
    tenant: &TenantId,
    project_id: Option<&str>,
    entry: &ConsolidatedEntry,
    consolidator_name: &str,
    inherited_ctx: &[String],
) -> Result<ChunkId> {
    let mut tags = vec![
        "kind:consolidated".to_string(),
        format!("priority:{}", entry.priority),
        format!("supersedes:{}", entry.supersedes.join(",")),
        format!("consolidator:{consolidator_name}"),
    ];
    tags.extend(inherited_ctx.iter().cloned());

    let mut chunk = MemoryChunk::new(tenant.clone(), &entry.text, ChunkType::Summary);
    if let Some(project_id) = project_id {
        chunk = chunk.with_project(ProjectId::from(project_id));
    }
    chunk = chunk.with_tags(tags);
    store.add(chunk).await
}

/// Soft-tombstone every source chunk an `entry` supersedes: set
/// `status = Superseded` and `superseded_by = <new_id>` on the
/// lifecycle overlay. The payload is never deleted. Returns the count
/// of sources successfully tombstoned.
async fn tombstone_sources<S: Store>(
    store: &S,
    tenant: &TenantId,
    entry: &ConsolidatedEntry,
    new_id: &ChunkId,
    now_ms: i64,
) -> Result<usize> {
    let persistent = store.as_persistent().ok_or_else(|| {
        MemdError::ValidationError(
            "consolidate requires a persistent store to tombstone superseded chunks".to_string(),
        )
    })?;
    let mut count = 0usize;
    for source_id in &entry.supersedes {
        let Ok(parsed) = ChunkId::parse(source_id) else {
            continue;
        };
        let delta = LifecycleDelta {
            status: Some(ChunkStatus::Superseded),
            superseded_by: Some(new_id.clone()),
            lifecycle_updated_at_ms: Some(now_ms),
            ..Default::default()
        };
        if persistent
            .update_lifecycle_if_exists(tenant, &parsed, &delta)
            .await?
        {
            count += 1;
        }
    }
    Ok(count)
}

/// Re-exec `memd consolidate` as a detached background child and
/// return immediately. The child runs the same scope without
/// `--background`.
fn spawn_background(options: &ConsolidateOptions) -> Result<Value> {
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
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
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
    use crate::store::persistent::{PersistentStore, PersistentStoreConfig};
    use crate::store::Store;
    use tempfile::tempdir;

    /// Serialises tests that set the process-global `MOCK_RESPONSE_ENV`.
    static MOCK_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let response = format!(
            r#"[{{"text":"Cache keys must be tenant-scoped.","supersedes":{},"priority":8}}]"#,
            serde_json::to_string(&ids).unwrap()
        );
        let _guard = MOCK_ENV_LOCK.lock().unwrap();
        std::env::set_var(crate::consolidate::MOCK_RESPONSE_ENV, &response);

        let opts = ConsolidateOptions {
            tenant_id: Some("t".to_string()),
            project_id: Some("p".to_string()),
            project_dir: dir.path().to_path_buf(),
            max_region: 50,
            dry_run: false,
            background: false,
            force: false,
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
        let response = format!(
            r#"[{{"text":"One tenant-wide lesson.","supersedes":{},"priority":8}}]"#,
            serde_json::to_string(&ids).unwrap()
        );
        let _guard = MOCK_ENV_LOCK.lock().unwrap();
        std::env::set_var(crate::consolidate::MOCK_RESPONSE_ENV, &response);
        let opts = ConsolidateOptions {
            tenant_id: Some("t".to_string()),
            project_id: None,
            project_dir: dir.path().to_path_buf(),
            max_region: 50,
            dry_run: false,
            background: false,
            force: false,
        };
        let result = consolidate_core(&store, opts, &MockEnvConsolidator)
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
    }
}
