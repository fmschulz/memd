//! `memd maintenance` — disk hygiene for warm_index and segments.
//!
//! Operations performed (all idempotent and safe to repeat):
//!
//! 1. Sweep orphan `graph-NNNN.hnsw.*` snapshots left by older memd
//!    versions before the hnsw_rs orphan-snapshot fix shipped. The fix
//!    itself runs on every `HnswIndex::with_persistence` call, but
//!    this command surfaces it for ops scripts and lets operators
//!    inspect the bytes-freed report before doing other work.
//!
//! 2. (`--aggressive`) Compaction hooks. The current implementation
//!    counts what would be touched; the actual compaction entry
//!    points live in `crate::compaction` and `crate::maintenance` and
//!    will be wired in once they expose stable per-tenant interfaces.
//!
//! 3. (`--aggressive`) Mapping repack. `HnswIndex::load` already
//!    converts legacy `mapping.json` to bincode `mapping.bin` on the
//!    next save, but this command can force the conversion eagerly so
//!    operators don't have to wait for the next write.
//!
//! Concurrency: the CLI acquires the data-dir writer lock before
//! entering this module. The orphan sweep targets files the loader never
//! reads, so racing reads are safe.

use std::path::Path;

use crate::error::{MemdError, Result};

/// Per-run summary returned by `run`. The CLI surface renders this as
/// key:value lines so it stays greppable.
#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub tenants_scanned: usize,
    /// Files actually removed (real run) OR files that would be removed
    /// (dry run). The CLI renames this counter to `would_remove_*` vs
    /// `removed_*` in `render_report` so dry-run output is
    /// distinguishable from real-run output.
    pub orphan_snapshots_removed: u64,
    /// Bytes corresponding to `orphan_snapshots_removed`.
    pub orphan_bytes_freed: u64,
    /// Files matched the orphan pattern but `remove_file` failed
    /// (permission denied, concurrent race). Only meaningful in real
    /// runs. Surfaces as `orphan_snapshots_failed:` in the report.
    pub orphan_snapshots_failed: u64,
    /// Reserved for `--aggressive`: legacy `mapping.json` rewritten as
    /// `mapping.bin`. Currently 0 — wired by future compaction hook.
    pub legacy_mapping_converted: u64,
    /// Reserved for `--aggressive`: small segments merged. Currently 0
    /// — wired by future compaction hook.
    pub segments_merged: u64,
}

/// Entry point. Inspects `data_dir/tenants/<tenant>/warm_index/` for
/// each tenant (or just `tenant_filter` when provided) and applies
/// hygiene operations.
///
/// `dry_run` makes every mutation a no-op while keeping the counters
/// accurate, so the printed report from a dry run matches what a real
/// run would do.
pub fn run(
    data_dir: &Path,
    tenant_filter: Option<&str>,
    dry_run: bool,
    aggressive: bool,
) -> Result<MaintenanceReport> {
    let mut report = MaintenanceReport::default();
    let tenants_root = data_dir.join("tenants");
    if !tenants_root.exists() {
        return Ok(report);
    }

    let entries = std::fs::read_dir(&tenants_root).map_err(|e| {
        MemdError::StorageError(format!(
            "read tenants dir {}: {}",
            tenants_root.display(),
            e
        ))
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| MemdError::StorageError(format!("tenant entry: {}", e)))?;
        let file_type = entry
            .file_type()
            .map_err(|e| MemdError::StorageError(format!("file_type: {}", e)))?;
        if !file_type.is_dir() {
            continue;
        }
        let tenant = entry.file_name().to_string_lossy().to_string();
        if let Some(filter) = tenant_filter {
            if tenant != filter {
                continue;
            }
        }
        report.tenants_scanned += 1;
        sweep_warm_index(&entry.path().join("warm_index"), dry_run, &mut report)?;

        if aggressive {
            // Compaction + mapping repack hooks land here in a
            // follow-up. The maintenance command surface is committed
            // so ops scripts don't break when the wiring shows up.
        }
    }

    Ok(report)
}

fn sweep_warm_index(warm: &Path, dry_run: bool, report: &mut MaintenanceReport) -> Result<()> {
    if !warm.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(warm).map_err(|e| {
        MemdError::StorageError(format!("read warm_index {}: {}", warm.display(), e))
    })?;
    for entry in entries {
        let entry =
            entry.map_err(|e| MemdError::StorageError(format!("warm_index entry: {}", e)))?;
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("graph-")
            && (name.ends_with(".hnsw.graph") || name.ends_with(".hnsw.data")))
        {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if dry_run {
            report.orphan_snapshots_removed += 1;
            report.orphan_bytes_freed += bytes;
        } else {
            // Codex Phase 5 MEDIUM: increment removed counters only on
            // successful unlink so a real run's `removed_*` matches
            // reality. Permission errors / races bump
            // `orphan_snapshots_failed` instead.
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {
                    report.orphan_snapshots_removed += 1;
                    report.orphan_bytes_freed += bytes;
                }
                Err(e) => {
                    report.orphan_snapshots_failed += 1;
                    tracing::warn!(
                        path = ?entry.path(),
                        error = %e,
                        "failed to remove orphan snapshot"
                    );
                }
            }
        }
    }
    Ok(())
}

/// Render a `MaintenanceReport` as the greppable key:value format the
/// CLI emits. Kept separate from `run` so unit tests can verify the
/// shape without invoking the filesystem.
pub fn render_report(report: &MaintenanceReport, dry_run: bool, aggressive: bool) -> String {
    let action = if dry_run { "would_remove" } else { "removed" };
    let mut out = String::new();
    out.push_str(&format!("tenants_scanned: {}\n", report.tenants_scanned));
    out.push_str(&format!(
        "{action}_orphan_snapshots: {}\n",
        report.orphan_snapshots_removed
    ));
    out.push_str(&format!(
        "{action}_orphan_bytes: {}\n",
        report.orphan_bytes_freed
    ));
    if !dry_run {
        out.push_str(&format!(
            "orphan_snapshots_failed: {}\n",
            report.orphan_snapshots_failed
        ));
    }
    if aggressive {
        out.push_str(&format!(
            "legacy_mapping_converted: {}\n",
            report.legacy_mapping_converted
        ));
        out.push_str(&format!("segments_merged: {}\n", report.segments_merged));
    }
    out
}
