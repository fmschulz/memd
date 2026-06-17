//! Concurrent writer storm regression test.
//!
//! Expected green: concurrent `memd add` processes share one data dir,
//! serialize through the writer flock, and leave every acknowledged
//! payload readable.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

#[derive(Debug)]
struct AcknowledgedWrite {
    writer: usize,
    round: usize,
    token: String,
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

fn wait_for_pid(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pid_is_running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    pid_is_running(pid)
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
    assert!(wait_for_pid(pid, Duration::from_secs(10)));
    (
        WorkerGuard {
            data_dir: data_dir.to_path_buf(),
        },
        pid,
    )
}

#[test]
fn concurrent_writer_storm_keeps_every_acknowledged_chunk_readable() {
    run_storm(false);
}

#[test]
fn concurrent_writer_storm_routes_through_live_worker() {
    run_storm(true);
}

fn run_storm(route_via_worker: bool) {
    const WRITERS: usize = 8;
    const ROUNDS: usize = 3;

    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let worker = if route_via_worker {
        Some(start_worker(&data_dir))
    } else {
        None
    };
    let worker_pid = worker.as_ref().map(|(_, pid)| *pid);

    let mut acknowledged = Vec::new();
    let mut nonzero_add_stderr = Vec::new();

    for round in 0..ROUNDS {
        let mut children = Vec::with_capacity(WRITERS);

        for writer in 0..WRITERS {
            let token = format!("tokw{writer}r{round}n{nonce}");
            let text = format!("storm writer {writer} round {round} token {token}");
            let warm_mode = if route_via_worker { "required" } else { "off" };
            let lock_timeout = if route_via_worker { "1" } else { "120000" };
            let child = Command::new(memd_bin())
                .arg("--data-dir")
                .arg(&data_dir)
                .env("MEMD_WRITER_LOCK_TIMEOUT_MS", lock_timeout)
                .args([
                    "add",
                    "--warm",
                    warm_mode,
                    "--tenant-id",
                    "storm",
                    "--project-id",
                    "storm",
                    "--chunk-type",
                    "summary",
                    "--tags",
                    "kind:note",
                    "--text",
                ])
                .arg(text)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn memd add");

            children.push((writer, token, child));
        }

        for (writer, token, child) in children {
            let output = child.wait_with_output().expect("wait for memd add");
            if output.status.success() {
                acknowledged.push(AcknowledgedWrite {
                    writer,
                    round,
                    token,
                });
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let diagnostic = format!(
                    "writer={writer} round={round} status={} stderr={stderr}",
                    output.status
                );
                eprintln!("non-zero add exit: {diagnostic}");
                nonzero_add_stderr.push(diagnostic);
            }
        }
    }

    if acknowledged.is_empty() {
        panic!(
            "no add command was acknowledged; non-zero stderr:\n{}",
            nonzero_add_stderr.join("\n")
        );
    }

    assert_eq!(
        acknowledged.len(),
        WRITERS * ROUNDS,
        "every writer round should be acknowledged after flock serialization; non-zero add exits: {}\nnon-zero add stderr:\n{}",
        nonzero_add_stderr.len(),
        nonzero_add_stderr.join("\n")
    );

    if let Some(pid) = worker_pid {
        assert!(pid_is_running(pid), "worker pid {pid} stopped during storm");
        assert_eq!(warm_pids(&data_dir).into_iter().next(), Some(pid));
    }

    let export = Command::new(memd_bin())
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["export", "--tenant-id", "storm", "--format", "json"])
        .output()
        .expect("run memd export");

    assert!(
        export.status.success(),
        "export failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&export.stderr),
        String::from_utf8_lossy(&export.stdout)
    );

    let export_stdout = String::from_utf8_lossy(&export.stdout).to_string();
    let (exported_chunk_count, exported_texts) = match serde_json::from_str::<Value>(&export_stdout)
    {
        Ok(Value::Array(chunks)) => {
            let exported_chunk_count = chunks.len();
            let texts = chunks
                .into_iter()
                .filter_map(|chunk| {
                    chunk
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>();
            (exported_chunk_count, Some(texts))
        }
        Ok(_) => (0, Some(Vec::new())),
        Err(_) => (0, None),
    };

    let mut violations = Vec::new();
    for write in &acknowledged {
        let found = match &exported_texts {
            Some(texts) => texts.iter().any(|text| text.contains(&write.token)),
            None => export_stdout.contains(&write.token),
        };

        if !found {
            violations.push(format!(
                "lost acknowledged write: writer={} round={} token={}",
                write.writer, write.round, write.token
            ));
        }
    }

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
        panic!("audit output did not report unreadable_active_chunks:\n{audit_stdout}")
    });
    if unreadable > 0 {
        violations.push(format!(
            "audit unreadable_active_chunks = {unreadable} (want 0)"
        ));
    }

    assert!(
        violations.is_empty(),
        "concurrent writer storm corrupted acknowledged writes\nviolations:\n{}\nacknowledged: {} / {}\nexported chunk count: {}\nnon-zero add exits: {}\nnon-zero add stderr:\n{}",
        violations.join("\n"),
        acknowledged.len(),
        WRITERS * ROUNDS,
        exported_chunk_count,
        nonzero_add_stderr.len(),
        nonzero_add_stderr.join("\n")
    );
}
