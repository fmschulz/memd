use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::args::ExportFormat;
use crate::error::{MemdError, Result};
use crate::store::metadata::MetadataStore;
use crate::store::{Store, StoreHealthSnapshot, StoreStats, TenantManager};
use crate::types::{ChunkType, MemoryChunk, TenantId};

#[derive(Debug, Clone)]
pub(super) struct AuditOptions {
    pub(super) tenant_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) page_size: usize,
    pub(super) duplicate_examples: usize,
    pub(super) top_projects: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct AuditReport {
    generated_unix_ms: i64,
    data_dir: Option<String>,
    storage: Option<StorageReport>,
    totals: AuditTotals,
    tenants: Vec<TenantAudit>,
}

#[derive(Debug, Default, Serialize)]
struct AuditTotals {
    tenant_count: usize,
    metadata_active_chunks: usize,
    scanned_chunks: usize,
    unreadable_active_chunks: usize,
    generated_digest_chunks: usize,
    generated_wrapper_chunks: usize,
    routine_progress_chunks: usize,
    unbounded_progress_chunks: usize,
    unbounded_progress_older_30d: usize,
    unscoped_chunks: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct StorageReport {
    pub(super) total_bytes: u64,
    metadata_db_bytes: u64,
    sparse_index_bytes: u64,
    tenants_bytes: u64,
    warm_bytes: u64,
}

#[derive(Debug, Serialize)]
struct TenantAudit {
    tenant_id: String,
    project_id_filter: Option<String>,
    stats: StoreStatsReport,
    disk: Option<TenantDiskReport>,
    health: Option<StoreHealthSnapshot>,
    metadata_active_chunks: usize,
    scanned_chunks: usize,
    unreadable_active_chunks: usize,
    readable_active_ratio: f64,
    generated_digest_chunks: usize,
    generated_digest_ratio: f64,
    generated_wrapper_chunks: usize,
    generated_wrapper_ratio: f64,
    routine_progress_chunks: usize,
    unbounded_progress_chunks: usize,
    unbounded_progress_older_30d: usize,
    unscoped_chunks: usize,
    age_buckets: AgeBuckets,
    chunk_types_scanned: BTreeMap<String, usize>,
    kind_tags: BTreeMap<String, usize>,
    projects: Vec<ProjectAudit>,
    project_alias_groups: Vec<ProjectAliasGroup>,
}

#[derive(Debug, Serialize)]
struct StoreStatsReport {
    total_chunks: usize,
    active_chunks: usize,
    candidate_chunks: usize,
    deleted_chunks: usize,
    chunk_types_active: BTreeMap<String, usize>,
    chunk_types_deleted: BTreeMap<String, usize>,
    chunk_types_all: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct TenantDiskReport {
    total_bytes: u64,
    segment_count: usize,
}

#[derive(Debug, Default, Serialize)]
struct AgeBuckets {
    last_24h: usize,
    last_7d: usize,
    last_30d: usize,
    older_30d: usize,
    missing_timestamp: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectAudit {
    project_id: Option<String>,
    chunks: usize,
    generated_digest_chunks: usize,
    generated_wrapper_chunks: usize,
    routine_progress_chunks: usize,
    unbounded_progress_chunks: usize,
    unbounded_progress_older_30d: usize,
    unscoped: bool,
}

#[derive(Debug, Serialize)]
struct ProjectAliasGroup {
    normalized: String,
    variants: Vec<ProjectAliasVariant>,
}

#[derive(Debug, Serialize)]
struct ProjectAliasVariant {
    project_id: String,
    chunks: usize,
}

#[derive(Debug, Default)]
struct ChunkSummary {
    scanned_chunks: usize,
    generated_digest_chunks: usize,
    generated_wrapper_chunks: usize,
    routine_progress_chunks: usize,
    unbounded_progress_chunks: usize,
    unbounded_progress_older_30d: usize,
    unscoped_chunks: usize,
    age_buckets: AgeBuckets,
    chunk_types: BTreeMap<String, usize>,
    kind_tags: BTreeMap<String, usize>,
    projects: HashMap<Option<String>, ProjectAccumulator>,
}

#[derive(Debug, Clone, Default)]
struct ProjectAccumulator {
    chunks: usize,
    generated_digest_chunks: usize,
    generated_wrapper_chunks: usize,
    routine_progress_chunks: usize,
    unbounded_progress_chunks: usize,
    unbounded_progress_older_30d: usize,
}

#[derive(Debug, Clone)]
pub(super) struct ScannedChunk {
    pub(super) chunk: MemoryChunk,
    expires_at_ms: Option<i64>,
}

pub(super) async fn run_audit<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: AuditOptions,
) -> Result<AuditReport> {
    let page_size = options.page_size.clamp(1, 10_000);
    let duplicate_examples = options.duplicate_examples.min(50);
    let top_projects = options.top_projects.max(1);
    let tenants = resolve_tenants(store, tenant_manager, options.tenant_id.as_deref()).await?;
    let data_dir = tenant_manager.map(|tm| tm.data_dir().display().to_string());
    let storage = tenant_manager.and_then(|tm| storage_report(tm.data_dir()).ok());

    let mut reports = Vec::with_capacity(tenants.len());
    let mut totals = AuditTotals {
        tenant_count: tenants.len(),
        ..Default::default()
    };
    let now_ms = now_ms();

    for tenant in tenants {
        let stats = store.stats(&tenant).await?;
        let tenant_active_chunks = stats.active_chunks;
        let health = store
            .health_snapshot(&tenant, options.project_id.as_deref(), duplicate_examples)
            .await?
            .map(|mut snapshot| {
                snapshot.duplicates.examples.truncate(duplicate_examples);
                snapshot
            });
        let disk = tenant_manager
            .and_then(|tm| tm.tenant_disk_stats(&tenant).ok())
            .map(|stats| TenantDiskReport {
                total_bytes: stats.total_bytes,
                segment_count: stats.segment_count,
            });
        let chunks =
            collect_scanned_chunks(store, &tenant, options.project_id.as_deref(), page_size)
                .await?;
        let summary = summarize_chunks(&chunks, now_ms);
        let metadata_active_chunks = health
            .as_ref()
            .map(|snapshot| snapshot.counts.active_chunks)
            .unwrap_or_else(|| {
                if options.project_id.is_none() {
                    tenant_active_chunks
                } else {
                    summary.scanned_chunks
                }
            });
        let unreadable_active_chunks =
            metadata_active_chunks.saturating_sub(summary.scanned_chunks);

        totals.metadata_active_chunks += metadata_active_chunks;
        totals.scanned_chunks += summary.scanned_chunks;
        totals.unreadable_active_chunks += unreadable_active_chunks;
        totals.generated_digest_chunks += summary.generated_digest_chunks;
        totals.generated_wrapper_chunks += summary.generated_wrapper_chunks;
        totals.routine_progress_chunks += summary.routine_progress_chunks;
        totals.unbounded_progress_chunks += summary.unbounded_progress_chunks;
        totals.unbounded_progress_older_30d += summary.unbounded_progress_older_30d;
        totals.unscoped_chunks += summary.unscoped_chunks;

        reports.push(TenantAudit {
            tenant_id: tenant.to_string(),
            project_id_filter: options.project_id.clone(),
            stats: StoreStatsReport::from_stats(stats),
            disk,
            health,
            metadata_active_chunks,
            scanned_chunks: summary.scanned_chunks,
            unreadable_active_chunks,
            readable_active_ratio: ratio(summary.scanned_chunks, metadata_active_chunks),
            generated_digest_chunks: summary.generated_digest_chunks,
            generated_digest_ratio: ratio(summary.generated_digest_chunks, summary.scanned_chunks),
            generated_wrapper_chunks: summary.generated_wrapper_chunks,
            generated_wrapper_ratio: ratio(
                summary.generated_wrapper_chunks,
                summary.scanned_chunks,
            ),
            routine_progress_chunks: summary.routine_progress_chunks,
            unbounded_progress_chunks: summary.unbounded_progress_chunks,
            unbounded_progress_older_30d: summary.unbounded_progress_older_30d,
            unscoped_chunks: summary.unscoped_chunks,
            age_buckets: summary.age_buckets,
            chunk_types_scanned: summary.chunk_types,
            kind_tags: summary.kind_tags,
            projects: render_project_rows(summary.projects.clone(), top_projects),
            project_alias_groups: alias_groups(&summary.projects),
        });
    }

    Ok(AuditReport {
        generated_unix_ms: now_ms,
        data_dir,
        storage,
        totals,
        tenants: reports,
    })
}

pub(super) fn render_audit_report(report: &AuditReport, format: ExportFormat) -> Result<String> {
    match format {
        ExportFormat::Json => Ok(format!("{}\n", serde_json::to_string_pretty(report)?)),
        ExportFormat::Jsonl => {
            let mut out = String::new();
            for tenant in &report.tenants {
                out.push_str(&serde_json::to_string(tenant)?);
                out.push('\n');
            }
            Ok(out)
        }
        ExportFormat::Markdown => Ok(render_markdown(report)),
    }
}

pub(super) fn strict_should_fail(report: &AuditReport) -> bool {
    report.totals.unreadable_active_chunks > 0
}

pub(super) async fn resolve_tenants<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    tenant_id: Option<&str>,
) -> Result<Vec<TenantId>> {
    if let Some(tenant_id) = tenant_id {
        return Ok(vec![TenantId::new(tenant_id)?]);
    }

    let mut tenants = BTreeMap::new();
    for tenant in store.list_tenants().await? {
        tenants.insert(tenant.to_string(), tenant);
    }
    if let Some(tm) = tenant_manager {
        for tenant in tm.list_tenants()? {
            tenants.insert(tenant.to_string(), tenant);
        }
    }
    Ok(tenants.into_values().collect())
}

pub(super) async fn collect_scanned_chunks<S: Store>(
    store: &S,
    tenant: &TenantId,
    project_id: Option<&str>,
    page_size: usize,
) -> Result<Vec<ScannedChunk>> {
    let mut chunks = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = if let Some(persistent) = store.as_persistent() {
            let metadata_rows = persistent
                .metadata()
                .list_for_project(tenant, project_id, page_size, offset)?;
            let mut rows = Vec::with_capacity(metadata_rows.len());
            for meta in metadata_rows {
                if let Some(chunk) = persistent
                    .get_chunk_for_retrieval(tenant, &meta.chunk_id, "audit")
                    .await?
                {
                    rows.push(ScannedChunk {
                        chunk,
                        expires_at_ms: meta.lifecycle.expires_at_ms,
                    });
                }
            }
            rows
        } else {
            store
                .list_chunks_for_project(tenant, project_id, page_size, offset)
                .await?
                .into_iter()
                .map(|chunk| ScannedChunk {
                    chunk,
                    expires_at_ms: None,
                })
                .collect()
        };
        if page.is_empty() {
            break;
        }
        let got = page.len();
        chunks.extend(page);
        if got < page_size {
            break;
        }
        offset = offset.saturating_add(got);
    }
    Ok(chunks)
}

fn summarize_chunks(chunks: &[ScannedChunk], now_ms: i64) -> ChunkSummary {
    let mut summary = ChunkSummary::default();
    for scanned in chunks {
        let chunk = &scanned.chunk;
        summary.scanned_chunks += 1;
        let project_id = chunk.project_id.as_option().map(str::to_string);
        if project_id.is_none() {
            summary.unscoped_chunks += 1;
        }
        *summary
            .chunk_types
            .entry(chunk.chunk_type.to_string())
            .or_insert(0) += 1;
        for tag in &chunk.tags {
            if tag.starts_with("kind:") {
                *summary.kind_tags.entry(tag.clone()).or_insert(0) += 1;
            }
        }

        let generated_digest = is_generated_digest(&chunk.tags);
        let generated_wrapper = is_generated_wrapper_text(&chunk.text);
        if generated_digest {
            summary.generated_digest_chunks += 1;
        }
        if generated_wrapper {
            summary.generated_wrapper_chunks += 1;
        }
        record_age(&mut summary.age_buckets, chunk.timestamp_created, now_ms);
        let routine_progress = is_routine_progress_summary(chunk);
        if routine_progress {
            summary.routine_progress_chunks += 1;
            if scanned.expires_at_ms.is_none() {
                summary.unbounded_progress_chunks += 1;
                if is_older_than_days(chunk.timestamp_created, now_ms, 30) {
                    summary.unbounded_progress_older_30d += 1;
                }
            }
        }

        let project = summary.projects.entry(project_id.clone()).or_default();
        project.chunks += 1;
        if generated_digest {
            project.generated_digest_chunks += 1;
        }
        if generated_wrapper {
            project.generated_wrapper_chunks += 1;
        }
        if routine_progress {
            project.routine_progress_chunks += 1;
            if scanned.expires_at_ms.is_none() {
                project.unbounded_progress_chunks += 1;
                if is_older_than_days(chunk.timestamp_created, now_ms, 30) {
                    project.unbounded_progress_older_30d += 1;
                }
            }
        }
    }
    summary
}

fn record_age(buckets: &mut AgeBuckets, timestamp_created: i64, now_ms: i64) {
    if timestamp_created <= 0 || now_ms <= 0 {
        buckets.missing_timestamp += 1;
        return;
    }
    let age_ms = now_ms.saturating_sub(timestamp_created);
    const DAY_MS: i64 = 86_400_000;
    if age_ms <= DAY_MS {
        buckets.last_24h += 1;
    } else if age_ms <= 7 * DAY_MS {
        buckets.last_7d += 1;
    } else if age_ms <= 30 * DAY_MS {
        buckets.last_30d += 1;
    } else {
        buckets.older_30d += 1;
    }
}

fn is_older_than_days(timestamp_created: i64, now_ms: i64, days: i64) -> bool {
    if timestamp_created <= 0 || now_ms <= 0 {
        return false;
    }
    let age_ms = now_ms.saturating_sub(timestamp_created);
    age_ms > days.saturating_mul(86_400_000)
}

fn is_generated_digest(tags: &[String]) -> bool {
    let generated = tags.iter().any(|tag| tag == "task:status:generated");
    let digest = tags
        .iter()
        .any(|tag| tag.starts_with("task:role:") || tag.starts_with("task:digest:"));
    generated && digest
}

fn is_generated_wrapper_text(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.starts_with("task digest status generated")
        || lowered.contains("artifact role: highlight_library")
        || lowered.contains("artifact role: failure_library")
        || lowered.contains("artifact role: decision_library")
        || lowered.contains("artifact role: evidence_library")
        || lowered.contains("highlight library for ")
        || lowered.contains("failure library for ")
        || lowered.contains("decision library for ")
        || lowered.contains("evidence library for ")
}

fn is_routine_progress_summary(chunk: &MemoryChunk) -> bool {
    chunk.chunk_type == ChunkType::Summary
        && chunk.tags.iter().any(|tag| tag == "kind:progress")
        && !has_durable_progress_override(&chunk.tags)
}

fn has_durable_progress_override(tags: &[String]) -> bool {
    tags.iter().any(|tag| {
        tag.starts_with("priority:")
            || tag.starts_with("importance:")
            || matches!(
                tag.as_str(),
                "kind:evidence"
                    | "kind:decision"
                    | "kind:finish"
                    | "kind:consolidated"
                    | "retention:durable"
                    | "validated:true"
                    | "supports:true"
            )
            || tag.starts_with("evidence:")
            || tag.starts_with("source:evidence")
    })
}

fn render_project_rows(
    projects: HashMap<Option<String>, ProjectAccumulator>,
    top_projects: usize,
) -> Vec<ProjectAudit> {
    let mut rows = projects
        .into_iter()
        .map(|(project_id, acc)| ProjectAudit {
            unscoped: project_id.is_none(),
            project_id,
            chunks: acc.chunks,
            generated_digest_chunks: acc.generated_digest_chunks,
            generated_wrapper_chunks: acc.generated_wrapper_chunks,
            routine_progress_chunks: acc.routine_progress_chunks,
            unbounded_progress_chunks: acc.unbounded_progress_chunks,
            unbounded_progress_older_30d: acc.unbounded_progress_older_30d,
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .chunks
            .cmp(&left.chunks)
            .then_with(|| left.project_id.cmp(&right.project_id))
    });
    rows.truncate(top_projects);
    rows
}

fn alias_groups(projects: &HashMap<Option<String>, ProjectAccumulator>) -> Vec<ProjectAliasGroup> {
    let mut grouped: BTreeMap<String, Vec<ProjectAliasVariant>> = BTreeMap::new();
    for (project_id, acc) in projects {
        let Some(project_id) = project_id else {
            continue;
        };
        let normalized = normalize_project_id(project_id);
        if normalized.is_empty() {
            continue;
        }
        grouped
            .entry(normalized)
            .or_default()
            .push(ProjectAliasVariant {
                project_id: project_id.clone(),
                chunks: acc.chunks,
            });
    }

    let mut groups = grouped
        .into_iter()
        .filter_map(|(normalized, mut variants)| {
            if variants.len() < 2 {
                return None;
            }
            variants.sort_by(|left, right| {
                right
                    .chunks
                    .cmp(&left.chunks)
                    .then_with(|| left.project_id.cmp(&right.project_id))
            });
            Some(ProjectAliasGroup {
                normalized,
                variants,
            })
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        let left_total: usize = left.variants.iter().map(|v| v.chunks).sum();
        let right_total: usize = right.variants.iter().map(|v| v.chunks).sum();
        right_total
            .cmp(&left_total)
            .then_with(|| left.normalized.cmp(&right.normalized))
    });
    groups
}

fn normalize_project_id(project_id: &str) -> String {
    project_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

pub(super) fn storage_report(data_dir: &Path) -> Result<StorageReport> {
    Ok(StorageReport {
        total_bytes: path_size(data_dir)?,
        metadata_db_bytes: path_size(&data_dir.join("metadata.db"))?,
        sparse_index_bytes: path_size(&data_dir.join("sparse_index"))?,
        tenants_bytes: path_size(&data_dir.join("tenants"))?,
        warm_bytes: path_size(&data_dir.join("warm"))?,
    })
}

fn path_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let meta = path.metadata().map_err(MemdError::IoError)?;
    if meta.is_file() {
        return Ok(meta.len());
    }
    if !meta.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path).map_err(MemdError::IoError)? {
        let entry = entry.map_err(MemdError::IoError)?;
        total = total.saturating_add(path_size(&entry.path())?);
    }
    Ok(total)
}

fn render_markdown(report: &AuditReport) -> String {
    let mut out = String::new();
    out.push_str("# memd audit\n\n");
    out.push_str(&format!(
        "- generated_unix_ms: `{}`\n",
        report.generated_unix_ms
    ));
    if let Some(data_dir) = &report.data_dir {
        out.push_str(&format!("- data_dir: `{data_dir}`\n"));
    }
    out.push_str(&format!(
        "- tenants: `{}`; metadata_active_chunks: `{}`; scanned_chunks: `{}`; unreadable_active_chunks: `{}`; generated_digest_chunks: `{}`; generated_wrapper_chunks: `{}`; routine_progress_chunks: `{}`; unbounded_progress_chunks: `{}`; unbounded_progress_older_30d: `{}`; unscoped_chunks: `{}`\n",
        report.totals.tenant_count,
        report.totals.metadata_active_chunks,
        report.totals.scanned_chunks,
        report.totals.unreadable_active_chunks,
        report.totals.generated_digest_chunks,
        report.totals.generated_wrapper_chunks,
        report.totals.routine_progress_chunks,
        report.totals.unbounded_progress_chunks,
        report.totals.unbounded_progress_older_30d,
        report.totals.unscoped_chunks
    ));
    if let Some(storage) = &report.storage {
        out.push_str("\n## Storage\n\n");
        out.push_str(&format!("- total_bytes: `{}`\n", storage.total_bytes));
        out.push_str(&format!(
            "- metadata_db_bytes: `{}`\n",
            storage.metadata_db_bytes
        ));
        out.push_str(&format!(
            "- sparse_index_bytes: `{}`\n",
            storage.sparse_index_bytes
        ));
        out.push_str(&format!("- tenants_bytes: `{}`\n", storage.tenants_bytes));
        out.push_str(&format!("- warm_bytes: `{}`\n", storage.warm_bytes));
    }

    for tenant in &report.tenants {
        out.push_str(&format!("\n## Tenant `{}`\n\n", tenant.tenant_id));
        if let Some(project_id) = &tenant.project_id_filter {
            out.push_str(&format!("- project_id_filter: `{project_id}`\n"));
        }
        out.push_str(&format!(
            "- stats_total_chunks: `{}`; active: `{}`; deleted: `{}`\n",
            tenant.stats.total_chunks, tenant.stats.active_chunks, tenant.stats.deleted_chunks
        ));
        if let Some(disk) = &tenant.disk {
            out.push_str(&format!(
                "- disk_total_bytes: `{}`; segment_count: `{}`\n",
                disk.total_bytes, disk.segment_count
            ));
        }
        out.push_str(&format!(
            "- metadata_active_in_scope: `{}`; scanned_chunks: `{}`; unreadable_active: `{}`; readable_active_ratio: `{:.3}`\n",
            tenant.metadata_active_chunks,
            tenant.scanned_chunks,
            tenant.unreadable_active_chunks,
            tenant.readable_active_ratio
        ));
        out.push_str(&format!(
            "- generated_digest: `{}` ({:.1}%); generated_wrappers: `{}` ({:.1}%); unscoped: `{}`\n",
            tenant.generated_digest_chunks,
            tenant.generated_digest_ratio * 100.0,
            tenant.generated_wrapper_chunks,
            tenant.generated_wrapper_ratio * 100.0,
            tenant.unscoped_chunks
        ));
        out.push_str(&format!(
            "- routine_progress: `{}`; unbounded_progress: `{}`; unbounded_progress_older_30d: `{}`\n",
            tenant.routine_progress_chunks,
            tenant.unbounded_progress_chunks,
            tenant.unbounded_progress_older_30d
        ));
        out.push_str(&format!(
            "- age_buckets: last_24h=`{}`, last_7d=`{}`, last_30d=`{}`, older_30d=`{}`, missing=`{}`\n",
            tenant.age_buckets.last_24h,
            tenant.age_buckets.last_7d,
            tenant.age_buckets.last_30d,
            tenant.age_buckets.older_30d,
            tenant.age_buckets.missing_timestamp
        ));
        if let Some(health) = &tenant.health {
            out.push_str(&format!(
                "- duplicate_rows: `{}` ({:.1}%); duplicate_groups: `{}`; index_indexed: `{:.1}%`\n",
                health.duplicates.duplicate_row_count,
                health.duplicates.duplicate_row_ratio * 100.0,
                health.duplicates.exact_duplicate_group_count,
                health.index_coverage.indexed_percentage
            ));
        }
        if !tenant.projects.is_empty() {
            out.push_str("\n### Projects\n\n");
            for project in &tenant.projects {
                let label = project.project_id.as_deref().unwrap_or("<unscoped>");
                out.push_str(&format!(
                    "- `{label}`: chunks=`{}`, generated_digest=`{}`, generated_wrappers=`{}`, routine_progress=`{}`, unbounded_progress=`{}`, unbounded_progress_older_30d=`{}`\n",
                    project.chunks,
                    project.generated_digest_chunks,
                    project.generated_wrapper_chunks,
                    project.routine_progress_chunks,
                    project.unbounded_progress_chunks,
                    project.unbounded_progress_older_30d
                ));
            }
        }
        if !tenant.project_alias_groups.is_empty() {
            out.push_str("\n### Project Alias Candidates\n\n");
            for group in &tenant.project_alias_groups {
                let variants = group
                    .variants
                    .iter()
                    .map(|variant| format!("{} ({})", variant.project_id, variant.chunks))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("- `{}`: {variants}\n", group.normalized));
            }
        }
    }

    out
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

impl StoreStatsReport {
    fn from_stats(stats: StoreStats) -> Self {
        Self {
            total_chunks: stats.total_chunks,
            active_chunks: stats.active_chunks,
            candidate_chunks: stats.candidate_chunks,
            deleted_chunks: stats.deleted_chunks,
            chunk_types_active: to_btree(stats.chunk_types_active),
            chunk_types_deleted: to_btree(stats.chunk_types_deleted),
            chunk_types_all: to_btree(stats.chunk_types_all),
        }
    }
}

fn to_btree(map: HashMap<String, usize>) -> BTreeMap<String, usize> {
    map.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChunkType, ProjectId};

    fn chunk(text: &str, project_id: Option<&str>, tags: Vec<&str>, timestamp: i64) -> MemoryChunk {
        let mut chunk =
            MemoryChunk::new(TenantId::new("tenant").unwrap(), text, ChunkType::Summary);
        chunk.timestamp_created = timestamp;
        if let Some(project_id) = project_id {
            chunk = chunk.with_project(ProjectId::from(project_id));
        }
        chunk.with_tags(tags.into_iter().map(str::to_string).collect())
    }

    fn scanned(chunk: MemoryChunk, expires_at_ms: Option<i64>) -> ScannedChunk {
        ScannedChunk {
            chunk,
            expires_at_ms,
        }
    }

    #[test]
    fn strict_should_fail_tracks_unreadable_active_chunks() {
        let mut report = AuditReport {
            generated_unix_ms: 1,
            data_dir: None,
            storage: None,
            totals: AuditTotals::default(),
            tenants: Vec::new(),
        };
        assert!(!strict_should_fail(&report));

        report.totals.unreadable_active_chunks = 1;
        assert!(strict_should_fail(&report));
    }

    #[test]
    fn summary_counts_generated_digest_wrappers_and_aliases() {
        let now = 100 * 86_400_000;
        let chunks = vec![
            scanned(chunk(
                "Task digest status generated for task digest_task_highlight_library::project_bester_hosting_highlight_library.\nArtifact role: highlight_library\nSummary: Highlight library for bester_hosting contains 0 ranked lessons.",
                Some("bester_hosting"),
                vec!["task:status:generated", "task:role:highlight_library"],
                now - 1_000,
            ), None),
            scanned(chunk(
                "Validation: tailscale status shows bester-server online.",
                Some("bester-hosting"),
                vec!["kind:evidence"],
                now - 2 * 86_400_000,
            ), None),
            scanned(chunk(
                "Decision: keep userspace tailscale with TS_EXTRA_ARGS.",
                None,
                vec!["kind:decision"],
                now - 40 * 86_400_000,
            ), None),
        ];

        let summary = summarize_chunks(&chunks, now);
        assert_eq!(summary.scanned_chunks, 3);
        assert_eq!(summary.generated_digest_chunks, 1);
        assert_eq!(summary.generated_wrapper_chunks, 1);
        assert_eq!(summary.unscoped_chunks, 1);
        assert_eq!(summary.age_buckets.last_24h, 1);
        assert_eq!(summary.age_buckets.last_7d, 1);
        assert_eq!(summary.age_buckets.older_30d, 1);

        let aliases = alias_groups(&summary.projects);
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].normalized, "besterhosting");
        let variants = aliases[0]
            .variants
            .iter()
            .map(|variant| variant.project_id.as_str())
            .collect::<Vec<_>>();
        assert!(variants.contains(&"bester-hosting"));
        assert!(variants.contains(&"bester_hosting"));
    }

    #[test]
    fn summary_counts_unbounded_routine_progress_debt() {
        let now = 100 * 86_400_000;
        let chunks = vec![
            scanned(
                chunk(
                    "Mapped auth middleware touchpoints; next step is validation.",
                    Some("auth"),
                    vec!["kind:progress"],
                    now - 40 * 86_400_000,
                ),
                None,
            ),
            scanned(
                chunk(
                    "Mapped API touchpoints; next step is validation.",
                    Some("api"),
                    vec!["kind:progress"],
                    now - 2 * 86_400_000,
                ),
                Some(now + 14 * 86_400_000),
            ),
            scanned(
                chunk(
                    "Validation: auth tests passed.",
                    Some("auth"),
                    vec!["kind:progress", "kind:evidence"],
                    now - 40 * 86_400_000,
                ),
                None,
            ),
            scanned(
                chunk(
                    "Reusable progress lesson.",
                    Some("auth"),
                    vec!["kind:progress", "priority:8"],
                    now - 40 * 86_400_000,
                ),
                None,
            ),
        ];

        let summary = summarize_chunks(&chunks, now);
        assert_eq!(summary.routine_progress_chunks, 2);
        assert_eq!(summary.unbounded_progress_chunks, 1);
        assert_eq!(summary.unbounded_progress_older_30d, 1);
        let auth = summary.projects.get(&Some("auth".to_string())).unwrap();
        assert_eq!(auth.routine_progress_chunks, 1);
        assert_eq!(auth.unbounded_progress_chunks, 1);
        assert_eq!(auth.unbounded_progress_older_30d, 1);
    }

    #[test]
    fn markdown_renders_core_audit_fields() {
        let tenant = TenantAudit {
            tenant_id: "tenant".to_string(),
            project_id_filter: None,
            stats: StoreStatsReport::from_stats(StoreStats {
                total_chunks: 3,
                active_chunks: 3,
                ..Default::default()
            }),
            disk: Some(TenantDiskReport {
                total_bytes: 123,
                segment_count: 2,
            }),
            health: None,
            metadata_active_chunks: 4,
            scanned_chunks: 3,
            unreadable_active_chunks: 1,
            readable_active_ratio: 0.75,
            generated_digest_chunks: 1,
            generated_digest_ratio: 1.0 / 3.0,
            generated_wrapper_chunks: 1,
            generated_wrapper_ratio: 1.0 / 3.0,
            routine_progress_chunks: 2,
            unbounded_progress_chunks: 1,
            unbounded_progress_older_30d: 1,
            unscoped_chunks: 1,
            age_buckets: AgeBuckets {
                last_24h: 1,
                last_7d: 1,
                last_30d: 0,
                older_30d: 1,
                missing_timestamp: 0,
            },
            chunk_types_scanned: BTreeMap::new(),
            kind_tags: BTreeMap::new(),
            projects: vec![ProjectAudit {
                project_id: Some("proj".to_string()),
                chunks: 2,
                generated_digest_chunks: 1,
                generated_wrapper_chunks: 1,
                routine_progress_chunks: 2,
                unbounded_progress_chunks: 1,
                unbounded_progress_older_30d: 1,
                unscoped: false,
            }],
            project_alias_groups: Vec::new(),
        };
        let report = AuditReport {
            generated_unix_ms: 1,
            data_dir: Some("/tmp/memd".to_string()),
            storage: Some(StorageReport {
                total_bytes: 999,
                metadata_db_bytes: 100,
                sparse_index_bytes: 200,
                tenants_bytes: 300,
                warm_bytes: 400,
            }),
            totals: AuditTotals {
                tenant_count: 1,
                metadata_active_chunks: 4,
                scanned_chunks: 3,
                unreadable_active_chunks: 1,
                generated_digest_chunks: 1,
                generated_wrapper_chunks: 1,
                routine_progress_chunks: 2,
                unbounded_progress_chunks: 1,
                unbounded_progress_older_30d: 1,
                unscoped_chunks: 1,
            },
            tenants: vec![tenant],
        };

        let rendered = render_markdown(&report);
        assert!(rendered.contains("# memd audit"));
        assert!(rendered.contains("unreadable_active_chunks: `1`"));
        assert!(rendered.contains("readable_active_ratio: `0.750`"));
        assert!(rendered.contains("generated_digest: `1`"));
        assert!(rendered.contains("unbounded_progress_older_30d: `1`"));
        assert!(rendered.contains("## Tenant `tenant`"));
        assert!(rendered.contains("`proj`: chunks=`2`"));
        assert!(rendered.contains("routine_progress=`2`"));
    }
}
