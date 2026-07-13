//! `memd doctor` — diagnose host wiring and per-repo scope.
//!
//! Reports whether the user's machine is set up for the
//! "clone → it just works in any repo" UX:
//!
//! - `memd` binary discoverable + version
//! - Data directory present (and chunk count if readable)
//! - Global agent rules wired: Claude Code (`~/.claude/CLAUDE.md`),
//!   Codex (`~/.codex/AGENTS.md`), Cursor (`~/.cursor/rules/*.mdc`)
//! - Claude Code `SessionStart` hook wired in `~/.claude/settings.json`
//! - Current project scope (`.memd/project_scope.json`) if cwd is a
//!   memd-aware repo
//!
//! By default `doctor` exits 0 and is informational; `--strict` exits 2
//! when any doctor check fails. Use `--format json` for machine-readable
//! output suitable for `--quiet` checks.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::args::ExportFormat;
use crate::error::Result;
use crate::store::Store;
use crate::types::TenantId;

/// Marker the installer drops into agent rule files. Identical
/// substring is used by both `~/.claude/CLAUDE.md` and
/// `~/.codex/AGENTS.md`, so a single grep covers both surfaces.
const ENFORCEMENT_MARKER: &str = "memd-enforcement:start";

/// Marker used inside the wired `SessionStart` hook command.
const SESSION_HOOK_MARKER: &str = "memd session-start";
const PROJECT_DRIFT_LOW_CHUNK_THRESHOLD: usize = 2;
const PROJECT_DRIFT_MIN_DOMINANT_CHUNKS: usize = 10;
const PROJECT_DRIFT_DOMINANCE_RATIO: usize = 5;
const PROJECT_DRIFT_SCAN_LIMIT: usize = 5_000;
const PROJECT_DRIFT_PAGE_SIZE: usize = 500;

#[derive(Debug, Clone)]
pub(super) struct DoctorOptions {
    pub(super) project_dir: PathBuf,
    /// Resolved global --data-dir; doctor diagnoses this store, not a
    /// hardcoded ~/.memd/data.
    pub(super) data_dir: Option<PathBuf>,
    pub(super) format: ExportFormat,
}

/// Run the doctor command. Returns the structured report as JSON; the
/// CLI dispatcher renders it according to `options.format`.
pub(super) async fn run_doctor<S: Store>(store: &S, options: DoctorOptions) -> Result<Value> {
    let mut report = collect_report(&options.project_dir, options.data_dir.as_deref()).await;
    if let Some(scope) = report.get_mut("project_scope") {
        enrich_project_scope_memory(store, scope).await;
    }
    match options.format {
        ExportFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ExportFormat::Jsonl => {
            println!("{}", serde_json::to_string(&report)?);
        }
        ExportFormat::Markdown => {
            print!("{}", render_text(&report));
        }
    }
    Ok(report)
}

async fn collect_report(project_dir: &Path, resolved_data_dir: Option<&Path>) -> Value {
    let binary = check_binary();
    let data_dir = check_data_dir(resolved_data_dir);
    let warm_worker = check_warm_worker(resolved_data_dir).await;
    let claude_rules = check_rules_file(home_path(".claude/CLAUDE.md").as_deref());
    let codex_rules = check_rules_file(home_path(".codex/AGENTS.md").as_deref());
    let cursor_rules = check_cursor_rules();
    let session_hook = check_session_hook();
    let project_scope = check_project_scope(project_dir);

    json!({
        "binary": binary,
        "data_dir": data_dir,
        "warm_worker": warm_worker,
        "global_rules": {
            "claude_md": claude_rules,
            "codex_agents_md": codex_rules,
            "cursor_rules_mdc": cursor_rules,
        },
        "session_start_hook": session_hook,
        "project_scope": project_scope,
    })
}

fn check_binary() -> Value {
    let exe = std::env::current_exe().ok();
    let on_path = find_on_path("memd");
    let version = env!("CARGO_PKG_VERSION");
    // "ok" means `memd` is discoverable via PATH AND not version-skewed
    // against this process — a stale PATH binary means hooks and skills
    // run old code no matter what was just built.
    let path_version = on_path.as_deref().and_then(binary_version);
    let skew = matches!(&path_version, Some(v) if v != version);
    let fix = if on_path.is_none() {
        "run: make install (from the memd repo)"
    } else if skew {
        "run: make install (from the memd repo); then hash -r to clear the shell PATH cache"
    } else {
        ""
    };
    let mut value = json!({
        "ok": on_path.is_some() && !skew,
        "current_exe": exe.as_ref().map(|p| p.display().to_string()),
        "on_path": on_path.as_ref().map(|p| p.display().to_string()),
        "version": version,
        "path_version": path_version,
        "fix": fix,
    });
    if skew {
        value["reason"] = json!(format!(
            "memd on PATH is v{} but this process is v{version}",
            value["path_version"].as_str().unwrap_or("unknown")
        ));
    }
    value
}

/// Actual version of the binary at `path` via `<path> --version`
/// (clap prints `memd X.Y.Z`), so doctor reports the PATH binary's
/// real version instead of this process's compile-time version.
fn binary_version(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .last()
        .map(str::to_string)
}

/// Walk `$PATH` for an executable named `name`. Returns the first
/// match. Unix-only executable bit check; on other platforms any
/// regular file with the name counts.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match candidate.metadata() {
                Ok(meta) if meta.permissions().mode() & 0o111 != 0 => return Some(candidate),
                _ => continue,
            }
        }
        #[cfg(not(unix))]
        {
            return Some(candidate);
        }
    }
    None
}

fn check_data_dir(resolved: Option<&Path>) -> Value {
    let path = resolved
        .map(Path::to_path_buf)
        .or_else(|| dirs::home_dir().map(|h| h.join(".memd").join("data")));
    let exists = path.as_ref().map(|p| p.exists()).unwrap_or(false);
    let tenant_count = path.as_ref().map(|p| count_tenant_dirs(p)).unwrap_or(0);
    let fresh = !exists || tenant_count == 0;
    let writable = if exists {
        path.as_ref()
            .map(|path| probe_data_dir_writable(path))
            .unwrap_or(false)
    } else {
        true
    };
    let fix = if writable {
        String::new()
    } else {
        format!(
            "check write permissions on {}",
            path.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "?".to_string())
        )
    };
    let mut value = json!({
        "ok": writable,
        "path": path.as_ref().map(|p| p.display().to_string()),
        "tenant_count": tenant_count,
        "fresh": fresh,
        "fix": fix,
    });
    if fresh {
        value["note"] = json!("empty — fresh install");
    }
    value
}

/// Ping the warm worker for the resolved data dir and flag
/// worker-vs-CLI version skew: a stale daemon serves old code for
/// every project on the machine until it is restarted.
async fn check_warm_worker(data_dir: Option<&Path>) -> Value {
    let Some(data_dir) = data_dir else {
        return json!({"ok": true, "note": "no data dir resolved", "fix": ""});
    };
    match super::warm::warm_ping_identity(data_dir).await {
        Err(_) => json!({
            "ok": true,
            "status": "not_running",
            "note": "no warm worker running (started on demand by --warm auto)",
            "fix": "",
        }),
        Ok(identity) => {
            let worker_version = identity
                .get("memd_version")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let cli_version = env!("CARGO_PKG_VERSION");
            let skew = worker_version != cli_version;
            let mut value = json!({
                "ok": !skew,
                "status": "running",
                "pid": identity.get("pid").cloned().unwrap_or(Value::Null),
                "worker_version": worker_version,
                "cli_version": cli_version,
                "fix": if skew {
                    format!(
                        "run: memd --data-dir {} warm stop (a fresh worker starts on the next warm command)",
                        data_dir.display()
                    )
                } else {
                    String::new()
                },
            });
            if skew {
                value["reason"] = json!(format!(
                    "warm worker is v{} but CLI is v{cli_version}",
                    value["worker_version"].as_str().unwrap_or("unknown")
                ));
            }
            value
        }
    }
}

fn probe_data_dir_writable(path: &Path) -> bool {
    let probe = path.join(format!(".memd-doctor-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn count_tenant_dirs(data_dir: &Path) -> usize {
    let tenants = data_dir.join("tenants");
    let Ok(entries) = std::fs::read_dir(&tenants) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .count()
}

fn check_rules_file(path: Option<&Path>) -> Value {
    let Some(path) = path else {
        return json!({
            "ok": false,
            "reason": "no home dir",
            "fix": "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)",
        });
    };
    if !path.exists() {
        return json!({
            "ok": false,
            "path": path.display().to_string(),
            "reason": "file missing",
            "fix": "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)",
        });
    }
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    let wired = contents.contains(ENFORCEMENT_MARKER);
    json!({
        "ok": wired,
        "path": path.display().to_string(),
        "reason": if wired { Value::Null } else { json!("memd-enforcement block missing") },
        "fix": if wired { "" } else { "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)" },
    })
}

fn check_cursor_rules() -> Value {
    let Some(home) = dirs::home_dir() else {
        return json!({
            "ok": false,
            "reason": "no home dir",
            "fix": "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)",
        });
    };
    let dir = home.join(".cursor").join("rules");
    let path = dir.join("memd.mdc");
    if path.exists() {
        return json!({
            "ok": true,
            "path": path.display().to_string(),
            "fix": "",
        });
    }
    // Fall back: any .mdc rule that references memd counts as
    // "wired" so users with custom Cursor setups aren't flagged.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("mdc") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                if text.to_lowercase().contains("memd") {
                    return json!({
                        "ok": true,
                        "path": p.display().to_string(),
                        "note": "matched a non-default .mdc referencing memd",
                        "fix": "",
                    });
                }
            }
        }
    }
    json!({
        "ok": false,
        "path": path.display().to_string(),
        "reason": "no memd Cursor rule found",
        "fix": "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)",
    })
}

fn check_session_hook() -> Value {
    let Some(home) = dirs::home_dir() else {
        return json!({
            "ok": false,
            "reason": "no home dir",
            "fix": "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)",
        });
    };
    let path = home.join(".claude").join("settings.json");
    if !path.exists() {
        return json!({
            "ok": false,
            "path": path.display().to_string(),
            "reason": "settings.json missing",
            "fix": "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)",
        });
    }
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    // Parse the JSON and walk to `hooks.SessionStart[].hooks[].command`
    // before substring-matching — that's the only place a *real*
    // SessionStart hook lives. Substring-matching the whole file
    // would false-positive on stale comments or unrelated keys.
    let wired = match serde_json::from_str::<Value>(&contents) {
        Ok(parsed) => session_start_hook_wired(&parsed),
        Err(_) => {
            return json!({
                "ok": false,
                "path": path.display().to_string(),
                "reason": "settings.json is not valid JSON",
                "fix": "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)",
            });
        }
    };
    json!({
        "ok": wired,
        "path": path.display().to_string(),
        "reason": if wired { Value::Null } else { json!("SessionStart hook for memd missing") },
        "fix": if wired { "" } else { "run: make install-enforcement (or bash memd-skill/install_memd_enforcement.sh)" },
    })
}

/// Returns true if `settings` contains a `hooks.SessionStart[].hooks[].command`
/// entry whose command string mentions `memd session-start`.
fn session_start_hook_wired(settings: &Value) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(|s| s.as_array())
        .map(|groups| {
            groups.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(|inner| inner.as_array())
                    .map(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .map(|s| s.contains(SESSION_HOOK_MARKER))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn check_project_scope(project_dir: &Path) -> Value {
    let scope_path = project_dir.join(".memd").join("project_scope.json");
    if !scope_path.exists() {
        return json!({
            "ok": true,
            "path": scope_path.display().to_string(),
            "note": "none yet (created by memd session-start)",
            "fix": "",
        });
    }
    let text = std::fs::read_to_string(&scope_path).unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    let Some(parsed) = parsed else {
        return json!({
            "ok": false,
            "path": scope_path.display().to_string(),
            "reason": "malformed JSON",
            "fix": "run: memd session-start --project-dir . (rewrites .memd/project_scope.json)",
        });
    };
    json!({
        "ok": true,
        "path": scope_path.display().to_string(),
        "tenant_id": parsed.get("tenant_id"),
        "project_id": parsed.get("project_id"),
        "fix": "",
    })
}

async fn enrich_project_scope_memory<S: Store>(store: &S, project_scope: &mut Value) {
    let Some(scope) = project_scope.as_object_mut() else {
        return;
    };
    if !scope
        .get("ok")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return;
    }

    let tenant_id = scope
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let project_id = scope.get("project_id").and_then(|value| value.as_str());
    let Some(project_id) = project_id.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    let Ok(tenant) = TenantId::new(tenant_id) else {
        scope.insert(
            "memory".to_string(),
            json!({"ok": false, "reason": "invalid tenant_id in project scope"}),
        );
        return;
    };

    let counts = match project_counts_for_tenant(store, &tenant).await {
        Ok(counts) => counts,
        Err(err) => {
            scope.insert(
                "memory".to_string(),
                json!({"ok": false, "reason": format!("could not inspect project memory: {err}")}),
            );
            return;
        }
    };
    let configured_chunks = counts
        .iter()
        .find_map(|(candidate, count)| (candidate.as_deref() == Some(project_id)).then_some(*count))
        .unwrap_or(0);
    let normalized = normalize_project_id_for_drift(project_id);
    let similar = counts
        .into_iter()
        .filter_map(|(candidate, count)| {
            let candidate = candidate?;
            if candidate == project_id {
                return None;
            }
            if normalize_project_id_for_drift(&candidate) != normalized {
                return None;
            }
            Some(json!({
                "project_id": candidate,
                "active_chunks": count,
                "normalized_id": normalized,
            }))
        })
        .collect::<Vec<_>>();
    let dominant_similar_chunks = similar
        .iter()
        .filter_map(|entry| entry.get("active_chunks").and_then(|value| value.as_u64()))
        .max()
        .unwrap_or(0) as usize;

    let mut memory = json!({
        "ok": true,
        "configured_project_active_chunks": configured_chunks,
        "similar_project_ids": similar,
    });
    let has_similar_project_ids = !memory["similar_project_ids"]
        .as_array()
        .map(Vec::is_empty)
        .unwrap_or(true);
    let similar_dominates_configured = dominant_similar_chunks >= PROJECT_DRIFT_MIN_DOMINANT_CHUNKS
        && dominant_similar_chunks
            > configured_chunks.saturating_mul(PROJECT_DRIFT_DOMINANCE_RATIO);
    if has_similar_project_ids
        && (configured_chunks <= PROJECT_DRIFT_LOW_CHUNK_THRESHOLD || similar_dominates_configured)
    {
        let variants = memory["similar_project_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.get("project_id").and_then(|value| value.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        memory["ok"] = json!(false);
        memory["warning"] = json!(format!(
            "configured project_id `{project_id}` has {configured_chunks} active chunks, but similar project_id(s) exist: {variants}"
        ));
        scope.insert("ok".to_string(), json!(false));
        scope.insert(
            "reason".to_string(),
            json!("configured project_id has little/no memory but similar project IDs exist"),
        );
    }
    scope.insert("memory".to_string(), memory);
}

async fn project_counts_for_tenant<S: Store>(
    store: &S,
    tenant: &TenantId,
) -> crate::error::Result<Vec<(Option<String>, usize)>> {
    if let Some(persistent) = store.as_persistent() {
        return persistent.metadata().project_counts(tenant, 1_000);
    }
    let scanned = scan_project_counts_for_tenant(store, tenant).await?;
    if !scanned.is_empty() {
        return Ok(scanned);
    }
    project_counts_from_default_metadata(tenant, 1_000)
}

async fn scan_project_counts_for_tenant<S: Store>(
    store: &S,
    tenant: &TenantId,
) -> crate::error::Result<Vec<(Option<String>, usize)>> {
    let mut counts = std::collections::BTreeMap::<Option<String>, usize>::new();
    let mut offset = 0usize;
    while offset < PROJECT_DRIFT_SCAN_LIMIT {
        let limit = PROJECT_DRIFT_PAGE_SIZE.min(PROJECT_DRIFT_SCAN_LIMIT - offset);
        let chunks = store.list_chunks(tenant, limit, offset).await?;
        if chunks.is_empty() {
            break;
        }
        offset = offset.saturating_add(limit);
        for chunk in chunks {
            *counts
                .entry(chunk.project_id.as_option().map(str::to_string))
                .or_insert(0) += 1;
        }
    }
    Ok(counts.into_iter().collect())
}

fn project_counts_from_default_metadata(
    tenant: &TenantId,
    limit: usize,
) -> crate::error::Result<Vec<(Option<String>, usize)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let Some(path) =
        dirs::home_dir().map(|home| home.join(".memd").join("data").join("metadata.db"))
    else {
        return Ok(Vec::new());
    };
    if !path.exists() {
        return Ok(Vec::new());
    }

    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT project_id, COUNT(*) AS active_chunks
         FROM chunks
         WHERE tenant_id = ?1 AND status NOT IN ('candidate', 'deleted')
         GROUP BY project_id
         ORDER BY active_chunks DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![tenant.as_str(), limit as i64], |row| {
        let project_id: Option<String> = row.get(0)?;
        let count: i64 = row.get(1)?;
        Ok((project_id, count.max(0) as usize))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

fn normalize_project_id_for_drift(project_id: &str) -> String {
    project_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn home_path(suffix: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(suffix))
}

pub(super) fn failing_checks(report: &Value) -> Vec<String> {
    [
        ("binary on PATH", &report["binary"]),
        ("data dir", &report["data_dir"]),
        ("warm worker", &report["warm_worker"]),
        ("claude rules", &report["global_rules"]["claude_md"]),
        ("codex rules", &report["global_rules"]["codex_agents_md"]),
        ("cursor rules", &report["global_rules"]["cursor_rules_mdc"]),
        ("SessionStart hook", &report["session_start_hook"]),
        ("project scope", &report["project_scope"]),
    ]
    .into_iter()
    .filter(|(_, value)| !value.get("ok").and_then(|ok| ok.as_bool()).unwrap_or(false))
    .map(|(name, _)| name.to_string())
    .collect()
}

/// Render a human-readable report. One line per check; `[ok]` /
/// `[--]` prefixes are stable so users can grep them.
fn render_text(report: &Value) -> String {
    let mut out = String::new();
    out.push_str("memd doctor\n");
    out.push_str("===========\n");

    push_line(&mut out, "binary on PATH", &report["binary"], |v| {
        let on_path = v
            .get("on_path")
            .and_then(|p| p.as_str())
            .unwrap_or("not found");
        let current = v.get("current_exe").and_then(|p| p.as_str()).unwrap_or("?");
        let version = v.get("version").and_then(|p| p.as_str()).unwrap_or("?");
        match v.get("path_version").and_then(|p| p.as_str()) {
            Some(path_version) if path_version != version => format!(
                "{on_path} (v{path_version} on PATH; this process v{version}; current_exe={current})"
            ),
            _ => format!("{on_path} (v{version}; current_exe={current})"),
        }
    });
    push_line(&mut out, "data dir", &report["data_dir"], |v| {
        let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("?");
        if v.get("fresh").and_then(|fresh| fresh.as_bool()) == Some(true) {
            format!("{path} (empty — fresh install)")
        } else {
            format!(
                "{} ({} tenants)",
                path,
                v.get("tenant_count").and_then(|p| p.as_u64()).unwrap_or(0),
            )
        }
    });
    push_line(&mut out, "warm worker", &report["warm_worker"], |v| match v
        .get("status")
        .and_then(|p| p.as_str())
    {
        Some("running") => format!(
            "running (worker v{}, cli v{})",
            v.get("worker_version")
                .and_then(|p| p.as_str())
                .unwrap_or("?"),
            v.get("cli_version").and_then(|p| p.as_str()).unwrap_or("?"),
        ),
        _ => v
            .get("note")
            .and_then(|p| p.as_str())
            .unwrap_or("not running")
            .to_string(),
    });
    let rules = &report["global_rules"];
    push_line(
        &mut out,
        "Claude CLAUDE.md",
        &rules["claude_md"],
        path_or_reason,
    );
    push_line(
        &mut out,
        "Codex AGENTS.md",
        &rules["codex_agents_md"],
        path_or_reason,
    );
    push_line(
        &mut out,
        "Cursor rule",
        &rules["cursor_rules_mdc"],
        path_or_reason,
    );
    push_line(
        &mut out,
        "Claude SessionStart hook",
        &report["session_start_hook"],
        path_or_reason,
    );
    push_line(&mut out, "project scope", &report["project_scope"], |v| {
        if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
            if let Some(note) = v.get("note").and_then(|note| note.as_str()) {
                return note.to_string();
            }
            let mut detail = format!(
                "tenant={} project={}",
                v.get("tenant_id").and_then(|p| p.as_str()).unwrap_or("?"),
                v.get("project_id").and_then(|p| p.as_str()).unwrap_or("?"),
            );
            if let Some(count) = v
                .get("memory")
                .and_then(|m| m.get("configured_project_active_chunks"))
                .and_then(|count| count.as_u64())
            {
                detail.push_str(&format!(" chunks={count}"));
            }
            detail
        } else if let Some(memory_warning) = v
            .get("memory")
            .and_then(|m| m.get("warning"))
            .and_then(|warning| warning.as_str())
        {
            memory_warning.to_string()
        } else {
            path_or_reason(v)
        }
    });

    let failing = failing_checks(report);
    if failing.len() > 1 {
        out.push_str("fix everything: make install (from the memd repo)\n");
    }
    if !failing.is_empty() {
        out.push_str(&format!(
            "failing: {} ({})\n",
            failing.len(),
            failing.join(", ")
        ));
    }

    out
}

fn path_or_reason(v: &Value) -> String {
    if let Some(reason) = v.get("reason").and_then(|r| r.as_str()) {
        return reason.to_string();
    }
    v.get("path")
        .and_then(|p| p.as_str())
        .unwrap_or("?")
        .to_string()
}

fn push_line<F: Fn(&Value) -> String>(out: &mut String, label: &str, v: &Value, detail: F) {
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    let tag = if ok { "[ok]" } else { "[--]" };
    let mut line = format!("{tag} {label}: {}", detail(v));
    if !ok {
        if let Some(fix) = v.get("fix").and_then(|fix| fix.as_str()) {
            line.push_str(&format!(" -> fix: {fix}"));
        }
    }
    out.push_str(&format!("{line}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkType, MemoryChunk, MemoryStore, ProjectId, Store, TenantId};
    use tempfile::tempdir;

    #[tokio::test]
    async fn report_includes_all_sections() {
        let dir = tempdir().unwrap();
        let report = collect_report(dir.path(), None).await;
        for key in [
            "binary",
            "data_dir",
            "warm_worker",
            "global_rules",
            "session_start_hook",
            "project_scope",
        ] {
            assert!(
                report.get(key).is_some(),
                "report missing key: {key} (got {report})"
            );
        }
        for key in ["claude_md", "codex_agents_md", "cursor_rules_mdc"] {
            assert!(
                report["global_rules"].get(key).is_some(),
                "global_rules missing key: {key}"
            );
        }
    }

    #[test]
    fn failing_checks_empty_for_all_ok_report() {
        let report = json!({
            "binary": {"ok": true},
            "data_dir": {"ok": true},
            "warm_worker": {"ok": true},
            "global_rules": {
                "claude_md": {"ok": true},
                "codex_agents_md": {"ok": true},
                "cursor_rules_mdc": {"ok": true},
            },
            "session_start_hook": {"ok": true},
            "project_scope": {"ok": true},
        });
        assert!(failing_checks(&report).is_empty());
    }

    #[test]
    fn failing_checks_returns_failed_names_in_order() {
        let report = json!({
            "binary": {"ok": true},
            "data_dir": {"ok": false},
            "warm_worker": {"ok": true},
            "global_rules": {
                "claude_md": {"ok": false},
                "codex_agents_md": {"ok": true},
                "cursor_rules_mdc": {"ok": true},
            },
            "session_start_hook": {"ok": true},
            "project_scope": {"ok": true},
        });
        assert_eq!(
            failing_checks(&report),
            vec!["data dir".to_string(), "claude rules".to_string()]
        );
    }

    #[test]
    fn project_scope_reports_ok_for_valid_file() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{"tenant_id":"alice","project_id":"demo"}"#,
        )
        .unwrap();
        let v = check_project_scope(dir.path());
        assert_eq!(v["ok"], true);
        assert_eq!(v["tenant_id"], "alice");
        assert_eq!(v["project_id"], "demo");
    }

    #[test]
    fn project_scope_reports_missing_as_fresh_with_helpful_note() {
        let dir = tempdir().unwrap();
        let v = check_project_scope(dir.path());
        assert_eq!(v["ok"], true);
        let note = v["note"].as_str().unwrap_or("");
        assert!(note.contains("session-start"), "note was: {note}");
    }

    #[test]
    fn project_scope_reports_malformed_json() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(dir.path().join(".memd/project_scope.json"), "{not json").unwrap();
        let v = check_project_scope(dir.path());
        assert_eq!(v["ok"], false);
        assert_eq!(v["reason"], "malformed JSON");
        assert_eq!(
            v["fix"],
            "run: memd session-start --project-dir . (rewrites .memd/project_scope.json)"
        );
    }

    #[test]
    fn project_id_drift_normalization_ignores_separators() {
        assert_eq!(
            normalize_project_id_for_drift("bester_hosting"),
            normalize_project_id_for_drift("bester-hosting")
        );
        assert_ne!(
            normalize_project_id_for_drift("bester_hosting"),
            normalize_project_id_for_drift("bester-hosting-old")
        );
    }

    #[tokio::test]
    async fn project_scope_warns_on_similar_project_id_with_no_memory() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{"tenant_id":"fschulz","project_id":"bester_hosting"}"#,
        )
        .unwrap();
        let store = MemoryStore::new();
        let tenant = TenantId::new("fschulz").unwrap();
        store
            .add(
                MemoryChunk::new(
                    tenant,
                    "Useful gateway restore lesson under the hyphenated project id.",
                    ChunkType::Summary,
                )
                .with_project(ProjectId::from("bester-hosting")),
            )
            .await
            .unwrap();

        let mut v = check_project_scope(dir.path());
        enrich_project_scope_memory(&store, &mut v).await;

        assert_eq!(v["ok"], false);
        assert_eq!(
            v["reason"],
            "configured project_id has little/no memory but similar project IDs exist"
        );
        assert_eq!(v["memory"]["configured_project_active_chunks"], 0);
        assert_eq!(
            v["memory"]["similar_project_ids"][0]["project_id"],
            "bester-hosting"
        );
        assert!(v["memory"]["warning"]
            .as_str()
            .unwrap_or_default()
            .contains("bester_hosting"));
    }

    #[tokio::test]
    async fn project_scope_warns_when_similar_project_id_dominates_existing_memory() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{"tenant_id":"fschulz","project_id":"bester_hosting"}"#,
        )
        .unwrap();
        let store = MemoryStore::new();
        let tenant = TenantId::new("fschulz").unwrap();
        for index in 0..8 {
            store
                .add(
                    MemoryChunk::new(
                        tenant.clone(),
                        format!("Low-volume underscore project record {index}."),
                        ChunkType::Summary,
                    )
                    .with_project(ProjectId::from("bester_hosting")),
                )
                .await
                .unwrap();
        }
        for index in 0..50 {
            store
                .add(
                    MemoryChunk::new(
                        tenant.clone(),
                        format!("Dominant hyphenated project record {index}."),
                        ChunkType::Summary,
                    )
                    .with_project(ProjectId::from("bester-hosting")),
                )
                .await
                .unwrap();
        }

        let mut v = check_project_scope(dir.path());
        enrich_project_scope_memory(&store, &mut v).await;

        assert_eq!(v["ok"], false);
        assert_eq!(v["memory"]["configured_project_active_chunks"], 8);
        assert_eq!(
            v["memory"]["similar_project_ids"][0]["project_id"],
            "bester-hosting"
        );
    }

    #[tokio::test]
    async fn project_scope_does_not_warn_for_unrelated_project_names() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{"tenant_id":"fschulz","project_id":"bester_hosting"}"#,
        )
        .unwrap();
        let store = MemoryStore::new();
        let tenant = TenantId::new("fschulz").unwrap();
        store
            .add(
                MemoryChunk::new(tenant, "Unrelated memory.", ChunkType::Summary)
                    .with_project(ProjectId::from("bester-hosting-old")),
            )
            .await
            .unwrap();

        let mut v = check_project_scope(dir.path());
        enrich_project_scope_memory(&store, &mut v).await;

        assert_eq!(v["ok"], true);
        assert_eq!(v["memory"]["configured_project_active_chunks"], 0);
        assert_eq!(
            v["memory"]["similar_project_ids"].as_array().unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn render_text_emits_stable_ok_dash_prefixes() {
        let dir = tempdir().unwrap();
        let report = collect_report(dir.path(), None).await;
        let text = render_text(&report);
        // Each check produces a single line; `[ok]` or `[--]` is the
        // grep contract for human consumers and screen-scrapers.
        let lines = text
            .lines()
            .filter(|l| l.starts_with("[ok]") || l.starts_with("[--]"))
            .count();
        assert_eq!(lines, 8, "expected 8 status lines, got:\n{text}");
    }

    #[test]
    fn render_text_adds_fixes_and_multi_failure_footer() {
        let report = json!({
            "binary": {"ok": false, "on_path": "missing", "current_exe": "?", "version": "test", "fix": "run: make install (from the memd repo)"},
            "data_dir": {"ok": false, "path": "/tmp/memd-data", "tenant_count": 0, "fresh": false, "fix": "check write permissions on /tmp/memd-data"},
            "warm_worker": {"ok": true, "status": "not_running", "note": "no warm worker running (started on demand by --warm auto)"},
            "global_rules": {
                "claude_md": {"ok": true},
                "codex_agents_md": {"ok": true},
                "cursor_rules_mdc": {"ok": true},
            },
            "session_start_hook": {"ok": true},
            "project_scope": {"ok": true, "note": "none yet (created by memd session-start)"},
        });
        let text = render_text(&report);
        assert!(text.contains(
            "[--] binary on PATH: missing (vtest; current_exe=?) -> fix: run: make install"
        ));
        assert!(text.contains("[--] data dir: /tmp/memd-data (0 tenants) -> fix: check write permissions on /tmp/memd-data"));
        assert!(text.contains("fix everything: make install (from the memd repo)"));
    }

    #[test]
    fn session_start_hook_wired_detects_real_hook_shape() {
        let wired = json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "memd session-start --project-dir ."}]}
                ]
            }
        });
        assert!(session_start_hook_wired(&wired));

        // String appears outside the SessionStart hook chain → not wired.
        let stale = json!({
            "comment": "old config used to have memd session-start",
            "hooks": {"PreToolUse": []}
        });
        assert!(!session_start_hook_wired(&stale));

        // SessionStart array present but no memd command in any inner hook.
        let unrelated = json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "echo hello"}]}
                ]
            }
        });
        assert!(!session_start_hook_wired(&unrelated));

        // Empty settings shouldn't false-positive.
        assert!(!session_start_hook_wired(&json!({})));
    }

    #[test]
    fn check_binary_reports_path_discoverability_and_current_exe() {
        let v = check_binary();
        // Always present (Option may be null but key exists).
        assert!(v.get("current_exe").is_some());
        assert!(v.get("on_path").is_some());
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        // `ok` reflects on_path AND no version skew against this
        // process; environment-independent expectation.
        let skew = matches!(
            v["path_version"].as_str(),
            Some(pv) if pv != env!("CARGO_PKG_VERSION")
        );
        let expected_ok = v["on_path"].as_str().is_some() && !skew;
        assert_eq!(v["ok"].as_bool().unwrap_or(false), expected_ok);
    }
}
