#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use tempfile::tempdir;

fn memd_bin() -> String {
    env!("CARGO_BIN_EXE_memd").to_string()
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

fn add_command(data_dir: &Path, text: &str, tags: &str) -> Command {
    let mut command = Command::new(memd_bin());
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args([
            "add",
            "--warm",
            "off",
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

fn assert_success(output: Output, label: &str) -> Output {
    assert!(
        output.status.success(),
        "{label} failed: stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

fn active_chunks(data_dir: &Path) -> usize {
    let conn = Connection::open(data_dir.join("metadata.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.query_row(
        "SELECT COUNT(*)
         FROM chunks
         WHERE tenant_id = 't'
           AND status != 'deleted'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .unwrap() as usize
}

fn add_counts(data_dir: &Path) -> (usize, usize) {
    let conn = Connection::open(data_dir.join("metadata.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    let admitted = conn
        .query_row(
            "SELECT COUNT(*)
             FROM usage_events
             WHERE op = 'add'
               AND outcome = 'admitted'
               AND tenant = 't'
               AND project = 'p'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize;
    let rejected = conn
        .query_row(
            "SELECT COUNT(*)
             FROM usage_events
             WHERE op = 'add'
               AND outcome LIKE 'rejected:%'
               AND tenant = 't'
               AND project = 'p'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize;
    (admitted, rejected)
}

fn seed_search_events(data_dir: &Path) {
    let conn = Connection::open(data_dir.join("metadata.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    let ts = now_ms();
    for idx in 0..4 {
        conn.execute(
            "INSERT INTO usage_events (
                ts_unix_ms, op, tenant, project, outcome, chunk_count, bytes, detail
             )
             VALUES (?1, 'search', 't', 'p', 'hits:3', 3, NULL, ?2)",
            rusqlite::params![ts, format!(r#"{{"q_hash":"health-hit-{idx}","k":3}}"#)],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO usage_events (
            ts_unix_ms, op, tenant, project, outcome, chunk_count, bytes, detail
         )
         VALUES (?1, 'search', 't', 'p', 'zero_hits', 0, NULL, ?2)",
        rusqlite::params![ts, r#"{"q_hash":"health-miss","k":3}"#],
    )
    .unwrap();
}

fn run_memory_md(data_dir: &Path, project_dir: &Path, tenant: &str, project: &str) -> Output {
    Command::new(memd_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args([
            "memory-md",
            "--tenant-id",
            tenant,
            "--project-id",
            project,
            "--project-dir",
        ])
        .arg(project_dir)
        .args(["--output", "memory.md"])
        .output()
        .expect("run memd memory-md")
}

#[test]
fn health_header_reflects_ledger_and_chunks() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let unique = nonce();

    let high_a = format!(
        "Validation: memory health priority lesson alpha {unique} records durable session-start evidence with enough detail.\n\
         Agent action: Verify the memory health header counts alpha as a high-priority lesson."
    );
    let high_b = format!(
        "Decision: memory health priority lesson beta {unique} keeps explicit priority visible for future startup checks.\n\
         Agent action: Reuse beta when confirming priority-tagged chunks remain counted."
    );
    let plain = format!(
        "Validation: memory health plain lesson gamma {unique} confirms active chunk accounting without a priority tag.\n\
         Agent action: Use gamma only as a non-priority active chunk control."
    );

    assert_success(
        run_add(add_command(&data_dir, &high_a, "kind:finish,priority:9")),
        "add high_a",
    );
    assert_success(
        run_add(add_command(&data_dir, &high_b, "kind:decision,priority:9")),
        "add high_b",
    );
    assert_success(
        run_add(add_command(&data_dir, &plain, "kind:finish")),
        "add plain",
    );
    seed_search_events(&data_dir);

    let active = active_chunks(&data_dir);
    let (admitted, rejected) = add_counts(&data_dir);
    assert_eq!(active, 3);
    assert_eq!(admitted, 3);
    assert_eq!(rejected, 0);

    assert_success(
        run_memory_md(&data_dir, &project_dir, "t", "p"),
        "memory-md",
    );
    let content = std::fs::read_to_string(project_dir.join("memory.md")).unwrap();

    assert_eq!(content.matches("## Memory health").count(), 1);
    let health_pos = content.find("## Memory health").expect("health header");
    let scope_pos = content.find("## Scope").expect("scope header");
    assert!(
        health_pos < scope_pos,
        "health header must appear before scope:\n{content}"
    );
    assert!(content.contains(&format!(
        "- chunks: {active} active (+{admitted} added, {rejected} rejected, 7d)"
    )));
    assert!(content.contains("- retrieval: 5 searches, 80% hit rate (7d)"));
    assert!(content.contains("- learned: 2 high-priority + 0 consolidated lessons (7d)"));
    assert!(
        !content.lines().any(|line| line.starts_with("- [warn]")),
        "unexpected warning line:\n{content}"
    );
}

#[test]
fn fresh_store_renders_zero_state_header() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    assert_success(
        run_memory_md(&data_dir, &project_dir, "fresh_t", "fresh_p"),
        "memory-md",
    );
    let content = std::fs::read_to_string(project_dir.join("memory.md")).unwrap();

    assert_eq!(content.matches("## Memory health").count(), 1);
    assert!(content.contains("- chunks: 0 active (+0 added, 0 rejected, 7d)"));
    assert!(content.contains("- retrieval: 0 searches (7d)"));
    assert!(content.contains("- learned: 0 high-priority + 0 consolidated lessons (7d)"));
    assert!(
        !content.lines().any(|line| line.starts_with("- [warn]")),
        "unexpected warning line:\n{content}"
    );
}
