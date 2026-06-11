use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::args::{ExportFormat, ProjectScopeConfig};
use super::paths::absolutize_project_dir;
use crate::error::Result;
use crate::store::metadata::MetadataStore;
use crate::store::{Store, StoreStats, TenantManager};
use crate::types::{ChunkType, MemoryChunk, TenantId};

const PURGE_COMMAND_LIMIT: usize = 10_000;
const RETRIEVAL_QUERIES_PATH: &str = "evals/bench/queries/retrieval_queries.jsonl";

#[derive(Debug, Clone)]
pub(super) struct CleanupPlanOptions {
    pub(super) tenant_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) archive_dir: PathBuf,
    pub(super) older_than_days: u64,
    pub(super) candidate_limit: usize,
    pub(super) page_size: usize,
    pub(super) top_projects: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct CleanupPlanReport {
    generated_unix_ms: i64,
    data_dir: Option<String>,
    archive_dir: String,
    older_than_days: u64,
    candidate_limit: usize,
    safety: CleanupSafety,
    totals: CleanupTotals,
    approval_summary: ApprovalSummary,
    post_cleanup_verification: PostCleanupVerification,
    approval_items: Vec<ApprovalItem>,
    tenants: Vec<TenantCleanupPlan>,
}

#[derive(Debug, Serialize)]
struct CleanupSafety {
    destructive_actions_included: bool,
    approval_required: bool,
    note: String,
}

#[derive(Debug, Default, Serialize)]
struct CleanupTotals {
    tenants_scanned: usize,
    tenants_with_approval_items: usize,
    metadata_active_chunks: usize,
    active_chunks: usize,
    deleted_chunks: usize,
    scanned_chunks: usize,
    unreadable_active_chunks: usize,
    routine_progress_chunks: usize,
    unbounded_progress_chunks: usize,
    unbounded_progress_older_30d: usize,
    hidden_purge_candidates: usize,
    estimated_purge_payload_bytes: usize,
}

#[derive(Debug, Default, Serialize)]
struct ApprovalSummary {
    total_items: usize,
    command_kinds: Vec<ApprovalCommandKindSummary>,
    actions: Vec<ApprovalActionSummary>,
    destructive_command_previews: usize,
    verification_command_previews: usize,
    estimated_batches: usize,
    batch_command_previews: usize,
    unreadable_active_rows_in_purge_previews: usize,
}

#[derive(Debug, Serialize)]
struct ApprovalCommandKindSummary {
    command_kind: String,
    count: usize,
}

#[derive(Debug, Serialize)]
struct ApprovalActionSummary {
    action: String,
    count: usize,
    command_kinds: Vec<ApprovalCommandKindSummary>,
    scope_chunks: usize,
    metadata_active_chunks: usize,
    unreadable_active_chunks: usize,
    generated_digest_chunks: usize,
    generated_wrapper_chunks: usize,
    hidden_purge_candidates: usize,
    destructive_command_previews: usize,
    verification_command_previews: usize,
    estimated_batches: usize,
    batch_command_previews: usize,
    example_approval_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PostCleanupVerification {
    note: String,
    commands: Vec<PostCleanupVerificationCommand>,
}

#[derive(Debug, Serialize)]
struct PostCleanupVerificationCommand {
    label: String,
    command: String,
    pass_criteria: String,
}

#[derive(Debug, Default)]
struct ApprovalActionAccumulator {
    count: usize,
    command_kinds: BTreeMap<String, usize>,
    scope_chunks: usize,
    metadata_active_chunks: usize,
    unreadable_active_chunks: usize,
    generated_digest_chunks: usize,
    generated_wrapper_chunks: usize,
    hidden_purge_candidates: usize,
    destructive_command_previews: usize,
    verification_command_previews: usize,
    estimated_batches: usize,
    batch_command_previews: usize,
    example_approval_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TenantCleanupPlan {
    tenant_id: String,
    project_id_filter: Option<String>,
    disk_total_bytes: Option<u64>,
    stats: StoreStatsReport,
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
    hidden_purge_candidates: usize,
    estimated_purge_payload_bytes: usize,
    classification: Vec<String>,
    reasons: Vec<String>,
    export_command: String,
    purge_command_preview: Option<String>,
    projects: Vec<ProjectCleanupPlan>,
}

#[derive(Debug, Serialize)]
struct ProjectCleanupPlan {
    project_id: Option<String>,
    chunks: usize,
    generated_digest_chunks: usize,
    generated_wrapper_chunks: usize,
    routine_progress_chunks: usize,
    unbounded_progress_chunks: usize,
    unbounded_progress_older_30d: usize,
    classification: Vec<String>,
    reasons: Vec<String>,
    export_command: String,
}

#[derive(Debug, Serialize)]
struct StoreStatsReport {
    total_chunks: usize,
    active_chunks: usize,
    deleted_chunks: usize,
}

#[derive(Debug, Serialize)]
struct ApprovalItem {
    approval_id: String,
    action: String,
    tenant_id: String,
    project_id: Option<String>,
    command_kind: String,
    command_is_destructive: bool,
    scope_chunks: usize,
    metadata_active_chunks: usize,
    unreadable_active_chunks: usize,
    tenant_disk_total_bytes: Option<u64>,
    generated_digest_chunks: usize,
    generated_wrapper_chunks: usize,
    generated_digest_ratio: f64,
    generated_wrapper_ratio: f64,
    hidden_purge_candidates: usize,
    estimated_payload_bytes: usize,
    risk: String,
    reason: String,
    command_preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    destructive_command_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_command_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_batches: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    batch_command_previews: Vec<CleanupBatchCommandPreview>,
}

#[derive(Debug, Serialize)]
struct CleanupBatchCommandPreview {
    batch: usize,
    limit: usize,
    archive: String,
    dry_run_command: String,
    destructive_command: String,
    verification_command: String,
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
struct ScannedChunk {
    chunk: MemoryChunk,
    expires_at_ms: Option<i64>,
}

pub(super) async fn run_cleanup_plan<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: CleanupPlanOptions,
) -> Result<CleanupPlanReport> {
    let page_size = options.page_size.clamp(1, 10_000);
    let top_projects = options.top_projects.max(1);
    let older_than_days = options.older_than_days.max(1);
    let candidate_limit = options.candidate_limit.clamp(1, 10_000);
    let tenants = resolve_tenants(store, tenant_manager, options.tenant_id.as_deref()).await?;
    let data_dir = tenant_manager.map(|tm| tm.data_dir().display().to_string());
    let archive_dir = options.archive_dir.display().to_string();
    let generated_unix_ms = now_ms();
    let cutoff_ms =
        generated_unix_ms.saturating_sub((older_than_days as i64).saturating_mul(86_400_000));
    let mut tenant_plans = Vec::with_capacity(tenants.len());
    let mut totals = CleanupTotals {
        tenants_scanned: tenants.len(),
        ..Default::default()
    };

    for tenant in tenants {
        let stats = store.stats(&tenant).await?;
        let disk_total_bytes = tenant_manager
            .and_then(|tm| tm.tenant_disk_stats(&tenant).ok())
            .map(|stats| stats.total_bytes);
        let chunks =
            collect_scanned_chunks(store, &tenant, options.project_id.as_deref(), page_size)
                .await?;
        let summary = summarize_chunks(&chunks, generated_unix_ms);
        let metadata_active_chunks = scoped_metadata_active_chunks(
            store,
            &tenant,
            options.project_id.as_deref(),
            stats.active_chunks,
            summary.scanned_chunks,
        )
        .await?;
        let unreadable_active_chunks =
            metadata_active_chunks.saturating_sub(summary.scanned_chunks);
        let purge = hidden_purge_summary(
            store,
            &tenant,
            options.project_id.as_deref(),
            cutoff_ms,
            candidate_limit,
        )
        .await?;
        let projects = render_project_plans(
            tenant.as_str(),
            &summary.projects,
            &options.archive_dir,
            top_projects,
        );
        let (classification, reasons) = classify_tenant(
            tenant.as_str(),
            &stats,
            &summary,
            &purge,
            unreadable_active_chunks,
        );
        let export_command = export_command(
            tenant.as_str(),
            options.project_id.as_deref(),
            &options.archive_dir,
        );
        let purge_command_preview = (purge.candidate_count > 0).then(|| {
            purge_command(
                tenant.as_str(),
                options.project_id.as_deref(),
                older_than_days,
                &options.archive_dir,
            )
        });

        totals.active_chunks += stats.active_chunks;
        totals.deleted_chunks += stats.deleted_chunks;
        totals.metadata_active_chunks += metadata_active_chunks;
        totals.scanned_chunks += summary.scanned_chunks;
        totals.unreadable_active_chunks += unreadable_active_chunks;
        totals.routine_progress_chunks += summary.routine_progress_chunks;
        totals.unbounded_progress_chunks += summary.unbounded_progress_chunks;
        totals.unbounded_progress_older_30d += summary.unbounded_progress_older_30d;
        totals.hidden_purge_candidates += purge.candidate_count;
        totals.estimated_purge_payload_bytes += purge.estimated_payload_bytes;

        tenant_plans.push(TenantCleanupPlan {
            tenant_id: tenant.to_string(),
            project_id_filter: options.project_id.clone(),
            disk_total_bytes,
            stats: StoreStatsReport::from_stats(stats),
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
            hidden_purge_candidates: purge.candidate_count,
            estimated_purge_payload_bytes: purge.estimated_payload_bytes,
            classification,
            reasons,
            export_command,
            purge_command_preview,
            projects,
        });
    }

    tenant_plans.sort_by(|left, right| {
        action_weight(&right.classification)
            .cmp(&action_weight(&left.classification))
            .then_with(|| {
                right
                    .disk_total_bytes
                    .unwrap_or_default()
                    .cmp(&left.disk_total_bytes.unwrap_or_default())
            })
            .then_with(|| right.scanned_chunks.cmp(&left.scanned_chunks))
            .then_with(|| left.tenant_id.cmp(&right.tenant_id))
    });

    let approval_items = approval_items(&tenant_plans, &options.archive_dir);
    totals.tenants_with_approval_items = tenant_plans
        .iter()
        .filter(|tenant| {
            tenant
                .classification
                .iter()
                .any(|class| class != "keep_by_default")
        })
        .count();

    let verification_project_scope = read_project_scope_for_verification(&options.project_dir);
    let has_retrieval_queries = retrieval_queries_available(&options.project_dir);

    Ok(CleanupPlanReport {
        generated_unix_ms,
        data_dir,
        archive_dir,
        older_than_days,
        candidate_limit,
        safety: CleanupSafety {
            destructive_actions_included: false,
            approval_required: true,
            note: "This is a dry-run planning report. It emits command previews only; run export/purge commands manually after approving the exact tenant/project list.".to_string(),
        },
        totals,
        approval_summary: approval_summary(&approval_items),
        post_cleanup_verification: post_cleanup_verification_commands(
            options.tenant_id.as_deref(),
            options.project_id.as_deref(),
            &options.project_dir,
            &options.archive_dir,
            older_than_days,
            candidate_limit,
            page_size,
            top_projects,
            verification_project_scope.as_ref(),
            has_retrieval_queries,
        ),
        approval_items,
        tenants: tenant_plans,
    })
}

pub(super) fn render_cleanup_plan(
    report: &CleanupPlanReport,
    format: ExportFormat,
) -> Result<String> {
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

async fn resolve_tenants<S: Store>(
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

async fn collect_scanned_chunks<S: Store>(
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
                    .get_chunk_for_retrieval(tenant, &meta.chunk_id, "cleanup_plan")
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

async fn scoped_metadata_active_chunks<S: Store>(
    store: &S,
    tenant: &TenantId,
    project_id: Option<&str>,
    tenant_active_chunks: usize,
    scanned_chunks: usize,
) -> Result<usize> {
    if let Some(snapshot) = store.health_snapshot(tenant, project_id, 0).await? {
        return Ok(snapshot.counts.active_chunks);
    }
    if project_id.is_none() {
        Ok(tenant_active_chunks)
    } else {
        Ok(scanned_chunks)
    }
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
        let generated_digest = is_generated_digest(&chunk.tags);
        let generated_wrapper = is_generated_wrapper_text(&chunk.text);
        if generated_digest {
            summary.generated_digest_chunks += 1;
        }
        if generated_wrapper {
            summary.generated_wrapper_chunks += 1;
        }
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

        let project = summary.projects.entry(project_id).or_default();
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

#[derive(Debug, Default)]
struct HiddenPurgeSummary {
    candidate_count: usize,
    estimated_payload_bytes: usize,
}

async fn hidden_purge_summary<S: Store>(
    store: &S,
    tenant: &TenantId,
    project_id: Option<&str>,
    cutoff_ms: i64,
    limit: usize,
) -> Result<HiddenPurgeSummary> {
    let Some(persistent) = store.as_persistent() else {
        return Ok(HiddenPurgeSummary::default());
    };
    let candidates = persistent
        .metadata()
        .list_hard_purge_candidates(tenant, project_id, cutoff_ms, limit)?;
    let mut estimated_payload_bytes = 0usize;
    for candidate in &candidates {
        estimated_payload_bytes += candidate
            .canonical_text
            .as_ref()
            .map(String::len)
            .unwrap_or(0);
    }
    Ok(HiddenPurgeSummary {
        candidate_count: candidates.len(),
        estimated_payload_bytes,
    })
}

fn render_project_plans(
    tenant_id: &str,
    projects: &HashMap<Option<String>, ProjectAccumulator>,
    archive_dir: &Path,
    top_projects: usize,
) -> Vec<ProjectCleanupPlan> {
    let mut rows = projects
        .iter()
        .map(|(project_id, acc)| {
            let (classification, reasons) = classify_project(project_id.as_deref(), acc);
            ProjectCleanupPlan {
                project_id: project_id.clone(),
                chunks: acc.chunks,
                generated_digest_chunks: acc.generated_digest_chunks,
                generated_wrapper_chunks: acc.generated_wrapper_chunks,
                routine_progress_chunks: acc.routine_progress_chunks,
                unbounded_progress_chunks: acc.unbounded_progress_chunks,
                unbounded_progress_older_30d: acc.unbounded_progress_older_30d,
                classification,
                reasons,
                export_command: export_command(tenant_id, project_id.as_deref(), archive_dir),
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        action_weight(&right.classification)
            .cmp(&action_weight(&left.classification))
            .then_with(|| right.chunks.cmp(&left.chunks))
            .then_with(|| left.project_id.cmp(&right.project_id))
    });
    rows.truncate(top_projects);
    rows
}

fn classify_tenant(
    tenant_id: &str,
    stats: &StoreStats,
    summary: &ChunkSummary,
    purge: &HiddenPurgeSummary,
    unreadable_active_chunks: usize,
) -> (Vec<String>, Vec<String>) {
    let mut classes = Vec::new();
    let mut reasons = Vec::new();
    if let Some(reason) = throwaway_name_reason(tenant_id) {
        classes.push("archive_delete_candidate".to_string());
        reasons.push(format!("tenant name matches throwaway pattern: {reason}"));
    }
    if stats.active_chunks == 0 && stats.total_chunks > 0 {
        classes.push("archive_delete_candidate".to_string());
        reasons.push("tenant has no active chunks".to_string());
    }
    if purge.candidate_count > 0 {
        classes.push("hard_purge_ready".to_string());
        reasons.push(format!(
            "{} hidden rows are older than the cutoff and eligible for archive-first purge",
            purge.candidate_count
        ));
    }
    if unreadable_active_chunks > 0 {
        classes.push("payload_integrity_review".to_string());
        reasons.push(format!(
            "{unreadable_active_chunks} active metadata rows could not be loaded from payload segments during the scan"
        ));
    }
    if summary.scanned_chunks > 0 {
        let digest_ratio = ratio(summary.generated_digest_chunks, summary.scanned_chunks);
        let wrapper_ratio = ratio(summary.generated_wrapper_chunks, summary.scanned_chunks);
        if digest_ratio >= 0.75 || wrapper_ratio >= 0.05 {
            classes.push("high_noise_review".to_string());
            reasons.push(format!(
                "generated digest ratio {:.1}%, generated wrapper ratio {:.1}%",
                digest_ratio * 100.0,
                wrapper_ratio * 100.0
            ));
        }
    }
    if summary.unbounded_progress_older_30d > 0 {
        classes.push("legacy_progress_retention_review".to_string());
        reasons.push(format!(
            "{} routine progress summaries older than 30 days have no retention deadline",
            summary.unbounded_progress_older_30d
        ));
    }
    if classes.is_empty() {
        classes.push("keep_by_default".to_string());
        reasons.push(
            "no throwaway pattern, hidden purge candidates, high generated-noise ratio, or legacy unbounded progress detected"
                .to_string(),
        );
    }
    (dedupe(classes), reasons)
}

fn classify_project(
    project_id: Option<&str>,
    acc: &ProjectAccumulator,
) -> (Vec<String>, Vec<String>) {
    let mut classes = Vec::new();
    let mut reasons = Vec::new();
    match project_id {
        Some(project_id) => {
            if let Some(reason) = throwaway_name_reason(project_id) {
                classes.push("archive_delete_candidate".to_string());
                reasons.push(format!("project name matches throwaway pattern: {reason}"));
            }
        }
        None => {
            classes.push("scope_review".to_string());
            reasons.push("project_id is missing".to_string());
        }
    }
    if acc.chunks > 0 {
        let digest_ratio = ratio(acc.generated_digest_chunks, acc.chunks);
        let wrapper_ratio = ratio(acc.generated_wrapper_chunks, acc.chunks);
        if digest_ratio >= 0.75 || wrapper_ratio >= 0.05 {
            classes.push("high_noise_review".to_string());
            reasons.push(format!(
                "generated digest ratio {:.1}%, generated wrapper ratio {:.1}%",
                digest_ratio * 100.0,
                wrapper_ratio * 100.0
            ));
        }
    }
    if acc.unbounded_progress_older_30d > 0 {
        classes.push("legacy_progress_retention_review".to_string());
        reasons.push(format!(
            "{} routine progress summaries older than 30 days have no retention deadline",
            acc.unbounded_progress_older_30d
        ));
    }
    if classes.is_empty() {
        classes.push("keep_by_default".to_string());
        reasons.push(
            "no throwaway pattern, missing scope, high generated-noise ratio, or legacy unbounded progress detected"
                .to_string(),
        );
    }
    (dedupe(classes), reasons)
}

fn approval_items(tenants: &[TenantCleanupPlan], archive_dir: &Path) -> Vec<ApprovalItem> {
    let mut items = Vec::new();
    for tenant in tenants {
        if tenant
            .classification
            .iter()
            .any(|class| class == "hard_purge_ready")
        {
            if let Some(command) = &tenant.purge_command_preview {
                let archive = hidden_purge_archive_path(
                    &tenant.tenant_id,
                    tenant.project_id_filter.as_deref(),
                    archive_dir,
                );
                items.push(ApprovalItem {
                    approval_id: approval_id(
                        "hard_purge_hidden_rows",
                        &tenant.tenant_id,
                        tenant.project_id_filter.as_deref(),
                    ),
                    action: "hard_purge_hidden_rows".to_string(),
                    tenant_id: tenant.tenant_id.clone(),
                    project_id: tenant.project_id_filter.clone(),
                    command_kind: "archive_first_purge".to_string(),
                    command_is_destructive: true,
                    scope_chunks: tenant.scanned_chunks,
                    metadata_active_chunks: tenant.metadata_active_chunks,
                    unreadable_active_chunks: tenant.unreadable_active_chunks,
                    tenant_disk_total_bytes: tenant.disk_total_bytes,
                    generated_digest_chunks: tenant.generated_digest_chunks,
                    generated_wrapper_chunks: tenant.generated_wrapper_chunks,
                    generated_digest_ratio: tenant.generated_digest_ratio,
                    generated_wrapper_ratio: tenant.generated_wrapper_ratio,
                    hidden_purge_candidates: tenant.hidden_purge_candidates,
                    estimated_payload_bytes: tenant.estimated_purge_payload_bytes,
                    risk: "low: hidden/expired/superseded/deleted rows only, archive-first"
                        .to_string(),
                    reason: format!(
                        "{} hidden rows; estimated payload bytes {}",
                        tenant.hidden_purge_candidates, tenant.estimated_purge_payload_bytes
                    ),
                    command_preview: command.clone(),
                    destructive_command_preview: Some(command.clone()),
                    verification_command_preview: Some(purge_archive_verify_command(
                        &archive,
                        &tenant.tenant_id,
                        tenant.project_id_filter.as_deref(),
                        1,
                    )),
                    estimated_batches: Some(1),
                    batch_command_previews: Vec::new(),
                });
            }
        }
        if tenant
            .classification
            .iter()
            .any(|class| class == "payload_integrity_review")
        {
            let first_batch_limit = tenant
                .unreadable_active_chunks
                .clamp(1, PURGE_COMMAND_LIMIT);
            let batch_previews = unreadable_purge_batch_previews(
                &tenant.tenant_id,
                tenant.project_id_filter.as_deref(),
                tenant.unreadable_active_chunks,
                archive_dir,
            );
            items.push(ApprovalItem {
                approval_id: approval_id(
                    "review_payload_integrity_tenant",
                    &tenant.tenant_id,
                    tenant.project_id_filter.as_deref(),
                ),
                action: "review_payload_integrity_tenant".to_string(),
                tenant_id: tenant.tenant_id.clone(),
                project_id: tenant.project_id_filter.clone(),
                command_kind: "dry_run_unreadable_purge".to_string(),
                command_is_destructive: false,
                scope_chunks: tenant.scanned_chunks,
                metadata_active_chunks: tenant.metadata_active_chunks,
                unreadable_active_chunks: tenant.unreadable_active_chunks,
                tenant_disk_total_bytes: tenant.disk_total_bytes,
                generated_digest_chunks: tenant.generated_digest_chunks,
                generated_wrapper_chunks: tenant.generated_wrapper_chunks,
                generated_digest_ratio: tenant.generated_digest_ratio,
                generated_wrapper_ratio: tenant.generated_wrapper_ratio,
                hidden_purge_candidates: tenant.hidden_purge_candidates,
                estimated_payload_bytes: tenant.estimated_purge_payload_bytes,
                risk: "medium: metadata references active rows that normal retrieval/export cannot load"
                    .to_string(),
                reason: tenant.reasons.join("; "),
                command_preview: unreadable_purge_dry_run_command(
                    &tenant.tenant_id,
                    tenant.project_id_filter.as_deref(),
                    tenant.unreadable_active_chunks,
                ),
                destructive_command_preview: Some(unreadable_purge_apply_command(
                    &tenant.tenant_id,
                    tenant.project_id_filter.as_deref(),
                    tenant.unreadable_active_chunks,
                    archive_dir,
                )),
                verification_command_preview: Some(purge_archive_verify_command(
                    &unreadable_purge_archive_path(
                        &tenant.tenant_id,
                        tenant.project_id_filter.as_deref(),
                        1,
                        archive_dir,
                    ),
                    &tenant.tenant_id,
                    tenant.project_id_filter.as_deref(),
                    first_batch_limit,
                )),
                estimated_batches: Some(estimated_batches(tenant.unreadable_active_chunks)),
                batch_command_previews: batch_previews,
            });
        }
        if tenant
            .classification
            .iter()
            .any(|class| class == "archive_delete_candidate")
        {
            items.push(ApprovalItem {
                approval_id: approval_id(
                    "review_archive_delete_tenant",
                    &tenant.tenant_id,
                    tenant.project_id_filter.as_deref(),
                ),
                action: "review_archive_delete_tenant".to_string(),
                tenant_id: tenant.tenant_id.clone(),
                project_id: tenant.project_id_filter.clone(),
                command_kind: "export_review".to_string(),
                command_is_destructive: false,
                scope_chunks: tenant.scanned_chunks,
                metadata_active_chunks: tenant.metadata_active_chunks,
                unreadable_active_chunks: tenant.unreadable_active_chunks,
                tenant_disk_total_bytes: tenant.disk_total_bytes,
                generated_digest_chunks: tenant.generated_digest_chunks,
                generated_wrapper_chunks: tenant.generated_wrapper_chunks,
                generated_digest_ratio: tenant.generated_digest_ratio,
                generated_wrapper_ratio: tenant.generated_wrapper_ratio,
                hidden_purge_candidates: tenant.hidden_purge_candidates,
                estimated_payload_bytes: tenant.estimated_purge_payload_bytes,
                risk: "high: tenant contains active rows; export and inspect before deleting"
                    .to_string(),
                reason: tenant.reasons.join("; "),
                command_preview: tenant.export_command.clone(),
                destructive_command_preview: None,
                verification_command_preview: None,
                estimated_batches: None,
                batch_command_previews: Vec::new(),
            });
        }
        if tenant
            .classification
            .iter()
            .any(|class| class == "legacy_progress_retention_review")
            && !tenant
                .classification
                .iter()
                .any(|class| class == "archive_delete_candidate")
        {
            items.push(ApprovalItem {
                approval_id: approval_id(
                    "review_legacy_progress_retention",
                    &tenant.tenant_id,
                    tenant.project_id_filter.as_deref(),
                ),
                action: "review_legacy_progress_retention".to_string(),
                tenant_id: tenant.tenant_id.clone(),
                project_id: tenant.project_id_filter.clone(),
                command_kind: "export_review".to_string(),
                command_is_destructive: false,
                scope_chunks: tenant.unbounded_progress_older_30d,
                metadata_active_chunks: tenant.metadata_active_chunks,
                unreadable_active_chunks: tenant.unreadable_active_chunks,
                tenant_disk_total_bytes: tenant.disk_total_bytes,
                generated_digest_chunks: tenant.generated_digest_chunks,
                generated_wrapper_chunks: tenant.generated_wrapper_chunks,
                generated_digest_ratio: tenant.generated_digest_ratio,
                generated_wrapper_ratio: tenant.generated_wrapper_ratio,
                hidden_purge_candidates: tenant.hidden_purge_candidates,
                estimated_payload_bytes: 0,
                risk: "medium: active legacy progress rows need review before expiry, consolidation, or deletion".to_string(),
                reason: format!(
                    "{} routine progress summaries older than 30 days have no retention deadline ({} total unbounded routine progress summaries)",
                    tenant.unbounded_progress_older_30d,
                    tenant.unbounded_progress_chunks
                ),
                command_preview: tenant.export_command.clone(),
                destructive_command_preview: None,
                verification_command_preview: None,
                estimated_batches: None,
                batch_command_previews: Vec::new(),
            });
        }
        if tenant
            .classification
            .iter()
            .any(|class| class == "high_noise_review")
            && !tenant
                .classification
                .iter()
                .any(|class| class == "archive_delete_candidate")
        {
            items.push(ApprovalItem {
                approval_id: approval_id(
                    "review_high_noise_tenant",
                    &tenant.tenant_id,
                    tenant.project_id_filter.as_deref(),
                ),
                action: "review_high_noise_tenant".to_string(),
                tenant_id: tenant.tenant_id.clone(),
                project_id: tenant.project_id_filter.clone(),
                command_kind: "export_review".to_string(),
                command_is_destructive: false,
                scope_chunks: tenant.scanned_chunks,
                metadata_active_chunks: tenant.metadata_active_chunks,
                unreadable_active_chunks: tenant.unreadable_active_chunks,
                tenant_disk_total_bytes: tenant.disk_total_bytes,
                generated_digest_chunks: tenant.generated_digest_chunks,
                generated_wrapper_chunks: tenant.generated_wrapper_chunks,
                generated_digest_ratio: tenant.generated_digest_ratio,
                generated_wrapper_ratio: tenant.generated_wrapper_ratio,
                hidden_purge_candidates: tenant.hidden_purge_candidates,
                estimated_payload_bytes: tenant.estimated_purge_payload_bytes,
                risk: "medium: review before consolidation, migration, or deletion".to_string(),
                reason: tenant.reasons.join("; "),
                command_preview: tenant.export_command.clone(),
                destructive_command_preview: None,
                verification_command_preview: None,
                estimated_batches: None,
                batch_command_previews: Vec::new(),
            });
        }
        for project in &tenant.projects {
            let project_scope_is_already_represented =
                tenant.project_id_filter.as_deref() == project.project_id.as_deref();
            if project
                .classification
                .iter()
                .any(|class| class == "archive_delete_candidate")
            {
                items.push(ApprovalItem {
                    approval_id: approval_id(
                        "review_archive_delete_project",
                        &tenant.tenant_id,
                        project.project_id.as_deref(),
                    ),
                    action: "review_archive_delete_project".to_string(),
                    tenant_id: tenant.tenant_id.clone(),
                    project_id: project.project_id.clone(),
                    command_kind: "export_review".to_string(),
                    command_is_destructive: false,
                    scope_chunks: project.chunks,
                    metadata_active_chunks: project.chunks,
                    unreadable_active_chunks: 0,
                    tenant_disk_total_bytes: tenant.disk_total_bytes,
                    generated_digest_chunks: project.generated_digest_chunks,
                    generated_wrapper_chunks: project.generated_wrapper_chunks,
                    generated_digest_ratio: ratio(project.generated_digest_chunks, project.chunks),
                    generated_wrapper_ratio: ratio(
                        project.generated_wrapper_chunks,
                        project.chunks,
                    ),
                    hidden_purge_candidates: 0,
                    estimated_payload_bytes: 0,
                    risk:
                        "medium: project contains active rows; export and inspect before deleting"
                            .to_string(),
                    reason: project.reasons.join("; "),
                    command_preview: project.export_command.clone(),
                    destructive_command_preview: None,
                    verification_command_preview: None,
                    estimated_batches: None,
                    batch_command_previews: Vec::new(),
                });
            }
            if project.classification.iter().any(|class| {
                class == "high_noise_review"
                    || class == "scope_review"
                    || class == "legacy_progress_retention_review"
            }) && !project_scope_is_already_represented
                && !project
                    .classification
                    .iter()
                    .any(|class| class == "archive_delete_candidate")
            {
                items.push(ApprovalItem {
                    approval_id: approval_id(
                        "review_project_scope_or_noise",
                        &tenant.tenant_id,
                        project.project_id.as_deref(),
                    ),
                    action: "review_project_scope_or_noise".to_string(),
                    tenant_id: tenant.tenant_id.clone(),
                    project_id: project.project_id.clone(),
                    command_kind: "export_review".to_string(),
                    command_is_destructive: false,
                    scope_chunks: project.chunks,
                    metadata_active_chunks: project.chunks,
                    unreadable_active_chunks: 0,
                    tenant_disk_total_bytes: tenant.disk_total_bytes,
                    generated_digest_chunks: project.generated_digest_chunks,
                    generated_wrapper_chunks: project.generated_wrapper_chunks,
                    generated_digest_ratio: ratio(project.generated_digest_chunks, project.chunks),
                    generated_wrapper_ratio: ratio(
                        project.generated_wrapper_chunks,
                        project.chunks,
                    ),
                    hidden_purge_candidates: 0,
                    estimated_payload_bytes: 0,
                    risk: "medium: inspect export before migration, consolidation, or deletion"
                        .to_string(),
                    reason: project.reasons.join("; "),
                    command_preview: project.export_command.clone(),
                    destructive_command_preview: None,
                    verification_command_preview: None,
                    estimated_batches: None,
                    batch_command_previews: Vec::new(),
                });
            }
        }
    }
    items
}

fn approval_summary(items: &[ApprovalItem]) -> ApprovalSummary {
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_action: BTreeMap<String, ApprovalActionAccumulator> = BTreeMap::new();
    let mut destructive_command_previews = 0usize;
    let mut verification_command_previews = 0usize;
    let mut estimated_batches = 0usize;
    let mut batch_command_previews = 0usize;
    let mut unreadable_active_rows_in_purge_previews = 0usize;

    for item in items {
        *by_kind.entry(item.command_kind.clone()).or_default() += 1;
        let action = by_action.entry(item.action.clone()).or_default();
        action.count += 1;
        *action
            .command_kinds
            .entry(item.command_kind.clone())
            .or_default() += 1;
        action.scope_chunks += item.scope_chunks;
        action.metadata_active_chunks += item.metadata_active_chunks;
        action.unreadable_active_chunks += item.unreadable_active_chunks;
        action.generated_digest_chunks += item.generated_digest_chunks;
        action.generated_wrapper_chunks += item.generated_wrapper_chunks;
        action.hidden_purge_candidates += item.hidden_purge_candidates;
        if action.example_approval_ids.len() < 3 {
            action.example_approval_ids.push(item.approval_id.clone());
        }
        if item.destructive_command_preview.is_some() {
            destructive_command_previews += 1;
            action.destructive_command_previews += 1;
        }
        if item.verification_command_preview.is_some() {
            verification_command_previews += 1;
            action.verification_command_previews += 1;
        }
        estimated_batches += item.estimated_batches.unwrap_or(0);
        action.estimated_batches += item.estimated_batches.unwrap_or(0);
        batch_command_previews += item.batch_command_previews.len();
        action.batch_command_previews += item.batch_command_previews.len();
        if item.command_kind == "dry_run_unreadable_purge" {
            unreadable_active_rows_in_purge_previews += item.unreadable_active_chunks;
        }
    }

    ApprovalSummary {
        total_items: items.len(),
        command_kinds: by_kind
            .into_iter()
            .map(|(command_kind, count)| ApprovalCommandKindSummary {
                command_kind,
                count,
            })
            .collect(),
        actions: by_action
            .into_iter()
            .map(|(action, acc)| ApprovalActionSummary {
                action,
                count: acc.count,
                command_kinds: acc
                    .command_kinds
                    .into_iter()
                    .map(|(command_kind, count)| ApprovalCommandKindSummary {
                        command_kind,
                        count,
                    })
                    .collect(),
                scope_chunks: acc.scope_chunks,
                metadata_active_chunks: acc.metadata_active_chunks,
                unreadable_active_chunks: acc.unreadable_active_chunks,
                generated_digest_chunks: acc.generated_digest_chunks,
                generated_wrapper_chunks: acc.generated_wrapper_chunks,
                hidden_purge_candidates: acc.hidden_purge_candidates,
                destructive_command_previews: acc.destructive_command_previews,
                verification_command_previews: acc.verification_command_previews,
                estimated_batches: acc.estimated_batches,
                batch_command_previews: acc.batch_command_previews,
                example_approval_ids: acc.example_approval_ids,
            })
            .collect(),
        destructive_command_previews,
        verification_command_previews,
        estimated_batches,
        batch_command_previews,
        unreadable_active_rows_in_purge_previews,
    }
}

fn approval_id(action: &str, tenant_id: &str, project_id: Option<&str>) -> String {
    match project_id {
        Some(project_id) => format!(
            "{}:{}:{}",
            action,
            sanitize_filename(tenant_id),
            sanitize_filename(project_id)
        ),
        None => format!("{}:{}", action, sanitize_filename(tenant_id)),
    }
}

fn post_cleanup_verification_commands(
    tenant_id: Option<&str>,
    project_id: Option<&str>,
    project_dir: &Path,
    archive_dir: &Path,
    older_than_days: u64,
    candidate_limit: usize,
    page_size: usize,
    top_projects: usize,
    project_scope: Option<&ProjectScopeConfig>,
    has_retrieval_queries: bool,
) -> PostCleanupVerification {
    let scope = scoped_cli_flags(tenant_id, project_id);
    let memory_scope = scoped_project_verification_flags(tenant_id, project_id, project_scope);
    let retrieval_scope = retrieval_verification_scope(tenant_id, project_id, project_scope);
    let project_dir_arg = shell_quote_path(project_dir);
    let mut commands = vec![
        PostCleanupVerificationCommand {
            label: "audit_after_cleanup".to_string(),
            command: format!(
                "memd audit{} --format markdown --output {} --page-size {} --top-projects {}",
                scope,
                shell_quote_path(Path::new("tasks/memd-post-cleanup-audit.md")),
                page_size,
                top_projects,
            ),
            pass_criteria: "Audit completes and approved scopes show lower unreadable_active_chunks, hidden_purge_candidates, or disk bytes without unexpected new high-risk classifications.".to_string(),
        },
        PostCleanupVerificationCommand {
            label: "cleanup_plan_rerun".to_string(),
            command: format!(
                "memd cleanup-plan{} --project-dir {} --format markdown --output {} --archive-dir {} --older-than-days {} --candidate-limit {} --page-size {} --top-projects {}",
                scope,
                project_dir_arg,
                shell_quote_path(Path::new("tasks/memd-cleanup-plan-after.md")),
                shell_quote_path(archive_dir),
                older_than_days,
                candidate_limit,
                page_size,
                top_projects,
            ),
            pass_criteria: "Rerun plan no longer proposes already-approved purge batches, and any remaining approval items have new explicit approval IDs for separate review.".to_string(),
        },
    ];

    if has_retrieval_queries {
        if let Some((retrieval_tenant, retrieval_project)) = retrieval_scope {
            commands.push(PostCleanupVerificationCommand {
                label: "retrieval_quality".to_string(),
                command: eval_retrieval_command(
                    &retrieval_tenant,
                    retrieval_project.as_deref(),
                    project_dir,
                ),
                pass_criteria: "Command exits 0 with hit_rate_at_k >= 0.8, known_recall_at_k >= 0.6, and MRR >= 0.35 for the fixed sparse-judgment retrieval queries.".to_string(),
            });
        }
    }

    commands.extend([
        PostCleanupVerificationCommand {
            label: "startup_memory_quality".to_string(),
            command: format!(
                "memd eval-memory-md{} --project-dir {} --output {} --min-useful-ratio 0.8 --max-generated-wrappers 0",
                memory_scope,
                project_dir_arg,
                shell_quote_path(Path::new("tasks/memory-post-cleanup.md")),
            ),
            pass_criteria: "Command exits 0 with useful_ratio >= 0.8, generated_wrapper_count == 0, and no displayed items missing reason metadata or concrete agent action guidance.".to_string(),
        },
        PostCleanupVerificationCommand {
            label: "refresh_project_memory".to_string(),
            command: format!(
                "memd memory-md{} --project-dir {} --output {} --explain-output {}",
                memory_scope,
                project_dir_arg,
                shell_quote_path(Path::new("tasks/memory.md")),
                shell_quote_path(Path::new("tasks/memory-post-cleanup-explain.json")),
            ),
            pass_criteria: "tasks/memory.md refreshes successfully and the explain report shows displayed records are project-relevant durable takeaways rather than generated wrappers.".to_string(),
        },
        PostCleanupVerificationCommand {
            label: "host_wiring".to_string(),
            command: format!(
                "memd doctor --project-dir {} --format markdown",
                project_dir_arg
            ),
            pass_criteria: "Doctor report keeps binary, data directory, project scope, and agent-rule wiring in ok or explicitly understood states.".to_string(),
        },
    ]);

    PostCleanupVerification {
        note: "Run these non-destructive checks after any approved purge/archive cleanup and before considering storage reduction complete.".to_string(),
        commands,
    }
}

fn read_project_scope_for_verification(project_dir: &Path) -> Option<ProjectScopeConfig> {
    let project_dir = absolutize_project_dir(project_dir).ok()?;
    let path = project_dir.join(".memd/project_scope.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn retrieval_queries_available(project_dir: &Path) -> bool {
    absolutize_project_dir(project_dir)
        .map(|project_dir| project_dir.join(RETRIEVAL_QUERIES_PATH).is_file())
        .unwrap_or(false)
}

fn scoped_project_verification_flags(
    tenant_id: Option<&str>,
    project_id: Option<&str>,
    project_scope: Option<&ProjectScopeConfig>,
) -> String {
    if project_id.is_some() {
        return scoped_cli_flags(tenant_id, project_id);
    }
    if let Some(scope) = project_scope {
        return scoped_cli_flags(Some(&scope.tenant_id), scope.project_id.as_deref());
    }
    String::new()
}

fn retrieval_verification_scope(
    tenant_id: Option<&str>,
    project_id: Option<&str>,
    project_scope: Option<&ProjectScopeConfig>,
) -> Option<(String, Option<String>)> {
    if let Some(project_id) = project_id {
        return tenant_id.map(|tenant_id| (tenant_id.to_string(), Some(project_id.to_string())));
    }
    project_scope.map(|scope| (scope.tenant_id.clone(), scope.project_id.clone()))
}

fn eval_retrieval_command(tenant_id: &str, project_id: Option<&str>, project_dir: &Path) -> String {
    let mut command = format!("memd eval-retrieval --tenant-id {}", shell_quote(tenant_id));
    if let Some(project_id) = project_id {
        command.push_str(&format!(" --project-id {}", shell_quote(project_id)));
    }
    command.push_str(&format!(
        " --project-dir {} --queries {} --k 5 --min-hit-rate-at-k 0.8 --min-known-recall-at-k 0.6 --min-mrr 0.35",
        shell_quote_path(project_dir),
        shell_quote(RETRIEVAL_QUERIES_PATH)
    ));
    command
}

fn hidden_purge_archive_path(
    tenant_id: &str,
    project_id: Option<&str>,
    archive_dir: &Path,
) -> PathBuf {
    match project_id {
        Some(project_id) => archive_dir.join(format!(
            "{}__{}__hidden_purge_archive.json",
            sanitize_filename(tenant_id),
            sanitize_filename(project_id)
        )),
        None => archive_dir.join(format!(
            "{}__hidden_purge_archive.json",
            sanitize_filename(tenant_id)
        )),
    }
}

fn unreadable_purge_archive_path(
    tenant_id: &str,
    project_id: Option<&str>,
    batch: usize,
    archive_dir: &Path,
) -> PathBuf {
    let batch = batch.max(1);
    match project_id {
        Some(project_id) => archive_dir.join(format!(
            "{}__{}__unreadable_active_batch{:03}_archive.json",
            sanitize_filename(tenant_id),
            sanitize_filename(project_id),
            batch
        )),
        None => archive_dir.join(format!(
            "{}__unreadable_active_batch{:03}_archive.json",
            sanitize_filename(tenant_id),
            batch
        )),
    }
}

fn estimated_batches(candidate_count: usize) -> usize {
    candidate_count
        .max(1)
        .saturating_add(PURGE_COMMAND_LIMIT - 1)
        / PURGE_COMMAND_LIMIT
}

fn unreadable_purge_dry_run_command(
    tenant_id: &str,
    project_id: Option<&str>,
    unreadable_active_chunks: usize,
) -> String {
    let limit = unreadable_active_chunks.clamp(1, PURGE_COMMAND_LIMIT);
    match project_id {
        Some(project_id) => format!(
            "memd purge --tenant-id {} --project-id {} --include-unreadable-active --limit {}",
            shell_quote(tenant_id),
            shell_quote(project_id),
            limit
        ),
        None => format!(
            "memd purge --tenant-id {} --include-unreadable-active --limit {}",
            shell_quote(tenant_id),
            limit
        ),
    }
}

fn unreadable_purge_apply_command(
    tenant_id: &str,
    project_id: Option<&str>,
    unreadable_active_chunks: usize,
    archive_dir: &Path,
) -> String {
    let limit = unreadable_active_chunks.clamp(1, PURGE_COMMAND_LIMIT);
    let archive = unreadable_purge_archive_path(tenant_id, project_id, 1, archive_dir);
    unreadable_purge_apply_command_with_archive(tenant_id, project_id, limit, &archive)
}

fn unreadable_purge_apply_command_with_archive(
    tenant_id: &str,
    project_id: Option<&str>,
    limit: usize,
    archive: &Path,
) -> String {
    match project_id {
        Some(project_id) => format!(
            "memd purge --tenant-id {} --project-id {} --include-unreadable-active --limit {} --archive {} --apply --vacuum-metadata",
            shell_quote(tenant_id),
            shell_quote(project_id),
            limit,
            shell_quote_path(&archive)
        ),
        None => format!(
            "memd purge --tenant-id {} --include-unreadable-active --limit {} --archive {} --apply --vacuum-metadata",
            shell_quote(tenant_id),
            limit,
            shell_quote_path(&archive)
        ),
    }
}

fn unreadable_purge_batch_previews(
    tenant_id: &str,
    project_id: Option<&str>,
    unreadable_active_chunks: usize,
    archive_dir: &Path,
) -> Vec<CleanupBatchCommandPreview> {
    let batches = estimated_batches(unreadable_active_chunks);
    let mut rows = Vec::with_capacity(batches);
    for batch in 1..=batches {
        let previous = (batch - 1).saturating_mul(PURGE_COMMAND_LIMIT);
        let remaining = unreadable_active_chunks.saturating_sub(previous);
        let limit = remaining.clamp(1, PURGE_COMMAND_LIMIT);
        let archive = unreadable_purge_archive_path(tenant_id, project_id, batch, archive_dir);
        rows.push(CleanupBatchCommandPreview {
            batch,
            limit,
            archive: archive.display().to_string(),
            dry_run_command: unreadable_purge_dry_run_command(tenant_id, project_id, limit),
            destructive_command: unreadable_purge_apply_command_with_archive(
                tenant_id, project_id, limit, &archive,
            ),
            verification_command: purge_archive_verify_command(
                &archive, tenant_id, project_id, limit,
            ),
        });
    }
    rows
}

fn purge_archive_verify_command(
    archive: &Path,
    tenant_id: &str,
    project_id: Option<&str>,
    min_records: usize,
) -> String {
    let mut command = format!(
        "memd purge-archive --archive {} --expect-tenant-id {} --min-records {}",
        shell_quote_path(archive),
        shell_quote(tenant_id),
        min_records
    );
    if let Some(project_id) = project_id {
        command.push_str(&format!(" --expect-project-id {}", shell_quote(project_id)));
    }
    command
}

fn action_weight(classes: &[String]) -> usize {
    if classes
        .iter()
        .any(|class| class == "archive_delete_candidate")
    {
        5
    } else if classes
        .iter()
        .any(|class| class == "payload_integrity_review")
    {
        4
    } else if classes.iter().any(|class| class == "hard_purge_ready") {
        3
    } else if classes
        .iter()
        .any(|class| class == "high_noise_review" || class == "legacy_progress_retention_review")
    {
        2
    } else if classes.iter().any(|class| class == "scope_review") {
        1
    } else {
        0
    }
}

fn throwaway_name_reason(name: &str) -> Option<&'static str> {
    let lowered = name.to_ascii_lowercase();
    let patterns = [
        ("smoke", "smoke"),
        ("test", "test"),
        ("benchmark", "benchmark"),
        ("bench", "bench"),
        ("eval", "eval"),
        ("tmp", "tmp"),
        ("scratch", "scratch"),
        ("demo", "demo"),
        ("fixture", "fixture"),
        ("synthetic", "synthetic"),
        ("quickstart", "quickstart"),
        ("sandbox", "sandbox"),
        ("phase", "phase"),
    ];
    patterns
        .iter()
        .find_map(|(needle, reason)| lowered.contains(needle).then_some(*reason))
}

fn export_command(tenant_id: &str, project_id: Option<&str>, archive_dir: &Path) -> String {
    let mut path = archive_dir.join(sanitize_filename(tenant_id));
    if let Some(project_id) = project_id {
        path.set_file_name(format!(
            "{}__{}.omf.json",
            sanitize_filename(tenant_id),
            sanitize_filename(project_id)
        ));
    } else {
        path.set_extension("omf.json");
    }
    match project_id {
        Some(project_id) => format!(
            "memd export-omf --tenant-id {} --project-id {} --include-history true --output {}",
            shell_quote(tenant_id),
            shell_quote(project_id),
            shell_quote_path(&path)
        ),
        None => format!(
            "memd export-omf --tenant-id {} --include-history true --output {}",
            shell_quote(tenant_id),
            shell_quote_path(&path)
        ),
    }
}

fn purge_command(
    tenant_id: &str,
    project_id: Option<&str>,
    older_than_days: u64,
    archive_dir: &Path,
) -> String {
    let archive = hidden_purge_archive_path(tenant_id, project_id, archive_dir);
    let mut command = format!(
        "memd purge --tenant-id {} --older-than-days {} --archive {} --apply --rewrite-segments --vacuum-metadata",
        shell_quote(tenant_id),
        older_than_days,
        shell_quote_path(&archive)
    );
    if let Some(project_id) = project_id {
        command = format!(
            "memd purge --tenant-id {} --project-id {} --older-than-days {} --archive {} --apply --rewrite-segments --vacuum-metadata",
            shell_quote(tenant_id),
            shell_quote(project_id),
            older_than_days,
            shell_quote_path(&archive)
        );
    }
    command
}

fn scoped_cli_flags(tenant_id: Option<&str>, project_id: Option<&str>) -> String {
    let mut flags = String::new();
    if let Some(tenant_id) = tenant_id {
        flags.push_str(&format!(" --tenant-id {}", shell_quote(tenant_id)));
    }
    if let Some(project_id) = project_id {
        flags.push_str(&format!(" --project-id {}", shell_quote(project_id)));
    }
    flags
}

fn sanitize_filename(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unscoped".to_string()
    } else {
        sanitized
    }
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote(&path.display().to_string())
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=+".contains(ch))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
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

fn is_older_than_days(timestamp_created: i64, now_ms: i64, days: i64) -> bool {
    if timestamp_created <= 0 || now_ms <= 0 {
        return false;
    }
    let age_ms = now_ms.saturating_sub(timestamp_created);
    age_ms > days.saturating_mul(86_400_000)
}

fn render_markdown(report: &CleanupPlanReport) -> String {
    let mut out = String::new();
    out.push_str("# memd cleanup plan\n\n");
    out.push_str(&format!(
        "- generated_unix_ms: `{}`\n",
        report.generated_unix_ms
    ));
    if let Some(data_dir) = &report.data_dir {
        out.push_str(&format!("- data_dir: `{data_dir}`\n"));
    }
    out.push_str(&format!("- archive_dir: `{}`\n", report.archive_dir));
    out.push_str(&format!(
        "- safety: destructive_actions_included=`{}`; approval_required=`{}`\n",
        report.safety.destructive_actions_included, report.safety.approval_required
    ));
    out.push_str(&format!("- note: {}\n", report.safety.note));
    out.push_str(&format!(
        "- tenants_scanned: `{}`; tenants_with_approval_items: `{}`; metadata_active_chunks: `{}`; scanned_chunks: `{}`; unreadable_active_chunks: `{}`; routine_progress_chunks: `{}`; unbounded_progress_chunks: `{}`; unbounded_progress_older_30d: `{}`; hidden_purge_candidates: `{}`; estimated_purge_payload_bytes: `{}`\n",
        report.totals.tenants_scanned,
        report.totals.tenants_with_approval_items,
        report.totals.metadata_active_chunks,
        report.totals.scanned_chunks,
        report.totals.unreadable_active_chunks,
        report.totals.routine_progress_chunks,
        report.totals.unbounded_progress_chunks,
        report.totals.unbounded_progress_older_30d,
        report.totals.hidden_purge_candidates,
        report.totals.estimated_purge_payload_bytes
    ));

    out.push_str("\n## Post-Cleanup Verification\n\n");
    out.push_str(&format!("{}\n\n", report.post_cleanup_verification.note));
    for command in &report.post_cleanup_verification.commands {
        out.push_str(&format!(
            "- `{}`: `{}`\n  - pass: {}\n",
            command.label, command.command, command.pass_criteria
        ));
    }

    if !report.approval_items.is_empty() {
        out.push_str("\n## Approval Summary\n\n");
        out.push_str(&format!(
            "- total_items: `{}`; destructive_command_previews: `{}`; verification_command_previews: `{}`; estimated_batches: `{}`; batch_command_previews: `{}`; unreadable_active_rows_in_purge_previews: `{}`\n",
            report.approval_summary.total_items,
            report.approval_summary.destructive_command_previews,
            report.approval_summary.verification_command_previews,
            report.approval_summary.estimated_batches,
            report.approval_summary.batch_command_previews,
            report.approval_summary.unreadable_active_rows_in_purge_previews
        ));
        for row in &report.approval_summary.command_kinds {
            out.push_str(&format!("- `{}`: `{}`\n", row.command_kind, row.count));
        }
        if !report.approval_summary.actions.is_empty() {
            out.push_str("\n### Action Rollups\n\n");
            for row in &report.approval_summary.actions {
                let command_kinds = row
                    .command_kinds
                    .iter()
                    .map(|kind| format!("{}={}", kind.command_kind, kind.count))
                    .collect::<Vec<_>>()
                    .join(", ");
                let examples = if row.example_approval_ids.is_empty() {
                    "none".to_string()
                } else {
                    row.example_approval_ids
                        .iter()
                        .map(|id| format!("`{id}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                out.push_str(&format!(
                    "- `{}`: items=`{}`; command_kinds=`{}`; scope_chunks=`{}`; metadata_active_chunks=`{}`; unreadable_active_chunks=`{}`; generated_digest_chunks=`{}`; generated_wrapper_chunks=`{}`; hidden_purge_candidates=`{}`; destructive_previews=`{}`; verification_previews=`{}`; estimated_batches=`{}`; batch_command_previews=`{}`; examples={}\n",
                    row.action,
                    row.count,
                    command_kinds,
                    row.scope_chunks,
                    row.metadata_active_chunks,
                    row.unreadable_active_chunks,
                    row.generated_digest_chunks,
                    row.generated_wrapper_chunks,
                    row.hidden_purge_candidates,
                    row.destructive_command_previews,
                    row.verification_command_previews,
                    row.estimated_batches,
                    row.batch_command_previews,
                    examples
                ));
            }
        }

        out.push_str("\n## Approval Items\n\n");
        for item in &report.approval_items {
            out.push_str(&format!(
                "- action=`{}` tenant=`{}`",
                item.action, item.tenant_id
            ));
            if let Some(project_id) = &item.project_id {
                out.push_str(&format!(" project=`{project_id}`"));
            }
            out.push_str(&format!(
                "\n  - approval_id: `{}`\n  - command_kind: `{}`; command_is_destructive: `{}`\n  - scope_chunks: `{}`; metadata_active_chunks: `{}`; unreadable_active_chunks: `{}`; tenant_disk_total_bytes: `{}`\n  - generated_digest_chunks: `{}`; generated_wrapper_chunks: `{}`; generated_digest_ratio: `{:.3}`; generated_wrapper_ratio: `{:.3}`\n  - hidden_purge_candidates: `{}`; estimated_payload_bytes: `{}`\n  - risk: {}\n  - reason: {}\n  - command: `{}`\n",
                item.approval_id,
                item.command_kind,
                item.command_is_destructive,
                item.scope_chunks,
                item.metadata_active_chunks,
                item.unreadable_active_chunks,
                item.tenant_disk_total_bytes
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                item.generated_digest_chunks,
                item.generated_wrapper_chunks,
                item.generated_digest_ratio,
                item.generated_wrapper_ratio,
                item.hidden_purge_candidates,
                item.estimated_payload_bytes,
                item.risk,
                item.reason,
                item.command_preview
            ));
            if let Some(batches) = item.estimated_batches {
                out.push_str(&format!("  - estimated_batches: `{batches}`\n"));
            }
            if let Some(command) = &item.destructive_command_preview {
                out.push_str(&format!("  - destructive_command: `{command}`\n"));
            }
            if let Some(command) = &item.verification_command_preview {
                out.push_str(&format!("  - verify_archive: `{command}`\n"));
            }
            if !item.batch_command_previews.is_empty() {
                out.push_str("  - batch_commands:\n");
                for batch in &item.batch_command_previews {
                    out.push_str(&format!(
                        "    - batch=`{}`; limit=`{}`; archive=`{}`\n",
                        batch.batch, batch.limit, batch.archive
                    ));
                    out.push_str(&format!("      - dry_run: `{}`\n", batch.dry_run_command));
                    out.push_str(&format!(
                        "      - destructive_command: `{}`\n",
                        batch.destructive_command
                    ));
                    out.push_str(&format!(
                        "      - verify_archive: `{}`\n",
                        batch.verification_command
                    ));
                }
            }
        }
    }

    for tenant in &report.tenants {
        out.push_str(&format!("\n## Tenant `{}`\n\n", tenant.tenant_id));
        out.push_str(&format!(
            "- classification: `{}`\n",
            tenant.classification.join(", ")
        ));
        out.push_str(&format!("- reasons: {}\n", tenant.reasons.join("; ")));
        out.push_str(&format!(
            "- chunks: total=`{}`, active=`{}`, deleted=`{}`, metadata_active_in_scope=`{}`, scanned=`{}`, unreadable_active=`{}`, readable_active_ratio=`{:.3}`\n",
            tenant.stats.total_chunks,
            tenant.stats.active_chunks,
            tenant.stats.deleted_chunks,
            tenant.metadata_active_chunks,
            tenant.scanned_chunks,
            tenant.unreadable_active_chunks,
            tenant.readable_active_ratio
        ));
        if let Some(bytes) = tenant.disk_total_bytes {
            out.push_str(&format!("- disk_total_bytes: `{bytes}`\n"));
        }
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
            "- hidden_purge_candidates: `{}`; estimated_purge_payload_bytes: `{}`\n",
            tenant.hidden_purge_candidates, tenant.estimated_purge_payload_bytes
        ));
        out.push_str(&format!("- export: `{}`\n", tenant.export_command));
        if let Some(command) = &tenant.purge_command_preview {
            out.push_str(&format!("- purge_preview: `{command}`\n"));
        }
        if !tenant.projects.is_empty() {
            out.push_str("\n### Projects\n\n");
            for project in &tenant.projects {
                let label = project.project_id.as_deref().unwrap_or("<unscoped>");
                out.push_str(&format!(
                    "- `{label}`: chunks=`{}`, routine_progress=`{}`, unbounded_progress=`{}`, unbounded_progress_older_30d=`{}`, classification=`{}`, reasons={}\n",
                    project.chunks,
                    project.routine_progress_chunks,
                    project.unbounded_progress_chunks,
                    project.unbounded_progress_older_30d,
                    project.classification.join(", "),
                    project.reasons.join("; ")
                ));
            }
        }
    }
    out
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeMap::new();
    for value in values {
        seen.entry(value.clone()).or_insert(value);
    }
    seen.into_values().collect()
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
            deleted_chunks: stats.deleted_chunks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoreStats;
    use crate::types::{ChunkType, ProjectId};

    fn chunk(text: &str, project_id: Option<&str>, tags: Vec<&str>) -> MemoryChunk {
        let mut chunk =
            MemoryChunk::new(TenantId::new("tenant").unwrap(), text, ChunkType::Summary);
        if let Some(project_id) = project_id {
            chunk = chunk.with_project(ProjectId::from(project_id));
        }
        chunk.with_tags(tags.into_iter().map(str::to_string).collect())
    }

    fn chunk_at(
        text: &str,
        project_id: Option<&str>,
        tags: Vec<&str>,
        timestamp_created: i64,
    ) -> MemoryChunk {
        let mut chunk = chunk(text, project_id, tags);
        chunk.timestamp_created = timestamp_created;
        chunk
    }

    fn scanned(chunk: MemoryChunk, expires_at_ms: Option<i64>) -> ScannedChunk {
        ScannedChunk {
            chunk,
            expires_at_ms,
        }
    }

    fn project_scope(tenant_id: &str, project_id: Option<&str>) -> ProjectScopeConfig {
        ProjectScopeConfig {
            tenant_id: tenant_id.to_string(),
            project_id: project_id.map(str::to_string),
            interface: "cli".to_string(),
            cli_command: "memd".to_string(),
            agent_context_output: ".memd/context.md".to_string(),
            project_dir: ".".to_string(),
        }
    }

    #[test]
    fn classifier_marks_throwaway_and_generated_noise() {
        let chunks = vec![
            scanned(
                chunk(
                    "Task digest status generated. Summary: Highlight library for smoke contains 0 ranked lessons.",
                    Some("smoke_project"),
                    vec!["task:status:generated", "task:role:highlight_library"],
                ),
                None,
            ),
            scanned(
                chunk(
                    "Task digest status generated. Summary: Highlight library for smoke contains 0 ranked lessons.",
                    Some("smoke_project"),
                    vec!["task:status:generated", "task:role:highlight_library"],
                ),
                None,
            ),
        ];
        let summary = summarize_chunks(&chunks, now_ms());
        let stats = StoreStats {
            total_chunks: 2,
            active_chunks: 2,
            ..Default::default()
        };
        let purge = HiddenPurgeSummary::default();
        let (classes, reasons) = classify_tenant("smoke_test", &stats, &summary, &purge, 0);
        assert!(classes.contains(&"archive_delete_candidate".to_string()));
        assert!(classes.contains(&"high_noise_review".to_string()));
        assert!(reasons.iter().any(|reason| reason.contains("throwaway")));
    }

    #[test]
    fn classifier_marks_hidden_purge_ready() {
        let summary = ChunkSummary::default();
        let stats = StoreStats::default();
        let purge = HiddenPurgeSummary {
            candidate_count: 3,
            estimated_payload_bytes: 99,
        };
        let (classes, reasons) = classify_tenant("memd", &stats, &summary, &purge, 0);
        assert_eq!(classes, vec!["hard_purge_ready"]);
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("3 hidden rows")));
    }

    #[test]
    fn classifier_marks_unreadable_payload_review() {
        let summary = ChunkSummary::default();
        let stats = StoreStats {
            total_chunks: 10,
            active_chunks: 10,
            ..Default::default()
        };
        let purge = HiddenPurgeSummary::default();
        let (classes, reasons) = classify_tenant("memd", &stats, &summary, &purge, 7);
        assert!(classes.contains(&"payload_integrity_review".to_string()));
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("7 active metadata rows")));
    }

    #[test]
    fn classifier_marks_legacy_unbounded_progress_review() {
        let now = 100 * 86_400_000;
        let chunks = vec![
            scanned(
                chunk_at(
                    "Mapped auth middleware touchpoints; next step is validation.",
                    Some("auth"),
                    vec!["kind:progress"],
                    now - 40 * 86_400_000,
                ),
                None,
            ),
            scanned(
                chunk_at(
                    "Mapped API touchpoints; next step is validation.",
                    Some("api"),
                    vec!["kind:progress"],
                    now - 2 * 86_400_000,
                ),
                Some(now + 14 * 86_400_000),
            ),
            scanned(
                chunk_at(
                    "Decision: keep the auth migration behind the existing flag.",
                    Some("auth"),
                    vec!["kind:progress", "kind:decision"],
                    now - 40 * 86_400_000,
                ),
                None,
            ),
        ];

        let summary = summarize_chunks(&chunks, now);
        assert_eq!(summary.routine_progress_chunks, 2);
        assert_eq!(summary.unbounded_progress_chunks, 1);
        assert_eq!(summary.unbounded_progress_older_30d, 1);
        let auth = summary
            .projects
            .get(&Some("auth".to_string()))
            .expect("auth project summary");
        assert_eq!(auth.routine_progress_chunks, 1);
        assert_eq!(auth.unbounded_progress_chunks, 1);
        assert_eq!(auth.unbounded_progress_older_30d, 1);

        let stats = StoreStats {
            total_chunks: 3,
            active_chunks: 3,
            ..Default::default()
        };
        let purge = HiddenPurgeSummary::default();
        let (classes, reasons) = classify_tenant("memd", &stats, &summary, &purge, 0);
        assert!(classes.contains(&"legacy_progress_retention_review".to_string()));
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("1 routine progress summaries older than 30 days")));

        let (project_classes, project_reasons) = classify_project(Some("auth"), auth);
        assert!(project_classes.contains(&"legacy_progress_retention_review".to_string()));
        assert!(project_reasons
            .iter()
            .any(|reason| reason.contains("1 routine progress summaries older than 30 days")));
    }

    #[test]
    fn approval_items_include_unreadable_purge_dry_run() {
        let tenant = TenantCleanupPlan {
            tenant_id: "memd".to_string(),
            project_id_filter: Some("memd".to_string()),
            disk_total_bytes: Some(100),
            stats: StoreStatsReport {
                total_chunks: 20,
                active_chunks: 20,
                deleted_chunks: 0,
            },
            metadata_active_chunks: 20,
            scanned_chunks: 3,
            unreadable_active_chunks: 17,
            readable_active_ratio: 0.15,
            generated_digest_chunks: 0,
            generated_digest_ratio: 0.0,
            generated_wrapper_chunks: 0,
            generated_wrapper_ratio: 0.0,
            routine_progress_chunks: 0,
            unbounded_progress_chunks: 0,
            unbounded_progress_older_30d: 0,
            unscoped_chunks: 0,
            hidden_purge_candidates: 0,
            estimated_purge_payload_bytes: 0,
            classification: vec!["payload_integrity_review".to_string()],
            reasons: vec!["17 active metadata rows could not be loaded".to_string()],
            export_command: export_command("memd", Some("memd"), Path::new("/tmp/archive")),
            purge_command_preview: None,
            projects: Vec::new(),
        };

        let items = approval_items(&[tenant], Path::new("/tmp/archive"));
        let item = items
            .iter()
            .find(|item| item.action == "review_payload_integrity_tenant")
            .expect("payload integrity approval item");
        assert_eq!(item.command_kind, "dry_run_unreadable_purge");
        assert!(!item.command_is_destructive);
        assert_eq!(item.unreadable_active_chunks, 17);
        assert_eq!(
            item.command_preview,
            "memd purge --tenant-id memd --project-id memd --include-unreadable-active --limit 17"
        );
        assert_eq!(item.estimated_batches, Some(1));
        assert_eq!(item.batch_command_previews.len(), 1);
        assert_eq!(item.batch_command_previews[0].batch, 1);
        assert_eq!(item.batch_command_previews[0].limit, 17);
        assert_eq!(
            item.destructive_command_preview.as_deref(),
            Some("memd purge --tenant-id memd --project-id memd --include-unreadable-active --limit 17 --archive /tmp/archive/memd__memd__unreadable_active_batch001_archive.json --apply --vacuum-metadata")
        );
        assert_eq!(
            item.verification_command_preview.as_deref(),
            Some("memd purge-archive --archive /tmp/archive/memd__memd__unreadable_active_batch001_archive.json --expect-tenant-id memd --min-records 17 --expect-project-id memd")
        );
    }

    #[test]
    fn approval_items_include_legacy_progress_retention_review() {
        let tenant = TenantCleanupPlan {
            tenant_id: "memd".to_string(),
            project_id_filter: Some("memd".to_string()),
            disk_total_bytes: Some(100),
            stats: StoreStatsReport {
                total_chunks: 20,
                active_chunks: 20,
                deleted_chunks: 0,
            },
            metadata_active_chunks: 20,
            scanned_chunks: 20,
            unreadable_active_chunks: 0,
            readable_active_ratio: 1.0,
            generated_digest_chunks: 0,
            generated_digest_ratio: 0.0,
            generated_wrapper_chunks: 0,
            generated_wrapper_ratio: 0.0,
            routine_progress_chunks: 9,
            unbounded_progress_chunks: 9,
            unbounded_progress_older_30d: 4,
            unscoped_chunks: 0,
            hidden_purge_candidates: 0,
            estimated_purge_payload_bytes: 0,
            classification: vec!["legacy_progress_retention_review".to_string()],
            reasons: vec![
                "4 routine progress summaries older than 30 days have no retention deadline"
                    .to_string(),
            ],
            export_command: export_command("memd", Some("memd"), Path::new("/tmp/archive")),
            purge_command_preview: None,
            projects: Vec::new(),
        };

        let items = approval_items(&[tenant], Path::new("/tmp/archive"));
        let item = items
            .iter()
            .find(|item| item.action == "review_legacy_progress_retention")
            .expect("legacy progress approval item");
        assert_eq!(item.command_kind, "export_review");
        assert!(!item.command_is_destructive);
        assert_eq!(item.scope_chunks, 4);
        assert_eq!(item.metadata_active_chunks, 20);
        assert_eq!(item.estimated_payload_bytes, 0);
        assert_eq!(
            item.command_preview,
            "memd export-omf --tenant-id memd --project-id memd --include-history true --output /tmp/archive/memd__memd.omf.json"
        );
        assert!(item.destructive_command_preview.is_none());
        assert!(item.verification_command_preview.is_none());
        assert!(item
            .reason
            .contains("4 routine progress summaries older than 30 days"));
        assert!(item
            .reason
            .contains("9 total unbounded routine progress summaries"));
    }

    #[test]
    fn unreadable_purge_batch_previews_emit_unique_archives() {
        let previews = unreadable_purge_batch_previews(
            "memd",
            Some("memd"),
            PURGE_COMMAND_LIMIT * 2 + 17,
            Path::new("/tmp/archive"),
        );

        assert_eq!(previews.len(), 3);
        assert_eq!(previews[0].batch, 1);
        assert_eq!(previews[0].limit, PURGE_COMMAND_LIMIT);
        assert!(previews[0]
            .archive
            .ends_with("memd__memd__unreadable_active_batch001_archive.json"));
        assert_eq!(previews[1].batch, 2);
        assert_eq!(previews[1].limit, PURGE_COMMAND_LIMIT);
        assert!(previews[1]
            .archive
            .ends_with("memd__memd__unreadable_active_batch002_archive.json"));
        assert_eq!(previews[2].batch, 3);
        assert_eq!(previews[2].limit, 17);
        assert!(previews[2]
            .archive
            .ends_with("memd__memd__unreadable_active_batch003_archive.json"));
        assert!(previews[2].dry_run_command.ends_with("--limit 17"));
        assert!(previews[2].destructive_command.contains("--limit 17"));
        assert!(previews[0]
            .verification_command
            .contains("--min-records 10000"));
        assert!(previews[2]
            .verification_command
            .contains("--min-records 17"));
        assert!(previews[2]
            .verification_command
            .contains("--expect-project-id memd"));
    }

    #[test]
    fn approval_summary_rolls_up_commands_and_batches() {
        let items = vec![
            ApprovalItem {
                approval_id: "review_payload_integrity_tenant:memd".to_string(),
                action: "review_payload_integrity_tenant".to_string(),
                tenant_id: "memd".to_string(),
                project_id: None,
                command_kind: "dry_run_unreadable_purge".to_string(),
                command_is_destructive: false,
                scope_chunks: 1,
                metadata_active_chunks: 11,
                unreadable_active_chunks: 10,
                tenant_disk_total_bytes: None,
                generated_digest_chunks: 2,
                generated_wrapper_chunks: 1,
                generated_digest_ratio: 0.0,
                generated_wrapper_ratio: 0.0,
                hidden_purge_candidates: 0,
                estimated_payload_bytes: 0,
                risk: "medium".to_string(),
                reason: "unreadable".to_string(),
                command_preview: "dry run".to_string(),
                destructive_command_preview: Some("apply".to_string()),
                verification_command_preview: Some("verify".to_string()),
                estimated_batches: Some(2),
                batch_command_previews: vec![
                    CleanupBatchCommandPreview {
                        batch: 1,
                        limit: 10,
                        archive: "archive-1.json".to_string(),
                        dry_run_command: "dry-run 1".to_string(),
                        destructive_command: "apply 1".to_string(),
                        verification_command: "verify 1".to_string(),
                    },
                    CleanupBatchCommandPreview {
                        batch: 2,
                        limit: 10,
                        archive: "archive-2.json".to_string(),
                        dry_run_command: "dry-run 2".to_string(),
                        destructive_command: "apply 2".to_string(),
                        verification_command: "verify 2".to_string(),
                    },
                ],
            },
            ApprovalItem {
                approval_id: "review_high_noise_tenant:memd".to_string(),
                action: "review_high_noise_tenant".to_string(),
                tenant_id: "memd".to_string(),
                project_id: None,
                command_kind: "export_review".to_string(),
                command_is_destructive: false,
                scope_chunks: 1,
                metadata_active_chunks: 1,
                unreadable_active_chunks: 0,
                tenant_disk_total_bytes: None,
                generated_digest_chunks: 3,
                generated_wrapper_chunks: 2,
                generated_digest_ratio: 0.0,
                generated_wrapper_ratio: 0.0,
                hidden_purge_candidates: 0,
                estimated_payload_bytes: 0,
                risk: "medium".to_string(),
                reason: "review".to_string(),
                command_preview: "export".to_string(),
                destructive_command_preview: None,
                verification_command_preview: None,
                estimated_batches: None,
                batch_command_previews: Vec::new(),
            },
        ];

        let summary = approval_summary(&items);
        assert_eq!(summary.total_items, 2);
        assert_eq!(summary.destructive_command_previews, 1);
        assert_eq!(summary.verification_command_previews, 1);
        assert_eq!(summary.estimated_batches, 2);
        assert_eq!(summary.batch_command_previews, 2);
        assert_eq!(summary.unreadable_active_rows_in_purge_previews, 10);
        assert!(summary
            .command_kinds
            .iter()
            .any(|row| row.command_kind == "dry_run_unreadable_purge" && row.count == 1));
        assert!(summary
            .command_kinds
            .iter()
            .any(|row| row.command_kind == "export_review" && row.count == 1));
        let purge_action = summary
            .actions
            .iter()
            .find(|row| row.action == "review_payload_integrity_tenant")
            .expect("payload integrity action summary");
        assert_eq!(purge_action.count, 1);
        assert_eq!(purge_action.metadata_active_chunks, 11);
        assert_eq!(purge_action.unreadable_active_chunks, 10);
        assert_eq!(purge_action.generated_digest_chunks, 2);
        assert_eq!(purge_action.generated_wrapper_chunks, 1);
        assert_eq!(purge_action.destructive_command_previews, 1);
        assert_eq!(purge_action.verification_command_previews, 1);
        assert_eq!(purge_action.estimated_batches, 2);
        assert_eq!(purge_action.batch_command_previews, 2);
        assert_eq!(
            purge_action.example_approval_ids,
            vec!["review_payload_integrity_tenant:memd".to_string()]
        );
    }

    #[test]
    fn render_markdown_includes_approval_commands() {
        let report = CleanupPlanReport {
            generated_unix_ms: 1,
            data_dir: Some("/tmp/memd".to_string()),
            archive_dir: "/tmp/archive".to_string(),
            older_than_days: 30,
            candidate_limit: 10,
            safety: CleanupSafety {
                destructive_actions_included: false,
                approval_required: true,
                note: "dry run".to_string(),
            },
            totals: CleanupTotals {
                tenants_scanned: 1,
                tenants_with_approval_items: 1,
                hidden_purge_candidates: 1,
                estimated_purge_payload_bytes: 10,
                ..Default::default()
            },
            approval_summary: ApprovalSummary {
                total_items: 1,
                command_kinds: vec![ApprovalCommandKindSummary {
                    command_kind: "archive_first_purge".to_string(),
                    count: 1,
                }],
                actions: vec![ApprovalActionSummary {
                    action: "hard_purge_hidden_rows".to_string(),
                    count: 1,
                    command_kinds: vec![ApprovalCommandKindSummary {
                        command_kind: "archive_first_purge".to_string(),
                        count: 1,
                    }],
                    scope_chunks: 0,
                    metadata_active_chunks: 0,
                    unreadable_active_chunks: 0,
                    generated_digest_chunks: 0,
                    generated_wrapper_chunks: 0,
                    hidden_purge_candidates: 1,
                    destructive_command_previews: 1,
                    verification_command_previews: 1,
                    estimated_batches: 1,
                    batch_command_previews: 1,
                    example_approval_ids: vec!["hard_purge_hidden_rows:smoke".to_string()],
                }],
                destructive_command_previews: 1,
                verification_command_previews: 1,
                estimated_batches: 1,
                batch_command_previews: 1,
                unreadable_active_rows_in_purge_previews: 0,
            },
            post_cleanup_verification: post_cleanup_verification_commands(
                Some("smoke"),
                None,
                Path::new("."),
                Path::new("/tmp/archive"),
                30,
                10,
                1000,
                15,
                None,
                false,
            ),
            approval_items: vec![ApprovalItem {
                approval_id: "hard_purge_hidden_rows:smoke".to_string(),
                action: "hard_purge_hidden_rows".to_string(),
                tenant_id: "smoke".to_string(),
                project_id: None,
                command_kind: "archive_first_purge".to_string(),
                command_is_destructive: true,
                scope_chunks: 0,
                metadata_active_chunks: 0,
                unreadable_active_chunks: 0,
                tenant_disk_total_bytes: Some(100),
                generated_digest_chunks: 0,
                generated_wrapper_chunks: 0,
                generated_digest_ratio: 0.0,
                generated_wrapper_ratio: 0.0,
                hidden_purge_candidates: 1,
                estimated_payload_bytes: 10,
                risk: "low".to_string(),
                reason: "hidden rows".to_string(),
                command_preview: "memd purge --tenant-id smoke".to_string(),
                destructive_command_preview: Some("memd purge --tenant-id smoke".to_string()),
                verification_command_preview: Some(
                    "memd purge-archive --archive /tmp/archive/smoke__hidden_purge_archive.json --expect-tenant-id smoke --min-records 1".to_string(),
                ),
                estimated_batches: Some(1),
                batch_command_previews: vec![CleanupBatchCommandPreview {
                    batch: 1,
                    limit: 1,
                    archive: "/tmp/archive/smoke__hidden_purge_batch001_archive.json"
                        .to_string(),
                    dry_run_command: "memd purge --tenant-id smoke --limit 1".to_string(),
                    destructive_command:
                        "memd purge --tenant-id smoke --limit 1 --archive /tmp/archive/smoke__hidden_purge_batch001_archive.json --apply"
                            .to_string(),
                    verification_command:
                        "memd purge-archive --archive /tmp/archive/smoke__hidden_purge_batch001_archive.json --expect-tenant-id smoke --min-records 1"
                            .to_string(),
                }],
            }],
            tenants: Vec::new(),
        };
        let rendered = render_markdown(&report);
        assert!(rendered.contains("# memd cleanup plan"));
        assert!(rendered.contains("Approval Summary"));
        assert!(rendered.contains("destructive_command_previews: `1`"));
        assert!(rendered.contains("batch_command_previews: `1`"));
        assert!(rendered.contains("Action Rollups"));
        assert!(rendered.contains("Post-Cleanup Verification"));
        assert!(rendered.contains("hard_purge_hidden_rows"));
        assert!(rendered.contains("Approval Items"));
        assert!(rendered.contains("approval_id"));
        assert!(rendered.contains("command_is_destructive: `true`"));
        assert!(rendered.contains("memd purge --tenant-id smoke"));
        assert!(rendered.contains("memd audit --tenant-id smoke"));
        assert!(rendered.contains("memd eval-memory-md --project-dir ."));
        assert!(rendered.contains("verify_archive"));
        assert!(rendered.contains("batch_commands"));
        assert!(rendered.contains("batch=`1`; limit=`1`"));
        assert!(rendered.contains("dry_run: `memd purge --tenant-id smoke --limit 1`"));
    }

    #[test]
    fn post_cleanup_verification_commands_include_scope_and_thresholds() {
        let verification = post_cleanup_verification_commands(
            Some("tenant 1"),
            Some("project 2"),
            Path::new("."),
            Path::new("tasks/archive dir"),
            45,
            321,
            2000,
            7,
            None,
            true,
        );

        assert_eq!(verification.commands.len(), 6);
        let cleanup = verification
            .commands
            .iter()
            .find(|command| command.label == "cleanup_plan_rerun")
            .expect("cleanup plan verification command");
        assert!(cleanup
            .command
            .contains("memd cleanup-plan --tenant-id 'tenant 1' --project-id 'project 2'"));
        assert!(cleanup.command.contains("--older-than-days 45"));
        assert!(cleanup.command.contains("--candidate-limit 321"));
        assert!(cleanup.command.contains("--page-size 2000"));
        assert!(cleanup.command.contains("--top-projects 7"));
        assert!(cleanup.command.contains("'tasks/archive dir'"));

        let retrieval = verification
            .commands
            .iter()
            .find(|command| command.label == "retrieval_quality")
            .expect("retrieval verification command");
        assert!(retrieval
            .command
            .contains("memd eval-retrieval --tenant-id 'tenant 1' --project-id 'project 2'"));
        assert!(retrieval
            .command
            .contains("--queries evals/bench/queries/retrieval_queries.jsonl"));
        assert!(retrieval.command.contains("--min-hit-rate-at-k 0.8"));
        assert!(retrieval.command.contains("--min-known-recall-at-k 0.6"));
        assert!(retrieval.command.contains("--min-mrr 0.35"));

        let quality = verification
            .commands
            .iter()
            .find(|command| command.label == "startup_memory_quality")
            .expect("memory quality verification command");
        assert!(quality.command.contains("--min-useful-ratio 0.8"));
        assert!(quality.command.contains("--max-generated-wrappers 0"));
        assert!(quality.pass_criteria.contains("useful_ratio >= 0.8"));
    }

    #[test]
    fn post_cleanup_verification_uses_repo_memory_scope_for_tenant_only_plan() {
        let verification = post_cleanup_verification_commands(
            Some("advanced_benchmark"),
            None,
            Path::new("."),
            Path::new("tasks/archive"),
            30,
            1000,
            1000,
            15,
            Some(&project_scope("memd", Some("memd"))),
            true,
        );

        let quality = verification
            .commands
            .iter()
            .find(|command| command.label == "startup_memory_quality")
            .expect("memory quality verification command");
        assert_eq!(
            quality.command,
            "memd eval-memory-md --tenant-id memd --project-id memd --project-dir . --output tasks/memory-post-cleanup.md --min-useful-ratio 0.8 --max-generated-wrappers 0"
        );

        let retrieval = verification
            .commands
            .iter()
            .find(|command| command.label == "retrieval_quality")
            .expect("retrieval verification command");
        assert!(retrieval
            .command
            .contains("memd eval-retrieval --tenant-id memd --project-id memd"));
        assert!(!retrieval
            .command
            .contains("advanced_benchmark --project-id"));

        let audit = verification
            .commands
            .iter()
            .find(|command| command.label == "audit_after_cleanup")
            .expect("audit verification command");
        assert!(audit
            .command
            .contains("memd audit --tenant-id advanced_benchmark"));
    }

    #[test]
    fn approval_items_include_high_noise_reviews() {
        let tenant = TenantCleanupPlan {
            tenant_id: "memd".to_string(),
            project_id_filter: Some("memd".to_string()),
            disk_total_bytes: Some(100),
            stats: StoreStatsReport {
                total_chunks: 10,
                active_chunks: 10,
                deleted_chunks: 0,
            },
            metadata_active_chunks: 10,
            scanned_chunks: 10,
            unreadable_active_chunks: 0,
            readable_active_ratio: 1.0,
            generated_digest_chunks: 9,
            generated_digest_ratio: 0.9,
            generated_wrapper_chunks: 1,
            generated_wrapper_ratio: 0.1,
            routine_progress_chunks: 0,
            unbounded_progress_chunks: 0,
            unbounded_progress_older_30d: 0,
            unscoped_chunks: 0,
            hidden_purge_candidates: 0,
            estimated_purge_payload_bytes: 0,
            classification: vec!["high_noise_review".to_string()],
            reasons: vec!["generated digest ratio 90.0%".to_string()],
            export_command: export_command("memd", Some("memd"), Path::new("/tmp/archive")),
            purge_command_preview: None,
            projects: vec![ProjectCleanupPlan {
                project_id: Some("memd".to_string()),
                chunks: 10,
                generated_digest_chunks: 9,
                generated_wrapper_chunks: 1,
                routine_progress_chunks: 0,
                unbounded_progress_chunks: 0,
                unbounded_progress_older_30d: 0,
                classification: vec!["high_noise_review".to_string()],
                reasons: vec!["generated digest ratio 90.0%".to_string()],
                export_command: export_command("memd", Some("memd"), Path::new("/tmp/archive")),
            }],
        };

        let items = approval_items(&[tenant], Path::new("/tmp/archive"));
        assert!(items
            .iter()
            .any(|item| item.action == "review_high_noise_tenant"));
        assert!(items.iter().any(|item| {
            item.action == "review_high_noise_tenant" && item.project_id.as_deref() == Some("memd")
        }));
        assert!(items
            .iter()
            .any(|item| item.command_preview.contains("--include-history true")));
        assert!(!items.iter().any(|item| {
            item.action == "review_project_scope_or_noise"
                && item.project_id.as_deref() == Some("memd")
        }));
    }
}
