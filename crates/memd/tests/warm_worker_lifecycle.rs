#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tempfile::tempdir;

const WRITERS: usize = 8;

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
                sigkill(pid);
            }
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let pids = warm_worker_cmdline_pids(&self.data_dir);
            if pids.is_empty() {
                break;
            }
            for pid in pids {
                sigkill(pid);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
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

fn warm_socket_files(data_dir: &Path) -> Vec<PathBuf> {
    let warm_dir = data_dir.join("warm");
    let Ok(entries) = std::fs::read_dir(warm_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("memd.sock"))
        .filter(|path| path.exists())
        .collect()
}

fn warm_pids(data_dir: &Path) -> Vec<u32> {
    warm_pid_files(data_dir)
        .into_iter()
        .filter_map(|path| read_pid_file(&path))
        .collect()
}

fn read_pid_file(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().next()?.trim().parse::<u32>().ok())
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

fn sigkill(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // Test cleanup only: ignore ESRCH/races with natural worker exit.
    let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
}

fn warm_worker_cmdline_pids(data_dir: &Path) -> Vec<u32> {
    let data_dir = data_dir.to_string_lossy();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_string_lossy().parse::<u32>().ok()?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            let cmdline = String::from_utf8_lossy(&cmdline);
            if cmdline.contains("warm-worker") && cmdline.contains(data_dir.as_ref()) {
                Some(pid)
            } else {
                None
            }
        })
        .collect()
}

fn parse_worker_pid(stdout: &[u8], data_dir: &Path) -> u32 {
    let value: Value = serde_json::from_slice(stdout).expect("warm command JSON");
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

fn stop_worker(data_dir: &Path) -> Output {
    Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(["warm", "stop"])
        .output()
        .expect("run memd warm stop")
}

fn warm_status_pid(data_dir: &Path) -> u32 {
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
    parse_worker_pid(&output.stdout, data_dir)
}

fn add_command(data_dir: &Path, warm: &str, tenant: &str, project: &str, text: &str) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args([
            "add",
            "--warm",
            warm,
            "--tenant-id",
            tenant,
            "--project-id",
            project,
            "--chunk-type",
            "summary",
            "--tags",
            "kind:note",
            "--text",
        ])
        .arg(text);
    command
}

fn spawn_lifecycle_add(data_dir: &Path, writer: usize, round: usize, token: &str) -> Child {
    add_command(
        data_dir,
        "auto",
        "storm",
        "storm",
        &format!("lifecycle writer {writer} round {round} token {token}"),
    )
    .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "15000")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn memd add")
}

fn wait_lifecycle_round(
    children: Vec<(usize, String, Child)>,
    acknowledged: &mut Vec<String>,
    require_all: bool,
) -> Vec<String> {
    let mut diagnostics = Vec::new();
    for (writer, token, child) in children {
        let output = child.wait_with_output().expect("wait for memd add");
        if output.status.success() {
            acknowledged.push(token);
        } else {
            let diagnostic = format!(
                "writer={writer} status={} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
            eprintln!("non-zero add exit: {diagnostic}");
            diagnostics.push(diagnostic);
        }
    }
    if require_all {
        assert!(
            diagnostics.is_empty(),
            "all lifecycle adds should succeed; diagnostics:\n{}",
            diagnostics.join("\n")
        );
    }
    diagnostics
}

fn spawn_lifecycle_round(
    data_dir: &Path,
    round: usize,
    nonce: u128,
) -> Vec<(usize, String, Child)> {
    (0..WRITERS)
        .map(|writer| {
            let token = format!("lifecycle_w{writer}_r{round}_{nonce}");
            let child = spawn_lifecycle_add(data_dir, writer, round, &token);
            (writer, token, child)
        })
        .collect()
}

fn assert_export_contains(data_dir: &Path, tenant: &str, tokens: &[String]) {
    let output = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(["export", "--tenant-id", tenant, "--format", "json"])
        .output()
        .expect("run memd export");
    assert!(
        output.status.success(),
        "export failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let missing = tokens
        .iter()
        .filter(|token| !stdout.contains(token.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "export missed acknowledged tokens:\n{}\nstdout:\n{}",
        missing.join("\n"),
        stdout
    );
}

fn unreadable_active_chunks(stdout: &str) -> Option<u64> {
    if let Ok(json) = serde_json::from_str::<Value>(stdout) {
        if let Some(count) = json
            .get("totals")
            .and_then(|totals| totals.get("unreadable_active_chunks"))
            .and_then(Value::as_u64)
        {
            return Some(count);
        }
    }

    ["unreadable_active_chunks:", "unreadable_active:"]
        .iter()
        .find_map(|needle| number_after(stdout, needle))
}

fn number_after(haystack: &str, needle: &str) -> Option<u64> {
    let start = haystack.find(needle)? + needle.len();
    let digits = haystack[start..]
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();

    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[test]
fn kill9_mid_storm_recovers_with_fresh_worker_and_no_data_loss() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, pid0) = start_worker(&data_dir);
    let nonce = nonce();
    let mut acknowledged = Vec::new();

    let round0 = spawn_lifecycle_round(&data_dir, 0, nonce);
    wait_lifecycle_round(round0, &mut acknowledged, true);

    let round1 = spawn_lifecycle_round(&data_dir, 1, nonce);
    std::thread::sleep(Duration::from_millis(150));
    sigkill(pid0);
    let diagnostics = wait_lifecycle_round(round1, &mut acknowledged, false);
    if !diagnostics.is_empty() {
        eprintln!(
            "round 1 tolerated non-zero exits after SIGKILL:\n{}",
            diagnostics.join("\n")
        );
    }
    assert!(
        wait_for_pid_exit(pid0, Duration::from_secs(10)),
        "worker pid {pid0} still running after SIGKILL"
    );

    let round2 = spawn_lifecycle_round(&data_dir, 2, nonce);
    wait_lifecycle_round(round2, &mut acknowledged, true);

    let final_token = format!("lifecycle_final_{nonce}");
    let final_add = add_command(
        &data_dir,
        "required",
        "storm",
        "storm",
        &format!("lifecycle final token {final_token}"),
    )
    .output()
    .expect("run final required add");
    assert!(
        final_add.status.success(),
        "final required add failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&final_add.stderr),
        String::from_utf8_lossy(&final_add.stdout)
    );
    acknowledged.push(final_token.clone());

    let pid1 = warm_status_pid(&data_dir);
    assert_ne!(pid1, pid0, "fresh worker reused killed pid {pid0}");
    assert!(
        pid_is_running(pid1),
        "fresh worker pid {pid1} is not running"
    );

    let search = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "search",
            "--warm",
            "required",
            "--tenant-id",
            "storm",
            "--query",
        ])
        .arg(&final_token)
        .output()
        .expect("run required search");
    assert!(
        search.status.success(),
        "search failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&search.stderr),
        String::from_utf8_lossy(&search.stdout)
    );
    let search_stdout = String::from_utf8_lossy(&search.stdout);
    assert!(
        search_stdout.contains(&final_token),
        "search did not contain {final_token}:\n{search_stdout}"
    );

    let stop = stop_worker(&data_dir);
    assert!(
        stop.status.success(),
        "warm stop failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&stop.stderr),
        String::from_utf8_lossy(&stop.stdout)
    );

    assert_export_contains(&data_dir, "storm", &acknowledged);

    let audit = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["audit", "--tenant-id", "storm", "--format", "json"])
        .output()
        .expect("run memd audit");
    assert!(
        audit.status.success(),
        "audit failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&audit.stderr),
        String::from_utf8_lossy(&audit.stdout)
    );
    let audit_stdout = String::from_utf8_lossy(&audit.stdout);
    let unreadable = unreadable_active_chunks(&audit_stdout).unwrap_or_else(|| {
        panic!("audit output did not report unreadable chunks:\n{audit_stdout}")
    });
    assert_eq!(unreadable, 0, "audit stdout:\n{audit_stdout}");

    drop(guard);
}

#[test]
fn maintenance_against_live_worker_fails_actionably_then_succeeds_after_stop() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, pid) = start_worker(&data_dir);

    let maintenance = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(&data_dir)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "300")
        .args(["maintenance"])
        .output()
        .expect("run memd maintenance");
    assert!(
        !maintenance.status.success(),
        "maintenance unexpectedly succeeded while worker {pid} was live"
    );
    let stderr = String::from_utf8_lossy(&maintenance.stderr);
    assert!(
        stderr.contains("writer lock held by another process"),
        "stderr:\n{stderr}"
    );
    assert!(stderr.contains("memd warm stop"), "stderr:\n{stderr}");

    let stop = stop_worker(&data_dir);
    assert!(
        stop.status.success(),
        "warm stop failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&stop.stderr),
        String::from_utf8_lossy(&stop.stdout)
    );

    let maintenance = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(&data_dir)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "300")
        .args(["maintenance"])
        .output()
        .expect("run memd maintenance after stop");
    assert!(
        maintenance.status.success(),
        "maintenance after stop failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&maintenance.stderr),
        String::from_utf8_lossy(&maintenance.stdout)
    );

    drop(guard);
}

#[test]
fn stale_socket_and_pid_file_do_not_block_single_client_respawn() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, pid0) = start_worker(&data_dir);

    sigkill(pid0);
    assert!(
        wait_for_pid_exit(pid0, Duration::from_secs(10)),
        "worker pid {pid0} still running after SIGKILL"
    );
    assert!(
        !warm_socket_files(&data_dir).is_empty(),
        "expected stale socket file under {:?}",
        data_dir.join("warm")
    );
    assert!(
        !warm_pid_files(&data_dir).is_empty(),
        "expected stale pid file under {:?}",
        data_dir.join("warm")
    );

    let token = format!("stale_respawn_{}", nonce());
    let add = add_command(
        &data_dir,
        "required",
        "storm",
        "storm",
        &format!("stale respawn token {token}"),
    )
    .output()
    .expect("run required add after SIGKILL");
    assert!(
        add.status.success(),
        "required add after SIGKILL failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&add.stderr),
        String::from_utf8_lossy(&add.stdout)
    );

    let pid1 = warm_pid_files(&data_dir)
        .into_iter()
        .find_map(|path| read_pid_file(&path))
        .expect("worker-written pid file");
    assert_ne!(pid1, pid0, "pid file still points at killed worker {pid0}");
    assert!(
        pid_is_running(pid1),
        "pid file worker {pid1} is not running"
    );

    let stop = stop_worker(&data_dir);
    assert!(
        stop.status.success(),
        "warm stop failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&stop.stderr),
        String::from_utf8_lossy(&stop.stdout)
    );

    drop(guard);
}
