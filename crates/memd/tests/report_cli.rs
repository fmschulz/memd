#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::types::{ChunkType, MemoryChunk, ProjectId, TenantId};
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
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "10000")
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
        .arg(text)
        .env("MEMD_WRITER_LOCK_TIMEOUT_MS", "10000");
    command
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

fn agent_context_command(data_dir: &Path, warm: &str, query: &str) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args([
            "agent-context",
            "--warm",
            warm,
            "--tenant-id",
            "t",
            "--project-id",
            "p",
            "--query",
        ])
        .arg(query)
        .args(["--k", "2"]);
    command
}

fn report_command(data_dir: &Path, warm: &str, format: &str, since: &str) -> Command {
    let mut command = Command::new(memd_bin());
    command.arg("--data-dir").arg(data_dir).args([
        "report", "--warm", warm, "--format", format, "--since", since,
    ]);
    if let Some(parent) = data_dir.parent() {
        command.current_dir(parent);
    }
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

fn add_success(data_dir: &Path, warm: &str, text: &str, tags: &str) -> Value {
    let output = assert_success(
        add_command(data_dir, warm, text, tags)
            .output()
            .expect("run memd add"),
        "add",
    );
    serde_json::from_slice(&output.stdout).expect("add JSON")
}

fn search_success(data_dir: &Path, warm: &str, query: &str) {
    assert_success(
        search_command(data_dir, warm, query)
            .output()
            .expect("run memd search"),
        "search",
    );
}

fn report_json(data_dir: &Path, warm: &str, strict: bool) -> Output {
    let mut command = report_command(data_dir, warm, "json", "24h");
    if strict {
        command.arg("--strict");
    }
    command.output().expect("run memd report")
}

fn has_warn_line(report: &Value, name: &str) -> bool {
    report
        .pointer("/self_diagnosis/lines")
        .and_then(Value::as_array)
        .is_some_and(|lines| {
            lines.iter().any(|line| {
                line.get("level").and_then(Value::as_str) == Some("warn")
                    && line.get("name").and_then(Value::as_str) == Some(name)
            })
        })
}

#[test]
fn end_to_end_json_matches_seeded_ground_truth() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (guard, _pid) = start_worker(&data_dir);
    let unique = nonce();
    let search_token = format!("report_hit_token_{unique}");
    let miss_token = format!("report_miss_token_{unique}");
    // Search on a non-empty store always returns vector top-k results, so a
    // guaranteed zero-hit query must run while the store is still empty.
    search_success(&data_dir, "required", &miss_token);

    let priority_text = format!(
        "Validation: priority report digest entry {search_token} records a concrete outcome.\nAgent action: Verify report JSON includes this priority digest entry."
    );
    let priority = add_success(
        &data_dir,
        "required",
        &priority_text,
        "kind:decision,priority:9",
    );
    let priority_chunk_id = priority
        .get("chunk_id")
        .and_then(Value::as_str)
        .expect("priority chunk id")
        .to_string();

    let progress_text = format!(
        "Validation: report ledger progress chunk {unique} confirms admitted add accounting."
    );
    add_success(&data_dir, "required", &progress_text, "kind:progress");

    let consolidated_text = format!(
        "Validation: consolidated report lesson {unique} is available.\nAgent action: Reuse the consolidated report lesson when checking diagnostics."
    );
    add_success(
        &data_dir,
        "required",
        &consolidated_text,
        "kind:consolidated",
    );

    let rejected_text = format!("starting to look at things {unique}");
    let rejected = add_command(&data_dir, "required", &rejected_text, "kind:progress")
        .output()
        .expect("run rejected add");
    assert!(
        !rejected.status.success(),
        "rejected add unexpectedly succeeded: stdout:\n{}",
        String::from_utf8_lossy(&rejected.stdout)
    );

    search_success(&data_dir, "required", &search_token);
    search_success(&data_dir, "required", &format!("{search_token} priority"));
    assert_success(
        agent_context_command(&data_dir, "required", &search_token)
            .output()
            .expect("run memd agent-context"),
        "agent-context",
    );

    let output = assert_success(report_json(&data_dir, "required", false), "report");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_slice(&output.stdout).expect("report JSON");

    assert_eq!(
        value
            .pointer("/growth/adds/admitted")
            .and_then(Value::as_u64),
        Some(3)
    );
    assert_eq!(
        value
            .pointer("/growth/adds/rejected/total")
            .and_then(Value::as_u64),
        Some(1)
    );
    let rejected_by_reason = value
        .pointer("/growth/adds/rejected/by_reason")
        .and_then(Value::as_object)
        .expect("rejected by_reason object");
    assert_eq!(rejected_by_reason.len(), 1, "{rejected_by_reason:?}");
    let (reason, count) = rejected_by_reason.iter().next().unwrap();
    assert!(reason.contains("low-signal"), "{reason}");
    assert_eq!(count.as_u64(), Some(1));
    assert_eq!(
        value
            .pointer("/growth/adds/downgraded")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        value
            .pointer("/retrieval_usefulness/searches")
            .and_then(Value::as_u64),
        Some(3)
    );
    assert_eq!(
        value
            .pointer("/retrieval_usefulness/zero_hits")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        value
            .pointer("/retrieval_usefulness/agent_context_calls")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        value
            .pointer("/retrieval_usefulness/distinct_queries")
            .and_then(Value::as_u64),
        Some(3)
    );
    assert_eq!(
        value
            .pointer("/growth/store_totals/active_chunks")
            .and_then(Value::as_u64),
        Some(3)
    );
    assert_eq!(
        value
            .pointer("/learning_digest/consolidated_in_window")
            .and_then(Value::as_u64),
        Some(1)
    );
    let digest_entries = value
        .pointer("/learning_digest/entries")
        .and_then(Value::as_array)
        .expect("digest entries");
    let priority_entry = digest_entries
        .iter()
        .find(|entry| {
            entry
                .get("chunk_id")
                .and_then(Value::as_str)
                .is_some_and(|chunk_id| chunk_id == priority_chunk_id)
        })
        .expect("priority digest entry");
    assert_eq!(
        priority_entry.get("agent_action").and_then(Value::as_str),
        // explicit_agent_action (shared with memory-md) trims trailing '.'/';'.
        Some("Verify report JSON includes this priority digest entry")
    );
    assert!(
        value
            .pointer("/self_diagnosis/warn_count")
            .and_then(Value::as_u64)
            .is_some(),
        "warn_count missing"
    );
    assert!(
        !value["retrieval_usefulness"]
            .to_string()
            .contains(&search_token),
        "raw query token leaked into retrieval section: {stdout}"
    );

    stop_worker(&data_dir);
    drop(guard);
}

#[test]
fn markdown_contains_all_sections() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let text = format!(
        "Validation: markdown report smoke {} confirms section rendering.",
        nonce()
    );
    add_success(&data_dir, "off", &text, "kind:progress");

    let output = assert_success(
        report_command(&data_dir, "off", "markdown", "24h")
            .output()
            .expect("run markdown report"),
        "markdown report",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for header in [
        "## Growth",
        "## Learning digest",
        "## Retrieval usefulness",
        "## Self-diagnosis",
    ] {
        assert!(stdout.contains(header), "missing {header}:\n{stdout}");
    }
    assert!(
        stdout.contains("- [ok]") || stdout.contains("- [warn]"),
        "missing diagnosis status:\n{stdout}"
    );
    assert!(
        stdout.contains("count-level retrieval only, not per-chunk serve ids"),
        "missing honest granularity line:\n{stdout}"
    );
    assert!(
        stdout.contains("- high_priority_in_window: `"),
        "missing high-priority learning digest line:\n{stdout}"
    );
}

#[test]
fn strict_exit_codes() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let (_guard, _pid) = start_worker(&data_dir);
    let unique = nonce();
    for i in 0..21 {
        search_success(
            &data_dir,
            "required",
            &format!("strict_zero_hit_{unique}_{i}"),
        );
    }

    let strict = report_json(&data_dir, "required", true);
    assert_eq!(
        strict.status.code(),
        Some(2),
        "strict stdout:\n{}",
        String::from_utf8_lossy(&strict.stdout)
    );
    let non_strict = report_json(&data_dir, "required", false);
    assert!(
        non_strict.status.success(),
        "non-strict failed: stderr:\n{}",
        String::from_utf8_lossy(&non_strict.stderr)
    );
    let strict_value: Value = serde_json::from_slice(&strict.stdout).expect("strict report JSON");
    let non_strict_value: Value =
        serde_json::from_slice(&non_strict.stdout).expect("non-strict report JSON");
    assert!(
        has_warn_line(&strict_value, "zero_hit_share"),
        "{}",
        String::from_utf8_lossy(&strict.stdout)
    );
    assert!(
        has_warn_line(&non_strict_value, "zero_hit_share"),
        "{}",
        String::from_utf8_lossy(&non_strict.stdout)
    );

    let direct = report_json(&data_dir, "off", true);
    assert_eq!(direct.status.code(), Some(2));

    let healthy_tmp = tempdir().unwrap();
    let healthy_data = healthy_tmp.path().join("memd_data");
    let (_healthy_guard, _pid) = start_worker(&healthy_data);
    let token = format!("strict_healthy_hit_{}", nonce());
    let text = format!("Validation: strict healthy chunk {token} is searchable.");
    add_success(&healthy_data, "required", &text, "kind:progress");
    search_success(&healthy_data, "required", &token);
    let healthy = report_json(&healthy_data, "required", true);
    assert!(
        healthy.status.success(),
        "healthy strict failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&healthy.stderr),
        String::from_utf8_lossy(&healthy.stdout)
    );
}

#[test]
fn since_parsing_rejects_garbage() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    for value in ["banana", "0d", "7w"] {
        let output = report_command(&data_dir, "off", "markdown", value)
            .output()
            .expect("run invalid report");
        assert!(!output.status.success(), "{value} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid --since"), "{stderr}");
        assert!(stderr.contains("<N>d"), "{stderr}");
        assert!(stderr.contains("<N>h"), "{stderr}");
    }

    for value in ["24h", "3d"] {
        let output = report_command(&data_dir, "off", "markdown", value)
            .output()
            .expect("run valid report");
        assert!(
            output.status.success(),
            "{value} failed: stderr:\n{}\nstdout:\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[tokio::test]
async fn perf_10k_chunk_store_under_bound() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let config = PersistentStoreConfig {
        data_dir: data_dir.clone(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    let tenant = TenantId::new("t").unwrap();
    let project = ProjectId::from("p");
    let unique = nonce();

    for batch_start in (0..10_000).step_by(500) {
        let mut batch = Vec::with_capacity(500);
        for i in batch_start..batch_start + 500 {
            let text = format!(
                "Validation: perf report chunk {unique} #{i} records deterministic synthetic content.\nAgent action: Verify perf report chunk {i} remains readable."
            );
            let mut chunk = MemoryChunk::new(tenant.clone(), text, ChunkType::Summary)
                .with_project(project.clone());
            if i < 250 {
                chunk = chunk.with_tags(vec![
                    "kind:consolidated".to_string(),
                    "priority:9".to_string(),
                ]);
            } else {
                chunk = chunk.with_tags(vec!["kind:progress".to_string()]);
            }
            batch.push(chunk);
        }
        store.add_batch(batch).await.unwrap();
    }
    drop(store);

    let start = Instant::now();
    let output = report_command(&data_dir, "off", "json", "30d")
        .output()
        .expect("run perf report");
    let elapsed = start.elapsed();
    println!("report perf 10k chunks: {} ms", elapsed.as_millis());
    assert!(
        output.status.success(),
        "perf report failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    // CI bound is intentionally generous; the real target is under 2 seconds.
    assert!(elapsed < Duration::from_secs(10), "elapsed={elapsed:?}");
    let value: Value = serde_json::from_slice(&output.stdout).expect("report JSON");
    assert_eq!(
        value
            .pointer("/growth/store_totals/active_chunks")
            .and_then(Value::as_u64),
        Some(10_000)
    );
}
