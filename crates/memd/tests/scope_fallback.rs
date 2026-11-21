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
        .env("MEMD_DEFAULT_TENANT", "scope_tenant")
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

/// The first flagless `add` creates the project scope: tenant from
/// `MEMD_DEFAULT_TENANT`, project from the directory name.
fn assert_scope_created_by_first_add(project_dir: &Path) {
    let scope = project_dir.join(".memd/project_scope.json");
    assert!(
        scope.is_file(),
        "the first add must create {}",
        scope.display()
    );
    let parsed: Value = serde_json::from_slice(&std::fs::read(&scope).unwrap()).unwrap();
    assert_eq!(parsed["tenant_id"], "scope_tenant");
    assert_eq!(parsed["project_id"], "scoped_project");
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

    let token = format!("scope_fallback_root_{}", nonce());
    let text = format!("Validated scope fallback root retrieval token {token}.");
    assert_success(add_text(&home, &data_dir, &project_dir, &text, &[]), "add");
    assert_scope_created_by_first_add(&project_dir);

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

    let token = format!("scope_fallback_nested_{}", nonce());
    let text = format!("Validated nested scope fallback retrieval token {token}.");
    assert_success(add_text(&home, &data_dir, &project_dir, &text, &[]), "add");
    assert_scope_created_by_first_add(&project_dir);

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
fn search_without_scope_fails_and_creates_nothing() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let data_dir = tmp.path().join("data");
    let project_dir = tmp.path().join("scoped_project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();

    let output = assert_failure(
        search(&home, &data_dir, &project_dir, "anything"),
        "search without scope",
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("the first `memd add` creates the scope"),
        "{stderr}"
    );
    assert!(!project_dir.join(".memd").exists());
}

/// A first write that fails must leave no scope behind: a project without a
/// stored fact has no `.memd` directory.
fn unscoped_project(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.join("home");
    let data_dir = tmp.join("data");
    let project_dir = tmp.join("scoped_project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    (home, data_dir, project_dir)
}

#[test]
fn failing_note_leaves_no_scope() {
    let tmp = tempdir().unwrap();
    let (home, data_dir, project_dir) = unscoped_project(tmp.path());

    let output = command(&home, &data_dir, &project_dir)
        .args(["experience", "note", "missing-case.json"])
        .output()
        .expect("run memd experience note");

    assert_failure(output, "note with a missing file");
    assert!(!project_dir.join(".memd").exists());
}

#[test]
fn quality_gate_rejection_leaves_no_scope() {
    let tmp = tempdir().unwrap();
    let (home, data_dir, project_dir) = unscoped_project(tmp.path());

    let output = command(&home, &data_dir, &project_dir)
        .args([
            "add",
            "--warm",
            "off",
            "--chunk-type",
            "summary",
            "--tags",
            "kind:progress",
            "--text",
            "starting to inspect the files",
        ])
        .output()
        .expect("run memd add");

    let output = assert_failure(output, "low-signal add");
    assert!(String::from_utf8_lossy(&output.stderr).contains("rejected by quality gate"));
    assert!(!project_dir.join(".memd").exists());
}

#[test]
fn failing_store_open_leaves_no_scope() {
    let tmp = tempdir().unwrap();
    let (home, _data_dir, project_dir) = unscoped_project(tmp.path());
    let not_a_directory = tmp.path().join("data-is-a-file");
    std::fs::write(&not_a_directory, "not a store").unwrap();

    let output = add_text(
        &home,
        &not_a_directory,
        &project_dir,
        "Validated that a failed store open leaves no scope behind.",
        &[],
    );

    assert_failure(output, "add with an unusable data dir");
    assert!(!project_dir.join(".memd").exists());
}

#[test]
fn session_start_in_an_empty_project_writes_nothing_until_the_first_add() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let data_dir = tmp.path().join("data");
    let project_dir = tmp.path().join("scoped_project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    let start = |label| {
        let output = command(&home, &data_dir, &project_dir)
            .args(["session-start", "--project-dir", "."])
            .output()
            .expect("run memd session-start");
        let output = assert_success(output, label);
        serde_json::from_slice::<Value>(&output.stdout).expect("session-start JSON")
    };

    let before = start("session-start before the first add");
    assert_eq!(before["skipped"], "no_stored_facts");
    assert_eq!(before["project_id"], "scoped_project");
    assert!(!project_dir.join(".memd").exists());
    assert!(!project_dir.join("memory.md").exists());

    let text = "Validated that the first add creates the scope and memory.md.";
    assert_success(add_text(&home, &data_dir, &project_dir, text, &[]), "add");
    assert_scope_created_by_first_add(&project_dir);

    let after = start("session-start after the first add");
    assert!(after.get("skipped").is_none(), "{after}");
    assert!(project_dir.join("memory.md").exists());
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
