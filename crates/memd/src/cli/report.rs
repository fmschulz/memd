use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use tracing::info;

use super::args::ReportFormat;
use super::audit::{collect_scanned_chunks, resolve_tenants, storage_report};
use super::memory_md::explicit_agent_action;
use crate::error::{MemdError, Result};
use crate::hit_stats::{serve_counts_since, HitStats};
use crate::omf::time::format_rfc3339_ms;
use crate::store::usage::{usage_retention_ms, UsageEvent, UsageEventRecord, UsageOp};
use crate::store::{Store, TenantManager};
use crate::types::MemoryChunk;

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;
const REPORT_DEFAULT_TOP: usize = 5;
const REPORT_SCAN_PAGE_SIZE: usize = 10_000;

pub(super) struct ReportOptions {
    pub(super) tenant_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) since: String,
    pub(super) top: usize,
    pub(super) format: ReportFormat,
    pub(super) served_via_worker: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    header: ReportHeader,
    growth: GrowthSection,
    learning_digest: LearningDigestSection,
    retrieval_usefulness: RetrievalUsefulnessSection,
    self_diagnosis: SelfDiagnosisSection,
}

#[derive(Debug, Serialize)]
struct ReportHeader {
    window: WindowHeader,
    scope: ScopeHeader,
    generated_unix_ms: i64,
    memd_version: String,
}

#[derive(Debug, Serialize)]
struct WindowHeader {
    raw: String,
    since_unix_ms: i64,
    since_utc: String,
}

#[derive(Debug, Serialize)]
struct ScopeHeader {
    tenant_id: Option<String>,
    project_id: Option<String>,
    description: String,
}

#[derive(Debug, Serialize)]
struct GrowthSection {
    adds: AddGrowth,
    deletes: usize,
    imported: usize,
    purged: usize,
    expired_in_window: usize,
    superseded_in_window: usize,
    store_totals: StoreTotals,
}

#[derive(Debug, Serialize)]
struct AddGrowth {
    admitted: usize,
    bytes_added: i64,
    downgraded: usize,
    rejected: RejectedGrowth,
}

#[derive(Debug, Default, Serialize)]
struct RejectedGrowth {
    total: usize,
    by_reason: BTreeMap<String, usize>,
}

#[derive(Debug, Default, Serialize)]
struct StoreTotals {
    tenant_count: usize,
    active_chunks: usize,
    deleted_chunks: usize,
}

#[derive(Debug, Serialize)]
struct LearningDigestSection {
    consolidated_in_window: usize,
    high_priority_in_window: usize,
    entries: Vec<LearningDigestEntry>,
}

#[derive(Debug, Serialize)]
struct LearningDigestEntry {
    chunk_id: String,
    tenant_id: String,
    project_id: Option<String>,
    priority: f32,
    first_line: String,
    agent_action: String,
}

#[derive(Debug, Serialize)]
struct RetrievalUsefulnessSection {
    searches: usize,
    zero_hits: usize,
    hit_rate: Option<f64>,
    distinct_queries: usize,
    agent_context_calls: usize,
    /// One-line availability/summary note for the per-chunk serve log.
    per_chunk_serve_counts: String,
    /// Distinct chunks served at least once in the window.
    distinct_served_chunks: usize,
    /// Total per-chunk serve events in the window (sum of hit counts).
    total_serves: usize,
    /// Most-served chunks in the window, highest first.
    top_served_chunks: Vec<ServedChunkStat>,
}

/// One chunk's retrieval-serve tally from the central hit log.
#[derive(Debug, Serialize)]
struct ServedChunkStat {
    chunk_id: String,
    hit_count: u32,
    selected_count: u32,
}

#[derive(Debug, Serialize)]
struct SelfDiagnosisSection {
    warn_count: usize,
    lines: Vec<DiagnosisLine>,
}

#[derive(Debug, Serialize)]
struct DiagnosisLine {
    level: DiagnosisLevel,
    name: String,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosisLevel {
    Ok,
    Warn,
    Info,
}

#[derive(Debug)]
struct ScannedTenant {
    metadata_active_chunks: usize,
    scanned_chunks: usize,
    chunks: Vec<MemoryChunk>,
}

#[derive(Debug)]
struct DigestCandidate {
    entry: LearningDigestEntry,
    priority: f32,
    timestamp_created: i64,
}

struct DiagnosisInputs<'a> {
    options: &'a ReportOptions,
    growth: &'a GrowthSection,
    retrieval: &'a RetrievalUsefulnessSection,
    scanned_tenants: &'a [ScannedTenant],
    ledger_row_count: i64,
    ledger_min_ts: Option<i64>,
    generated_unix_ms: i64,
    window_ms: i64,
    tenant_manager: Option<&'a TenantManager>,
}

pub(super) async fn cli_report_rendered<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: ReportOptions,
) -> Result<(String, usize)> {
    let report = collect_report(store, tenant_manager, &options).await?;
    let warn_count = report.self_diagnosis.warn_count;
    let rendered = render_report(&report, options.format)?;
    store.record_usage_event(UsageEvent {
        op: UsageOp::Report,
        tenant: options.tenant_id.clone(),
        project: options.project_id.clone(),
        outcome: "ok".to_string(),
        chunk_count: None,
        bytes: None,
        detail: Some(
            json!({
                "since": options.since,
                "format": report_format_name(options.format),
            })
            .to_string(),
        ),
    });
    info!(warn_count, "report rendered");
    Ok((rendered, warn_count))
}

async fn collect_report<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: &ReportOptions,
) -> Result<Report> {
    let ps = store.as_persistent().ok_or_else(|| {
        MemdError::ValidationError("report requires a persistent store (not --in-memory)".into())
    })?;
    let window_ms = parse_since_window_ms(&options.since)?;
    let generated_unix_ms = now_ms();
    let since_ms = generated_unix_ms.saturating_sub(window_ms);
    let since_utc = format_utc_ms(since_ms);

    let usage_events = ps.metadata().usage_events_since(
        since_ms,
        options.tenant_id.as_deref(),
        options.project_id.as_deref(),
    )?;
    let lifecycle_counts = ps.metadata().lifecycle_status_counts_since(
        since_ms,
        options.tenant_id.as_deref(),
        options.project_id.as_deref(),
    )?;
    let (ledger_row_count, ledger_min_ts) = ps.metadata().usage_ledger_stats()?;

    let tenants = resolve_tenants(store, tenant_manager, options.tenant_id.as_deref()).await?;
    let mut store_totals = StoreTotals {
        tenant_count: tenants.len(),
        ..Default::default()
    };
    let mut scanned_tenants = Vec::with_capacity(tenants.len());
    for tenant in tenants {
        let stats = store.stats(&tenant).await?;
        store_totals.active_chunks += stats.active_chunks;
        store_totals.deleted_chunks += stats.deleted_chunks;
        let chunks = collect_scanned_chunks(
            store,
            &tenant,
            options.project_id.as_deref(),
            REPORT_SCAN_PAGE_SIZE,
        )
        .await?;
        scanned_tenants.push(ScannedTenant {
            metadata_active_chunks: stats.active_chunks,
            scanned_chunks: chunks.len(),
            chunks: chunks.into_iter().map(|scanned| scanned.chunk).collect(),
        });
    }

    // Per-chunk serve counts come from the central hit log under the
    // store data_dir, scoped to the same window, tenant, and project as the
    // usage-event metrics above (the central log mixes tenants/projects).
    let serve_stats = serve_counts_since(
        ps.data_dir(),
        since_ms,
        options.tenant_id.as_deref(),
        options.project_id.as_deref(),
    );

    let growth = build_growth(&usage_events, lifecycle_counts, store_totals);
    let learning_digest = build_learning_digest(&scanned_tenants, since_ms, options.top);
    let retrieval_usefulness = build_retrieval_usefulness(&usage_events, &serve_stats, options.top);
    let self_diagnosis = build_self_diagnosis(DiagnosisInputs {
        options,
        growth: &growth,
        retrieval: &retrieval_usefulness,
        scanned_tenants: &scanned_tenants,
        ledger_row_count,
        ledger_min_ts,
        generated_unix_ms,
        window_ms,
        tenant_manager,
    });

    Ok(Report {
        header: ReportHeader {
            window: WindowHeader {
                raw: options.since.clone(),
                since_unix_ms: since_ms,
                since_utc,
            },
            scope: ScopeHeader {
                tenant_id: options.tenant_id.clone(),
                project_id: options.project_id.clone(),
                description: scope_description(
                    options.tenant_id.as_deref(),
                    options.project_id.as_deref(),
                ),
            },
            generated_unix_ms,
            memd_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        growth,
        learning_digest,
        retrieval_usefulness,
        self_diagnosis,
    })
}

pub(super) async fn memory_health_lines<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    tenant_id: &str,
    project_id: Option<&str>,
) -> Result<Vec<String>> {
    let options = ReportOptions {
        tenant_id: Some(tenant_id.to_string()),
        project_id: project_id.map(str::to_string),
        since: "7d".to_string(),
        top: REPORT_DEFAULT_TOP,
        format: ReportFormat::Markdown,
        served_via_worker: false,
    };
    let report = collect_report(store, tenant_manager, &options).await?;
    let mut lines = vec![
        format!(
            "chunks: {} active (+{} added, {} rejected, 7d)",
            report.growth.store_totals.active_chunks,
            report.growth.adds.admitted,
            report.growth.adds.rejected.total
        ),
        if report.retrieval_usefulness.searches == 0 {
            "retrieval: 0 searches (7d)".to_string()
        } else {
            format!(
                "retrieval: {} searches, {:.0}% hit rate (7d)",
                report.retrieval_usefulness.searches,
                report.retrieval_usefulness.hit_rate.unwrap_or(0.0) * 100.0
            )
        },
        format!(
            "learned: {} high-priority + {} consolidated lessons (7d)",
            report.learning_digest.high_priority_in_window,
            report.learning_digest.consolidated_in_window
        ),
    ];
    lines.extend(
        report
            .self_diagnosis
            .lines
            .iter()
            .filter(|line| line.level == DiagnosisLevel::Warn)
            .map(|line| format!("[warn] {}: {}", line.name, line.detail)),
    );
    Ok(lines)
}

pub(super) fn parse_since_window_ms(s: &str) -> Result<i64> {
    let trimmed = s.trim();
    let invalid = || {
        MemdError::ValidationError(format!(
            "invalid --since value '{s}': use <N>d for days or <N>h for hours (e.g. 7d, 24h, 30d); max 3650d or 87600h"
        ))
    };
    if trimmed.len() < 2 {
        return Err(invalid());
    }

    let (number, suffix) = trimmed.split_at(trimmed.len() - 1);
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(invalid());
    }
    let value = number.parse::<i64>().map_err(|_| invalid())?;
    if value < 1 {
        return Err(invalid());
    }

    match suffix.to_ascii_lowercase().as_str() {
        "d" if value <= 3650 => Ok(value.saturating_mul(DAY_MS)),
        "h" if value <= 87600 => Ok(value.saturating_mul(HOUR_MS)),
        "d" | "h" => Err(invalid()),
        _ => Err(invalid()),
    }
}

fn build_growth(
    events: &[UsageEventRecord],
    lifecycle_counts: BTreeMap<String, usize>,
    store_totals: StoreTotals,
) -> GrowthSection {
    let mut adds = AddGrowth {
        admitted: 0,
        bytes_added: 0,
        downgraded: 0,
        rejected: RejectedGrowth::default(),
    };
    let mut deletes = 0usize;
    let mut imported = 0usize;
    let mut purged = 0usize;

    for event in events.iter().filter(|event| event.op != "report") {
        match event.op.as_str() {
            "add" if event.outcome == "admitted" => {
                adds.admitted += positive_chunk_count(event);
                adds.bytes_added += positive_i64(event.bytes);
            }
            "add" if event.outcome == "downgraded" => {
                adds.downgraded += positive_chunk_count(event);
            }
            "add" if event.outcome.starts_with("rejected:") => {
                adds.rejected.total += 1;
                let reason = event
                    .outcome
                    .strip_prefix("rejected:")
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("unknown")
                    .to_string();
                *adds.rejected.by_reason.entry(reason).or_insert(0) += 1;
            }
            "delete" if event.outcome == "ok" => deletes += 1,
            "import_omf" => imported += positive_chunk_count(event),
            "purge" => purged += positive_chunk_count(event),
            _ => {}
        }
    }

    GrowthSection {
        adds,
        deletes,
        imported,
        purged,
        expired_in_window: lifecycle_counts.get("expired").copied().unwrap_or(0),
        superseded_in_window: lifecycle_counts.get("superseded").copied().unwrap_or(0),
        store_totals,
    }
}

fn build_learning_digest(
    scanned_tenants: &[ScannedTenant],
    since_ms: i64,
    top: usize,
) -> LearningDigestSection {
    let mut consolidated_in_window = 0usize;
    let mut high_priority_in_window = 0usize;
    let mut candidates = Vec::new();

    for scanned in scanned_tenants {
        for chunk in &scanned.chunks {
            if chunk.timestamp_created < since_ms {
                continue;
            }
            let consolidated = has_tag(&chunk.tags, "kind:consolidated");
            if consolidated {
                consolidated_in_window += 1;
            }
            let explicit_priority = explicit_priority_value(&chunk.tags);
            if !consolidated && explicit_priority.is_some_and(|value| value >= 8.0) {
                high_priority_in_window += 1;
            }
            let Some(priority) = explicit_priority.or_else(|| consolidated.then_some(8.0)) else {
                continue;
            };
            if explicit_priority.is_some_and(|value| value < 8.0) && !consolidated {
                continue;
            }
            candidates.push(DigestCandidate {
                priority,
                timestamp_created: chunk.timestamp_created,
                entry: LearningDigestEntry {
                    chunk_id: chunk.chunk_id.to_string(),
                    tenant_id: chunk.tenant_id.to_string(),
                    project_id: chunk.project_id.as_option().map(str::to_string),
                    priority,
                    first_line: first_line_preview(&chunk.text),
                    agent_action: explicit_agent_action(&chunk.text)
                        .unwrap_or_else(|| "none recorded".to_string()),
                },
            });
        }
    }

    candidates.sort_by(|left, right| {
        right
            .priority
            .partial_cmp(&left.priority)
            .unwrap_or(Ordering::Equal)
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
            .then_with(|| left.entry.chunk_id.cmp(&right.entry.chunk_id))
    });
    candidates.truncate(top);

    LearningDigestSection {
        consolidated_in_window,
        high_priority_in_window,
        entries: candidates
            .into_iter()
            .map(|candidate| candidate.entry)
            .collect(),
    }
}

fn build_retrieval_usefulness(
    events: &[UsageEventRecord],
    serve_stats: &HashMap<String, HitStats>,
    top: usize,
) -> RetrievalUsefulnessSection {
    let mut searches_total = 0usize;
    let mut zero_hits = 0usize;
    let mut hashes = BTreeSet::new();
    let mut agent_context_calls = 0usize;

    for event in events.iter().filter(|event| event.op != "report") {
        match event.op.as_str() {
            "search" => {
                searches_total += 1;
                if event.outcome == "zero_hits" {
                    zero_hits += 1;
                }
                if let Some(hash) = event.detail.as_deref().and_then(extract_q_hash) {
                    hashes.insert(hash);
                }
            }
            "agent_context" => agent_context_calls += 1,
            _ => {}
        }
    }

    let distinct_served_chunks = serve_stats.len();
    let total_serves: usize = serve_stats.values().map(|s| s.hit_count as usize).sum();
    // Rank by serve count, then selected count, then chunk_id for a
    // stable order; take the top N for display.
    let mut ranked: Vec<(&String, &HitStats)> = serve_stats.iter().collect();
    ranked.sort_by(|a, b| {
        b.1.hit_count
            .cmp(&a.1.hit_count)
            .then(b.1.selected_count.cmp(&a.1.selected_count))
            .then(a.0.cmp(b.0))
    });
    let top_served_chunks = ranked
        .iter()
        .take(top)
        .map(|(chunk_id, stats)| ServedChunkStat {
            chunk_id: (*chunk_id).clone(),
            hit_count: stats.hit_count,
            selected_count: stats.selected_count,
        })
        .collect();

    let per_chunk_serve_counts = if distinct_served_chunks == 0 {
        "none recorded in window (central hit log empty; scattered pre-centralization logs are not counted)"
            .to_string()
    } else {
        format!("distinct_chunks={distinct_served_chunks}; total_serves={total_serves}")
    };

    RetrievalUsefulnessSection {
        searches: searches_total,
        zero_hits,
        hit_rate: (searches_total > 0).then(|| 1.0 - zero_hits as f64 / searches_total as f64),
        distinct_queries: hashes.len(),
        agent_context_calls,
        per_chunk_serve_counts,
        distinct_served_chunks,
        total_serves,
        top_served_chunks,
    }
}

fn build_self_diagnosis(inputs: DiagnosisInputs<'_>) -> SelfDiagnosisSection {
    let mut lines = Vec::new();
    if inputs.options.project_id.is_some() {
        lines.push(DiagnosisLine::info(
            "unreadable_active_chunks",
            "unreadable check skipped (project-filtered view)",
        ));
    } else {
        let metadata_active: usize = inputs
            .scanned_tenants
            .iter()
            .map(|tenant| tenant.metadata_active_chunks)
            .sum();
        let scanned: usize = inputs
            .scanned_tenants
            .iter()
            .map(|tenant| tenant.scanned_chunks)
            .sum();
        let unreadable = metadata_active.saturating_sub(scanned);
        let detail = format!(
            "metadata_active={metadata_active}; scanned_readable={scanned}; unreadable={unreadable}"
        );
        if unreadable > 0 {
            lines.push(DiagnosisLine::warn("unreadable_active_chunks", detail));
        } else {
            lines.push(DiagnosisLine::ok("unreadable_active_chunks", detail));
        }
    }

    lines.push(zero_hit_diagnosis(inputs.retrieval));
    lines.push(admit_ratio_diagnosis(inputs.growth));
    lines.push(ledger_health_diagnosis(
        inputs.ledger_row_count,
        inputs.ledger_min_ts,
        inputs.generated_unix_ms,
    ));
    lines.push(warm_worker_diagnosis(inputs.options.served_via_worker));
    lines.push(storage_growth_diagnosis(
        inputs.growth.adds.bytes_added,
        inputs.window_ms,
        inputs.tenant_manager,
    ));

    let warn_count = lines
        .iter()
        .filter(|line| line.level == DiagnosisLevel::Warn)
        .count();
    SelfDiagnosisSection { warn_count, lines }
}

fn zero_hit_diagnosis(retrieval: &RetrievalUsefulnessSection) -> DiagnosisLine {
    let share = if retrieval.searches == 0 {
        0.0
    } else {
        retrieval.zero_hits as f64 / retrieval.searches as f64
    };
    let detail = if retrieval.searches < 20 {
        format!(
            "low sample: searches={}; zero_hits={}; zero_hit_share={:.1}%",
            retrieval.searches,
            retrieval.zero_hits,
            share * 100.0
        )
    } else {
        format!(
            "searches={}; zero_hits={}; zero_hit_share={:.1}%",
            retrieval.searches,
            retrieval.zero_hits,
            share * 100.0
        )
    };
    if retrieval.searches >= 20 && share > 0.5 {
        DiagnosisLine::warn("zero_hit_share", detail)
    } else {
        DiagnosisLine::ok("zero_hit_share", detail)
    }
}

fn admit_ratio_diagnosis(growth: &GrowthSection) -> DiagnosisLine {
    let total_adds = growth.adds.admitted + growth.adds.downgraded + growth.adds.rejected.total;
    let ratio = if total_adds == 0 {
        0.0
    } else {
        growth.adds.admitted as f64 / total_adds as f64
    };
    let sample = if total_adds < 20 { "low sample: " } else { "" };
    let detail = format!(
        "{sample}total_adds={total_adds}; admitted={}; downgraded={}; rejected={}; admitted_ratio={:.1}%",
        growth.adds.admitted,
        growth.adds.downgraded,
        growth.adds.rejected.total,
        ratio * 100.0
    );
    if total_adds >= 20 && ratio < 0.3 {
        DiagnosisLine::warn("admit_ratio", detail)
    } else {
        DiagnosisLine::ok("admit_ratio", detail)
    }
}

fn ledger_health_diagnosis(
    ledger_row_count: i64,
    ledger_min_ts: Option<i64>,
    generated_unix_ms: i64,
) -> DiagnosisLine {
    let cutoff = generated_unix_ms.saturating_sub(usage_retention_ms());
    match ledger_min_ts {
        Some(min_ts) if min_ts < cutoff => DiagnosisLine::warn(
            "ledger_health",
            format!(
                "row_count={ledger_row_count}; oldest_row={} is older than retention cutoff {}",
                format_utc_ms(min_ts),
                format_utc_ms(cutoff)
            ),
        ),
        Some(min_ts) => DiagnosisLine::ok(
            "ledger_health",
            format!(
                "row_count={ledger_row_count}; oldest_row={} within retention",
                format_utc_ms(min_ts)
            ),
        ),
        None => DiagnosisLine::ok(
            "ledger_health",
            format!("row_count={ledger_row_count}; no usage rows"),
        ),
    }
}

fn warm_worker_diagnosis(served_via_worker: bool) -> DiagnosisLine {
    if served_via_worker {
        DiagnosisLine::info(
            "warm_worker",
            format!(
                "served by warm worker pid {}, memd {}",
                std::process::id(),
                env!("CARGO_PKG_VERSION")
            ),
        )
    } else {
        DiagnosisLine::info("warm_worker", "warm worker not consulted (direct path)")
    }
}

fn storage_growth_diagnosis(
    bytes_added: i64,
    window_ms: i64,
    tenant_manager: Option<&TenantManager>,
) -> DiagnosisLine {
    let window_days = window_ms as f64 / DAY_MS as f64;
    let bytes_per_day = if window_days > 0.0 {
        bytes_added as f64 / window_days
    } else {
        0.0
    };
    let total = tenant_manager
        .and_then(|tm| storage_report(tm.data_dir()).ok())
        .map(|report| report.total_bytes);
    let detail = match total {
        Some(total_bytes) => {
            format!("bytes_added_per_day={bytes_per_day:.1}; total_store_bytes={total_bytes}")
        }
        None => format!("bytes_added_per_day={bytes_per_day:.1}; total_store_bytes=unavailable"),
    };
    DiagnosisLine::info("storage_growth_slope", detail)
}

fn render_report(report: &Report, format: ReportFormat) -> Result<String> {
    match format {
        ReportFormat::Json => Ok(format!("{}\n", serde_json::to_string_pretty(report)?)),
        ReportFormat::Markdown => Ok(render_markdown(report)),
    }
}

fn render_markdown(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("# memd report\n\n");
    let _ = writeln!(
        out,
        "- window: `{}` (since `{}` / `{}`)",
        report.header.window.raw,
        report.header.window.since_utc,
        report.header.window.since_unix_ms
    );
    let _ = writeln!(out, "- scope: `{}`", report.header.scope.description);
    let _ = writeln!(
        out,
        "- generated_unix_ms: `{}`",
        report.header.generated_unix_ms
    );
    let _ = writeln!(out, "- memd_version: `{}`", report.header.memd_version);

    out.push_str("\n## Growth\n\n");
    let _ = writeln!(
        out,
        "- adds: admitted=`{}`; downgraded=`{}`; rejected=`{}`; bytes_added=`{}`",
        report.growth.adds.admitted,
        report.growth.adds.downgraded,
        report.growth.adds.rejected.total,
        report.growth.adds.bytes_added
    );
    if !report.growth.adds.rejected.by_reason.is_empty() {
        let reasons = report
            .growth
            .adds
            .rejected
            .by_reason
            .iter()
            .map(|(reason, count)| format!("{reason}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "- rejected_reasons: `{reasons}`");
    }
    let _ = writeln!(
        out,
        "- deletes=`{}`; imported=`{}`; purged=`{}`; expired_in_window=`{}`; superseded_in_window=`{}`",
        report.growth.deletes,
        report.growth.imported,
        report.growth.purged,
        report.growth.expired_in_window,
        report.growth.superseded_in_window
    );
    let _ = writeln!(
        out,
        "- store_totals: tenants=`{}`; active_chunks=`{}`; deleted_chunks=`{}`",
        report.growth.store_totals.tenant_count,
        report.growth.store_totals.active_chunks,
        report.growth.store_totals.deleted_chunks
    );

    out.push_str("\n## Learning digest\n\n");
    let _ = writeln!(
        out,
        "- consolidated_in_window: `{}`",
        report.learning_digest.consolidated_in_window
    );
    let _ = writeln!(
        out,
        "- high_priority_in_window: `{}`",
        report.learning_digest.high_priority_in_window
    );
    if report.learning_digest.entries.is_empty() {
        out.push_str("- none\n");
    } else {
        for entry in &report.learning_digest.entries {
            let _ = writeln!(
                out,
                "- `{}` priority=`{:.1}`: {} | Agent action: {} | chunk_id=`{}`",
                entry.tenant_id,
                entry.priority,
                entry.first_line,
                entry.agent_action,
                entry.chunk_id
            );
        }
    }

    out.push_str("\n## Retrieval usefulness\n\n");
    let hit_rate = report
        .retrieval_usefulness
        .hit_rate
        .map(|rate| format!("{:.1}%", rate * 100.0))
        .unwrap_or_else(|| "n/a".to_string());
    let _ = writeln!(
        out,
        "- searches=`{}`; zero_hits=`{}`; hit_rate=`{}`; distinct_queries=`{}`; agent_context_calls=`{}`",
        report.retrieval_usefulness.searches,
        report.retrieval_usefulness.zero_hits,
        hit_rate,
        report.retrieval_usefulness.distinct_queries,
        report.retrieval_usefulness.agent_context_calls
    );
    let _ = writeln!(
        out,
        "- per_chunk_serve_counts: {}",
        report.retrieval_usefulness.per_chunk_serve_counts
    );
    for served in &report.retrieval_usefulness.top_served_chunks {
        let _ = writeln!(
            out,
            "  - `{}`: served={}, selected={}",
            served.chunk_id, served.hit_count, served.selected_count
        );
    }

    out.push_str("\n## Self-diagnosis\n\n");
    for line in &report.self_diagnosis.lines {
        let _ = writeln!(
            out,
            "- [{}] {}: {}",
            line.level.as_markdown(),
            line.name,
            line.detail
        );
    }
    let _ = writeln!(out, "- warn_count: `{}`", report.self_diagnosis.warn_count);
    out
}

fn positive_chunk_count(event: &UsageEventRecord) -> usize {
    event.chunk_count.unwrap_or(0).max(0) as usize
}

fn positive_i64(value: Option<i64>) -> i64 {
    value.unwrap_or(0).max(0)
}

fn extract_q_hash(detail: &str) -> Option<String> {
    let value: Value = serde_json::from_str(detail).ok()?;
    value
        .get("q_hash")
        .and_then(Value::as_str)
        .filter(|hash| !hash.is_empty())
        .map(str::to_string)
}

fn explicit_priority_value(tags: &[String]) -> Option<f32> {
    tags.iter()
        .filter_map(|tag| {
            tag.strip_prefix("priority:")
                .or_else(|| tag.strip_prefix("importance:"))
                .and_then(|value| value.parse::<f32>().ok())
        })
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal))
}

fn has_tag(tags: &[String], needle: &str) -> bool {
    tags.iter().any(|tag| tag == needle)
}

fn first_line_preview(text: &str) -> String {
    truncate_chars(text.lines().next().unwrap_or("").trim(), 120)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut out = text.chars().take(keep).collect::<String>();
    out.push_str("...");
    out
}

fn scope_description(tenant_id: Option<&str>, project_id: Option<&str>) -> String {
    match (tenant_id, project_id) {
        (Some(tenant), Some(project)) => format!("tenant={tenant}; project={project}"),
        (Some(tenant), None) => format!("tenant={tenant}; project=all"),
        (None, Some(project)) => format!("tenant=all; project={project}"),
        (None, None) => "all".to_string(),
    }
}

fn report_format_name(format: ReportFormat) -> &'static str {
    match format {
        ReportFormat::Markdown => "markdown",
        ReportFormat::Json => "json",
    }
}

fn format_utc_ms(ms: i64) -> String {
    format_rfc3339_ms(ms).unwrap_or_else(|| ms.to_string())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

impl DiagnosisLine {
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: DiagnosisLevel::Ok,
            name: name.into(),
            detail: detail.into(),
        }
    }

    fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: DiagnosisLevel::Warn,
            name: name.into(),
            detail: detail.into(),
        }
    }

    fn info(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: DiagnosisLevel::Info,
            name: name.into(),
            detail: detail.into(),
        }
    }
}

impl DiagnosisLevel {
    fn as_markdown(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Info => "info",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_accepts_days_and_hours() {
        assert_eq!(parse_since_window_ms("7d").unwrap(), 7 * DAY_MS);
        assert_eq!(parse_since_window_ms("24h").unwrap(), 24 * HOUR_MS);
        assert_eq!(parse_since_window_ms(" 3D ").unwrap(), 3 * DAY_MS);
        assert_eq!(parse_since_window_ms("12H").unwrap(), 12 * HOUR_MS);
    }

    #[test]
    fn parse_since_rejects_invalid_values() {
        for value in ["", "0d", "7w", "banana", "7", "3651d", "87601h"] {
            let err = parse_since_window_ms(value).unwrap_err().to_string();
            assert!(err.contains("invalid --since value"), "{err}");
            assert!(err.contains("<N>d"), "{err}");
            assert!(err.contains("<N>h"), "{err}");
        }
    }

    #[test]
    fn build_growth_counts_only_successful_deletes() {
        let events = vec![
            UsageEventRecord {
                ts_unix_ms: 1,
                op: "delete".to_string(),
                outcome: "ok".to_string(),
                chunk_count: Some(1),
                bytes: None,
                detail: None,
            },
            UsageEventRecord {
                ts_unix_ms: 2,
                op: "delete".to_string(),
                outcome: "not_found".to_string(),
                chunk_count: Some(0),
                bytes: None,
                detail: None,
            },
        ];
        let growth = build_growth(
            &events,
            BTreeMap::new(),
            StoreTotals {
                tenant_count: 0,
                active_chunks: 0,
                deleted_chunks: 0,
            },
        );
        assert_eq!(growth.deletes, 1);
    }
}
