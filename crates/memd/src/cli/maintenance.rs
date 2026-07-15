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
//! 2. (`--aggressive`) Force-merge the global sparse index into one segment.
//!
//! Concurrency: the CLI acquires the data-dir writer lock before
//! entering this module. The orphan sweep targets files the loader never
//! reads, so racing reads are safe.

use std::path::Path;

use crate::compaction::SegmentMerger;
use crate::error::{MemdError, Result};
use crate::index::Bm25Index;

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
    /// Sparse-index segments merged, or that would be merged in dry-run mode.
    pub segments_merged: u64,
    /// Sparse-index segment count before aggressive maintenance.
    pub sparse_segments_before: u64,
    /// Sparse-index segment count after, or predicted after, maintenance.
    pub sparse_segments_after: u64,
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
    }

    if aggressive {
        let (before, after) = merge_sparse_segments(data_dir, dry_run)?;
        report.sparse_segments_before = before;
        report.sparse_segments_after = after;
        report.segments_merged = before.saturating_sub(after);
    }

    Ok(report)
}

fn merge_sparse_segments(data_dir: &Path, dry_run: bool) -> Result<(u64, u64)> {
    let sparse_path = data_dir.join("sparse_index");
    if !sparse_path.exists() {
        return Ok((0, 0));
    }

    if dry_run {
        let Some(index) = Bm25Index::with_path_read_only(sparse_path)? else {
            return Ok((0, 0));
        };
        let before = index.segment_count()? as u64;
        let after = if index.total_docs()? == 0 {
            0
        } else {
            before.min(1)
        };
        return Ok((before, after));
    }

    let index = Bm25Index::with_path(Some(sparse_path))?;
    let result = SegmentMerger::new().merge(&index)?;
    Ok((result.segments_before as u64, result.segments_after as u64))
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
        out.push_str(&format!(
            "sparse_segments_before: {}\n",
            report.sparse_segments_before
        ));
        out.push_str(&format!(
            "sparse_segments_after: {}\n",
            report.sparse_segments_after
        ));
        out.push_str(&format!("segments_merged: {}\n", report.segments_merged));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SparseIndex;
    use crate::types::{ChunkId, TenantId};
    use tempfile::tempdir;

    #[test]
    fn aggressive_maintenance_force_merges_sparse_segments() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tenants/test")).unwrap();
        let sparse_path = dir.path().join("sparse_index");
        let tenant = TenantId::new("test").unwrap();
        {
            let index = Bm25Index::with_path(Some(sparse_path.clone())).unwrap();
            for i in 0..12 {
                index
                    .insert(
                        &tenant,
                        &ChunkId::new(),
                        &[format!("maintenance segment {i}")],
                    )
                    .unwrap();
                index.commit().unwrap();
            }
        }

        let preview = run(dir.path(), None, true, true).unwrap();
        assert!(preview.segments_merged > 0);
        assert_eq!(preview.sparse_segments_after, 1);
        let applied = run(dir.path(), None, false, true).unwrap();
        assert_eq!(applied.segments_merged, preview.segments_merged);
        assert_eq!(
            applied.sparse_segments_before,
            preview.sparse_segments_before
        );
        assert_eq!(applied.sparse_segments_after, 1);

        let index = Bm25Index::with_path_read_only(sparse_path)
            .unwrap()
            .unwrap();
        assert_eq!(index.segment_count().unwrap(), 1);
    }
}
