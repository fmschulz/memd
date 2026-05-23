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
//! Always exits 0 — `doctor` is informational. Use `--format json`
//! for machine-readable output suitable for `--quiet` checks.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::args::ExportFormat;
use crate::error::Result;

/// Marker the installer drops into agent rule files. Identical
/// substring is used by both `~/.claude/CLAUDE.md` and
/// `~/.codex/AGENTS.md`, so a single grep covers both surfaces.
const ENFORCEMENT_MARKER: &str = "memd-enforcement:start";

/// Marker used inside the wired `SessionStart` hook command.
const SESSION_HOOK_MARKER: &str = "memd session-start";

#[derive(Debug, Clone)]
pub(super) struct DoctorOptions {
    pub(super) project_dir: PathBuf,
    pub(super) format: ExportFormat,
}

/// Run the doctor command. Pure-ish: filesystem reads only, no
/// network, no store. Returns the structured report as JSON; the
/// CLI dispatcher renders it according to `options.format`.
pub(super) fn run_doctor(options: DoctorOptions) -> Result<Value> {
    let report = collect_report(&options.project_dir);
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

fn collect_report(project_dir: &Path) -> Value {
    let binary = check_binary();
    let data_dir = check_data_dir();
    let claude_rules = check_rules_file(home_path(".claude/CLAUDE.md").as_deref());
    let codex_rules = check_rules_file(home_path(".codex/AGENTS.md").as_deref());
    let cursor_rules = check_cursor_rules();
    let session_hook = check_session_hook();
    let project_scope = check_project_scope(project_dir);

    json!({
        "binary": binary,
        "data_dir": data_dir,
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
    // "ok" means `memd` is discoverable via PATH — that's what
    // matters for SessionStart hooks and skill invocations. Knowing
    // the current process's exe is informational only.
    json!({
        "ok": on_path.is_some(),
        "current_exe": exe.as_ref().map(|p| p.display().to_string()),
        "on_path": on_path.as_ref().map(|p| p.display().to_string()),
        "version": version,
    })
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

fn check_data_dir() -> Value {
    let path = dirs::home_dir().map(|h| h.join(".memd").join("data"));
    let exists = path.as_ref().map(|p| p.exists()).unwrap_or(false);
    let tenant_count = path.as_ref().map(|p| count_tenant_dirs(p)).unwrap_or(0);
    json!({
        "ok": exists,
        "path": path.as_ref().map(|p| p.display().to_string()),
        "tenant_count": tenant_count,
    })
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
        return json!({"ok": false, "reason": "no home dir"});
    };
    if !path.exists() {
        return json!({
            "ok": false,
            "path": path.display().to_string(),
            "reason": "file missing",
        });
    }
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    let wired = contents.contains(ENFORCEMENT_MARKER);
    json!({
        "ok": wired,
        "path": path.display().to_string(),
        "reason": if wired { Value::Null } else { json!("memd-enforcement block missing") },
    })
}

fn check_cursor_rules() -> Value {
    let Some(home) = dirs::home_dir() else {
        return json!({"ok": false, "reason": "no home dir"});
    };
    let dir = home.join(".cursor").join("rules");
    let path = dir.join("memd.mdc");
    if path.exists() {
        return json!({
            "ok": true,
            "path": path.display().to_string(),
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
                    });
                }
            }
        }
    }
    json!({
        "ok": false,
        "path": path.display().to_string(),
        "reason": "no memd Cursor rule found",
    })
}

fn check_session_hook() -> Value {
    let Some(home) = dirs::home_dir() else {
        return json!({"ok": false, "reason": "no home dir"});
    };
    let path = home.join(".claude").join("settings.json");
    if !path.exists() {
        return json!({
            "ok": false,
            "path": path.display().to_string(),
            "reason": "settings.json missing",
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
            });
        }
    };
    json!({
        "ok": wired,
        "path": path.display().to_string(),
        "reason": if wired { Value::Null } else { json!("SessionStart hook for memd missing") },
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
            "ok": false,
            "path": scope_path.display().to_string(),
            "reason": "no project scope (will be auto-created on next session-start)",
        });
    }
    let text = std::fs::read_to_string(&scope_path).unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    let Some(parsed) = parsed else {
        return json!({
            "ok": false,
            "path": scope_path.display().to_string(),
            "reason": "malformed JSON",
        });
    };
    json!({
        "ok": true,
        "path": scope_path.display().to_string(),
        "tenant_id": parsed.get("tenant_id"),
        "project_id": parsed.get("project_id"),
    })
}

fn home_path(suffix: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(suffix))
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
        format!("{on_path} (v{version}; current_exe={current})")
    });
    push_line(&mut out, "data dir", &report["data_dir"], |v| {
        format!(
            "{} ({} tenants)",
            v.get("path").and_then(|p| p.as_str()).unwrap_or("?"),
            v.get("tenant_count").and_then(|p| p.as_u64()).unwrap_or(0),
        )
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
            format!(
                "tenant={} project={}",
                v.get("tenant_id").and_then(|p| p.as_str()).unwrap_or("?"),
                v.get("project_id").and_then(|p| p.as_str()).unwrap_or("?"),
            )
        } else {
            path_or_reason(v)
        }
    });

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
    out.push_str(&format!("{tag} {label}: {}\n", detail(v)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn report_includes_all_sections() {
        let dir = tempdir().unwrap();
        let report = collect_report(dir.path());
        for key in [
            "binary",
            "data_dir",
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
    fn project_scope_reports_missing_with_helpful_reason() {
        let dir = tempdir().unwrap();
        let v = check_project_scope(dir.path());
        assert_eq!(v["ok"], false);
        let reason = v["reason"].as_str().unwrap_or("");
        assert!(reason.contains("auto-created"), "reason was: {reason}");
    }

    #[test]
    fn project_scope_reports_malformed_json() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(dir.path().join(".memd/project_scope.json"), "{not json").unwrap();
        let v = check_project_scope(dir.path());
        assert_eq!(v["ok"], false);
        assert_eq!(v["reason"], "malformed JSON");
    }

    #[test]
    fn render_text_emits_stable_ok_dash_prefixes() {
        let dir = tempdir().unwrap();
        let report = collect_report(dir.path());
        let text = render_text(&report);
        // Each check produces a single line; `[ok]` or `[--]` is the
        // grep contract for human consumers and screen-scrapers.
        let lines = text
            .lines()
            .filter(|l| l.starts_with("[ok]") || l.starts_with("[--]"))
            .count();
        assert_eq!(lines, 7, "expected 7 status lines, got:\n{text}");
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
        // `ok` reflects on_path, not current_exe.
        let expected_ok = v["on_path"].as_str().is_some();
        assert_eq!(v["ok"].as_bool().unwrap_or(false), expected_ok);
    }
}
