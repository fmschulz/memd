use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn run_doctor(home: &Path, strict: bool) -> Output {
    let mut command = Command::new(memd_bin());
    command.env("HOME", home).current_dir(home).arg("doctor");
    if strict {
        command.arg("--strict");
    }
    command.output().expect("run memd doctor")
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

fn add_command(data_dir: &Path, text: &str) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args([
            "add",
            "--warm",
            "off",
            "--tenant-id",
            "t",
            "--project-id",
            "p",
            "--chunk-type",
            "summary",
            "--tags",
            "kind:progress",
            "--text",
        ])
        .arg(text)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "10000");
    command
}

fn audit_strict_command(data_dir: &Path) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args(["audit", "--strict", "--format", "json"]);
    command
}

#[test]
fn doctor_strict_exits_two_without_creating_default_data_dir() {
    let home = tempdir().unwrap();
    let data_dir = home.path().join(".memd/data");

    let default = run_doctor(home.path(), false);
    assert!(
        default.status.success(),
        "doctor default failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&default.stderr),
        String::from_utf8_lossy(&default.stdout)
    );
    assert!(
        !data_dir.exists(),
        "doctor default created {}",
        data_dir.display()
    );

    let strict = run_doctor(home.path(), true);
    assert_eq!(
        strict.status.code(),
        Some(2),
        "doctor --strict stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&strict.stdout),
        String::from_utf8_lossy(&strict.stderr)
    );
    let stdout = String::from_utf8_lossy(&strict.stdout);
    assert!(stdout.contains("[--]"), "{stdout}");
    assert!(stdout.contains("failing: "), "{stdout}");
    assert!(
        !data_dir.exists(),
        "doctor --strict created {}",
        data_dir.display()
    );
}

#[test]
fn audit_strict_exits_zero_for_healthy_store() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let token = format!("audit_strict_{}", nonce());
    let text = format!("Validation: audit strict healthy store {token} remains readable.");

    assert_success(
        add_command(&data_dir, &text)
            .output()
            .expect("run memd add"),
        "add",
    );

    let audit = assert_success(
        audit_strict_command(&data_dir)
            .output()
            .expect("run memd audit --strict"),
        "audit --strict",
    );
    let stdout = String::from_utf8_lossy(&audit.stdout);
    assert!(
        stdout.contains("\"unreadable_active_chunks\": 0"),
        "{stdout}"
    );
}

#[test]
fn doctor_honors_global_data_dir_flag() {
    let home = tempdir().unwrap();
    let custom_data = home.path().join("custom_data");
    std::fs::create_dir_all(&custom_data).unwrap();
    assert_success(
        add_command(
            &custom_data,
            "Validation: doctor data-dir probe store remains readable.",
        )
        .output()
        .expect("run memd add"),
        "seed add",
    );

    let output = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(&custom_data)
        .args(["doctor", "--format", "json"])
        .env("HOME", home.path())
        .output()
        .expect("run memd doctor");
    let output = assert_success(output, "doctor --data-dir");

    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor JSON output");
    assert_eq!(
        report["data_dir"]["path"],
        custom_data.display().to_string(),
        "doctor must diagnose the resolved --data-dir, not ~/.memd/data"
    );
    assert_eq!(report["data_dir"]["tenant_count"], 1);
    assert_eq!(report["warm_worker"]["status"], "not_running");
}
