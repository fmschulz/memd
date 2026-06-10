use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tempfile::tempdir;

fn memd_bin() -> String {
    env!("CARGO_BIN_EXE_memd").to_string()
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos()
}

fn command(home: &Path, data_dir: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .env("HOME", home)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "10000")
        .arg("--data-dir")
        .arg(data_dir)
        .current_dir(cwd);
    command
}

fn assert_success(output: Output, label: &str) -> Output {
    assert!(
        output.status.success(),
        "{label} failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

fn assert_failure(output: Output, label: &str) -> Output {
    assert!(
        !output.status.success(),
        "{label} unexpectedly succeeded: stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

fn run_session_start(home: &Path, data_dir: &Path, project_dir: &Path, tenant: &str) {
    let output = command(home, data_dir, project_dir)
        .env("MEMD_DEFAULT_TENANT", tenant)
        .args(["session-start", "--project-dir", "."])
        .output()
        .expect("run memd session-start");
    assert_success(output, "session-start");
    assert!(project_dir.join(".memd/project_scope.json").is_file());
}

fn add_text(home: &Path, data_dir: &Path, cwd: &Path, text: &str, extra: &[&str]) -> Output {
    let mut command = command(home, data_dir, cwd);
    command.args([
        "add",
        "--warm",
        "off",
        "--chunk-type",
        "summary",
        "--tags",
        "kind:evidence",
    ]);
    command.args(extra);
    command.arg("--text").arg(text);
    command.output().expect("run memd add")
}

fn search(home: &Path, data_dir: &Path, cwd: &Path, query: &str) -> Output {
    command(home, data_dir, cwd)
        .args([
            "search", "--warm", "off", "--query", query, "--k", "5", "--format", "json",
        ])
        .output()
        .expect("run memd search")
}

fn report(home: &Path, data_dir: &Path, cwd: &Path) -> Output {
    command(home, data_dir, cwd)
        .args([
            "report", "--warm", "off", "--format", "json", "--since", "24h",
        ])
        .output()
        .expect("run memd report")
}

fn stats_total(home: &Path, data_dir: &Path, cwd: &Path, tenant_id: &str) -> u64 {
    let output = assert_success(
        command(home, data_dir, cwd)
            .args(["stats", "--tenant-id", tenant_id])
            .output()
            .expect("run memd stats"),
        "stats",
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("stats JSON");
    parsed["total_chunks"].as_u64().unwrap_or(0)
}

fn write_scope(project_dir: &Path, tenant_id: &str, project_id: &str) {
    let memd_dir = project_dir.join(".memd");
    std::fs::create_dir_all(&memd_dir).unwrap();
    let scope = json!({
        "tenant_id": tenant_id,
        "project_id": project_id,
        "read_tenants": [tenant_id],
        "interface": "cli",
        "cli_command": "memd",
        "agent_context_output": ".memd/context.md",
        "project_dir": project_dir.display().to_string(),
    });
    std::fs::write(
        memd_dir.join("project_scope.json"),
        format!("{}\n", serde_json::to_string_pretty(&scope).unwrap()),
    )
    .unwrap();
}

#[test]
fn scoped_project_commands_fall_back_to_project_scope() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let data_dir = tmp.path().join("data");
    let project_dir = tmp.path().join("scoped_project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();

    run_session_start(&home, &data_dir, &project_dir, "scope_tenant");

    let token = format!("scope_fallback_root_{}", nonce());
    let text = format!("Validated scope fallback root retrieval token {token}.");
    assert_success(add_text(&home, &data_dir, &project_dir, &text, &[]), "add");

    let search_output = assert_success(search(&home, &data_dir, &project_dir, &token), "search");
    let search_stdout = String::from_utf8_lossy(&search_output.stdout);
    assert!(search_stdout.contains(&token), "{search_stdout}");

    assert_success(report(&home, &data_dir, &project_dir), "report");
}

#[test]
fn nested_cwd_walks_up_to_project_scope() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let data_dir = tmp.path().join("data");
    let project_dir = tmp.path().join("scoped_project");
    let nested = project_dir.join("a").join("b");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&nested).unwrap();

    run_session_start(&home, &data_dir, &project_dir, "scope_tenant");

    let token = format!("scope_fallback_nested_{}", nonce());
    let text = format!("Validated nested scope fallback retrieval token {token}.");
    assert_success(add_text(&home, &data_dir, &project_dir, &text, &[]), "add");

    let search_output = assert_success(search(&home, &data_dir, &nested, &token), "search");
    let search_stdout = String::from_utf8_lossy(&search_output.stdout);
    assert!(search_stdout.contains(&token), "{search_stdout}");
}

#[test]
fn missing_scope_fails_with_actionable_error() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&home).unwrap();

    let output = assert_failure(
        add_text(
            &home,
            &data_dir,
            &home,
            "This add should fail before writing because no tenant can be resolved.",
            &[],
        ),
        "add without scope",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no --tenant-id given and no .memd/project_scope.json found upward from"),
        "{stderr}"
    );
}

#[test]
fn explicit_tenant_flags_override_scope_file() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let data_dir = tmp.path().join("data");
    let project_dir = tmp.path().join("scoped_project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    write_scope(&project_dir, "alpha", "scope_project");

    let token = format!("scope_override_beta_{}", nonce());
    let text = format!("Validated explicit beta override token {token}.");
    assert_success(
        add_text(
            &home,
            &data_dir,
            &project_dir,
            &text,
            &["--tenant-id", "beta"],
        ),
        "add beta",
    );

    assert!(stats_total(&home, &data_dir, &project_dir, "beta") >= 1);
    assert_eq!(stats_total(&home, &data_dir, &project_dir, "alpha"), 0);
}
