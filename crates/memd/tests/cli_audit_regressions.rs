//! CLI regressions from audit work package 3. All subprocesses use private homes.
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn cli(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memd"));
    command
        .env("HOME", home)
        .env("MEMD_AUTO_SCOPE", "1")
        .arg("--in-memory");
    command
}

#[test]
fn audit_metrics_operation_is_removed() {
    let home = tempfile::tempdir().unwrap();
    let output = cli(home.path())
        .args(["call", "memory.metrics", "--json", "{}", "--warm", "off"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "removed operation still succeeds");
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown"));
}

#[test]
fn audit_health_has_real_state_without_latency() {
    let home = tempfile::tempdir().unwrap();
    let output = cli(home.path())
        .args([
            "call",
            "memory.health",
            "--json",
            r#"{"tenant_id":"audit"}"#,
            "--warm",
            "off",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(payload.get("latency").is_none(), "{payload}");
    for key in [
        "scope",
        "counts",
        "duplicates",
        "index_coverage",
        "payload",
        "warnings",
    ] {
        assert!(payload.get(key).is_some(), "missing {key}");
    }
}

#[test]
fn audit_home_session_does_not_create_scope() {
    let home = tempfile::tempdir().unwrap();
    let output = cli(home.path())
        .args(["session-start", "--project-dir"])
        .arg(home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.path().join(".memd/project_scope.json").exists());
    let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["skipped"], "unsafe_auto_scope_root");
}

#[cfg(unix)]
#[test]
fn audit_root_and_home_alias_sessions_do_not_create_scope() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let aliases = tempfile::tempdir().unwrap();
    let alias = aliases.path().join("home-alias");
    symlink(home.path(), &alias).unwrap();
    for directory in [Path::new("/"), alias.as_path()] {
        let output = cli(home.path())
            .args(["session-start", "--project-dir"])
            .arg(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(payload["skipped"], "unsafe_auto_scope_root");
    }
    assert!(!home.path().join(".memd/project_scope.json").exists());
}

#[test]
fn audit_claude_code_session_provenance() {
    if std::env::var_os("MEMD_AUDIT_PROVENANCE_CHILD").is_some() {
        let context = memd::cli::capture_execution_context(Path::new("/tmp"));
        assert_eq!(context.harness.as_deref(), Some("claude"));
        assert_eq!(
            context.native_session_id.as_deref(),
            Some("claude-native-session")
        );
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "audit_claude_code_session_provenance",
            "--nocapture",
        ])
        .env("HOME", home.path())
        .env("MEMD_AUDIT_PROVENANCE_CHILD", "1")
        .env("CLAUDE_CODE_SESSION_ID", "claude-native-session")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("AIO_HARNESS")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn audit_consolidation_model_config_is_retained() {
    let config = memd::config::load_from_str("consolidation_model = 'custom-sonnet'\n").unwrap();
    let serialized = serde_json::to_value(config).unwrap();
    assert_eq!(serialized["consolidation_model"], "custom-sonnet");
}

#[test]
fn audit_init_uses_installer_memory_policy() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let output = cli(home.path())
        .args([
            "init",
            "--tenant-id",
            "audit",
            "--write-agent-files",
            "true",
            "--project-dir",
        ])
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let policy = include_str!("../../../memd-skill/memory-policy.md").trim();
    for name in ["AGENTS.md", "CLAUDE.md", ".memd/memory_guardrails.md"] {
        let text = std::fs::read_to_string(project.join(name)).unwrap();
        assert!(
            text.contains(policy),
            "{name} differs from installer policy"
        );
        assert!(!text.contains("Hard rule:"));
    }
}
