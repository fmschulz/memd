#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
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

fn add_command(data_dir: &Path, warm: &str, text: &str) -> Command {
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
        .arg(text);
    command
}

fn search_required(data_dir: &Path, query: &str) -> std::process::Output {
    Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args([
            "search",
            "--warm",
            "required",
            "--tenant-id",
            "t",
            "--query",
        ])
        .arg(query)
        .output()
        .expect("run memd search through worker")
}

fn assert_add_success(mut command: Command) {
    let output = command.output().expect("run memd add");
    assert!(
        output.status.success(),
        "add failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

fn assert_search_contains(data_dir: &Path, query: &str, token: &str) {
    let output = search_required(data_dir, query);
    assert!(
        output.status.success(),
        "search failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(token),
        "search did not contain {token}:\n{stdout}"
    );
}

fn warm_status(data_dir: &Path) -> Value {
    let output = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(["warm", "status"])
        .output()
        .expect("run memd warm status");
    assert!(
        output.status.success(),
        "warm status failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).expect("warm status JSON")
}

#[test]
fn worker_add_is_immediately_searchable_via_worker() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, pid) = start_worker(&data_dir);
    let token = format!("ryw_worker_add_{}", nonce());

    let mut add = add_command(
        &data_dir,
        "required",
        &format!("read your writes token {token}"),
    );
    add.env("MEMD_WRITER_LOCK_TIMEOUT_MS", "1");
    assert_add_success(add);

    assert!(pid_is_running(pid), "worker pid {pid} stopped after add");
    assert_search_contains(&data_dir, &token, &token);

    let query = "kubernetes scheduling latency";
    let first_search = search_required(&data_dir, query);
    assert!(
        first_search.status.success(),
        "baseline search failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&first_search.stderr),
        String::from_utf8_lossy(&first_search.stdout)
    );
    let marker = format!("ryw_cache_marker_{}", nonce());
    let mut add = add_command(
        &data_dir,
        "required",
        &format!("{query} cache invalidation marker {marker}"),
    );
    add.env("MEMD_WRITER_LOCK_TIMEOUT_MS", "1");
    assert_add_success(add);
    assert_search_contains(&data_dir, query, &marker);

    stop_worker(&data_dir);
    drop(guard);
}

#[test]
fn direct_write_while_worker_down_is_visible_after_worker_cold_start() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let token = format!("ryw_direct_cold_start_{}", nonce());

    assert_add_success(add_command(
        &data_dir,
        "off",
        &format!("direct write before worker token {token}"),
    ));

    let (guard, _pid) = start_worker(&data_dir);
    assert_search_contains(&data_dir, &token, &token);

    stop_worker(&data_dir);
    drop(guard);
}

#[test]
fn fifty_add_search_pairs_meet_latency_slo() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, _pid) = start_worker(&data_dir);

    let warmup = format!("ryw_warmup_{}", nonce());
    let mut warmup_add = add_command(&data_dir, "required", &format!("warmup token {warmup}"));
    warmup_add.env("MEMD_WRITER_LOCK_TIMEOUT_MS", "1");
    assert_add_success(warmup_add);
    assert_search_contains(&data_dir, &warmup, &warmup);

    let mut elapsed_ms = Vec::with_capacity(50);
    for _ in 0..50 {
        let token = format!("ryw_pair_{}", nonce());
        let mut add = add_command(
            &data_dir,
            "required",
            &format!("latency pair token {token}"),
        );
        add.env("MEMD_WRITER_LOCK_TIMEOUT_MS", "1");
        let started = Instant::now();
        assert_add_success(add);
        let output = search_required(&data_dir, &token);
        let elapsed = started.elapsed();
        assert!(
            output.status.success(),
            "search failed: stderr:\n{}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&token),
            "search did not contain {token}:\n{stdout}"
        );
        elapsed_ms.push(elapsed.as_millis());
    }

    elapsed_ms.sort_unstable();
    let p50 = elapsed_ms[25];
    let p95 = elapsed_ms[47];
    println!(
        "ryw_add_to_searchable_ms p50={} p95={} min={} max={} n=50",
        p50, p95, elapsed_ms[0], elapsed_ms[49]
    );
    // Measured p95 on the dev machine is 55-65 ms in release (Phase 1 gate);
    // 1000 ms still catches order-of-magnitude regressions. Debug builds run
    // ~20x slower embedding inference (CI measured p95 1179 ms on a 2-core
    // runner), so the latency bound is a release-only contract; debug runs
    // keep the correctness assertions above and the printed measurement.
    #[cfg(not(debug_assertions))]
    assert!(p95 < 1000, "p95 latency {p95}ms exceeds 1000ms");
    #[cfg(debug_assertions)]
    let _ = p95;

    stop_worker(&data_dir);
    drop(guard);
}

#[test]
fn worker_probe_detects_external_sqlite_mutation() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, _pid) = start_worker(&data_dir);

    let search = search_required(&data_dir, "probe baseline");
    assert!(
        search.status.success(),
        "baseline search failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&search.stderr),
        String::from_utf8_lossy(&search.stdout)
    );
    let status = warm_status(&data_dir);
    let probe = status
        .pointer("/result/ryw_probe")
        .expect("warm status result.ryw_probe");
    assert!(probe.get("checks").and_then(Value::as_u64).unwrap_or(0) >= 1);
    assert_eq!(
        probe
            .get("external_detected")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        0
    );

    let conn = Connection::open(data_dir.join("metadata.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ryw_probe_external_test(x INTEGER)",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO ryw_probe_external_test(x) VALUES (1)", [])
        .unwrap();
    drop(conn);

    let search = search_required(&data_dir, "probe after external mutation");
    assert!(
        search.status.success(),
        "post-mutation search failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&search.stderr),
        String::from_utf8_lossy(&search.stdout)
    );
    let status = warm_status(&data_dir);
    let probe = status
        .pointer("/result/ryw_probe")
        .expect("warm status result.ryw_probe after mutation");
    assert!(
        probe
            .get("external_detected")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            >= 1
    );
    assert!(
        probe.get("repairs").and_then(Value::as_u64).unwrap_or(0) >= 1,
        "probe stats after mutation: {probe}"
    );

    stop_worker(&data_dir);
    drop(guard);
}
