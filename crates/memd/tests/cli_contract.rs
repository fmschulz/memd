use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

fn memd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_memd")
}

fn write_project_scope(project_dir: &std::path::Path) {
    std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
    std::fs::write(
        project_dir.join(".memd/project_scope.json"),
        serde_json::to_vec(&serde_json::json!({
            "tenant_id": "scoped_tenant",
            "project_id": "scoped_project",
            "interface": "cli",
            "cli_command": "memd",
            "agent_context_output": ".memd/context.md",
            "project_dir": project_dir,
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn supported_cli_help_surfaces_resolve() {
    let commands = [
        vec!["--help"],
        vec!["add", "--help"],
        vec!["search", "--help"],
        vec!["agent-context", "--help"],
        vec!["memory-md", "--help"],
        vec!["eval-memory-md", "--help"],
        vec!["eval-retrieval", "--help"],
        vec!["eval-write-quality", "--help"],
        vec!["eval-outcome-ranking", "--help"],
        vec!["audit", "--help"],
        vec!["cleanup-plan", "--help"],
        vec!["purge", "--help"],
        vec!["purge-archive", "--help"],
        vec!["outcome", "--help"],
        vec!["call", "--help"],
        vec!["batch", "--help"],
        vec!["warm", "--help"],
    ];

    for args in commands {
        let output = Command::new(memd_bin())
            .args(&args)
            .output()
            .expect("run memd help");
        assert!(
            output.status.success(),
            "memd {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Usage:"),
            "missing usage in {args:?}: {stdout}"
        );
        if args.as_slice() == ["--help"] {
            assert!(
                !stdout.contains("--mode"),
                "top-level help must not expose removed server --mode flag"
            );
        }
        assert!(
            !stdout.contains("mcp"),
            "supported help must not advertise removed MCP startup in {args:?}"
        );
    }
}

#[test]
fn mode_mcp_is_rejected_by_current_cli() {
    let output = Command::new(memd_bin())
        .args(["--mode", "mcp"])
        .output()
        .expect("run memd --mode mcp");

    assert!(
        !output.status.success(),
        "--mode mcp unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--mode") || stderr.contains("unexpected argument"),
        "unexpected rejection message: {stderr}"
    );
}

#[test]
fn batch_cli_preserves_operation_response_shapes() {
    let jsonl = r#"{"tool":"memory.add","arguments":{"tenant_id":"cli_contract","project_id":"p","text":"contract alpha memory","type":"doc","tags":["contract"]}}
{"tool":"memory.search","arguments":{"tenant_id":"cli_contract","project_id":"p","query":"contract alpha","k":1,"compact":true,"include_text":true}}
{"tool":"task.start","arguments":{"tenant_id":"cli_contract","project_id":"p","agent_id":"agent","session_id":"session","goal":"contract task"}}
{"tool":"artifact.create","arguments":{"tenant_id":"cli_contract","project_id":"p","task_id":"contract-task","artifact_kind":"evidence","summary":"contract evidence","confidence":0.9}}
not-json
"#;

    let mut child = Command::new(memd_bin())
        .args([
            "--in-memory",
            "batch",
            "--jsonl",
            "-",
            "--continue-on-error",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn memd batch");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(jsonl.as_bytes())
        .expect("write jsonl");
    let output = child.wait_with_output().expect("wait for memd batch");

    assert!(
        output.status.success(),
        "batch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = String::from_utf8(output.stdout).expect("utf8 stdout");
    let rows = rows
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("jsonl row"))
        .collect::<Vec<_>>();

    assert_eq!(rows.len(), 5, "unexpected batch rows: {rows:#?}");
    assert_eq!(rows[0]["tool"], "memory.add");
    assert_eq!(rows[0]["ok"], true);
    assert!(rows[0]["result"]["chunk_id"].is_string());

    assert_eq!(rows[1]["tool"], "memory.search");
    assert_eq!(rows[1]["ok"], true);
    assert!(rows[1]["result"]["results"].is_array());
    assert!(rows[1]["result"]["results"][0]["chunk_id"].is_string());

    assert_eq!(rows[2]["tool"], "task.start");
    assert_eq!(rows[2]["ok"], true);
    assert!(rows[2]["result"]["task_id"].is_string());

    assert_eq!(rows[3]["tool"], "artifact.create");
    assert_eq!(rows[3]["ok"], true);
    assert!(rows[3]["result"]["artifact_id"].is_string());

    assert_eq!(rows[4]["ok"], false);
    assert!(rows[4]["error"]
        .as_str()
        .expect("error")
        .contains("invalid JSONL request"));
}

#[test]
fn call_inherits_repository_scope_for_structured_writes() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().join("project");
    let data_dir = temp.path().join("data");
    write_project_scope(&project_dir);

    let added = Command::new(memd_bin())
        .current_dir(&project_dir)
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "--search-variant",
            "bm25-only",
            "call",
            "memory.add",
            "--json",
            r#"{"text":"scoped call memory","type":"doc"}"#,
            "--warm",
            "off",
        ])
        .output()
        .expect("run scoped memd call");
    assert!(
        added.status.success(),
        "call failed: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let added: Value = serde_json::from_slice(&added.stdout).unwrap();
    let chunk_id = added["chunk_id"].as_str().expect("chunk id");

    let fetched = Command::new(memd_bin())
        .current_dir(&project_dir)
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["--search-variant", "bm25-only", "get", "--chunk-id"])
        .arg(chunk_id)
        .output()
        .expect("get scoped call write");
    assert!(
        fetched.status.success(),
        "get failed: {}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    let fetched: Value = serde_json::from_slice(&fetched.stdout).unwrap();
    assert_eq!(fetched["tenant_id"], "scoped_tenant");
    assert_eq!(fetched["project_id"], "scoped_project");
}

#[test]
fn batch_inherits_repository_scope_for_structured_operations() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().join("project");
    write_project_scope(&project_dir);
    let jsonl = r#"{"tool":"memory.add","arguments":{"text":"scoped batch memory","type":"doc"}}
{"tool":"memory.search","arguments":{"query":"scoped batch","k":1}}
"#;

    let mut child = Command::new(memd_bin())
        .current_dir(&project_dir)
        .args(["--in-memory", "batch", "--jsonl", "-", "--warm", "off"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn scoped memd batch");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(jsonl.as_bytes())
        .expect("write scoped jsonl");
    let output = child.wait_with_output().expect("wait for scoped batch");
    assert!(
        output.status.success(),
        "batch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["ok"], true);
    assert_eq!(rows[1]["ok"], true);
    assert_eq!(
        rows[1]["result"]["results"][0]["tenant_id"],
        "scoped_tenant"
    );
    assert_eq!(
        rows[1]["result"]["results"][0]["project_id"],
        "scoped_project"
    );
}

#[test]
fn batch_continue_on_error_keeps_receipts_after_scope_failure() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().join("project");
    std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
    std::fs::write(
        project_dir.join(".memd/project_scope.json"),
        "{ malformed scope",
    )
    .unwrap();
    let jsonl = r#"{"tool":"memory.add","arguments":{"tenant_id":"explicit_tenant","text":"receipt survives scope error","type":"doc"}}
{"tool":"memory.add","arguments":{"text":"must not be written","type":"doc"}}
{"tool":"memory.search","arguments":{"tenant_id":"explicit_tenant","query":"receipt survives","k":1}}
"#;

    let mut child = Command::new(memd_bin())
        .current_dir(&project_dir)
        .args([
            "--in-memory",
            "batch",
            "--jsonl",
            "-",
            "--continue-on-error",
            "--warm",
            "off",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn batch with malformed scope");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(jsonl.as_bytes())
        .expect("write malformed-scope jsonl");
    let output = child.wait_with_output().expect("wait for batch");
    assert!(
        output.status.success(),
        "batch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["ok"], true);
    assert_eq!(rows[1]["ok"], false);
    let scope_error = rows[1]["error"].as_str().unwrap();
    assert!(scope_error.contains("malformed"), "{scope_error}");
    assert!(
        scope_error.contains(".memd/project_scope.json"),
        "{scope_error}"
    );
    assert_eq!(rows[2]["ok"], true);
    assert_eq!(
        rows[2]["result"]["results"][0]["text"],
        "receipt survives scope error"
    );
}
