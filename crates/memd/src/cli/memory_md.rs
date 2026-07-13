use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::args::{CliQueryMode, ProjectScopeConfig};
use super::paths::absolutize_project_dir;
use super::report::memory_health_lines;
use super::search::cli_search_payload_silent;
use crate::error::{MemdError, Result};
use crate::hit_stats::{
    aggregate_hits_at_data_dir, aggregate_hits_in, HitStats, DEFAULT_SUMMARY_TTL_MS,
};
use crate::store::{Store, TenantManager};
use crate::types::TenantId;

mod action;
mod collect;
mod evaluate;
mod rank;
mod render;
mod state;

#[cfg(test)]
mod tests;

pub(super) use action::explicit_agent_action;
use collect::collect_project_state;
use evaluate::evaluate_agent_usefulness;
pub(super) use evaluate::run_memory_md_eval;
use rank::{
    assign_active_project_candidates, collect_ranked_takeaways_with_explanations,
    dedupe_memory_md_union, recompute_union_priorities, reconcile_candidate_explanations,
    sort_takeaways, suppress_unrelated_machine_takeaways, RankedTakeawayCollection,
};
use render::render_memory_md;

const PROJECT_QUERIES: &[(CliQueryMode, &str, &str)] = &[
    (
        CliQueryMode::FindHighlights,
        "project_highlights",
        "project takeaways best practices key decisions recurring issues important files paths how to solve",
    ),
    (
        CliQueryMode::FindDecisions,
        "project_decisions",
        "project architecture configuration deployment key decisions tradeoffs",
    ),
    (
        CliQueryMode::FindFailures,
        "project_failures",
        "project recurring failures bugs timeouts blockers fixes how to solve",
    ),
    (
        CliQueryMode::FindHighlights,
        "project_library",
        "consolidated highlight library ranked lessons future-agent uplift",
    ),
];

const GLOBAL_QUERIES: &[(CliQueryMode, &str, &str)] = &[
    (
        CliQueryMode::FindHighlights,
        "global_highlights",
        "machine wide reusable takeaways best practices recurring issues important paths how to solve",
    ),
    (
        CliQueryMode::FindDecisions,
        "global_decisions",
        "cross project general decisions best practices configuration deployment",
    ),
    (
        CliQueryMode::FindFailures,
        "global_failures",
        "cross project recurring failures timeouts blockers fixes how to solve",
    ),
    (
        CliQueryMode::FindHighlights,
        "global_library",
        "consolidated highlight library ranked lessons future-agent uplift",
    ),
];

/// Priority threshold above which a user-tagged lesson is preserved
/// even if a digest already covers its task. Mirrors the rule that
/// explicit `priority:N` always wins on overlap.
const USER_PRESERVE_PRIORITY_THRESHOLD: u8 = 8;

/// Recency window for the retrieval hit aggregator (days). Recent
/// hits drive the load-bearing priority bonus.
const HIT_WINDOW_DAYS: u32 = 30;

/// Age in ms above which a chunk with zero hits is considered stale.
const STALE_CHUNK_AGE_MS: i64 = 30 * 86_400_000;
const PROJECT_STATE_FILE_CAP_BYTES: u64 = 256 * 1024;
const HANDOFF_FILE_CAP_BYTES: u64 = 128 * 1024;
const GIT_STATUS_TIMEOUT: Duration = Duration::from_millis(1_500);
const READABLE_SCAN_PAGE_SIZE: usize = 1_000;
const READABLE_SCAN_MAX_METADATA_ROWS: usize = 10_000;

const TAKEAWAY_CATEGORIES: &[(&str, u8)] = &[
    ("Decisions", 0),
    ("Validated Fixes", 1),
    ("Known Failures", 2),
    ("Commands/Paths", 3),
    ("Open Follow-ups", 4),
    ("Evidence", 5),
    ("Other Takeaways", 6),
];

#[derive(Debug)]
pub(super) struct MemoryMdOptions {
    pub(super) tenant_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) output: PathBuf,
    pub(super) project_limit: usize,
    pub(super) global_limit: usize,
    pub(super) candidate_k: usize,
    pub(super) explain_output: Option<PathBuf>,
}

#[derive(Debug)]
pub(super) struct MemoryMdEvalOptions {
    pub(super) tenant_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) output: PathBuf,
    pub(super) project_limit: usize,
    pub(super) candidate_k: usize,
    pub(super) top_n: usize,
    pub(super) min_useful_ratio: f64,
    pub(super) max_generated_wrappers: usize,
    pub(super) agent_usefulness: bool,
    pub(super) gold_file: Option<PathBuf>,
}

pub(super) async fn refresh_memory_md<S: Store>(
    store: &S,
    options: MemoryMdOptions,
) -> Result<Value> {
    refresh_memory_md_with_health(store, None, options).await
}

pub(super) async fn refresh_memory_md_with_health<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: MemoryMdOptions,
) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let scope = read_project_scope(&project_dir)?;
    let generated_unix_ms = now_ms();
    let tenant_id = options
        .tenant_id
        .or_else(|| scope.as_ref().map(|scope| scope.tenant_id.clone()))
        .ok_or_else(|| {
            MemdError::ValidationError(
                "memory-md requires --tenant-id or .memd/project_scope.json".to_string(),
            )
        })?;
    let project_id = options
        .project_id
        .or_else(|| scope.as_ref().and_then(|scope| scope.project_id.clone()));
    let tenant = TenantId::new(&tenant_id)?;
    let candidate_k = options.candidate_k.clamp(1, 200);
    let project_limit = options.project_limit.min(10);
    let global_limit = options.global_limit.min(10);
    let union_pool_limit = candidate_k
        .saturating_mul(2)
        .min(200)
        .max(project_limit)
        .max(global_limit);
    let health_lines = match memory_health_lines(
        store,
        tenant_manager,
        tenant.as_str(),
        project_id.as_deref(),
    )
    .await
    {
        Ok(lines) => lines,
        Err(error) => {
            tracing::debug!(?error, "memory health header skipped");
            Vec::new()
        }
    };

    // Aggregate retrieval hits once per refresh; the same `HitStats`
    // map is shared with every `priority_score` call so we don't
    // re-read the JSONL log per chunk. Prefer the central store log
    // (keyed by globally-unique chunk_id, so other projects' records are
    // ignored when we look up this project's chunks); fall back to the
    // cwd-relative log only when there is no resolved data_dir.
    let hit_stats = match tenant_manager {
        Some(tm) => {
            aggregate_hits_at_data_dir(tm.data_dir(), HIT_WINDOW_DAYS, DEFAULT_SUMMARY_TTL_MS)
        }
        None => aggregate_hits_in(&project_dir, HIT_WINDOW_DAYS, DEFAULT_SUMMARY_TTL_MS),
    };
    let project_state = collect_project_state(
        store,
        &tenant,
        tenant.as_str(),
        project_id.as_deref(),
        &project_dir,
        scope.as_ref(),
        generated_unix_ms,
    )
    .await;
    let project_collection = if project_limit == 0 {
        RankedTakeawayCollection {
            takeaways: Vec::new(),
            explanations: Vec::new(),
        }
    } else {
        collect_ranked_takeaways_with_explanations(
            store,
            tenant.as_str(),
            project_id.as_deref(),
            PROJECT_QUERIES,
            candidate_k,
            union_pool_limit,
            &hit_stats,
            "project",
        )
        .await?
    };
    let global_collection = if global_limit == 0 {
        RankedTakeawayCollection {
            takeaways: Vec::new(),
            explanations: Vec::new(),
        }
    } else {
        collect_ranked_takeaways_with_explanations(
            store,
            tenant.as_str(),
            None,
            GLOBAL_QUERIES,
            candidate_k,
            union_pool_limit,
            &hit_stats,
            "machine_wide",
        )
        .await?
    };

    let RankedTakeawayCollection {
        takeaways: mut project_takeaways,
        explanations: mut project_explanations,
    } = project_collection;
    let RankedTakeawayCollection {
        takeaways: mut global_takeaways,
        explanations: mut global_explanations,
    } = global_collection;
    assign_active_project_candidates(
        &mut project_takeaways,
        &mut global_takeaways,
        &mut project_explanations,
        &mut global_explanations,
        project_id.as_deref(),
    );
    let union_breakdowns = recompute_union_priorities(
        &mut project_takeaways,
        &mut global_takeaways,
        &hit_stats,
        now_ms() as i64,
    );
    let mut union_suppressed = suppress_unrelated_machine_takeaways(&mut global_takeaways);
    union_suppressed.extend(dedupe_memory_md_union(
        &mut project_takeaways,
        &mut global_takeaways,
    ));
    sort_takeaways(&mut project_takeaways);
    sort_takeaways(&mut global_takeaways);
    project_takeaways.truncate(project_limit);
    global_takeaways.truncate(global_limit);
    reconcile_candidate_explanations(
        &mut project_explanations,
        "project",
        &project_takeaways,
        &union_suppressed,
        &union_breakdowns,
    );
    reconcile_candidate_explanations(
        &mut global_explanations,
        "machine_wide",
        &global_takeaways,
        &union_suppressed,
        &union_breakdowns,
    );
    let agent_usefulness =
        evaluate_agent_usefulness(&project_state, &project_takeaways, &global_takeaways);
    let output_path = if options.output.is_absolute() {
        options.output
    } else {
        project_dir.join(options.output)
    };
    let rendered = render_memory_md(
        &project_state,
        &health_lines,
        &project_takeaways,
        &global_takeaways,
    );
    std::fs::write(&output_path, rendered)?;

    let explain_output = if let Some(path) = options.explain_output {
        let path = if path.is_absolute() {
            path
        } else {
            project_dir.join(path)
        };
        let report = json!({
            "tenant_id": tenant.to_string(),
            "project_id": project_id.clone(),
            "generated_unix_ms": generated_unix_ms,
            "candidate_k": candidate_k,
            "union_pool_limit": union_pool_limit,
            "limits": {
                "project": project_limit,
                "machine_wide": global_limit,
            },
            "project_state": project_state.clone(),
            "agent_usefulness": agent_usefulness.clone(),
            "project": project_explanations,
            "machine_wide": global_explanations,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
        Some(path)
    } else {
        None
    };

    Ok(json!({
        "tenant_id": tenant.to_string(),
        "project_id": project_id,
        "generated_unix_ms": generated_unix_ms,
        "output": output_path,
        "explain_output": explain_output,
        "project_takeaways": project_takeaways.len(),
        "global_takeaways": global_takeaways.len(),
        "candidate_k": candidate_k,
        "union_pool_limit": union_pool_limit,
        "project_state": project_state,
        "agent_usefulness": agent_usefulness
    }))
}

fn read_project_scope(project_dir: &std::path::Path) -> Result<Option<ProjectScopeConfig>> {
    let path = project_dir.join(".memd/project_scope.json");
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    let scope = serde_json::from_str(&text).map_err(|e| {
        MemdError::ValidationError(format!("failed to parse {}: {e}", path.display()))
    })?;
    Ok(Some(scope))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
