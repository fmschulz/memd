#![cfg(unix)]

use std::fs::Permissions;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tempfile::tempdir;

fn memd_bin() -> String {
    env!("CARGO_BIN_EXE_memd").to_string()
}

#[derive(Debug)]
struct WorkerGuard {
    data_dir: PathBuf,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let _ = Command::new(memd_bin())
            .arg("--data-dir")
            .arg(&self.data_dir)
            .args(["warm", "stop"])
            .output();
        for pid in warm_pids(&self.data_dir) {
            if pid_is_running(pid) {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
        }
    }
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos()
}

fn warm_pid_files(data_dir: &Path) -> Vec<PathBuf> {
    let warm_dir = data_dir.join("warm");
    let Ok(entries) = std::fs::read_dir(warm_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("memd.pid"))
        .filter(|path| path.is_file())
        .collect()
}

fn warm_pids(data_dir: &Path) -> Vec<u32> {
    warm_pid_files(data_dir)
        .into_iter()
        .filter_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.lines().next()?.trim().parse::<u32>().ok())
        })
        .collect()
}

fn pid_is_running(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_is_running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !pid_is_running(pid)
}

fn parse_worker_pid(stdout: &[u8], data_dir: &Path) -> u32 {
    let value: Value = serde_json::from_slice(stdout).expect("warm start JSON");
    value
        .get("pid")
        .and_then(Value::as_u64)
        .or_else(|| value.pointer("/result/pid").and_then(Value::as_u64))
        .and_then(|pid| u32::try_from(pid).ok())
        .or_else(|| warm_pids(data_dir).into_iter().next())
        .expect("worker pid")
}

fn start_worker(data_dir: &Path) -> (WorkerGuard, u32) {
    let output = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(["warm", "start"])
        .output()
        .expect("run memd warm start");
    assert!(
        output.status.success(),
        "warm start failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let pid = parse_worker_pid(&output.stdout, data_dir);
    assert!(pid_is_running(pid), "worker pid {pid} is not running");
    (
        WorkerGuard {
            data_dir: data_dir.to_path_buf(),
        },
        pid,
    )
}

fn start_worker_from(data_dir: &Path, project_dir: &Path) -> (WorkerGuard, u32) {
    let output = Command::new(memd_bin())
        .current_dir(project_dir)
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--search-variant", "bm25-only", "warm", "start"])
        .output()
        .expect("run memd warm start from project");
    assert!(
        output.status.success(),
        "warm start failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let pid = parse_worker_pid(&output.stdout, data_dir);
    assert!(pid_is_running(pid), "worker pid {pid} is not running");
    (
        WorkerGuard {
            data_dir: data_dir.to_path_buf(),
        },
        pid,
    )
}

fn write_project_scope(project_dir: &Path, tenant_id: &str, project_id: &str) {
    std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
    std::fs::write(
        project_dir.join(".memd/project_scope.json"),
        serde_json::to_vec(&serde_json::json!({
            "tenant_id": tenant_id,
            "project_id": project_id,
            "interface": "cli",
            "cli_command": "memd",
            "agent_context_output": ".memd/context.md",
            "project_dir": project_dir,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn stop_worker(data_dir: &Path) {
    let output = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(["warm", "stop"])
        .output()
        .expect("run memd warm stop");
    assert!(
        output.status.success(),
        "warm stop failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

fn add_command(data_dir: &Path, warm: &str, token: &str) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args([
            "add",
            "--warm",
            warm,
            "--tenant-id",
            "t",
            "--project-id",
            "p",
            "--chunk-type",
            "summary",
            "--tags",
            "kind:note",
            "--text",
        ])
        .arg(format!("warm write routing token {token}"));
    command
}

fn export_contains(data_dir: &Path, token: &str) {
    let output = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(["export", "--tenant-id", "t", "--format", "json"])
        .output()
        .expect("run memd export");
    assert!(
        output.status.success(),
        "export failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(token),
        "export did not contain {token}:\n{stdout}"
    );
}

#[test]
fn add_via_worker_is_durable_and_lock_proof() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, pid) = start_worker(&data_dir);
    let token = format!("worker_durable_{}", nonce());

    let output = add_command(&data_dir, "required", &token)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "1")
        .output()
        .expect("run memd add through worker");
    assert!(
        output.status.success(),
        "worker add failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    assert!(pid_is_running(pid), "worker pid {pid} stopped after add");
    assert_eq!(warm_pids(&data_dir).into_iter().next(), Some(pid));

    stop_worker(&data_dir);
    assert!(wait_for_pid_exit(pid, Duration::from_secs(10)));
    export_contains(&data_dir, &token);
    drop(guard);
}

#[test]
fn worker_unreachable_auto_falls_back_to_direct_write() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let warm_dir = data_dir.join("warm");
    std::fs::create_dir_all(&warm_dir).unwrap();
    // This is meaningless when run as root; CI runners are non-root.
    std::fs::set_permissions(&warm_dir, Permissions::from_mode(0o555)).unwrap();

    let token = format!("auto_fallback_{}", nonce());
    let output = add_command(&data_dir, "auto", &token)
        .output()
        .expect("run memd add with auto warm fallback");
    std::fs::set_permissions(&warm_dir, Permissions::from_mode(0o755)).unwrap();

    assert!(
        output.status.success(),
        "auto fallback add failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        warm_pid_files(&data_dir).is_empty(),
        "unexpected worker pid files: {:?}",
        warm_pid_files(&data_dir)
    );
    assert_eq!(
        std::fs::read_dir(&warm_dir).unwrap().count(),
        0,
        "warm socket dir should not have been created"
    );
    export_contains(&data_dir, &token);
}

#[test]
fn direct_write_against_live_worker_fails_actionably_and_reads_coexist() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (_guard, pid) = start_worker(&data_dir);
    let seed = format!("search_seed_{}", nonce());
    let seed_output = add_command(&data_dir, "required", &seed)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "1")
        .output()
        .expect("seed via worker");
    assert!(
        seed_output.status.success(),
        "seed add failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&seed_output.stderr),
        String::from_utf8_lossy(&seed_output.stdout)
    );

    let direct_token = format!("direct_fail_{}", nonce());
    let direct = add_command(&data_dir, "off", &direct_token)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "500")
        .output()
        .expect("run direct add");
    assert!(
        !direct.status.success(),
        "direct add unexpectedly succeeded: stdout:\n{}",
        String::from_utf8_lossy(&direct.stdout)
    );
    let stderr = String::from_utf8_lossy(&direct.stderr);
    assert!(stderr.contains("writer lock held by another process"));
    assert!(stderr.contains(&format!("pid={pid}")), "stderr:\n{stderr}");
    assert!(stderr.contains("--warm"), "stderr:\n{stderr}");

    let search = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "search",
            "--warm",
            "off",
            "--tenant-id",
            "t",
            "--query",
            "seed",
        ])
        .output()
        .expect("run read-only search");
    assert!(
        search.status.success(),
        "search failed while worker was alive: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&search.stderr),
        String::from_utf8_lossy(&search.stdout)
    );
}

#[test]
fn warm_structured_operations_keep_client_scope_across_working_directories() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let worker_project = tmp.path().join("worker_project");
    let call_project = tmp.path().join("call_project");
    let unscoped_project = tmp.path().join("unscoped_project");
    let malformed_project = tmp.path().join("malformed_project");
    write_project_scope(&worker_project, "worker_tenant", "worker_project");
    write_project_scope(&call_project, "call_tenant", "call_project");
    std::fs::create_dir_all(&unscoped_project).unwrap();
    std::fs::create_dir_all(malformed_project.join(".memd")).unwrap();
    std::fs::write(
        malformed_project.join(".memd/project_scope.json"),
        "{ malformed scope",
    )
    .unwrap();
    let (guard, pid) = start_worker_from(&data_dir, &worker_project);

    let call_output = Command::new(memd_bin())
        .current_dir(&call_project)
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "--search-variant",
            "bm25-only",
            "call",
            "memory.add",
            "--json",
            r#"{"text":"client scoped warm call","type":"doc"}"#,
            "--warm",
            "required",
        ])
        .output()
        .expect("run scoped call through warm worker");
    assert!(
        call_output.status.success(),
        "warm call failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&call_output.stderr),
        String::from_utf8_lossy(&call_output.stdout)
    );
    let call_payload: Value = serde_json::from_slice(&call_output.stdout).unwrap();
    let call_chunk_id = call_payload["chunk_id"].as_str().expect("call chunk id");

    let jsonl = r#"{"tool":"memory.add","arguments":{"text":"unscoped warm batch","type":"doc"}}
"#;
    let mut child = Command::new(memd_bin())
        .current_dir(&unscoped_project)
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "--search-variant",
            "bm25-only",
            "batch",
            "--jsonl",
            "-",
            "--warm",
            "required",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn unscoped batch through warm worker");
    child
        .stdin
        .as_mut()
        .expect("batch stdin")
        .write_all(jsonl.as_bytes())
        .expect("write warm batch JSONL");
    let batch_output = child.wait_with_output().expect("wait for warm batch");
    assert!(
        batch_output.status.success(),
        "warm batch failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&batch_output.stderr),
        String::from_utf8_lossy(&batch_output.stdout)
    );
    let batch_row: Value = serde_json::from_slice(&batch_output.stdout).unwrap();
    assert_eq!(batch_row["ok"], true);
    let batch_chunk_id = batch_row["result"]["chunk_id"]
        .as_str()
        .expect("batch chunk id");

    let continue_jsonl = concat!(
        "{\"tool\":\"memory.add\",\"arguments\":{\"tenant_id\":\"receipt_tenant\",\"text\":\"warm receipt survives\",\"type\":\"doc\"}}\n",
        "{\"tool\":\"memory.add\",\"arguments\":{\"text\":\"must not route\",\"type\":\"doc\"}}\n",
        "{\"tool\":\"memory.search\",\"arguments\":{\"tenant_id\":\"receipt_tenant\",\"query\":\"warm receipt survives\",\"k\":1}}\n"
    );
    let mut continue_child = Command::new(memd_bin())
        .current_dir(&malformed_project)
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "--search-variant",
            "bm25-only",
            "batch",
            "--jsonl",
            "-",
            "--continue-on-error",
            "--warm",
            "required",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn malformed-scope warm batch");
    continue_child
        .stdin
        .as_mut()
        .expect("continue batch stdin")
        .write_all(continue_jsonl.as_bytes())
        .expect("write continue-on-error warm batch JSONL");
    let continue_output = continue_child
        .wait_with_output()
        .expect("wait for continue-on-error warm batch");
    assert!(
        continue_output.status.success(),
        "continue-on-error warm batch failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&continue_output.stderr),
        String::from_utf8_lossy(&continue_output.stdout)
    );
    let continue_rows = String::from_utf8(continue_output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(continue_rows.len(), 3);
    assert_eq!(continue_rows[0]["ok"], true);
    assert_eq!(continue_rows[1]["ok"], false);
    assert!(continue_rows[1]["error"]
        .as_str()
        .unwrap()
        .contains("malformed"));
    assert_eq!(continue_rows[2]["ok"], true);
    assert_eq!(
        continue_rows[2]["result"]["results"][0]["text"],
        "warm receipt survives"
    );

    stop_worker(&data_dir);
    assert!(wait_for_pid_exit(pid, Duration::from_secs(10)));

    let get = |tenant_id: &str, chunk_id: &str| {
        Command::new(memd_bin())
            .current_dir(&unscoped_project)
            .arg("--data-dir")
            .arg(&data_dir)
            .args([
                "--search-variant",
                "bm25-only",
                "get",
                "--tenant-id",
                tenant_id,
                "--chunk-id",
                chunk_id,
            ])
            .output()
            .expect("get warm structured write")
    };

    let call_get = get("call_tenant", call_chunk_id);
    assert!(
        call_get.status.success(),
        "scoped call get failed: {}",
        String::from_utf8_lossy(&call_get.stderr)
    );
    let call_chunk: Value = serde_json::from_slice(&call_get.stdout).unwrap();
    assert_eq!(call_chunk["project_id"], "call_project");

    let batch_get = get("default", batch_chunk_id);
    assert!(
        batch_get.status.success(),
        "unscoped batch get failed: {}",
        String::from_utf8_lossy(&batch_get.stderr)
    );
    let batch_chunk: Value = serde_json::from_slice(&batch_get.stdout).unwrap();
    assert!(batch_chunk["project_id"].is_null());

    for chunk_id in [call_chunk_id, batch_chunk_id] {
        let worker_scope_get = get("worker_tenant", chunk_id);
        assert!(
            worker_scope_get.status.success(),
            "worker-scope lookup failed: {}",
            String::from_utf8_lossy(&worker_scope_get.stderr)
        );
        let worker_scope_chunk: Value = serde_json::from_slice(&worker_scope_get.stdout).unwrap();
        assert!(
            worker_scope_chunk.is_null(),
            "worker cwd scope captured client write {chunk_id}: {worker_scope_chunk}"
        );
    }

    drop(guard);
}
