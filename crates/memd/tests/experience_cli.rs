#![cfg(unix)]

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TENANT: &str = "experience_cli_tenant";
const PROJECT: &str = "experience_cli_project";
const THREAD_ORIGIN: &str = "codex-thread-origin";
const THREAD_REPAIR: &str = "codex-thread-repair";

fn memd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_memd")
}

struct WorkerGuard {
    data_dir: PathBuf,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let _ = Command::new(memd_bin())
            .arg("--data-dir")
            .arg(&self.data_dir)
            .args(["--search-variant", "bm25-only", "warm", "stop"])
            .output();
        for pid in warm_pids(&self.data_dir) {
            if Path::new("/proc").join(pid.to_string()).exists() {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
        }
    }
}

fn warm_pids(data_dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("warm")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("memd.pid")).ok())
        .filter_map(|text| text.lines().next()?.trim().parse().ok())
        .collect()
}

fn write_project_scope(project_dir: &Path) {
    std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
    std::fs::write(
        project_dir.join(".memd/project_scope.json"),
        serde_json::to_vec(&json!({
            "tenant_id": TENANT,
            "project_id": PROJECT,
            "interface": "cli",
            "cli_command": "memd",
            "agent_context_output": ".memd/context.md",
            "project_dir": project_dir,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
}

fn base_command(data_dir: &Path, project_dir: &Path) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .current_dir(project_dir)
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--search-variant", "bm25-only"]);
    command
}

fn client_command(data_dir: &Path, project_dir: &Path, thread_id: &str) -> Command {
    let mut command = base_command(data_dir, project_dir);
    command
        .env("AIO_HARNESS", "codex")
        .env("AIO_MODEL", "simulation-model")
        .env("CODEX_SESSION_ID", format!("session-{thread_id}"))
        .env("CODEX_THREAD_ID", thread_id);
    command
}

fn experience_command(data_dir: &Path, project_dir: &Path, thread_id: &str, warm: &str) -> Command {
    let mut command = client_command(data_dir, project_dir, thread_id);
    command.args([
        "experience",
        "--tenant-id",
        TENANT,
        "--project-id",
        PROJECT,
        "--warm",
        warm,
    ]);
    command
}

fn successful_output(mut command: Command, label: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{label}: {error}"));
    assert!(
        output.status.success(),
        "{label} failed\nstderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

fn successful_json(command: Command, label: &str) -> (Value, String) {
    let output = successful_output(command, label);
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 CLI output");
    let value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("{label} returned invalid JSON: {error}\n{stdout}"));
    (value, stdout)
}

fn failed_output(mut command: Command, label: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{label}: {error}"));
    assert!(
        !output.status.success(),
        "{label} unexpectedly succeeded:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

fn start_worker(data_dir: &Path, project_dir: &Path) -> (WorkerGuard, u32) {
    let mut command = base_command(data_dir, project_dir);
    command
        .env("AIO_HARNESS", "codex")
        .env("CODEX_THREAD_ID", "worker-starter-thread")
        .args(["warm", "start"]);
    let (value, _) = successful_json(command, "start warm worker");
    let pid = value
        .get("pid")
        .and_then(Value::as_u64)
        .or_else(|| value.pointer("/result/pid").and_then(Value::as_u64))
        .and_then(|pid| u32::try_from(pid).ok())
        .or_else(|| warm_pids(data_dir).into_iter().next())
        .expect("warm worker PID");
    (
        WorkerGuard {
            data_dir: data_dir.to_path_buf(),
        },
        pid,
    )
}

fn worker_pid(data_dir: &Path, project_dir: &Path) -> u32 {
    let mut command = base_command(data_dir, project_dir);
    command.args(["warm", "status"]);
    let (value, _) = successful_json(command, "read warm worker status");
    value
        .get("pid")
        .and_then(Value::as_u64)
        .or_else(|| value.pointer("/result/pid").and_then(Value::as_u64))
        .and_then(|pid| u32::try_from(pid).ok())
        .or_else(|| warm_pids(data_dir).into_iter().next())
        .expect("running warm worker PID")
}

fn record_event(data_dir: &Path, project_dir: &Path, thread_id: &str, event: Value) -> Value {
    let mut command = experience_command(data_dir, project_dir, thread_id, "required");
    command
        .args(["record", "--json"])
        .arg(json!({"event": event}).to_string());
    successful_json(command, "record experience event").0
}

fn record_problem(
    data_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
    description: &str,
    symptom_terms: &[&str],
) -> Value {
    record_event(
        data_dir,
        project_dir,
        thread_id,
        json!({
            "kind": "problem",
            "description": description,
            "symptom_terms": symptom_terms,
        }),
    )
}

fn record_attempt(
    data_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
    problem_id: &str,
    previous_attempt_id: Option<&str>,
    action: &str,
) -> Value {
    record_event(
        data_dir,
        project_dir,
        thread_id,
        json!({
            "kind": "attempt",
            "problem_id": problem_id,
            "previous_attempt_id": previous_attempt_id,
            "action": action,
        }),
    )
}

// These fixture helpers mirror the explicit check CLI without hiding its arguments.
#[allow(clippy::too_many_arguments)]
fn run_check(
    data_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
    attempt_id: &str,
    evidence: &[&Path],
    timeout_seconds: u64,
    program: &Path,
    arguments: &[&Path],
) -> (Value, String) {
    successful_json(
        check_command(
            data_dir,
            project_dir,
            thread_id,
            PROJECT,
            attempt_id,
            evidence,
            timeout_seconds,
            program,
            arguments,
        ),
        "run experience check",
    )
}

// The builder mirrors the check CLI so each test controls every process input.
#[allow(clippy::too_many_arguments)]
fn check_command(
    data_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
    project_id: &str,
    attempt_id: &str,
    evidence: &[&Path],
    timeout_seconds: u64,
    program: &Path,
    arguments: &[&Path],
) -> Command {
    let mut command = client_command(data_dir, project_dir, thread_id);
    command.args([
        "experience",
        "--tenant-id",
        TENANT,
        "--project-id",
        project_id,
        "--warm",
        "required",
    ]);
    command.arg("check").arg(attempt_id);
    for path in evidence {
        command.arg("--evidence").arg(path);
    }
    command
        .arg("--timeout-seconds")
        .arg(timeout_seconds.to_string())
        .arg("--")
        .arg(program);
    for argument in arguments {
        command.arg(argument);
    }
    command
}

fn get_case(
    data_dir: &Path,
    project_dir: &Path,
    thread_id: &str,
    problem_id: &str,
    warm: &str,
) -> Value {
    let mut command = experience_command(data_dir, project_dir, thread_id, warm);
    command.args(["get", problem_id]);
    successful_json(command, "get experience case").0
}

fn find_lesson(
    data_dir: &Path,
    project_dir: &Path,
    symptom: &str,
    machine: &str,
    version: &str,
) -> Value {
    let mut command = experience_command(
        data_dir,
        project_dir,
        "codex-thread-later-session",
        "required",
    );
    command.arg("find").arg(symptom).args([
        "--machine",
        machine,
        "--tool",
        "repairer",
        "--version",
        version,
    ]);
    successful_json(command, "find experience lesson").0
}

fn artifact_id(value: &Value) -> &str {
    value["artifact_id"].as_str().expect("artifact_id")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn wait_until_process_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let proc_path = Path::new("/proc").join(pid.to_string());
    while Instant::now() < deadline {
        if !proc_path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !proc_path.exists()
}

fn wait_until_file_exists(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    path.exists()
}

#[test]
fn repair_recall_privacy_timeout_and_transfer_work_through_cli() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().join("project");
    let imported_project_dir = temp.path().join("imported-project");
    let data_dir = temp.path().join("primary-data");
    let imported_data_dir = temp.path().join("imported-data");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&imported_project_dir).unwrap();
    write_project_scope(&project_dir);
    write_project_scope(&imported_project_dir);

    let checker = temp.path().join("assert_repaired");
    write_executable(
        &checker,
        "#!/bin/sh\nsecret=$(cat \"$2\")\nprintf '%s' \"$secret\"\ngrep -qx 'repaired' \"$1\"\n",
    );
    let mutator = temp.path().join("mutate_source");
    write_executable(
        &mutator,
        "#!/bin/sh\nprintf 'changed-during-check\\n' >> \"$1\"\n",
    );
    let sleeper = temp.path().join("spawn_descendant");
    write_executable(
        &sleeper,
        "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' \"$!\" > \"$1\"\nwait\n",
    );

    let target = project_dir.join("repair-target.txt");
    let secret_file = project_dir.join("check-input.txt");
    let synthetic_secret = "synthetic-secret-output-must-not-persist";
    std::fs::write(&target, "broken\n").unwrap();
    std::fs::write(&secret_file, synthetic_secret).unwrap();

    let (_worker, initial_worker_pid) = start_worker(&data_dir, &project_dir);

    let problem = record_problem(
        &data_dir,
        &project_dir,
        THREAD_ORIGIN,
        "Repair checksum mismatch in generated output",
        &["checksum", "mismatch", "generated", "output"],
    );
    let problem_id = artifact_id(&problem).to_string();
    assert_eq!(
        problem.pointer("/provenance/execution/native_thread_id"),
        Some(&Value::String(THREAD_ORIGIN.to_string()))
    );

    let first_attempt = record_attempt(
        &data_dir,
        &project_dir,
        THREAD_ORIGIN,
        &problem_id,
        None,
        "Regenerate without fixing the source input",
    );
    let first_attempt_id = artifact_id(&first_attempt).to_string();
    let (failed_check, failed_check_stdout) = run_check(
        &data_dir,
        &project_dir,
        THREAD_ORIGIN,
        &first_attempt_id,
        &[&target],
        5,
        &checker,
        &[&target, &secret_file],
    );
    assert_eq!(failed_check["status"], "failed");
    assert_eq!(
        failed_check.pointer("/experience/stdout_sha256"),
        Some(&Value::String(sha256(synthetic_secret.as_bytes())))
    );
    assert!(!failed_check_stdout.contains(synthetic_secret));

    let second_attempt = record_attempt(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        &problem_id,
        Some(&first_attempt_id),
        "Repair the source file before regenerating",
    );
    let second_attempt_id = artifact_id(&second_attempt).to_string();
    assert_eq!(
        second_attempt.pointer("/provenance/execution/native_thread_id"),
        Some(&Value::String(THREAD_REPAIR.to_string()))
    );
    std::fs::write(&target, "repaired\n").unwrap();
    let (passing_check, passing_check_stdout) = run_check(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        &second_attempt_id,
        &[&target],
        5,
        &checker,
        &[&target, &secret_file],
    );
    assert_eq!(passing_check["status"], "passed");
    assert_eq!(
        passing_check.pointer("/experience/stdout_sha256"),
        Some(&Value::String(sha256(synthetic_secret.as_bytes())))
    );
    assert!(!passing_check_stdout.contains(synthetic_secret));
    let passing_check_id = artifact_id(&passing_check).to_string();

    let independent_assertion = Command::new(&checker)
        .arg(&target)
        .arg(&secret_file)
        .output()
        .expect("run independent repaired-file assertion");
    assert!(independent_assertion.status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "repaired\n");

    let lesson = record_event(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        json!({
            "kind": "lesson",
            "problem_id": problem_id,
            "supporting_check_id": passing_check_id,
            "guidance": "Repair the source input before regenerating checksum output.",
            "symptom_terms": ["checksum", "mismatch", "generated", "output"],
            "applicability": {
                "machine": "sim-machine",
                "tool": "repairer",
                "version": "1.0"
            },
            "visibility": "project"
        }),
    );
    let lesson_id = artifact_id(&lesson).to_string();

    let matched = find_lesson(
        &data_dir,
        &project_dir,
        "generated checksum mismatch",
        "sim-machine",
        "1.0",
    );
    assert_eq!(matched["status"], "matches");
    assert_eq!(matched["lessons"][0]["artifact"]["artifact_id"], lesson_id);
    for abstention in [
        find_lesson(
            &data_dir,
            &project_dir,
            "generated checksum mismatch",
            "wrong-machine",
            "1.0",
        ),
        find_lesson(
            &data_dir,
            &project_dir,
            "generated checksum mismatch",
            "sim-machine",
            "2.0",
        ),
        find_lesson(
            &data_dir,
            &project_dir,
            "unrelated volcanic banana",
            "sim-machine",
            "1.0",
        ),
    ] {
        assert_eq!(abstention["status"], "abstain");
        assert_eq!(abstention["reason"]["reason"], "no_match");
    }

    let case = get_case(
        &data_dir,
        &project_dir,
        "codex-thread-later-session",
        &problem_id,
        "required",
    );
    let case_json = serde_json::to_string(&case).unwrap();
    assert!(!case_json.contains(synthetic_secret));
    let linked_attempt = case["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["artifact_id"] == second_attempt_id)
        .unwrap();
    assert_eq!(
        linked_attempt["experience"]["previous_attempt_id"],
        first_attempt_id
    );

    let changed_source = project_dir.join("changed-source.txt");
    std::fs::write(&changed_source, "stable\n").unwrap();
    let changed_problem = record_problem(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        "Reject lessons when source changes during verification",
        &["source", "changed", "verification"],
    );
    let changed_problem_id = artifact_id(&changed_problem).to_string();
    let changed_attempt = record_attempt(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        &changed_problem_id,
        None,
        "Run a mutating verifier",
    );
    let changed_attempt_id = artifact_id(&changed_attempt).to_string();
    let (changed_check, _) = run_check(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        &changed_attempt_id,
        &[&changed_source],
        5,
        &mutator,
        &[&changed_source],
    );
    assert_eq!(changed_check["status"], "source_unverified");
    assert_ne!(
        changed_check.pointer("/experience/source_before/sha256"),
        changed_check.pointer("/experience/source_after/sha256")
    );
    let mut unsupported_lesson =
        experience_command(&data_dir, &project_dir, THREAD_REPAIR, "required");
    unsupported_lesson.args(["record", "--json"]).arg(
        json!({
            "event": {
                "kind": "lesson",
                "problem_id": changed_problem_id,
                "supporting_check_id": artifact_id(&changed_check),
                "guidance": "Trust the mutating check.",
                "symptom_terms": ["source", "changed", "verification"]
            }
        })
        .to_string(),
    );
    let unsupported = failed_output(unsupported_lesson, "record unsupported lesson");
    assert!(String::from_utf8_lossy(&unsupported.stderr)
        .contains("supporting check must have exit_code 0"));

    let timeout_problem = record_problem(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        "Bound verifier descendants on timeout",
        &["timeout", "descendant", "verifier"],
    );
    let timeout_problem_id = artifact_id(&timeout_problem).to_string();
    let timeout_attempt = record_attempt(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        &timeout_problem_id,
        None,
        "Run a verifier that leaves a child process",
    );
    let timeout_pid_file = project_dir.join("timeout-child.pid");
    let timeout_started = Instant::now();
    let (timeout_check, _) = run_check(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        artifact_id(&timeout_attempt),
        &[&target],
        1,
        &sleeper,
        &[&timeout_pid_file],
    );
    assert!(timeout_started.elapsed() < Duration::from_secs(10));
    assert_eq!(timeout_check["status"], "timed_out");
    assert_eq!(timeout_check["experience"]["timed_out"], true);
    let descendant_pid = std::fs::read_to_string(&timeout_pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(wait_until_process_gone(
        descendant_pid,
        Duration::from_secs(3)
    ));

    let mut search_origin = client_command(&data_dir, &project_dir, THREAD_ORIGIN);
    search_origin.args([
        "search",
        "--warm",
        "required",
        "--tenant-id",
        TENANT,
        "--project-id",
        PROJECT,
        "--query",
        "checksum mismatch repair",
        "--k",
        "2",
        "--compact",
    ]);
    let (search_origin, _) = successful_json(search_origin, "search as origin thread");
    assert!(!search_origin["results"].as_array().unwrap().is_empty());
    let origin_episode = search_origin["retrieval_episode_id"].as_str().unwrap();

    let mut search_repair = client_command(&data_dir, &project_dir, THREAD_REPAIR);
    search_repair.args([
        "search",
        "--warm",
        "required",
        "--tenant-id",
        TENANT,
        "--project-id",
        PROJECT,
        "--query",
        "generated output source repair",
        "--k",
        "2",
        "--compact",
    ]);
    let (search_repair, _) = successful_json(search_repair, "search as repair thread");
    assert!(!search_repair["results"].as_array().unwrap().is_empty());
    let repair_episode = search_repair["retrieval_episode_id"].as_str().unwrap();

    let connection = Connection::open(data_dir.join("metadata.db")).unwrap();
    for (episode_id, expected_thread) in [
        (origin_episode, THREAD_ORIGIN),
        (repair_episode, THREAD_REPAIR),
    ] {
        let stored_thread: String = connection
            .query_row(
                "SELECT thread_id FROM retrieval_episodes WHERE episode_id = ?1",
                [episode_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_thread, expected_thread);
        assert_ne!(stored_thread, "worker-starter-thread");
    }
    drop(connection);
    assert_eq!(worker_pid(&data_dir, &project_dir), initial_worker_pid);

    let bundle_path = temp.path().join("experience-bundle.json");
    let mut export = experience_command(&data_dir, &project_dir, "codex-thread-export", "required");
    export
        .arg("--output")
        .arg(&bundle_path)
        .arg("export")
        .args([&problem_id, &changed_problem_id, &timeout_problem_id]);
    successful_output(export, "export experience bundle");
    let bundle_text = std::fs::read_to_string(&bundle_path).unwrap();
    assert!(!bundle_text.contains(synthetic_secret));
    let bundle: Value = serde_json::from_str(&bundle_text).unwrap();
    let artifact_count = bundle["artifacts"].as_array().unwrap().len();
    assert_eq!(artifact_count, 12);

    let mut import = experience_command(
        &imported_data_dir,
        &imported_project_dir,
        "codex-thread-import",
        "off",
    );
    import.arg("import").arg(&bundle_path);
    let (first_import, _) = successful_json(import, "import experience bundle");
    assert_eq!(
        first_import["inserted_artifact_ids"]
            .as_array()
            .unwrap()
            .len(),
        artifact_count
    );
    assert!(first_import["replayed_artifact_ids"]
        .as_array()
        .unwrap()
        .is_empty());

    let mut replay = experience_command(
        &imported_data_dir,
        &imported_project_dir,
        "codex-thread-import",
        "off",
    );
    replay.arg("import").arg(&bundle_path);
    let (replay, _) = successful_json(replay, "replay experience bundle");
    assert!(replay["inserted_artifact_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        replay["replayed_artifact_ids"].as_array().unwrap().len(),
        artifact_count
    );

    let imported_case = get_case(
        &imported_data_dir,
        &imported_project_dir,
        "codex-thread-import",
        &problem_id,
        "off",
    );
    assert_eq!(imported_case, case);

    println!(
        "EXPERIENCE_SIMULATION_RESULT={}",
        json!({
            "scenario": "repair_recall_privacy_timeout_transfer",
            "problem_count": 3,
            "attempt_count": 4,
            "check_count": 4,
            "supported_lesson_count": 1,
            "recall_match_count": 1,
            "recall_abstention_count": 3,
            "native_thread_count": 2,
            "timeout_seconds": 1,
            "exported_artifact_count": artifact_count,
            "imported_artifact_count": first_import["inserted_artifact_ids"].as_array().unwrap().len(),
            "replayed_artifact_count": replay["replayed_artifact_ids"].as_array().unwrap().len(),
        })
    );
}

#[test]
fn interrupted_and_invalid_checks_stop_before_leaking_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let project_dir = temp.path().join("project");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_project_scope(&project_dir);

    let self_signal = temp.path().join("self_signal");
    write_executable(&self_signal, "#!/bin/sh\nkill -TERM $$\n");
    let marker_writer = temp.path().join("write_marker");
    write_executable(&marker_writer, "#!/bin/sh\nprintf 'ran\\n' > \"$1\"\n");
    let descendant_spawner = temp.path().join("interrupt_descendant");
    write_executable(
        &descendant_spawner,
        "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' \"$!\" > \"$1\"\nwait\n",
    );
    let evidence = project_dir.join("evidence.txt");
    std::fs::write(&evidence, "stable\n").unwrap();

    let (_worker, _) = start_worker(&data_dir, &project_dir);
    let problem = record_problem(
        &data_dir,
        &project_dir,
        THREAD_ORIGIN,
        "Keep explicit checks bounded and scoped",
        &["check", "process", "scope"],
    );
    let problem_id = artifact_id(&problem).to_string();
    let first_attempt = record_attempt(
        &data_dir,
        &project_dir,
        THREAD_ORIGIN,
        &problem_id,
        None,
        "Run a checker that terminates by signal",
    );
    let first_attempt_id = artifact_id(&first_attempt).to_string();
    let (signaled_check, _) = run_check(
        &data_dir,
        &project_dir,
        THREAD_ORIGIN,
        &first_attempt_id,
        &[&evidence],
        5,
        &self_signal,
        &[],
    );
    assert_eq!(signaled_check["status"], "failed");
    assert_eq!(signaled_check["experience"]["timed_out"], false);
    assert!(signaled_check["experience"]["exit_code"].is_null());

    let second_attempt = record_attempt(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        &problem_id,
        Some(&first_attempt_id),
        "Run a bounded checker",
    );
    let second_attempt_id = artifact_id(&second_attempt).to_string();

    let stale_marker = project_dir.join("stale-check-ran");
    let stale = check_command(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        PROJECT,
        &first_attempt_id,
        &[&evidence],
        5,
        &marker_writer,
        &[&stale_marker],
    );
    failed_output(stale, "reject stale check before execution");
    assert!(!stale_marker.exists());

    let wrong_scope_marker = project_dir.join("wrong-scope-check-ran");
    let wrong_scope = check_command(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        "different-project",
        &second_attempt_id,
        &[&evidence],
        5,
        &marker_writer,
        &[&wrong_scope_marker],
    );
    failed_output(wrong_scope, "reject wrong-scope check before execution");
    assert!(!wrong_scope_marker.exists());

    let descendant_pid_file = project_dir.join("interrupt-child.pid");
    let mut interrupted = check_command(
        &data_dir,
        &project_dir,
        THREAD_REPAIR,
        PROJECT,
        &second_attempt_id,
        &[&evidence],
        5,
        &descendant_spawner,
        &[&descendant_pid_file],
    );
    interrupted.stdout(Stdio::piped()).stderr(Stdio::piped());
    let client = interrupted
        .spawn()
        .expect("spawn interruptible memd client");
    assert!(wait_until_file_exists(
        &descendant_pid_file,
        Duration::from_secs(5)
    ));
    let descendant_pid = std::fs::read_to_string(&descendant_pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    // SAFETY: `client.id()` is the positive PID of the child started above.
    let signal_result = unsafe { libc::kill(client.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(signal_result, 0);
    let output = client.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("check interrupted before a complete receipt was available"));
    assert!(wait_until_process_gone(
        descendant_pid,
        Duration::from_secs(3)
    ));
}
