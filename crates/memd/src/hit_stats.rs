//! Retrieval hit counting (Phase 3).
//!
//! Every CLI search appends one JSONL line per returned chunk to
//! `.memd/data/hit_counts.jsonl`. [`aggregate_hits`] folds that log
//! into per-chunk [`HitStats`] within a recency window, cached to
//! `.memd/data/hit_counts.summary.json` with a TTL so the read path
//! stays cheap. `cli/memory_md.rs` feeds the aggregate into
//! `priority_score` so frequently-retrieved chunks rank higher and
//! never-retrieved stale chunks are demoted.
//!
//! The log is append-only and lock-free: each line is well under the
//! 4 KiB Linux atomic-append boundary, so concurrent writers cannot
//! interleave a single record. Counting is a soft signal — every IO
//! error here is swallowed so it can never fail a search.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// JSONL hit log, relative to the project root (cwd).
pub const HIT_LOG_REL_PATH: &str = ".memd/data/hit_counts.jsonl";

/// Cached aggregate, relative to the project root (cwd).
pub const HIT_SUMMARY_REL_PATH: &str = ".memd/data/hit_counts.summary.json";

/// Default aggregate cache TTL: one hour.
pub const DEFAULT_SUMMARY_TTL_MS: i64 = 3_600_000;

/// One retrieval event for one returned chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitRecord {
    pub ts_ms: i64,
    pub chunk_id: String,
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    pub query_mode: String,
    pub rank: usize,
    pub score: f64,
    pub selected: bool,
}

/// Folded per-chunk retrieval statistics within an aggregation window.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HitStats {
    /// Times the chunk appeared in a result set.
    pub hit_count: u32,
    /// Times the chunk was in the rendered (selected) result set.
    pub selected_count: u32,
    /// Most recent retrieval timestamp (Unix ms).
    pub last_ts_ms: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Maximum bytes per JSONL line. Linux guarantees atomic
/// `O_APPEND` writes only when the payload is below `PIPE_BUF`
/// (4 KiB). Records that exceed this limit are silently dropped so
/// concurrent writers can never interleave.
const MAX_RECORD_BYTES: usize = 4096;

/// Append one line per record to `.memd/data/hit_counts.jsonl` under
/// `cwd`. Best-effort: skips silently when there is no `.memd`
/// directory (not a memd project root) or on any IO error.
pub fn record_hits(records: &[HitRecord]) {
    if records.is_empty() {
        return;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    record_hits_in(&cwd, records);
}

/// `record_hits` with an explicit project root — used by tests.
///
/// Each record is serialised and written in its own `write_all` call
/// so the kernel sees one syscall per line. Lines above
/// `MAX_RECORD_BYTES` are dropped to keep every write below the
/// `PIPE_BUF` atomicity boundary; concurrent writers therefore
/// cannot interleave a single record.
pub fn record_hits_in(project_dir: &Path, records: &[HitRecord]) {
    if records.is_empty() || !project_dir.join(".memd").is_dir() {
        return;
    }
    write_hit_lines(&project_dir.join(HIT_LOG_REL_PATH), records);
}

/// Append hit records straight into a resolved store `data_dir`,
/// independent of the process cwd. The live search and agent_context
/// paths use this so every project's hits land in one central ledger
/// that both `memd report` and `memd memory-md` read — previously each
/// process wrote `hit_counts.jsonl` relative to its cwd, scattering the
/// log across project dirs where `report` never saw it.
///
/// `data_dir` already points at `.../.memd/data`, so the log file lives
/// directly inside it (no `.memd/data` suffix, unlike [`record_hits_in`]).
pub fn record_hits_to_data_dir(data_dir: &Path, records: &[HitRecord]) {
    if records.is_empty() {
        return;
    }
    write_hit_lines(&data_dir.join("hit_counts.jsonl"), records);
}

/// Append one JSONL line per record to `path`. Each line is written in
/// its own `write_all` so the kernel sees one syscall per line; lines
/// above `MAX_RECORD_BYTES` are dropped to stay under the `PIPE_BUF`
/// atomic-append boundary so concurrent writers never interleave.
fn write_hit_lines(path: &Path, records: &[HitRecord]) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    else {
        return;
    };
    for record in records {
        let Ok(mut line) = serde_json::to_string(record) else {
            continue;
        };
        line.push('\n');
        if line.len() > MAX_RECORD_BYTES {
            // Atomic-append boundary would be violated — drop to keep
            // concurrent writes safe.
            continue;
        }
        let _ = file.write_all(line.as_bytes());
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SummaryCache {
    generated_ms: i64,
    window_days: u32,
    stats: HashMap<String, HitStats>,
}

/// Aggregate the hit log into per-chunk [`HitStats`] for retrievals
/// within `window_days`. Reads `cwd`'s `.memd/data/`.
///
/// A fresh `hit_counts.summary.json` (same window, age under
/// `DEFAULT_SUMMARY_TTL_MS`) is reused; otherwise the JSONL log is
/// re-scanned and the cache rewritten.
pub fn aggregate_hits(window_days: u32) -> HashMap<String, HitStats> {
    let Ok(cwd) = std::env::current_dir() else {
        return HashMap::new();
    };
    aggregate_hits_in(&cwd, window_days, DEFAULT_SUMMARY_TTL_MS)
}

/// `aggregate_hits` with an explicit project root and TTL — used by
/// tests and by callers that need a non-default cache lifetime.
pub fn aggregate_hits_in(
    project_dir: &Path,
    window_days: u32,
    ttl_ms: i64,
) -> HashMap<String, HitStats> {
    aggregate_from(
        &project_dir.join(HIT_LOG_REL_PATH),
        &project_dir.join(HIT_SUMMARY_REL_PATH),
        window_days,
        ttl_ms,
    )
}

/// Aggregate the central hit log under a resolved store `data_dir`.
///
/// Stats are keyed by `chunk_id`, which is globally unique, so callers
/// that only look up their own chunk ids (e.g. `memd memory-md` scoring
/// one project's chunks) transparently ignore other projects' records
/// even though the central log mixes every project together.
pub fn aggregate_hits_at_data_dir(
    data_dir: &Path,
    window_days: u32,
    ttl_ms: i64,
) -> HashMap<String, HitStats> {
    aggregate_from(
        &data_dir.join("hit_counts.jsonl"),
        &data_dir.join("hit_counts.summary.json"),
        window_days,
        ttl_ms,
    )
}

/// Per-chunk serve stats from the central hit log since an absolute
/// `since_ms` cutoff, uncached, for an exact time window. Used by
/// `memd report` so per-chunk serve counts line up with the same window
/// as the add/search usage-event counts.
pub fn serve_counts_since(
    data_dir: &Path,
    since_ms: i64,
    tenant_id: Option<&str>,
    project_id: Option<&str>,
) -> HashMap<String, HitStats> {
    scan_hit_log_since(
        &data_dir.join("hit_counts.jsonl"),
        since_ms,
        tenant_id,
        project_id,
    )
}

fn aggregate_from(
    log_path: &Path,
    summary_path: &Path,
    window_days: u32,
    ttl_ms: i64,
) -> HashMap<String, HitStats> {
    let now = now_ms();

    if let Some(cached) = read_fresh_cache(summary_path, window_days, ttl_ms, now) {
        return cached;
    }

    let stats = scan_hit_log(log_path, window_days, now);
    write_cache(summary_path, window_days, now, &stats);
    stats
}

fn read_fresh_cache(
    summary_path: &Path,
    window_days: u32,
    ttl_ms: i64,
    now: i64,
) -> Option<HashMap<String, HitStats>> {
    let text = std::fs::read_to_string(summary_path).ok()?;
    let cache: SummaryCache = serde_json::from_str(&text).ok()?;
    if cache.window_days != window_days {
        return None;
    }
    if now.saturating_sub(cache.generated_ms) >= ttl_ms {
        return None;
    }
    Some(cache.stats)
}

fn scan_hit_log(log_path: &Path, window_days: u32, now: i64) -> HashMap<String, HitStats> {
    let cutoff = now.saturating_sub((window_days as i64).saturating_mul(86_400_000));
    // Window-based aggregation keys by chunk_id across all tenants/projects;
    // callers (memory-md priority scoring) look up only their own chunk
    // ids, so no tenant/project filter is applied here.
    scan_hit_log_since(log_path, cutoff, None, None)
}

fn scan_hit_log_since(
    log_path: &Path,
    cutoff: i64,
    tenant_id: Option<&str>,
    project_id: Option<&str>,
) -> HashMap<String, HitStats> {
    let mut stats: HashMap<String, HitStats> = HashMap::new();
    let Ok(text) = std::fs::read_to_string(log_path) else {
        return stats;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<HitRecord>(line) else {
            continue;
        };
        if record.ts_ms < cutoff {
            continue;
        }
        // The central log mixes tenants and projects; scope to match the
        // caller (e.g. `memd report --tenant-id`) so serve counts never leak
        // chunks from another tenant/project.
        if let Some(want) = tenant_id {
            if record.tenant_id != want {
                continue;
            }
        }
        if let Some(want) = project_id {
            if record.project_id.as_deref() != Some(want) {
                continue;
            }
        }
        let entry = stats.entry(record.chunk_id).or_default();
        entry.hit_count = entry.hit_count.saturating_add(1);
        if record.selected {
            entry.selected_count = entry.selected_count.saturating_add(1);
        }
        entry.last_ts_ms = entry.last_ts_ms.max(record.ts_ms);
    }
    stats
}

fn write_cache(summary_path: &Path, window_days: u32, now: i64, stats: &HashMap<String, HitStats>) {
    let Some(parent) = summary_path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let cache = SummaryCache {
        generated_ms: now,
        window_days,
        stats: stats.clone(),
    };
    if let Ok(text) = serde_json::to_string(&cache) {
        let _ = std::fs::write(summary_path, text);
    }
}

/// Canonical kebab-case name for a query mode, used in hit records.
pub fn query_mode_label(mode: &str) -> String {
    mode.trim().to_ascii_lowercase().replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record(chunk_id: &str, ts_ms: i64, selected: bool) -> HitRecord {
        HitRecord {
            ts_ms,
            chunk_id: chunk_id.to_string(),
            tenant_id: "t".to_string(),
            project_id: Some("p".to_string()),
            query_mode: "find-failures".to_string(),
            rank: 0,
            score: 0.5,
            selected,
        }
    }

    #[test]
    fn record_hits_skips_without_memd_dir() {
        let dir = tempdir().unwrap();
        record_hits_in(dir.path(), &[record("c1", now_ms(), true)]);
        assert!(!dir.path().join(HIT_LOG_REL_PATH).exists());
    }

    #[test]
    fn record_hits_appends_one_line_per_record() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        let now = now_ms();
        record_hits_in(
            dir.path(),
            &[record("c1", now, true), record("c2", now, false)],
        );
        record_hits_in(dir.path(), &[record("c1", now, true)]);
        let text = std::fs::read_to_string(dir.path().join(HIT_LOG_REL_PATH)).unwrap();
        assert_eq!(text.lines().count(), 3);
    }

    #[test]
    fn aggregate_groups_by_chunk_within_window() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        let now = now_ms();
        record_hits_in(
            dir.path(),
            &[
                record("c1", now, true),
                record("c1", now, false),
                record("c2", now, true),
                // Outside a 7-day window.
                record("c3", now - 30 * 86_400_000, true),
            ],
        );
        let stats = aggregate_hits_in(dir.path(), 7, DEFAULT_SUMMARY_TTL_MS);
        assert_eq!(stats["c1"].hit_count, 2);
        assert_eq!(stats["c1"].selected_count, 1);
        assert_eq!(stats["c2"].hit_count, 1);
        assert!(!stats.contains_key("c3"), "old record excluded by window");
    }

    #[test]
    fn aggregate_respects_cache_ttl() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        let now = now_ms();
        record_hits_in(dir.path(), &[record("c1", now, true)]);
        // Prime the cache.
        let first = aggregate_hits_in(dir.path(), 7, DEFAULT_SUMMARY_TTL_MS);
        assert_eq!(first["c1"].hit_count, 1);

        // New hits land in the log but the cache is still fresh, so a
        // re-aggregate within the TTL returns the stale (cached) value.
        record_hits_in(dir.path(), &[record("c1", now, true)]);
        let cached = aggregate_hits_in(dir.path(), 7, DEFAULT_SUMMARY_TTL_MS);
        assert_eq!(cached["c1"].hit_count, 1, "fresh cache reused");

        // With a zero TTL the cache is always stale, so the log is
        // re-scanned and the new hit is visible.
        let rescanned = aggregate_hits_in(dir.path(), 7, 0);
        assert_eq!(rescanned["c1"].hit_count, 2, "expired cache rescanned");
    }

    #[test]
    fn oversized_records_are_dropped_to_preserve_atomic_append() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        let now = now_ms();
        let mut huge = record("ok", now, true);
        // Forge a chunk_id over the 4 KiB boundary; the JSON line
        // therefore exceeds MAX_RECORD_BYTES.
        huge.chunk_id = "x".repeat(MAX_RECORD_BYTES + 100);
        record_hits_in(dir.path(), &[huge, record("ok2", now, true)]);
        let text = std::fs::read_to_string(dir.path().join(HIT_LOG_REL_PATH)).unwrap();
        assert_eq!(text.lines().count(), 1, "oversized line must be dropped");
        for line in text.lines() {
            assert!(
                line.len() < MAX_RECORD_BYTES,
                "every persisted line must stay under the atomic-append boundary"
            );
        }
    }

    #[test]
    fn aggregate_missing_log_is_empty() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        assert!(aggregate_hits_in(dir.path(), 7, DEFAULT_SUMMARY_TTL_MS).is_empty());
    }

    fn record_in(chunk_id: &str, ts_ms: i64, selected: bool, project: &str) -> HitRecord {
        HitRecord {
            ts_ms,
            chunk_id: chunk_id.to_string(),
            tenant_id: "t".to_string(),
            project_id: Some(project.to_string()),
            query_mode: "find-failures".to_string(),
            rank: 0,
            score: 0.5,
            selected,
        }
    }

    #[test]
    fn record_to_data_dir_writes_log_directly_under_data_dir() {
        // Central-log writes target `<data_dir>/hit_counts.jsonl`, with no
        // `.memd/data` suffix and no project-root `.memd` guard (the store
        // data_dir is always present).
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("memd_data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let now = now_ms();
        record_hits_to_data_dir(&data_dir, &[record("c1", now, true)]);
        let log = data_dir.join("hit_counts.jsonl");
        assert!(log.is_file(), "log must live directly under data_dir");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "one line per record"
        );
    }

    #[test]
    fn serve_counts_since_filters_by_tenant_project_and_window() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("memd_data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let now = now_ms();
        // A record from a different tenant that must not leak into a
        // tenant-scoped report (the central log mixes tenants).
        let mut other_tenant = record_in("z", now, true, "p1");
        other_tenant.tenant_id = "other".to_string();
        record_hits_to_data_dir(
            &data_dir,
            &[
                record_in("a", now, true, "p1"),
                record_in("a", now, false, "p1"),
                record_in("b", now, true, "p2"),
                other_tenant,
                // Outside the window for an explicit cutoff below.
                record_in("c", now - 30 * 86_400_000, true, "p1"),
            ],
        );

        // No tenant/project filter, recent cutoff: all in-window chunks.
        let all = serve_counts_since(&data_dir, now - 86_400_000, None, None);
        assert_eq!(all["a"].hit_count, 2);
        assert_eq!(all["a"].selected_count, 1);
        assert_eq!(all["b"].hit_count, 1);
        assert!(
            all.contains_key("z"),
            "other-tenant chunk present when unfiltered"
        );
        assert!(!all.contains_key("c"), "old record excluded by cutoff");

        // Tenant filter keeps only tenant "t" chunks.
        let scoped = serve_counts_since(&data_dir, now - 86_400_000, Some("t"), None);
        assert!(scoped.contains_key("a") && scoped.contains_key("b"));
        assert!(
            !scoped.contains_key("z"),
            "other-tenant chunk filtered out by tenant scope"
        );

        // Tenant + project filter keeps only p1's chunks within tenant "t".
        let p1 = serve_counts_since(&data_dir, now - 86_400_000, Some("t"), Some("p1"));
        assert!(p1.contains_key("a"));
        assert!(!p1.contains_key("b"), "p2 chunk filtered out");
    }

    #[test]
    fn aggregate_at_data_dir_reads_central_log() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("memd_data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let now = now_ms();
        record_hits_to_data_dir(
            &data_dir,
            &[record("c1", now, true), record("c1", now, false)],
        );
        // ttl=0 forces a fresh scan of the central log.
        let stats = aggregate_hits_at_data_dir(&data_dir, 7, 0);
        assert_eq!(stats["c1"].hit_count, 2);
        assert_eq!(stats["c1"].selected_count, 1);
    }
}
