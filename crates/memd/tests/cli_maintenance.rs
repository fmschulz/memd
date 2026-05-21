//! End-to-end test for `memd maintenance --aggressive --dry-run`.
//!
//! Drives the actual built `memd` binary against a temp data dir
//! pre-seeded with the kinds of files Phase 1 / 4 will leave behind
//! (orphan HNSW snapshots, legacy mapping.json files).

use std::process::Command;
use tempfile::tempdir;

fn memd_bin() -> String {
    env!("CARGO_BIN_EXE_memd").to_string()
}

fn seed_orphans(warm: &std::path::Path) {
    std::fs::create_dir_all(warm).unwrap();
    for n in [42u32, 7] {
        for ext in ["hnsw.graph", "hnsw.data"] {
            std::fs::write(warm.join(format!("graph-{n}.{ext}")), b"orphan").unwrap();
        }
    }
}

#[test]
fn maintenance_dry_run_reports_orphans_without_deleting() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let warm = data_dir.join("tenants").join("alpha").join("warm_index");
    seed_orphans(&warm);

    let output = Command::new(memd_bin())
        .args(["maintenance"])
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["--dry-run", "--aggressive"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("would_remove_orphan_snapshots: 4"),
        "unexpected stdout:\n{stdout}"
    );

    // Dry-run must NOT delete.
    assert!(warm.join("graph-42.hnsw.graph").exists());
    assert!(warm.join("graph-7.hnsw.data").exists());
}

#[test]
fn maintenance_aggressive_actually_removes_orphans() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let warm = data_dir.join("tenants").join("alpha").join("warm_index");
    seed_orphans(&warm);

    let output = Command::new(memd_bin())
        .args(["maintenance"])
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["--aggressive"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("removed_orphan_snapshots: 4"),
        "unexpected stdout:\n{stdout}"
    );

    assert!(!warm.join("graph-42.hnsw.graph").exists());
    assert!(!warm.join("graph-7.hnsw.data").exists());
}

#[test]
fn maintenance_respects_tenant_filter() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    seed_orphans(&data_dir.join("tenants").join("alpha").join("warm_index"));
    seed_orphans(&data_dir.join("tenants").join("beta").join("warm_index"));

    let output = Command::new(memd_bin())
        .args(["maintenance"])
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["--aggressive", "--tenant-id", "alpha"])
        .output()
        .unwrap();

    assert!(output.status.success());

    // alpha cleaned, beta untouched.
    assert!(!data_dir
        .join("tenants/alpha/warm_index/graph-42.hnsw.graph")
        .exists());
    assert!(data_dir
        .join("tenants/beta/warm_index/graph-42.hnsw.graph")
        .exists());
}

#[test]
fn maintenance_inherits_top_level_data_dir() {
    // Codex Phase 5 HIGH: `memd --data-dir /x maintenance` (no
    // subcommand --data-dir) must scan /x, not $HOME/.memd/data.
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("memd_data");
    let warm = data_dir.join("tenants").join("alpha").join("warm_index");
    seed_orphans(&warm);

    let output = Command::new(memd_bin())
        .args(["--data-dir"])
        .arg(&data_dir)
        .args(["maintenance", "--aggressive"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("removed_orphan_snapshots: 4"),
        "top-level --data-dir must be inherited; got:\n{stdout}"
    );
    assert!(!warm.join("graph-42.hnsw.graph").exists());
}

#[test]
fn maintenance_handles_missing_data_dir() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("nonexistent");

    let output = Command::new(memd_bin())
        .args(["maintenance"])
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["--aggressive"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "missing data dir should be a no-op, not an error; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tenants_scanned: 0"),
        "unexpected stdout:\n{stdout}"
    );
}
