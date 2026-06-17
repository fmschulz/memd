#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::tempdir;

fn memd_bin() -> String {
    // Isolate the worker cap from concurrent memd activity (see warm module
    // unit tests for the cap logic itself).
    use std::sync::Once;
    static UNCAP: Once = Once::new();
    UNCAP.call_once(|| std::env::set_var("MEMD_WARM_MAX_WORKERS", "1000000"));
    env!("CARGO_BIN_EXE_memd").to_string()
}

#[derive(Debug)]
struct UsageRow {
    op: String,
    outcome: String,
    tenant: Option<String>,
    project: Option<String>,
    chunk_count: Option<i64>,
    bytes: Option<i64>,
    detail: Option<String>,
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
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

fn usage_rows(data_dir: &Path) -> Vec<UsageRow> {
    let conn = Connection::open(data_dir.join("metadata.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT op, outcome, tenant, project, chunk_count, bytes, detail
             FROM usage_events
             ORDER BY id",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |row| {
            Ok(UsageRow {
                op: row.get(0)?,
                outcome: row.get(1)?,
                tenant: row.get(2)?,
                project: row.get(3)?,
                chunk_count: row.get(4)?,
                bytes: row.get(5)?,
                detail: row.get(6)?,
            })
        })
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn count_op(rows: &[UsageRow], op: &str) -> usize {
    rows.iter().filter(|row| row.op == op).count()
}

fn add_command(data_dir: &Path, warm: &str, text: &str, tags: &str) -> Command {
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
            tags,
            "--text",
        ])
        .arg(text);
    command
}

fn run_add(mut command: Command) -> Output {
    command.env("MEMD_WRITER_LOCK_TIMEOUT_MS", "10000");
    command.output().expect("run memd add")
}

fn assert_add_success(command: Command) -> Output {
    let output = run_add(command);
    assert!(
        output.status.success(),
        "add failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

fn admitted_text(token: &str) -> String {
    format!("Validation: cargo test -p memd passed after fixing usage ledger token {token}.")
}

fn rejected_text(token: &str) -> String {
    format!("starting to inspect the code {token}")
}

fn search_command(data_dir: &Path, warm: &str, query: &str) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args([
            "search",
            "--warm",
            warm,
            "--tenant-id",
            "t",
            "--project-id",
            "p",
            "--query",
        ])
        .arg(query);
    command
}

fn call_delete_command(data_dir: &Path, chunk_id: &str) -> Command {
    let mut command = Command::new(memd_bin());
    let json = format!(r#"{{"tenant_id":"t","chunk_id":"{chunk_id}"}}"#);
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args(["call", "memory.delete", "--warm", "off", "--json"])
        .arg(json)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "10000");
    command
}

fn search_required(data_dir: &Path, query: &str) -> Output {
    search_command(data_dir, "required", query)
        .output()
        .expect("run memd search through worker")
}

#[test]
fn direct_add_records_admitted_event() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let token = format!("usage_direct_add_{}", nonce());
    let text = admitted_text(&token);

    assert_add_success(add_command(&data_dir, "off", &text, "kind:progress"));

    let rows = usage_rows(&data_dir);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.op, "add");
    assert_eq!(row.outcome, "admitted");
    assert_eq!(row.tenant.as_deref(), Some("t"));
    assert_eq!(row.project.as_deref(), Some("p"));
    assert_eq!(row.chunk_count, Some(1));
    assert_eq!(row.bytes, Some(text.len() as i64));
}

#[test]
fn direct_add_rejected_records_reason() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let token = format!("usage_reject_{}", nonce());
    let text = rejected_text(&token);

    let output = run_add(add_command(&data_dir, "off", &text, "kind:progress"));
    assert!(
        !output.status.success(),
        "rejected add unexpectedly succeeded: stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let rows = usage_rows(&data_dir);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.op, "add");
    assert!(row.outcome.starts_with("rejected:"), "{row:?}");
    assert_eq!(row.chunk_count, Some(0));
}

#[test]
fn delete_missing_chunk_records_not_found_event() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let token = format!("usage_delete_missing_{}", nonce());
    let text = admitted_text(&token);

    let add = assert_add_success(add_command(&data_dir, "off", &text, "kind:progress"));
    let value: Value = serde_json::from_slice(&add.stdout).expect("add JSON");
    let chunk_id = value
        .get("chunk_id")
        .and_then(Value::as_str)
        .expect("chunk_id");

    let first_delete = call_delete_command(&data_dir, chunk_id)
        .output()
        .expect("run first memory.delete");
    assert!(
        first_delete.status.success(),
        "first delete failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&first_delete.stderr),
        String::from_utf8_lossy(&first_delete.stdout)
    );

    let second_delete = call_delete_command(&data_dir, chunk_id)
        .output()
        .expect("run second memory.delete");
    assert!(
        second_delete.status.success(),
        "second delete failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&second_delete.stderr),
        String::from_utf8_lossy(&second_delete.stdout)
    );

    let rows = usage_rows(&data_dir);
    let deletes = rows
        .iter()
        .filter(|row| row.op == "delete")
        .collect::<Vec<_>>();
    assert_eq!(deletes.len(), 2, "{rows:?}");
    assert_eq!(deletes[0].outcome, "ok");
    assert_eq!(deletes[0].chunk_count, Some(1));
    assert_eq!(deletes[1].outcome, "not_found");
    assert_eq!(deletes[1].chunk_count, Some(0));
}

#[test]
fn worker_search_and_agent_context_record_events() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, _pid) = start_worker(&data_dir);
    let token = format!("usage_worker_search_{}", nonce());
    let text = admitted_text(&token);

    let miss = format!("usage_zero_hit_{}", nonce());
    let zero_search = search_required(&data_dir, &miss);
    assert!(
        zero_search.status.success(),
        "zero-hit search failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&zero_search.stderr),
        String::from_utf8_lossy(&zero_search.stdout)
    );
    let rows = usage_rows(&data_dir);
    assert!(
        rows.iter()
            .any(|row| row.op == "search" && row.outcome == "zero_hits"),
        "{rows:?}"
    );

    assert_add_success(add_command(&data_dir, "required", &text, "kind:progress"));

    let hit_search = search_required(&data_dir, &token);
    assert!(
        hit_search.status.success(),
        "hit search failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&hit_search.stderr),
        String::from_utf8_lossy(&hit_search.stdout)
    );
    let rows = usage_rows(&data_dir);
    let hit_row = rows
        .iter()
        .find(|row| row.op == "search" && row.outcome.starts_with("hits:"))
        .expect("search hit usage row");
    let hits = hit_row
        .outcome
        .strip_prefix("hits:")
        .and_then(|value| value.parse::<i64>().ok())
        .expect("hits count");
    assert!(hits >= 1, "{hit_row:?}");
    let detail = hit_row.detail.as_deref().expect("search detail");
    assert!(detail.contains("\"q_len\""), "{detail}");
    assert!(detail.contains("\"k\""), "{detail}");
    assert!(detail.contains("\"q_hash\""), "{detail}");
    assert!(!detail.contains(&token), "{detail}");

    let search_rows_before = count_op(&rows, "search");
    let agent_context = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "agent-context",
            "--warm",
            "required",
            "--tenant-id",
            "t",
            "--query",
        ])
        .arg(&token)
        .args(["--k", "2"])
        .output()
        .expect("run memd agent-context through worker");
    assert!(
        agent_context.status.success(),
        "agent-context failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&agent_context.stderr),
        String::from_utf8_lossy(&agent_context.stdout)
    );

    let rows = usage_rows(&data_dir);
    assert_eq!(count_op(&rows, "search"), search_rows_before);
    let agent_rows = rows
        .iter()
        .filter(|row| row.op == "agent_context")
        .collect::<Vec<_>>();
    assert_eq!(agent_rows.len(), 1, "{rows:?}");
    assert!(agent_rows[0].chunk_count.unwrap_or(0) >= 1, "{rows:?}");

    stop_worker(&data_dir);
    drop(guard);
}

#[test]
fn read_only_search_records_nothing() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let token = format!("usage_read_only_search_{}", nonce());
    let text = admitted_text(&token);

    assert_add_success(add_command(&data_dir, "off", &text, "kind:progress"));
    let before = usage_rows(&data_dir);
    assert_eq!(before.len(), 1);

    let search = search_command(&data_dir, "off", &token)
        .output()
        .expect("run memd direct search");
    assert!(
        search.status.success(),
        "direct search failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&search.stderr),
        String::from_utf8_lossy(&search.stdout)
    );

    let after = usage_rows(&data_dir);
    assert_eq!(after.len(), before.len(), "{after:?}");
    assert_eq!(count_op(&after, "search"), 0, "{after:?}");
}

#[test]
fn retention_sweep_on_writer_open() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let first = admitted_text(&format!("usage_retention_first_{}", nonce()));
    let second = admitted_text(&format!("usage_retention_second_{}", nonce()));

    assert_add_success(add_command(&data_dir, "off", &first, "kind:progress"));

    let conn = Connection::open(data_dir.join("metadata.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute(
        "INSERT INTO usage_events (
            ts_unix_ms, op, tenant, project, outcome, chunk_count, bytes, detail
         )
         VALUES (?1, 'synthetic_old', 't', 'p', 'old', 0, NULL, NULL)",
        rusqlite::params![now_ms().saturating_sub(200 * 86_400_000)],
    )
    .unwrap();
    drop(conn);

    assert_add_success(add_command(&data_dir, "off", &second, "kind:progress"));

    let rows = usage_rows(&data_dir);
    assert_eq!(count_op(&rows, "add"), 2, "{rows:?}");
    assert_eq!(count_op(&rows, "synthetic_old"), 0, "{rows:?}");
}

#[test]
fn escape_hatch_env_disables_recording() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let token = format!("usage_escape_hatch_{}", nonce());
    let text = admitted_text(&token);
    let mut add = add_command(&data_dir, "off", &text, "kind:progress");
    add.env("MEMD_USAGE_LEDGER", "off");

    assert_add_success(add);

    let rows = usage_rows(&data_dir);
    assert!(rows.is_empty(), "{rows:?}");
}
