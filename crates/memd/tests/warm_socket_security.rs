#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::tempdir;

fn memd_bin() -> String {
    env!("CARGO_BIN_EXE_memd").to_string()
}

#[derive(Debug)]
struct WorkerGuard {
    data_dir: PathBuf,
    pid: u32,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let _ = Command::new(memd_bin())
            .arg("--data-dir")
            .arg(&self.data_dir)
            .args(["warm", "stop"])
            .output();
        if !wait_for_pid_exit(self.pid, Duration::from_secs(5)) {
            sigkill(self.pid);
        }
    }
}

fn unix_socket_bind_or_skip(test_name: &str) -> bool {
    let dir = tempdir().unwrap();
    let probe = dir.path().join("probe.sock");
    match UnixListener::bind(&probe) {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!(
                "skipping {test_name}: unix socket bind is not permitted in this environment"
            );
            false
        }
        Err(err) => panic!("bind unix socket probe for {test_name}: {err}"),
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
    let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
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

fn start_worker(data_dir: &Path) -> WorkerGuard {
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
    WorkerGuard {
        data_dir: data_dir.to_path_buf(),
        pid,
    }
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

fn wait_for_socket(data_dir: &Path, timeout: Duration) -> PathBuf {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(socket) = warm_socket_files(data_dir).into_iter().next() {
            return socket;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "warm socket did not appear under {:?}; pid files: {:?}",
        data_dir.join("warm"),
        warm_pid_files(data_dir)
    );
}

#[test]
fn warm_worker_socket_and_parent_are_private() {
    if !unix_socket_bind_or_skip("warm_worker_socket_and_parent_are_private") {
        return;
    }

    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let guard = start_worker(&data_dir);
    let socket = wait_for_socket(&data_dir, Duration::from_secs(10));

    let socket_mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(socket_mode, 0o600, "socket mode for {:?}", socket);

    let parent = socket.parent().expect("socket parent");
    let parent_mode = std::fs::metadata(parent).unwrap().permissions().mode() & 0o777;
    assert_eq!(parent_mode, 0o700, "socket parent mode for {:?}", parent);

    stop_worker(&data_dir);
    assert!(wait_for_pid_exit(guard.pid, Duration::from_secs(10)));
    drop(guard);
}
