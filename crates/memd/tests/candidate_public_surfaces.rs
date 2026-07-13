#![cfg(unix)]

use std::process::Command;

fn memd_bin() -> String {
    env!("CARGO_BIN_EXE_memd").to_string()
}

fn assert_success(output: std::process::Output, label: &str) -> String {
    assert!(
        output.status.success(),
        "{label} failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn add_summary(data_dir: &std::path::Path, text: &str) -> String {
    let output = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--search-variant", "bm25-only", "add", "--warm", "off"])
        .args([
            "--tenant-id",
            "t",
            "--project-id",
            "p",
            "--chunk-type",
            "summary",
            "--tags",
            "kind:decision,priority:9",
            "--text",
            text,
        ])
        .output()
        .unwrap();
    let stdout = assert_success(output, "add");
    serde_json::from_str::<serde_json::Value>(&stdout).unwrap()["chunk_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn candidate_text_never_reaches_agent_context_memory_md_or_report() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    let project_dir = temp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    add_summary(&data_dir, "visible surface control phoenix");
    let candidate_id = add_summary(&data_dir, "SECRET_CANDIDATE_PHOENIX_NEVER_PUBLIC");
    let connection = rusqlite::Connection::open(data_dir.join("metadata.db")).unwrap();
    connection
        .execute(
            "UPDATE chunks SET status = 'candidate' WHERE chunk_id = ?1",
            [&candidate_id],
        )
        .unwrap();
    drop(connection);

    let agent_context = assert_success(
        Command::new(memd_bin())
            .arg("--data-dir")
            .arg(&data_dir)
            .args(["--search-variant", "bm25-only"])
            .args([
                "agent-context",
                "--warm",
                "off",
                "--tenant-id",
                "t",
                "--project-id",
                "p",
                "--query",
                "phoenix",
                "--k",
                "10",
            ])
            .output()
            .unwrap(),
        "agent-context",
    );
    assert!(
        agent_context.contains("visible surface control phoenix"),
        "agent context did not return the visible control: {agent_context}"
    );
    assert!(!agent_context.contains("SECRET_CANDIDATE_PHOENIX_NEVER_PUBLIC"));

    assert_success(
        Command::new(memd_bin())
            .arg("--data-dir")
            .arg(&data_dir)
            .args(["--search-variant", "bm25-only"])
            .args([
                "memory-md",
                "--tenant-id",
                "t",
                "--project-id",
                "p",
                "--project-dir",
            ])
            .arg(&project_dir)
            .args(["--output", "memory.md"])
            .output()
            .unwrap(),
        "memory-md",
    );
    let memory_md = std::fs::read_to_string(project_dir.join("memory.md")).unwrap();
    assert!(!memory_md.contains("SECRET_CANDIDATE_PHOENIX_NEVER_PUBLIC"));

    let report = assert_success(
        Command::new(memd_bin())
            .arg("--data-dir")
            .arg(&data_dir)
            .args(["--search-variant", "bm25-only"])
            .args([
                "report", "--warm", "off", "--format", "json", "--since", "24h",
            ])
            .current_dir(&project_dir)
            .output()
            .unwrap(),
        "report",
    );
    assert!(!report.contains("SECRET_CANDIDATE_PHOENIX_NEVER_PUBLIC"));
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(
        report.pointer("/growth/store_totals/active_chunks"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        report.pointer("/growth/store_totals/candidate_chunks"),
        Some(&serde_json::json!(1))
    );
}
