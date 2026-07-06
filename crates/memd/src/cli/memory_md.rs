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
use super::search::cli_search_payload;
use crate::error::{MemdError, Result};
use crate::hit_stats::{
    aggregate_hits_at_data_dir, aggregate_hits_in, HitStats, DEFAULT_SUMMARY_TTL_MS,
};
use crate::store::{Store, TenantManager};
use crate::types::TenantId;

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

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ProjectState {
    generated_unix_ms: u128,
    tenant_id: String,
    project_id: Option<String>,
    configured_project_dir: Option<String>,
    resolved_project_dir: String,
    scope_warnings: Vec<String>,
    git: GitState,
    latest_task: Option<StateSignal>,
    latest_handoff: Option<StateSignal>,
    latest_vcs: Option<StateSignal>,
    next_actions: Vec<NextAction>,
    memory: MemoryState,
    collection_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct GitState {
    available: bool,
    not_git_repo: bool,
    branch: Option<String>,
    clean: Option<bool>,
    changed_entries: usize,
    summary: String,
    warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct StateSignal {
    source_path: String,
    line: Option<usize>,
    heading: Option<String>,
    text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct NextAction {
    source_path: String,
    line: usize,
    heading: Option<String>,
    text: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
struct MemoryState {
    metadata_active_chunks: Option<usize>,
    readable_active_chunks: Option<usize>,
    unreadable_active_chunks: Option<usize>,
    scan_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct AgentUsefulnessMetrics {
    latest_project_state_present: bool,
    scope_present: bool,
    git_state_present: bool,
    git_state_present_or_not_git_repo: bool,
    latest_work_present: bool,
    next_action_present: bool,
    no_open_tasks_detected: bool,
    scope_health_present: bool,
    memory_degraded_warning_present: bool,
    fragment_count: usize,
    duplicate_cluster_count: usize,
    boilerplate_action_count: usize,
    unrelated_machine_items: usize,
    source_backed_next_actions: bool,
    answerability_passed: bool,
}

#[derive(Debug, Clone)]
struct Takeaway {
    chunk_id: String,
    tenant_id: String,
    project_id: Option<String>,
    text: String,
    score: f32,
    priority: f32,
    chunk_type: String,
    timestamp_created: i64,
    tags: Vec<String>,
    sources: BTreeSet<String>,
    occurrences: usize,
}

#[derive(Debug, Clone)]
struct RankedTakeawayCollection {
    takeaways: Vec<Takeaway>,
    explanations: Vec<MemoryMdCandidateExplanation>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PriorityBreakdown {
    explicit: f32,
    kind_weight: f32,
    type_weight: f32,
    recurrence: f32,
    multi_query: f32,
    search_score: f32,
    library_bonus: f32,
    utility: f32,
    staleness_penalty: f32,
    total: f32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct MemoryMdCandidateExplanation {
    section: String,
    source: String,
    query: String,
    mode: String,
    raw_rank: usize,
    chunk_id: String,
    tenant_id: String,
    project_id: Option<String>,
    chunk_type: String,
    timestamp_created: i64,
    search_score: f32,
    priority_score: Option<f32>,
    priority_breakdown: Option<PriorityBreakdown>,
    display_status: String,
    filter_reason: Option<String>,
    display_rank: Option<usize>,
    generated_digest: bool,
    quality_flags: Vec<String>,
    topic_key: Option<String>,
    tags: Vec<String>,
    matched_sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TakeawayCategory {
    heading: &'static str,
    reason: &'static str,
    order: u8,
}

#[derive(Debug, Clone, PartialEq)]
struct MemoryMdQualityReport {
    displayed_count: usize,
    useful_count: usize,
    generated_wrapper_count: usize,
    missing_reason_count: usize,
    missing_action_count: usize,
    useful_ratio: f64,
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
            project_limit,
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
            global_limit,
            &hit_stats,
            "machine_wide",
        )
        .await?
    };

    let RankedTakeawayCollection {
        takeaways: project_takeaways,
        explanations: project_explanations,
    } = project_collection;
    let RankedTakeawayCollection {
        takeaways: global_takeaways,
        explanations: global_explanations,
    } = global_collection;
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
        "project_state": project_state,
        "agent_usefulness": agent_usefulness
    }))
}

pub(super) async fn run_memory_md_eval<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: MemoryMdEvalOptions,
) -> Result<Value> {
    if options.gold_file.is_some() {
        return run_memory_md_gold_eval(store, tenant_manager, &options).await;
    }

    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let output = options.output.clone();
    let rendered_path = if output.is_absolute() {
        output.clone()
    } else {
        project_dir.join(&output)
    };
    let refresh = refresh_memory_md_with_health(
        store,
        tenant_manager,
        MemoryMdOptions {
            tenant_id: options.tenant_id,
            project_id: options.project_id,
            project_dir,
            output,
            project_limit: options.project_limit,
            global_limit: if options.agent_usefulness { 2 } else { 0 },
            candidate_k: options.candidate_k,
            explain_output: None,
        },
    )
    .await?;
    let content = std::fs::read_to_string(&rendered_path)?;
    let top_n = options.top_n.clamp(1, 10);
    let report = evaluate_memory_md_quality(&content, top_n);
    let min_useful_ratio = options.min_useful_ratio.clamp(0.0, 1.0);

    let mut failures = Vec::new();
    if report.displayed_count == 0 {
        failures.push("no project takeaways were displayed".to_string());
    }
    if report.useful_ratio + f64::EPSILON < min_useful_ratio {
        failures.push(format!(
            "useful_ratio {:.3} below threshold {:.3}",
            report.useful_ratio, min_useful_ratio
        ));
    }
    if report.generated_wrapper_count > options.max_generated_wrappers {
        failures.push(format!(
            "generated_wrapper_count {} exceeds threshold {}",
            report.generated_wrapper_count, options.max_generated_wrappers
        ));
    }
    if report.missing_reason_count > 0 {
        failures.push(format!(
            "{} displayed items are missing reason metadata",
            report.missing_reason_count
        ));
    }
    if report.missing_action_count > 0 {
        failures.push(format!(
            "{} displayed items are missing concrete agent action guidance",
            report.missing_action_count
        ));
    }
    let agent_usefulness = if options.agent_usefulness {
        let metrics: AgentUsefulnessMetrics = serde_json::from_value(
            refresh
                .get("agent_usefulness")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .map_err(|error| {
            MemdError::ValidationError(format!("invalid agent_usefulness metrics: {error}"))
        })?;
        failures.extend(agent_usefulness_failures(&metrics));
        Some(metrics)
    } else {
        None
    };

    let payload = json!({
        "passed": failures.is_empty(),
        "output": rendered_path,
        "agent_usefulness": agent_usefulness,
        "top_n": top_n,
        "displayed_count": report.displayed_count,
        "useful_count": report.useful_count,
        "useful_ratio": report.useful_ratio,
        "generated_wrapper_count": report.generated_wrapper_count,
        "missing_reason_count": report.missing_reason_count,
        "missing_action_count": report.missing_action_count,
        "thresholds": {
            "min_useful_ratio": min_useful_ratio,
            "max_generated_wrappers": options.max_generated_wrappers,
        },
        "refresh": refresh,
        "failures": failures,
    });

    if !failures.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "memory-md quality thresholds failed: {}",
            serde_json::to_string(&payload)?
        )));
    }

    Ok(payload)
}

#[derive(Debug, Deserialize)]
struct MemoryMdGoldFile {
    projects: Vec<MemoryMdGoldProject>,
}

#[derive(Debug, Deserialize)]
struct MemoryMdGoldProject {
    name: Option<String>,
    project_dir: PathBuf,
    must_contain: Option<Vec<String>>,
    must_not_contain: Option<Vec<String>>,
    expected_git: Option<bool>,
    max_fragments: Option<usize>,
    max_unrelated_machine_items: Option<usize>,
}

async fn run_memory_md_gold_eval<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: &MemoryMdEvalOptions,
) -> Result<Value> {
    let gold_path = options
        .gold_file
        .as_ref()
        .expect("caller checked gold_file");
    let gold_text = fs::read_to_string(gold_path)?;
    let gold: MemoryMdGoldFile = serde_json::from_str(&gold_text).map_err(|error| {
        MemdError::ValidationError(format!("failed to parse {}: {error}", gold_path.display()))
    })?;

    let mut project_results = Vec::new();
    let mut failures = Vec::new();
    for (idx, project) in gold.projects.iter().enumerate() {
        let name = project
            .name
            .clone()
            .unwrap_or_else(|| project.project_dir.display().to_string());
        let output = std::env::temp_dir().join(format!("memd-memory-md-gold-{idx}.md"));
        let refresh = refresh_memory_md_with_health(
            store,
            tenant_manager,
            MemoryMdOptions {
                tenant_id: options.tenant_id.clone(),
                project_id: options.project_id.clone(),
                project_dir: project.project_dir.clone(),
                output: output.clone(),
                project_limit: options.project_limit,
                global_limit: 2,
                candidate_k: options.candidate_k,
                explain_output: None,
            },
        )
        .await?;
        let content = fs::read_to_string(&output)?;
        let metrics: AgentUsefulnessMetrics = serde_json::from_value(
            refresh
                .get("agent_usefulness")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .map_err(|error| {
            MemdError::ValidationError(format!("invalid agent_usefulness metrics: {error}"))
        })?;
        let mut project_failures = agent_usefulness_failures(&metrics);
        if let Some(expected_git) = project.expected_git {
            if let Some(failure) = expected_git_failure(&metrics, expected_git) {
                project_failures.push(failure);
            }
        }
        if let Some(max_fragments) = project.max_fragments {
            if metrics.fragment_count > max_fragments {
                project_failures.push(format!(
                    "fragment_count {} exceeds {}",
                    metrics.fragment_count, max_fragments
                ));
            }
        }
        if let Some(max_unrelated) = project.max_unrelated_machine_items {
            if metrics.unrelated_machine_items > max_unrelated {
                project_failures.push(format!(
                    "unrelated_machine_items {} exceeds {}",
                    metrics.unrelated_machine_items, max_unrelated
                ));
            }
        }
        for needle in project.must_contain.as_deref().unwrap_or(&[]) {
            if !content.contains(needle) {
                project_failures.push(format!("missing required text `{needle}`"));
            }
        }
        for needle in project.must_not_contain.as_deref().unwrap_or(&[]) {
            if content.contains(needle) {
                project_failures.push(format!("forbidden text present `{needle}`"));
            }
        }
        for failure in &project_failures {
            failures.push(format!("{name}: {failure}"));
        }
        project_results.push(json!({
            "name": name,
            "project_dir": project.project_dir.display().to_string(),
            "output": output,
            "agent_usefulness": metrics,
            "failures": project_failures,
        }));
    }

    let payload = json!({
        "passed": failures.is_empty(),
        "gold_file": gold_path,
        "projects": project_results,
        "failures": failures,
    });
    if !failures.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "memory-md gold-file thresholds failed: {}",
            serde_json::to_string(&payload)?
        )));
    }
    Ok(payload)
}

fn agent_usefulness_failures(metrics: &AgentUsefulnessMetrics) -> Vec<String> {
    let mut failures = Vec::new();
    if !metrics.latest_project_state_present {
        failures.push("latest project state is missing".to_string());
    }
    if !metrics.scope_present {
        failures.push("scope is missing".to_string());
    }
    if !metrics.git_state_present_or_not_git_repo {
        failures.push("git state is missing for a git project".to_string());
    }
    if !metrics.latest_work_present {
        failures.push("latest work signal is missing".to_string());
    }
    if !metrics.next_action_present && !metrics.no_open_tasks_detected {
        failures.push("next actions are missing while open tasks exist".to_string());
    }
    if !metrics.scope_health_present {
        failures.push("scope health is missing".to_string());
    }
    if metrics.fragment_count > 0 {
        failures.push(format!(
            "fragment_count {} exceeds threshold 0",
            metrics.fragment_count
        ));
    }
    if metrics.boilerplate_action_count > 0 {
        failures.push(format!(
            "boilerplate_action_count {} exceeds threshold 0",
            metrics.boilerplate_action_count
        ));
    }
    if metrics.unrelated_machine_items > 2 {
        failures.push(format!(
            "unrelated_machine_items {} exceeds threshold 2",
            metrics.unrelated_machine_items
        ));
    }
    if !metrics.source_backed_next_actions {
        failures.push("one or more next actions lack source path or line".to_string());
    }
    if !metrics.answerability_passed {
        failures.push("answerability_passed=false".to_string());
    }
    failures
}

fn expected_git_failure(metrics: &AgentUsefulnessMetrics, expected_git: bool) -> Option<String> {
    match (expected_git, metrics.git_state_present) {
        (true, false) => Some("expected git state but git_state_present=false".to_string()),
        (false, true) => Some("expected no git state but git_state_present=true".to_string()),
        _ => None,
    }
}

async fn collect_project_state<S: Store>(
    store: &S,
    tenant: &TenantId,
    tenant_id: &str,
    project_id: Option<&str>,
    project_dir: &Path,
    scope: Option<&ProjectScopeConfig>,
    generated_unix_ms: u128,
) -> ProjectState {
    let configured_project_dir = scope.map(|scope| scope.project_dir.clone());
    let mut collection_warnings = Vec::new();
    let mut scope_warnings = Vec::new();
    if let Some(configured) = configured_project_dir.as_deref() {
        if let Some(warning) = scope_path_drift_warning(configured, project_dir) {
            scope_warnings.push(warning);
        }
    }

    let git = collect_git_state(project_dir, "git");
    let latest_vcs = git
        .available
        .then(|| collect_latest_git_commit(project_dir, "git"))
        .flatten();
    let task_scan = collect_task_state(project_dir);
    collection_warnings.extend(task_scan.warnings);
    let handoff_scan = collect_handoff_state(project_dir);
    collection_warnings.extend(handoff_scan.warnings);
    let memory = collect_memory_state(store, tenant, project_id).await;

    ProjectState {
        generated_unix_ms,
        tenant_id: tenant_id.to_string(),
        project_id: project_id.map(str::to_string),
        configured_project_dir,
        resolved_project_dir: canonical_or_lexical_path(project_dir).display().to_string(),
        scope_warnings,
        git,
        latest_task: task_scan.latest_task,
        latest_handoff: handoff_scan.latest_handoff,
        latest_vcs,
        next_actions: task_scan.next_actions,
        memory,
        collection_warnings,
    }
}

#[derive(Debug, Default)]
struct TaskScan {
    latest_task: Option<StateSignal>,
    next_actions: Vec<NextAction>,
    warnings: Vec<String>,
}

#[derive(Debug, Default)]
struct HandoffScan {
    latest_handoff: Option<StateSignal>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct Heading {
    level: usize,
    title: String,
    line: usize,
}

fn collect_task_state(project_dir: &Path) -> TaskScan {
    let path = project_dir.join("tasks/todo.md");
    if !path.exists() {
        return TaskScan::default();
    }
    let relative = relative_path(project_dir, &path);
    let mut warnings = Vec::new();
    let text = match read_text_capped(&path, PROJECT_STATE_FILE_CAP_BYTES) {
        Ok((text, truncated)) => {
            if truncated {
                warnings.push(format!(
                    "{} was truncated to {} bytes while collecting project state",
                    relative, PROJECT_STATE_FILE_CAP_BYTES
                ));
            }
            text
        }
        Err(error) => {
            return TaskScan {
                warnings: vec![format!("could not read {relative}: {error}")],
                ..TaskScan::default()
            };
        }
    };

    let mut stack: Vec<Heading> = Vec::new();
    let mut headings = Vec::new();
    let mut next_actions = Vec::new();

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if let Some(heading) = parse_heading(line, line_no) {
            while stack
                .last()
                .map(|existing| existing.level >= heading.level)
                .unwrap_or(false)
            {
                stack.pop();
            }
            stack.push(heading.clone());
            headings.push(heading);
            continue;
        }

        if let Some(action) = parse_next_action(line) {
            let heading = stack.last().map(|heading| heading.title.clone());
            next_actions.push(NextAction {
                source_path: relative.clone(),
                line: line_no,
                heading,
                text: action,
            });
        }
    }

    let latest_task = if let Some(first_action) = next_actions.first() {
        Some(StateSignal {
            source_path: first_action.source_path.clone(),
            line: Some(first_action.line),
            heading: first_action.heading.clone(),
            text: format!("open action: {}", first_action.text),
        })
    } else if let Some(heading) = headings
        .iter()
        .rev()
        .find(|heading| active_heading(&heading.title))
    {
        Some(StateSignal {
            source_path: relative,
            line: Some(heading.line),
            heading: Some(heading.title.clone()),
            text: "latest active section".to_string(),
        })
    } else {
        headings
            .iter()
            .rev()
            .find(|heading| completed_or_dated_heading(&heading.title))
            .map(|heading| StateSignal {
                source_path: relative,
                line: Some(heading.line),
                heading: Some(heading.title.clone()),
                text: "latest completed or dated section".to_string(),
            })
    };

    TaskScan {
        latest_task,
        next_actions,
        warnings,
    }
}

fn collect_handoff_state(project_dir: &Path) -> HandoffScan {
    let dir = project_dir.join("docs/handoffs");
    if !dir.exists() {
        return HandoffScan::default();
    }
    let mut warnings = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            return HandoffScan {
                warnings: vec![format!(
                    "could not read {}: {error}",
                    relative_path(project_dir, &dir)
                )],
                ..HandoffScan::default()
            };
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            warnings.push("could not read one docs/handoffs entry".to_string());
            continue;
        };
        let path = entry.path();
        if path
            .components()
            .any(|component| component.as_os_str() == "_archive")
        {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        candidates.push((modified, path));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let Some((_, path)) = candidates.into_iter().next() else {
        return HandoffScan {
            warnings,
            ..HandoffScan::default()
        };
    };
    let relative = relative_path(project_dir, &path);
    let text = match read_text_capped(&path, HANDOFF_FILE_CAP_BYTES) {
        Ok((text, truncated)) => {
            if truncated {
                warnings.push(format!(
                    "{} was truncated to {} bytes while collecting handoff state",
                    relative, HANDOFF_FILE_CAP_BYTES
                ));
            }
            text
        }
        Err(error) => {
            warnings.push(format!("could not read {relative}: {error}"));
            return HandoffScan {
                warnings,
                ..HandoffScan::default()
            };
        }
    };

    let mut title = None;
    let mut status_lines = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if title.is_none() {
            title = parse_heading(line, idx + 1).map(|heading| (idx + 1, heading.title));
        }
        let trimmed = line.trim();
        let lowered = trimmed.to_ascii_lowercase();
        if lowered.starts_with("status:")
            || lowered.starts_with("- status:")
            || lowered.starts_with("next:")
            || lowered.starts_with("next step:")
            || lowered.starts_with("follow-up:")
        {
            status_lines.push(trimmed.trim_start_matches("- ").to_string());
        }
        if status_lines.len() >= 2 {
            break;
        }
    }

    let (line, title_text) = title.unwrap_or((1, relative.clone()));
    let signal_text = if status_lines.is_empty() {
        "latest handoff".to_string()
    } else {
        status_lines.join(" | ")
    };
    HandoffScan {
        latest_handoff: Some(StateSignal {
            source_path: relative,
            line: Some(line),
            heading: Some(title_text),
            text: signal_text,
        }),
        warnings,
    }
}

async fn collect_memory_state<S: Store>(
    store: &S,
    tenant: &TenantId,
    project_id: Option<&str>,
) -> MemoryState {
    let snapshot = match store.health_snapshot(tenant, project_id, 0).await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return MemoryState::default(),
        Err(error) => {
            return MemoryState {
                scan_warning: Some(format!("memory health scan failed: {error}")),
                ..MemoryState::default()
            };
        }
    };
    let metadata_active = snapshot.counts.active_chunks;
    let mut readable = 0usize;
    let mut warning = None;
    let mut offset = 0usize;
    let scan_limit = metadata_active.min(READABLE_SCAN_MAX_METADATA_ROWS);
    while offset < scan_limit {
        let limit = READABLE_SCAN_PAGE_SIZE.min(scan_limit.saturating_sub(offset));
        match store
            .list_chunks_for_project(tenant, project_id, limit, offset)
            .await
        {
            Ok(chunks) => readable = readable.saturating_add(chunks.len()),
            Err(error) => {
                warning = Some(format!(
                    "readable memory scan failed at offset {offset}: {error}"
                ));
                break;
            }
        }
        offset = offset.saturating_add(limit);
    }
    if metadata_active > scan_limit && warning.is_none() {
        warning = Some(format!(
            "readable memory scan partial: checked {scan_limit} of {metadata_active} active chunks; unreadable count may be understated"
        ));
    }
    let unreadable = scan_limit.saturating_sub(readable);
    MemoryState {
        metadata_active_chunks: Some(metadata_active),
        readable_active_chunks: Some(readable),
        unreadable_active_chunks: Some(unreadable),
        scan_warning: warning,
    }
}

fn collect_git_state(project_dir: &Path, git_binary: &str) -> GitState {
    let mut command = Command::new(git_binary);
    command
        .arg("-C")
        .arg(project_dir)
        .args(["status", "--short", "--branch"]);
    let output = match run_command_with_timeout(command, GIT_STATUS_TIMEOUT) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return GitState {
                available: false,
                not_git_repo: false,
                branch: None,
                clean: None,
                changed_entries: 0,
                summary: "git unavailable: executable not found".to_string(),
                warning: Some("git unavailable: executable not found".to_string()),
            };
        }
        Err(error) => {
            return GitState {
                available: false,
                not_git_repo: false,
                branch: None,
                clean: None,
                changed_entries: 0,
                summary: format!("git unavailable: {error}"),
                warning: Some(format!("git unavailable: {error}")),
            };
        }
    };

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.timed_out {
        return GitState {
            available: false,
            not_git_repo: false,
            branch: None,
            clean: None,
            changed_entries: 0,
            summary: "git unavailable: status timed out".to_string(),
            warning: Some("git unavailable: status timed out".to_string()),
        };
    }
    if !output.status_success {
        let not_git_repo = stderr.contains("not a git repository");
        let reason = if not_git_repo {
            "not a git repository".to_string()
        } else if stderr.is_empty() {
            "git status failed".to_string()
        } else {
            stderr
        };
        return GitState {
            available: false,
            not_git_repo,
            branch: None,
            clean: None,
            changed_entries: 0,
            summary: format!("git unavailable: {reason}"),
            warning: Some(format!("git unavailable: {reason}")),
        };
    }

    let mut lines = stdout.lines();
    let branch = lines
        .next()
        .and_then(|line| line.strip_prefix("## "))
        .map(|line| line.split("...").next().unwrap_or(line).trim().to_string())
        .filter(|branch| !branch.is_empty());
    let changed_entries = lines.filter(|line| !line.trim().is_empty()).count();
    let clean = changed_entries == 0;
    let branch_label = branch.as_deref().unwrap_or("<unknown>");
    let summary = if clean {
        format!("branch `{branch_label}`; clean")
    } else {
        format!("branch `{branch_label}`; dirty ({changed_entries} changed entries)")
    };
    GitState {
        available: true,
        not_git_repo: false,
        branch,
        clean: Some(clean),
        changed_entries,
        summary,
        warning: None,
    }
}

fn collect_latest_git_commit(project_dir: &Path, git_binary: &str) -> Option<StateSignal> {
    let mut command = Command::new(git_binary);
    command.arg("-C").arg(project_dir).args([
        "log",
        "-1",
        "--date=short",
        "--pretty=format:%h %cd %s",
    ]);
    let output = run_command_with_timeout(command, GIT_STATUS_TIMEOUT).ok()?;
    if output.timed_out || !output.status_success {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(StateSignal {
        source_path: ".git".to_string(),
        line: None,
        heading: Some("latest commit".to_string()),
        text,
    })
}

struct TimedCommandOutput {
    status_success: bool,
    timed_out: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_command_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> std::io::Result<TimedCommandOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = thread::spawn(move || read_child_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_child_pipe(stderr));
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let stdout = join_child_pipe(stdout_reader)?;
            let stderr = join_child_pipe(stderr_reader)?;
            return Ok(TimedCommandOutput {
                status_success: status.success(),
                timed_out: false,
                stdout,
                stderr,
            });
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let stdout = join_child_pipe(stdout_reader)?;
            let stderr = join_child_pipe(stderr_reader)?;
            return Ok(TimedCommandOutput {
                status_success: false,
                timed_out: true,
                stdout,
                stderr,
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_child_pipe(mut pipe: Option<impl Read>) -> io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    if let Some(pipe) = pipe.as_mut() {
        pipe.read_to_end(&mut buffer)?;
    }
    Ok(buffer)
}

fn join_child_pipe(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "command output reader panicked"))?
}

fn parse_heading(line: &str, line_no: usize) -> Option<Heading> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || !trimmed[level..].starts_with(' ') {
        return None;
    }
    let title = trimmed[level..].trim().to_string();
    (!title.is_empty()).then_some(Heading {
        level,
        title,
        line: line_no,
    })
}

fn parse_next_action(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with("- [x]")
        || trimmed.starts_with("- [X]")
        || trimmed.starts_with("* [x]")
        || trimmed.starts_with("* [X]")
    {
        return None;
    }
    let body = trimmed
        .strip_prefix("- [ ]")
        .or_else(|| trimmed.strip_prefix("* [ ]"))
        .map(str::trim)
        .or_else(|| strip_bullet(trimmed));
    let candidate = body.unwrap_or(trimmed).trim();
    let lowered = candidate.to_ascii_lowercase();
    let explicit = lowered.starts_with("next step:")
        || lowered.starts_with("follow-up:")
        || lowered.starts_with("followup:")
        || lowered.starts_with("todo:")
        || lowered.starts_with("todo ")
        || lowered.starts_with("pending:")
        || lowered.starts_with("pending ");
    if trimmed.starts_with("- [ ]") || trimmed.starts_with("* [ ]") || explicit {
        Some(candidate.trim_end_matches('.').trim().to_string()).filter(|s| !s.is_empty())
    } else {
        None
    }
}

fn strip_bullet(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .map(str::trim)
}

fn active_heading(title: &str) -> bool {
    let lowered = title.to_ascii_lowercase();
    lowered.contains("in progress")
        || lowered.contains("todo")
        || lowered.contains("pending")
        || lowered.contains("open")
}

fn completed_or_dated_heading(title: &str) -> bool {
    let lowered = title.to_ascii_lowercase();
    lowered.contains("done")
        || lowered.contains("complete")
        || lowered.contains("completed")
        || lowered.contains("202")
}

fn read_text_capped(path: &Path, cap: u64) -> std::io::Result<(String, bool)> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > cap;
    if truncated {
        bytes.truncate(cap as usize);
    }
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

fn scope_path_drift_warning(configured: &str, resolved_project_dir: &Path) -> Option<String> {
    let configured_path = PathBuf::from(configured);
    let configured_abs = if configured_path.is_absolute() {
        configured_path
    } else {
        resolved_project_dir.join(configured_path)
    };
    let configured_norm = canonical_or_lexical_path(&configured_abs);
    let resolved_norm = canonical_or_lexical_path(resolved_project_dir);
    (configured_norm != resolved_norm).then(|| {
        format!(
            "scope mismatch: configured project_dir `{}` differs from resolved project_dir `{}`",
            configured_norm.display(),
            resolved_norm.display()
        )
    })
}

fn canonical_or_lexical_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize_path(path))
}

fn lexical_normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(Path::new("/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn relative_path(project_dir: &Path, path: &Path) -> String {
    path.strip_prefix(project_dir)
        .unwrap_or(path)
        .display()
        .to_string()
}

async fn collect_ranked_takeaways_with_explanations<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: Option<&str>,
    queries: &[(CliQueryMode, &str, &str)],
    candidate_k: usize,
    limit: usize,
    hit_stats: &HashMap<String, HitStats>,
    section: &str,
) -> Result<RankedTakeawayCollection> {
    let mut by_chunk: HashMap<String, Takeaway> = HashMap::new();
    let mut explanations = Vec::new();

    for (mode, source, query) in queries {
        let payload = cli_search_payload(
            store,
            tenant_id.to_string(),
            project_id.map(str::to_string),
            (*query).to_string(),
            candidate_k,
            false,
            false,
            None,
            *mode,
            false,
            false,
            false,
        )
        .await?;
        merge_payload_candidates(
            &mut by_chunk,
            &mut explanations,
            &payload,
            section,
            source,
            query,
            *mode,
        );
    }

    let tag_counts = recurring_tag_counts(by_chunk.values());
    let now_ms = now_ms() as i64;
    let mut breakdowns = HashMap::new();
    let mut takeaways = by_chunk
        .into_values()
        .map(|mut takeaway| {
            let breakdown = priority_breakdown(&takeaway, &tag_counts, hit_stats, now_ms);
            takeaway.priority = breakdown.total;
            breakdowns.insert(takeaway.chunk_id.clone(), breakdown);
            takeaway
        })
        .collect::<Vec<_>>();
    let scored_takeaways = takeaways.clone();
    let mut suppressed_reasons = suppress_finishes_covered_by_libraries(&mut takeaways)
        .into_iter()
        .map(|id| (id, "covered_by_library".to_string()))
        .collect::<HashMap<_, _>>();
    suppressed_reasons.extend(filter_startup_takeaways(&mut takeaways, section));
    takeaways.sort_by(|left, right| {
        right
            .priority
            .partial_cmp(&left.priority)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
    });
    let displayed_ids = takeaways
        .iter()
        .take(limit)
        .enumerate()
        .map(|(idx, takeaway)| (takeaway.chunk_id.clone(), idx + 1))
        .collect::<HashMap<_, _>>();
    takeaways.truncate(limit);
    finalize_candidate_explanations(
        &mut explanations,
        &scored_takeaways,
        &suppressed_reasons,
        &displayed_ids,
        &breakdowns,
    );
    Ok(RankedTakeawayCollection {
        takeaways,
        explanations,
    })
}

/// Drop raw `task:kind:task_finish` takeaways already represented by
/// a `highlight_library` or `project_brief` digest. The digest's
/// summary line `Covers tasks: task:id:<X>, ...` lists the source
/// task IDs (emitted in `ensure_highlight_library_digest` and
/// `build_project_brief_digest_artifact`). User-tagged lessons with
/// explicit `priority:N` >= USER_PRESERVE_PRIORITY_THRESHOLD survive.
///
/// The covered set is keyed by `(project_id, task_id)` so a digest
/// from one project never suppresses a same-id finish from another —
/// this matters for the machine-wide section of `memory.md`, which
/// spans projects.
fn suppress_finishes_covered_by_libraries(takeaways: &mut Vec<Takeaway>) -> BTreeSet<String> {
    let covered: BTreeSet<(Option<String>, String)> = takeaways
        .iter()
        .filter(|takeaway| is_library_digest(&takeaway.tags))
        .flat_map(|takeaway| {
            extract_covered_task_ids(&takeaway.text)
                .into_iter()
                .map(|id| (takeaway.project_id.clone(), id))
                .collect::<Vec<_>>()
        })
        .collect();
    if covered.is_empty() {
        return BTreeSet::new();
    }
    let suppressed = takeaways
        .iter()
        .filter(|takeaway| is_suppressible_finish(takeaway, &covered))
        .map(|takeaway| takeaway.chunk_id.clone())
        .collect::<BTreeSet<_>>();
    takeaways.retain(|takeaway| !suppressed.contains(&takeaway.chunk_id));
    suppressed
}

fn filter_startup_takeaways(
    takeaways: &mut Vec<Takeaway>,
    section: &str,
) -> HashMap<String, String> {
    let mut suppressed = HashMap::new();
    for takeaway in takeaways.iter() {
        let reason = if has_boilerplate_action(takeaway) {
            Some("boilerplate_action")
        } else if section == "machine_wide" && !is_machine_wide_startup_relevant(takeaway) {
            Some("machine_wide_unrelated")
        } else {
            None
        };
        if let Some(reason) = reason {
            suppressed.insert(takeaway.chunk_id.clone(), reason.to_string());
        }
    }
    takeaways.retain(|takeaway| !suppressed.contains_key(&takeaway.chunk_id));
    suppressed.extend(suppress_duplicate_topic_takeaways(takeaways));
    suppressed
}

fn suppress_duplicate_topic_takeaways(takeaways: &mut Vec<Takeaway>) -> HashMap<String, String> {
    let mut best_by_topic: BTreeMap<String, String> = BTreeMap::new();
    for takeaway in takeaways.iter() {
        let key = topic_key(takeaway);
        match best_by_topic.get(&key) {
            Some(existing_id) => {
                let existing = takeaways
                    .iter()
                    .find(|candidate| candidate.chunk_id == *existing_id)
                    .expect("best id came from takeaways");
                if takeaway_preferred(takeaway, existing) {
                    best_by_topic.insert(key, takeaway.chunk_id.clone());
                }
            }
            None => {
                best_by_topic.insert(key, takeaway.chunk_id.clone());
            }
        }
    }

    let mut suppressed = HashMap::new();
    for takeaway in takeaways.iter() {
        let key = topic_key(takeaway);
        if best_by_topic.get(&key) != Some(&takeaway.chunk_id) {
            suppressed.insert(takeaway.chunk_id.clone(), format!("duplicate_topic:{key}"));
        }
    }
    takeaways.retain(|takeaway| !suppressed.contains_key(&takeaway.chunk_id));
    suppressed
}

fn takeaway_preferred(candidate: &Takeaway, current: &Takeaway) -> bool {
    candidate
        .priority
        .partial_cmp(&current.priority)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| candidate.timestamp_created.cmp(&current.timestamp_created))
        .then_with(|| current.chunk_id.cmp(&candidate.chunk_id))
        .is_gt()
}

fn topic_key(takeaway: &Takeaway) -> String {
    let mut tag_key = takeaway
        .tags
        .iter()
        .filter(|tag| {
            tag.starts_with("topic:")
                || tag.starts_with("repo:")
                || tag.starts_with("task:id:")
                || tag.starts_with("ctx:subsystem:")
                || tag.starts_with("ctx:file:")
        })
        .cloned()
        .collect::<Vec<_>>();
    tag_key.sort();
    tag_key.dedup();
    if !tag_key.is_empty() {
        return tag_key.into_iter().take(4).collect::<Vec<_>>().join("|");
    }

    normalized_topic_terms(&takeaway.text)
}

fn normalized_topic_terms(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or(text);
    let mut words = Vec::new();
    for raw in first_line.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        let word = raw.to_ascii_lowercase();
        if word.len() < 4 || TOPIC_STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        words.push(word);
        if words.len() >= 8 {
            break;
        }
    }
    if words.is_empty() {
        summarize_text(text, 64).to_ascii_lowercase()
    } else {
        words.join("-")
    }
}

const TOPIC_STOPWORDS: &[&str] = &[
    "after",
    "also",
    "because",
    "before",
    "from",
    "into",
    "keep",
    "lesson",
    "lessons",
    "memory",
    "project",
    "records",
    "task",
    "that",
    "this",
    "validation",
    "with",
];

/// True only for system-generated library digests. The
/// `task:status:generated` requirement guards against a user-authored
/// chunk spoofing a `task:role:*` tag to suppress real finishes.
fn is_library_digest(tags: &[String]) -> bool {
    let generated = tags.iter().any(|tag| tag == "task:status:generated");
    let role = tags
        .iter()
        .any(|tag| tag == "task:role:highlight_library" || tag == "task:role:project_brief");
    generated && role
}

fn is_suppressible_finish(
    takeaway: &Takeaway,
    covered: &BTreeSet<(Option<String>, String)>,
) -> bool {
    let is_finish = takeaway
        .tags
        .iter()
        .any(|tag| tag == "task:kind:task_finish");
    if !is_finish {
        return false;
    }
    if user_priority_at_least(&takeaway.tags, USER_PRESERVE_PRIORITY_THRESHOLD) {
        return false;
    }
    takeaway.tags.iter().any(|tag| {
        tag.strip_prefix("task:id:")
            .map(|id| covered.contains(&(takeaway.project_id.clone(), id.to_string())))
            .unwrap_or(false)
    })
}

/// True if `tags` carry an explicit `priority:`/`importance:` value at
/// or above `threshold`. Parsed as `f32` to mirror `explicit_priority`
/// so a decimal tag like `priority:8.5` is honoured.
fn user_priority_at_least(tags: &[String], threshold: u8) -> bool {
    tags.iter().any(|tag| {
        let value = tag
            .strip_prefix("priority:")
            .or_else(|| tag.strip_prefix("importance:"));
        match value.and_then(|v| v.parse::<f32>().ok()) {
            Some(n) => n.is_finite() && n >= threshold as f32,
            None => false,
        }
    })
}

fn extract_covered_task_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("Covers tasks:") else {
            continue;
        };
        for token in rest.split(',') {
            let token = token.trim();
            if let Some(id) = token.strip_prefix("task:id:") {
                let id = id.trim().trim_end_matches('.').trim_end_matches(';');
                if !id.is_empty() {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
}

fn merge_payload_candidates(
    by_chunk: &mut HashMap<String, Takeaway>,
    explanations: &mut Vec<MemoryMdCandidateExplanation>,
    payload: &Value,
    section: &str,
    source: &str,
    query: &str,
    mode: CliQueryMode,
) {
    let Some(results) = payload.get("results").and_then(Value::as_array) else {
        return;
    };
    for (idx, result) in results.iter().enumerate() {
        let Some(chunk_id) = result.get("chunk_id").and_then(Value::as_str) else {
            continue;
        };
        let text = result
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let tags = result
            .get("tags")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut explanation =
            candidate_explanation(section, source, query, mode, idx + 1, result, &tags, text);
        if text.is_empty() {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("empty_text".to_string());
            explanations.push(explanation);
            continue;
        }
        if is_generated_digest_takeaway(&tags) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("generated_digest_wrapper".to_string());
            explanations.push(explanation);
            continue;
        }
        if is_fragment_like_candidate(&tags, text) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("fragment_like".to_string());
            explanation.quality_flags.push("fragment_like".to_string());
            explanations.push(explanation);
            continue;
        }
        // Defence-in-depth: the lifecycle visibility filter already
        // hides superseded chunks, but skip anything still carrying a
        // `kind:superseded` tag so consolidated output never competes
        // with the raw chunks it replaced.
        if tags.iter().any(|tag| tag.starts_with("kind:superseded")) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("superseded_tag".to_string());
            explanations.push(explanation);
            continue;
        }
        let score = result.get("score").and_then(Value::as_f64).unwrap_or(0.0) as f32;

        let entry = by_chunk
            .entry(chunk_id.to_string())
            .or_insert_with(|| Takeaway {
                chunk_id: chunk_id.to_string(),
                tenant_id: result
                    .get("tenant_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                project_id: result
                    .get("project_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                text: text.to_string(),
                score,
                priority: 0.0,
                chunk_type: result
                    .get("chunk_type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                timestamp_created: result
                    .get("timestamp_created")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                tags,
                sources: BTreeSet::new(),
                occurrences: 0,
            });
        entry.score = entry.score.max(score);
        entry.occurrences = entry.occurrences.saturating_add(1);
        entry.sources.insert(source.to_string());
        explanations.push(explanation);
    }
}

fn candidate_explanation(
    section: &str,
    source: &str,
    query: &str,
    mode: CliQueryMode,
    raw_rank: usize,
    result: &Value,
    tags: &[String],
    text: &str,
) -> MemoryMdCandidateExplanation {
    let generated_digest = is_generated_digest_takeaway(tags)
        || text
            .to_ascii_lowercase()
            .contains("task digest status generated");
    let mut quality_flags = Vec::new();
    if is_fragment_like_candidate(tags, text) {
        quality_flags.push("fragment_like".to_string());
    }
    if generated_digest
        || text
            .to_ascii_lowercase()
            .contains("task digest status generated")
    {
        quality_flags.push("generated_wrapper".to_string());
    }
    MemoryMdCandidateExplanation {
        section: section.to_string(),
        source: source.to_string(),
        query: query.to_string(),
        mode: query_mode_label(mode).to_string(),
        raw_rank,
        chunk_id: result
            .get("chunk_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tenant_id: result
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        project_id: result
            .get("project_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        chunk_type: result
            .get("chunk_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        timestamp_created: result
            .get("timestamp_created")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        search_score: result.get("score").and_then(Value::as_f64).unwrap_or(0.0) as f32,
        priority_score: None,
        priority_breakdown: None,
        display_status: "candidate".to_string(),
        filter_reason: None,
        display_rank: None,
        generated_digest,
        quality_flags,
        topic_key: None,
        tags: tags.to_vec(),
        matched_sources: vec![source.to_string()],
    }
}

fn finalize_candidate_explanations(
    explanations: &mut [MemoryMdCandidateExplanation],
    scored_takeaways: &[Takeaway],
    suppressed_reasons: &HashMap<String, String>,
    displayed_ids: &HashMap<String, usize>,
    breakdowns: &HashMap<String, PriorityBreakdown>,
) {
    let by_id = scored_takeaways
        .iter()
        .map(|takeaway| (takeaway.chunk_id.as_str(), takeaway))
        .collect::<HashMap<_, _>>();
    for explanation in explanations {
        if explanation.display_status == "filtered" {
            continue;
        }
        if let Some(takeaway) = by_id.get(explanation.chunk_id.as_str()) {
            explanation.priority_score = Some(takeaway.priority);
            explanation.priority_breakdown = breakdowns.get(&takeaway.chunk_id).cloned();
            explanation.matched_sources = takeaway.sources.iter().cloned().collect();
            explanation.topic_key = Some(topic_key(takeaway));
        }
        if let Some(reason) = suppressed_reasons.get(&explanation.chunk_id) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some(reason.clone());
            explanation.quality_flags.push(reason.clone());
        } else if let Some(rank) = displayed_ids.get(&explanation.chunk_id) {
            explanation.display_status = "displayed".to_string();
            explanation.display_rank = Some(*rank);
        } else {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("below_display_limit".to_string());
        }
    }
}

fn query_mode_label(mode: CliQueryMode) -> &'static str {
    match mode {
        CliQueryMode::Generic => "generic",
        CliQueryMode::BriefProject => "brief_project",
        CliQueryMode::ResumeTask => "resume_task",
        CliQueryMode::FindFailures => "find_failures",
        CliQueryMode::FindDecisions => "find_decisions",
        CliQueryMode::FindEvidence => "find_evidence",
        CliQueryMode::FindHighlights => "find_highlights",
    }
}

fn recurring_tag_counts<'a>(
    takeaways: impl Iterator<Item = &'a Takeaway>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for takeaway in takeaways {
        for tag in takeaway.tags.iter().filter(|tag| high_signal_tag(tag)) {
            *counts.entry(tag.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

#[cfg(test)]
fn priority_score(
    takeaway: &Takeaway,
    tag_counts: &HashMap<String, usize>,
    hit_stats: &HashMap<String, HitStats>,
    now_ms: i64,
) -> f32 {
    priority_breakdown(takeaway, tag_counts, hit_stats, now_ms).total
}

fn priority_breakdown(
    takeaway: &Takeaway,
    tag_counts: &HashMap<String, usize>,
    hit_stats: &HashMap<String, HitStats>,
    now_ms: i64,
) -> PriorityBreakdown {
    let explicit = explicit_priority(&takeaway.tags).unwrap_or(0.0);
    let kind_weight = takeaway
        .tags
        .iter()
        .map(|tag| match tag.as_str() {
            "kind:decision" => 12.0,
            "kind:finish" => 10.0,
            "kind:evidence" => 8.0,
            "kind:run" => 5.0,
            "kind:progress" => 3.0,
            _ if tag.starts_with("ctx:file:") => 5.0,
            _ if tag.starts_with("ctx:subsystem:") => 4.0,
            _ => 0.0,
        })
        .sum::<f32>();
    let type_weight = match takeaway.chunk_type.as_str() {
        "decision" => 10.0,
        "summary" => 6.0,
        "research" => 5.0,
        "trace" => 3.0,
        "plan" => 2.0,
        _ => 0.0,
    };
    let recurrence = takeaway
        .tags
        .iter()
        .filter_map(|tag| tag_counts.get(tag))
        .map(|count| count.saturating_sub(1).min(5) as f32)
        .sum::<f32>();
    let multi_query = takeaway.occurrences.saturating_sub(1).min(4) as f32 * 3.0;
    let search_score = takeaway.score.clamp(0.0, 25.0) * 2.0;
    let library_bonus = if takeaway.tags.iter().any(|tag| {
        tag == "task:role:highlight_library"
            || tag == "task:role:project_brief"
            || tag.starts_with("kind:consolidated")
    }) {
        15.0
    } else {
        0.0
    };

    // Load-bearing priority: frequently-retrieved chunks get a boost
    // capped at +8; chunks with no hits and older than `STALE_CHUNK_AGE_MS`
    // get a -2 demotion so the working set surfaces over dormant lessons.
    let utility = hit_stats
        .get(&takeaway.chunk_id)
        .map(|stats| (stats.selected_count.min(10) as f32) * 0.8)
        .unwrap_or(0.0);
    let staleness_penalty = if !hit_stats.contains_key(&takeaway.chunk_id)
        && takeaway.timestamp_created > 0
        && now_ms.saturating_sub(takeaway.timestamp_created) > STALE_CHUNK_AGE_MS
    {
        -2.0
    } else {
        0.0
    };

    let total = explicit
        + kind_weight
        + type_weight
        + recurrence
        + multi_query
        + search_score
        + library_bonus
        + utility
        + staleness_penalty;

    PriorityBreakdown {
        explicit,
        kind_weight,
        type_weight,
        recurrence,
        multi_query,
        search_score,
        library_bonus,
        utility,
        staleness_penalty,
        total,
    }
}

fn explicit_priority(tags: &[String]) -> Option<f32> {
    tags.iter().find_map(|tag| {
        let value = tag
            .strip_prefix("priority:")
            .or_else(|| tag.strip_prefix("importance:"))?;
        let parsed = value.parse::<f32>().ok()?;
        if !parsed.is_finite() {
            // `priority:inf` / `priority:nan` must not pin a takeaway at max
            // rank with permanent suppression immunity.
            return None;
        }
        let scaled = if parsed <= 10.0 {
            parsed * 10.0
        } else {
            parsed.min(100.0)
        };
        Some(scaled.clamp(0.0, 100.0))
    })
}

fn high_signal_tag(tag: &str) -> bool {
    tag.starts_with("kind:")
        || tag.starts_with("ctx:")
        || tag.starts_with("priority:")
        || tag.starts_with("importance:")
}

fn is_generated_digest_takeaway(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == "task:status:generated")
        && tags
            .iter()
            .any(|tag| tag.starts_with("task:role:") || tag.starts_with("task:digest:"))
}

fn is_fragment_like_takeaway(takeaway: &Takeaway) -> bool {
    is_fragment_like_candidate(&takeaway.tags, &takeaway.text)
}

fn is_fragment_like_candidate(tags: &[String], text: &str) -> bool {
    if tags.iter().any(|tag| {
        tag.strip_prefix("chunk_index:")
            .and_then(|value| value.parse::<usize>().ok())
            .map(|idx| idx > 0)
            .unwrap_or(false)
    }) {
        return true;
    }

    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with("...")
        || trimmed
            .chars()
            .next()
            .map(|ch| matches!(ch, ',' | ';' | ':' | ')' | ']' | '}'))
            .unwrap_or(false)
    {
        return true;
    }

    let lowered = trimmed.to_ascii_lowercase();
    const CONTINUATION_PREFIXES: &[&str] = &[
        "and ",
        "but ",
        "which ",
        "where ",
        "then ",
        "therefore ",
        "from there ",
        "as a result ",
    ];
    CONTINUATION_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
}

fn has_boilerplate_action(takeaway: &Takeaway) -> bool {
    explicit_agent_action(&takeaway.text).is_none()
        && takeaway_category(takeaway).reason == "ranked project takeaway"
}

fn is_machine_wide_startup_relevant(takeaway: &Takeaway) -> bool {
    if takeaway.project_id.is_none() {
        return true;
    }
    if user_priority_at_least(&takeaway.tags, USER_PRESERVE_PRIORITY_THRESHOLD)
        && takeaway.tags.iter().any(|tag| {
            tag == "global"
                || tag == "always"
                || tag == "machine"
                || tag == "kind:consolidated"
                || tag.starts_with("global:")
                || tag.starts_with("machine:")
        })
    {
        return true;
    }
    let lowered = takeaway.text.to_ascii_lowercase();
    lowered.contains("machine-wide")
        || lowered.contains("all projects")
        || lowered.contains("global default")
        || lowered.contains("always ")
}

fn render_memory_md(
    project_state: &ProjectState,
    health_lines: &[String],
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
) -> String {
    let mut out = String::new();
    out.push_str("# memory.md\n\n");
    out.push_str("Generated by `memd memory-md`.\n\n");
    render_latest_project_state(&mut out, project_state);
    if !health_lines.is_empty() {
        out.push_str("## Memory health\n\n");
        for line in health_lines {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    out.push_str("## Session-Start Use\n\n");
    out.push_str("- Read this file before task-specific retrieval.\n");
    out.push_str("- Refresh it at the start of substantive sessions with `memd memory-md`.\n");
    out.push_str("- Then run task-specific `memd agent-context` or `memd search`.\n\n");
    out.push_str("## Agent Guidance\n\n");
    out.push_str("- Treat fact-library items as durable memory to verify against current files, logs, or tests before applying.\n");
    out.push_str("- Use `Latest Project State` for the first resume pass; use `memd agent-context` for task-specific retrieval.\n\n");
    out.push_str("## Scoring\n\n");
    out.push_str("- Explicit `priority:N` or `importance:N` tags dominate when present.\n");
    out.push_str("- Decisions, finishes, evidence, recurring tags, multi-query matches, and search score increase priority.\n");
    out.push_str("- Repeated lessons should be recorded again with a higher `priority:N` tag when they keep mattering.\n\n");

    render_section(&mut out, "Project Fact Library", project_takeaways);
    if !global_takeaways.is_empty() {
        render_section(&mut out, "Machine-Wide Fact Library", global_takeaways);
    }
    out
}

fn render_latest_project_state(out: &mut String, state: &ProjectState) {
    out.push_str("## Latest Project State\n\n");
    out.push_str("### Scope & Freshness\n\n");
    out.push_str(&format!("- tenant_id: `{}`\n", state.tenant_id));
    out.push_str(&format!(
        "- project_id: `{}`\n",
        state.project_id.as_deref().unwrap_or("<none>")
    ));
    if let Some(configured) = state.configured_project_dir.as_deref() {
        out.push_str(&format!("- configured_project_dir: `{configured}`\n"));
    } else {
        out.push_str("- configured_project_dir: `<none>`\n");
    }
    out.push_str(&format!(
        "- resolved_project_dir: `{}`\n",
        state.resolved_project_dir
    ));
    out.push_str(&format!(
        "- generated_unix_ms: `{}`\n\n",
        state.generated_unix_ms
    ));

    out.push_str("### Worktree\n\n");
    out.push_str(&format!("- git: {}\n\n", state.git.summary));

    out.push_str("### Latest Work\n\n");
    if let Some(task) = &state.latest_task {
        out.push_str(&format!("- task: {}\n", render_state_signal(task)));
    } else {
        out.push_str("- task: none detected in `tasks/todo.md`\n");
    }
    if let Some(handoff) = &state.latest_handoff {
        out.push_str(&format!("- handoff: {}\n", render_state_signal(handoff)));
    } else {
        out.push_str("- handoff: none detected under `docs/handoffs/`\n");
    }
    if let Some(vcs) = &state.latest_vcs {
        out.push_str(&format!("- vcs: {}\n", render_state_signal(vcs)));
    }
    out.push('\n');

    out.push_str("### Next Actions\n\n");
    if state.next_actions.is_empty() {
        out.push_str("- No open next actions detected from `tasks/todo.md`.\n\n");
    } else {
        for (idx, action) in state.next_actions.iter().take(3).enumerate() {
            out.push_str(&format!(
                "{}. {} ([{}:{}])\n",
                idx + 1,
                inline_code_text(&action.text),
                action.source_path,
                action.line
            ));
        }
        out.push('\n');
    }

    out.push_str("### Memory Warnings\n\n");
    let warnings = project_state_warnings(state);
    if warnings.is_empty() {
        out.push_str("- none detected\n\n");
    } else {
        for warning in warnings {
            out.push_str(&format!("- {warning}\n"));
        }
        out.push('\n');
    }
}

fn render_state_signal(signal: &StateSignal) -> String {
    let source = match signal.line {
        Some(line) => format!("{}:{}", signal.source_path, line),
        None => signal.source_path.clone(),
    };
    match signal.heading.as_deref() {
        Some(heading) if !heading.is_empty() => {
            format!(
                "{} - {} ([{}])",
                heading,
                inline_code_text(&signal.text),
                source
            )
        }
        _ => format!("{} ([{}])", inline_code_text(&signal.text), source),
    }
}

fn project_state_warnings(state: &ProjectState) -> Vec<String> {
    let mut warnings = Vec::new();
    warnings.extend(state.scope_warnings.iter().cloned());
    if let Some(warning) = &state.git.warning {
        warnings.push(warning.clone());
    }
    if let Some(unreadable) = state.memory.unreadable_active_chunks {
        if unreadable > 0 {
            warnings.push(format!(
                "memory degraded: {unreadable} active chunks could not be read from payload segments"
            ));
        }
    }
    if let Some(warning) = &state.memory.scan_warning {
        warnings.push(warning.clone());
    }
    warnings.extend(state.collection_warnings.iter().cloned());
    warnings
}

fn evaluate_agent_usefulness(
    state: &ProjectState,
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
) -> AgentUsefulnessMetrics {
    // These metrics are computed from the same structured state and filtered
    // startup items used by the renderer. They are a deterministic regression
    // gate for startup answerability, not an independent semantic judge.
    let latest_project_state_present = true;
    let scope_present = !state.tenant_id.is_empty() && !state.resolved_project_dir.is_empty();
    let git_state_present = state.git.available;
    let git_state_present_or_not_git_repo = state.git.available || state.git.not_git_repo;
    let latest_work_present =
        state.latest_task.is_some() || state.latest_handoff.is_some() || state.latest_vcs.is_some();
    let next_action_present = !state.next_actions.is_empty();
    let no_open_tasks_detected = state.next_actions.is_empty();
    let scope_health_present = scope_health_checked(state);
    let memory_degraded_warning_present = state
        .memory
        .unreadable_active_chunks
        .map(|count| count > 0)
        .unwrap_or(false);
    let fragment_count = project_takeaways
        .iter()
        .chain(global_takeaways.iter())
        .filter(|takeaway| is_fragment_like_takeaway(takeaway))
        .count();
    let duplicate_cluster_count = visible_duplicate_cluster_count(project_takeaways)
        + visible_duplicate_cluster_count(global_takeaways);
    let boilerplate_action_count = project_takeaways
        .iter()
        .chain(global_takeaways.iter())
        .filter(|takeaway| has_boilerplate_action(takeaway))
        .count();
    let unrelated_machine_items = global_takeaways
        .iter()
        .filter(|takeaway| !is_machine_wide_startup_relevant(takeaway))
        .count();
    let source_backed_next_actions = state
        .next_actions
        .iter()
        .all(|action| !action.source_path.is_empty() && action.line > 0);
    let answerability_passed = latest_project_state_present
        && scope_present
        && git_state_present_or_not_git_repo
        && latest_work_present
        && (next_action_present || no_open_tasks_detected)
        && fragment_count == 0
        && boilerplate_action_count == 0;

    AgentUsefulnessMetrics {
        latest_project_state_present,
        scope_present,
        git_state_present,
        git_state_present_or_not_git_repo,
        latest_work_present,
        next_action_present,
        no_open_tasks_detected,
        scope_health_present,
        memory_degraded_warning_present,
        fragment_count,
        duplicate_cluster_count,
        boilerplate_action_count,
        unrelated_machine_items,
        source_backed_next_actions,
        answerability_passed,
    }
}

fn scope_health_checked(state: &ProjectState) -> bool {
    if let Some(configured) = state.configured_project_dir.as_deref() {
        scope_path_drift_warning(configured, Path::new(&state.resolved_project_dir)).is_none()
            || state
                .scope_warnings
                .iter()
                .any(|warning| warning.starts_with("scope mismatch:"))
    } else {
        true
    }
}

fn visible_duplicate_cluster_count(takeaways: &[Takeaway]) -> usize {
    let mut counts = BTreeMap::new();
    for takeaway in takeaways {
        *counts.entry(topic_key(takeaway)).or_insert(0usize) += 1;
    }
    counts.values().filter(|count| **count > 1).count()
}

fn render_section(out: &mut String, title: &str, takeaways: &[Takeaway]) {
    out.push_str(&format!("## {title}\n\n"));
    if takeaways.is_empty() {
        out.push_str("- No takeaways found yet.\n\n");
        return;
    }

    let mut categorized = takeaways
        .iter()
        .map(|takeaway| (takeaway_category(takeaway), takeaway))
        .collect::<Vec<_>>();
    categorized.sort_by(|(left_category, left), (right_category, right)| {
        left_category
            .order
            .cmp(&right_category.order)
            .then_with(|| {
                right
                    .priority
                    .partial_cmp(&left.priority)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
    });

    for (heading, _) in TAKEAWAY_CATEGORIES {
        let group = categorized
            .iter()
            .filter(|(category, _)| category.heading == *heading)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("### {heading}\n\n"));
        for (idx, (category, takeaway)) in group.iter().enumerate() {
            render_takeaway(out, idx + 1, takeaway, category.reason);
        }
        out.push('\n');
    }
}

fn render_takeaway(out: &mut String, idx: usize, takeaway: &Takeaway, reason: &str) {
    out.push_str(&format!(
        "{}. {}\n",
        idx,
        summarize_text(&takeaway.text, 320)
    ));
    out.push_str(&format!(
        "   - priority: `{:.1}`; chunk: `{}`; type: `{}`; tenant: `{}`",
        takeaway.priority, takeaway.chunk_id, takeaway.chunk_type, takeaway.tenant_id
    ));
    if let Some(project_id) = takeaway.project_id.as_deref() {
        out.push_str(&format!("; project: `{project_id}`"));
    }
    if takeaway.timestamp_created > 0 {
        out.push_str(&format!(
            "; created_unix_ms: `{}`",
            takeaway.timestamp_created
        ));
    }
    out.push('\n');
    out.push_str(&format!("   - reason: `{reason}`\n"));
    out.push_str(&format!(
        "   - agent action: `{}`\n",
        inline_code_text(&agent_action_for_takeaway(takeaway, reason))
    ));
    if !takeaway.tags.is_empty() {
        out.push_str(&format!(
            "   - tags: `{}`\n",
            takeaway
                .tags
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !takeaway.sources.is_empty() {
        out.push_str(&format!(
            "   - matched: `{}`\n",
            takeaway
                .sources
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

fn agent_action_for_takeaway(takeaway: &Takeaway, reason: &str) -> String {
    if let Some(action) = explicit_agent_action(&takeaway.text) {
        return summarize_text(&action, 240);
    }

    let evidence = summarize_text(&takeaway.text, 180);
    match reason {
        "decision or rationale" => {
            format!("Apply this decision when the same scope appears: {evidence}")
        }
        "validated fix or result" => {
            format!("Reuse this validated fix when the same failure appears: {evidence}")
        }
        "failure or root-cause evidence" => {
            format!(
                "Check for this known failure before retrying and avoid repeating it: {evidence}"
            )
        }
        "command, path, or parameter evidence" => {
            format!("Use or verify this command/path exactly when relevant: {evidence}")
        }
        "explicit follow-up" => {
            format!(
                "Treat this as pending work and resolve it before claiming completion: {evidence}"
            )
        }
        "evidence or run result" => {
            format!("Use this as evidence only after confirming it still matches current files or tests: {evidence}")
        }
        _ => format!("Translate this takeaway into a task-specific rule before acting: {evidence}"),
    }
}

pub(super) fn explicit_agent_action(text: &str) -> Option<String> {
    const MARKERS: &[(&str, &str)] = &[
        ("agent action:", ""),
        ("action:", ""),
        ("rule:", ""),
        ("do:", "Do "),
        ("use:", "Use "),
        ("avoid:", "Avoid "),
        ("prefer:", "Prefer "),
        ("check:", "Check "),
        ("verify:", "Verify "),
        ("next step:", "Do next: "),
        ("follow-up:", "Follow up: "),
        ("followup:", "Follow up: "),
    ];

    for marker in explicit_action_markers(text, MARKERS) {
        let body = explicit_action_body(text, marker.start, marker.marker);
        if body.is_empty() {
            continue;
        }
        let candidate = format!("{}{}", marker.prefix, body);
        if is_concrete_agent_action_text(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Clone, Copy)]
struct ExplicitActionMarker<'a> {
    start: usize,
    marker: &'a str,
    prefix: &'a str,
}

fn explicit_action_markers<'a>(
    text: &str,
    markers: &'a [(&'a str, &'a str)],
) -> Vec<ExplicitActionMarker<'a>> {
    let lowered = text.to_ascii_lowercase();
    let mut found = Vec::new();
    for (marker, prefix) in markers {
        let mut search_start = 0;
        while let Some(relative_start) = lowered[search_start..].find(marker) {
            let start = search_start + relative_start;
            found.push(ExplicitActionMarker {
                start,
                marker,
                prefix,
            });
            search_start = start + marker.len();
        }
    }
    found.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.marker.len().cmp(&left.marker.len()))
    });
    found
}

fn explicit_action_body<'a>(text: &'a str, marker_start: usize, marker: &str) -> &'a str {
    let body_start = marker_start + marker.len();
    let lowered_tail = text[body_start..].to_ascii_lowercase();
    let line_end = text[body_start..]
        .find(|ch| matches!(ch, '\n' | '\r'))
        .map(|offset| body_start + offset)
        .unwrap_or(text.len());
    let next_marker = [
        "agent action:",
        "action:",
        "rule:",
        "do:",
        "use:",
        "avoid:",
        "prefer:",
        "check:",
        "verify:",
        "next step:",
        "follow-up:",
        "followup:",
    ]
    .iter()
    .filter_map(|candidate| {
        lowered_tail
            .find(candidate)
            .map(|offset| body_start + offset)
    })
    .min()
    .unwrap_or(text.len());
    let body_end = line_end.min(next_marker);
    text[body_start..body_end]
        .trim()
        .trim_end_matches(|ch| matches!(ch, '.' | ';'))
        .trim_end()
}

fn inline_code_text(text: &str) -> String {
    text.replace('`', "'")
}

fn takeaway_category(takeaway: &Takeaway) -> TakeawayCategory {
    let lowered = takeaway.text.to_ascii_lowercase();
    let has_tag = |needle: &str| takeaway.tags.iter().any(|tag| tag == needle);
    let has_source = |needle: &str| takeaway.sources.iter().any(|source| source == needle);

    if takeaway.chunk_type == "decision"
        || has_tag("kind:decision")
        || lowered.contains("decision:")
        || lowered.contains("rationale:")
    {
        return category("Decisions", "decision or rationale");
    }
    if lowered.contains("fix:")
        || lowered.contains("validated fix")
        || lowered.contains("validated:")
        || lowered.contains("validation:")
        || lowered.contains("fixed by")
        || lowered.contains("solution:")
        || lowered.contains("passed")
        || lowered.contains("confirmed")
        || lowered.contains("reproduced")
        || lowered.contains("0 failures")
        || lowered.contains("no failures")
        || lowered.contains("resolved after")
        || lowered.contains("resolved by")
    {
        return category("Validated Fixes", "validated fix or result");
    }
    // Require a real failure signal: a bare "failure" mention ("0
    // failures") or arrival via the *_failures retrieval query is not
    // failure evidence — filing successes here inverts their meaning.
    if has_tag("kind:failure")
        || lowered.contains("root cause")
        || lowered.contains("failed because")
        || lowered.contains("failure:")
        || lowered.contains("blocker")
    {
        return category("Known Failures", "failure or root-cause evidence");
    }
    if lowered.contains("command:")
        || lowered.contains("path:")
        || lowered.contains("parameter:")
        || lowered.contains("parameters:")
        || lowered.contains("/home/")
        || lowered.contains("crates/")
        || lowered.contains("tasks/")
        || lowered.contains(".rs")
        || lowered.contains(".md")
        || lowered.contains("http://")
        || lowered.contains("https://")
    {
        return category("Commands/Paths", "command, path, or parameter evidence");
    }
    if lowered.contains("next step:")
        || lowered.contains("follow-up:")
        || lowered.contains("followup:")
        || lowered.contains("followups:")
    {
        return category("Open Follow-ups", "explicit follow-up");
    }
    if has_tag("kind:evidence") || has_tag("kind:run") || has_source("project_highlights") {
        return category("Evidence", "evidence or run result");
    }

    category("Other Takeaways", "ranked project takeaway")
}

fn category(heading: &'static str, reason: &'static str) -> TakeawayCategory {
    let order = TAKEAWAY_CATEGORIES
        .iter()
        .find_map(|(candidate, order)| (*candidate == heading).then_some(*order))
        .unwrap_or(u8::MAX);
    TakeawayCategory {
        heading,
        reason,
        order,
    }
}

#[derive(Debug, Clone)]
struct DisplayedMemoryMdItem {
    category: String,
    text: String,
    details: Vec<String>,
}

fn evaluate_memory_md_quality(content: &str, top_n: usize) -> MemoryMdQualityReport {
    let items = parse_project_takeaways(content, top_n);
    let displayed_count = items.len();
    let useful_count = items
        .iter()
        .filter(|item| is_useful_display_item(item))
        .count();
    let generated_wrapper_count = items
        .iter()
        .filter(|item| is_generated_wrapper_display_item(item))
        .count();
    let missing_reason_count = items
        .iter()
        .filter(|item| !item.details.iter().any(|line| line.contains("reason: `")))
        .count();
    let missing_action_count = items
        .iter()
        .filter(|item| !has_concrete_agent_action(item))
        .count();
    let useful_ratio = if displayed_count == 0 {
        0.0
    } else {
        useful_count as f64 / displayed_count as f64
    };

    MemoryMdQualityReport {
        displayed_count,
        useful_count,
        generated_wrapper_count,
        missing_reason_count,
        missing_action_count,
        useful_ratio,
    }
}

fn parse_project_takeaways(content: &str, top_n: usize) -> Vec<DisplayedMemoryMdItem> {
    let mut in_project = false;
    let mut category = "Other Takeaways".to_string();
    let mut items = Vec::new();
    let mut current: Option<DisplayedMemoryMdItem> = None;

    for line in content.lines() {
        if line.starts_with("## ") {
            if in_project {
                if let Some(item) = current.take() {
                    items.push(item);
                }
                break;
            }
            let heading = line.trim();
            in_project = heading == "## Project Takeaways" || heading == "## Project Fact Library";
            continue;
        }
        if !in_project {
            continue;
        }
        if let Some(heading) = line.strip_prefix("### ") {
            if let Some(item) = current.take() {
                items.push(item);
                if items.len() >= top_n {
                    break;
                }
            }
            category = heading.trim().to_string();
            continue;
        }
        if let Some(text) = ordered_item_text(line) {
            if let Some(item) = current.take() {
                items.push(item);
                if items.len() >= top_n {
                    break;
                }
            }
            current = Some(DisplayedMemoryMdItem {
                category: category.clone(),
                text: text.to_string(),
                details: Vec::new(),
            });
            continue;
        }
        if let Some(item) = current.as_mut() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                item.details.push(trimmed.to_string());
            }
        }
    }
    if items.len() < top_n {
        if let Some(item) = current {
            items.push(item);
        }
    }
    items.truncate(top_n);
    items
}

fn ordered_item_text(line: &str) -> Option<&str> {
    let (number, rest) = line.split_once(". ")?;
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(rest.trim())
}

fn is_useful_display_item(item: &DisplayedMemoryMdItem) -> bool {
    if is_generated_wrapper_display_item(item) {
        return false;
    }
    if !has_concrete_agent_action(item) {
        return false;
    }

    matches!(
        item.category.as_str(),
        "Decisions"
            | "Validated Fixes"
            | "Known Failures"
            | "Commands/Paths"
            | "Open Follow-ups"
            | "Evidence"
    ) || {
        let lowered = item.text.to_ascii_lowercase();
        lowered.contains("decision:")
            || lowered.contains("rationale:")
            || lowered.contains("validation:")
            || lowered.contains("validated")
            || lowered.contains("root cause")
            || lowered.contains("command:")
            || lowered.contains("path:")
            || lowered.contains("follow-up:")
            || lowered.contains("next step:")
    }
}

fn has_concrete_agent_action(item: &DisplayedMemoryMdItem) -> bool {
    item.details.iter().any(|line| {
        let lowered = line.to_ascii_lowercase();
        let Some(action) = lowered.strip_prefix("- agent action: `") else {
            return false;
        };
        let action = action.trim_end_matches('`').trim();
        is_concrete_agent_action_text(action)
            && !action.contains("translate this takeaway into a task-specific rule")
    })
}

fn is_concrete_agent_action_text(action: &str) -> bool {
    action.chars().count() >= 24 && contains_action_verb(action)
}

fn contains_action_verb(text: &str) -> bool {
    // Shared with the write-admission gate so renderer and gate agree.
    text.split(|ch: char| !ch.is_ascii_alphabetic())
        .any(|word| {
            crate::write_admission::ACTION_VERBS.contains(&word.to_ascii_lowercase().as_str())
        })
}

fn is_generated_wrapper_display_item(item: &DisplayedMemoryMdItem) -> bool {
    let mut lowered = item.text.to_ascii_lowercase();
    for line in &item.details {
        lowered.push(' ');
        lowered.push_str(&line.to_ascii_lowercase());
    }
    lowered.contains("task digest status generated")
        || lowered.contains("task:status:generated")
        || lowered.contains("task:role:highlight_library")
        || lowered.contains("task:digest:")
        || (lowered.contains("highlight library for") && lowered.contains("ranked lessons"))
}

fn summarize_text(text: &str, limit: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let mut out = collapsed
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        DuplicateHealth, HealthCounts, IndexCoverageHealth, PayloadHealth, StoreHealthSnapshot,
        StoreStats,
    };
    use crate::types::{ChunkId, MemoryChunk};

    #[test]
    fn explicit_priority_scales_small_values_and_caps_large_values() {
        assert_eq!(explicit_priority(&["priority:7".to_string()]), Some(70.0));
        assert_eq!(
            explicit_priority(&["importance:120".to_string()]),
            Some(100.0)
        );
    }

    #[test]
    fn git_state_handles_clean_dirty_no_repo_and_missing_git() {
        let no_repo = tempfile::tempdir().unwrap();
        let no_repo_state = collect_git_state(no_repo.path(), "git");
        if no_repo_state.warning.as_deref() == Some("git unavailable: executable not found") {
            return;
        }
        assert!(no_repo_state.not_git_repo);
        assert!(!no_repo_state.available);

        let missing = collect_git_state(no_repo.path(), "definitely-not-a-git-binary");
        assert!(!missing.available);
        assert!(missing
            .warning
            .as_deref()
            .unwrap_or_default()
            .contains("executable not found"));

        let repo = tempfile::tempdir().unwrap();
        let init = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .arg("init")
            .output()
            .unwrap();
        assert!(init.status.success());
        let clean = collect_git_state(repo.path(), "git");
        assert!(clean.available);
        assert_eq!(clean.clean, Some(true));

        fs::write(repo.path().join("dirty.txt"), "dirty").unwrap();
        let dirty = collect_git_state(repo.path(), "git");
        assert!(dirty.available);
        assert_eq!(dirty.clean, Some(false));
        assert!(dirty.changed_entries > 0);
    }

    #[test]
    fn task_state_uses_nested_heading_for_first_open_action() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(
            tasks_dir.join("todo.md"),
            "# Completed\n\n- [x] old\n\n# Current\n\n## Deep Work\n\n- [ ] implement nested action\n",
        )
        .unwrap();

        let scan = collect_task_state(dir.path());
        let latest = scan.latest_task.expect("latest task");
        assert_eq!(latest.heading.as_deref(), Some("Deep Work"));
        assert_eq!(scan.next_actions.len(), 1);
        assert_eq!(scan.next_actions[0].line, 9);
        assert_eq!(scan.next_actions[0].source_path, "tasks/todo.md");
    }

    #[test]
    fn task_state_ignores_checked_boxes_and_incidental_pending_text() {
        let dir = tempfile::tempdir().unwrap();
        let tasks_dir = dir.path().join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(
            tasks_dir.join("todo.md"),
            "# Current\n\n- [x] resolved the pending migration\n- note: this pending migration is already closed\n- [ ] real open item\n",
        )
        .unwrap();

        let scan = collect_task_state(dir.path());
        assert_eq!(scan.next_actions.len(), 1);
        assert_eq!(scan.next_actions[0].text, "real open item");
    }

    #[test]
    fn handoff_state_renders_title_once_and_stable_spacing() {
        let dir = tempfile::tempdir().unwrap();
        let handoff_dir = dir.path().join("docs/handoffs");
        fs::create_dir_all(&handoff_dir).unwrap();
        fs::write(
            handoff_dir.join("2026-06-28-release.md"),
            "# Handoff 2026-06-28\n\nStatus: active\nNext step: ship release\n",
        )
        .unwrap();

        let scan = collect_handoff_state(dir.path());
        let handoff = scan.latest_handoff.expect("latest handoff");
        assert_eq!(handoff.heading.as_deref(), Some("Handoff 2026-06-28"));
        assert_eq!(handoff.text, "Status: active | Next step: ship release");

        let rendered_signal = render_state_signal(&handoff);
        assert!(!rendered_signal.contains("Handoff 2026-06-28 - Handoff 2026-06-28"));

        let mut state = make_project_state();
        state.latest_handoff = Some(handoff);
        state.latest_vcs = None;
        let mut rendered = String::new();
        render_latest_project_state(&mut rendered, &state);
        assert!(rendered.contains("- handoff: Handoff 2026-06-28 -"));
        assert!(rendered.contains("Status: active"));
        assert!(rendered.contains("Next step: ship release"));
        assert!(!rendered.contains("\n\n\n### Next Actions"));
    }

    #[test]
    fn scope_path_normalization_warns_only_on_real_drift() {
        let dir = tempfile::tempdir().unwrap();
        let configured_same = format!("{}/.", dir.path().display());
        assert!(scope_path_drift_warning(&configured_same, dir.path()).is_none());

        let drift = dir.path().join("other");
        let warning =
            scope_path_drift_warning(drift.to_str().unwrap(), dir.path()).expect("drift warning");
        assert!(warning.contains("scope mismatch"));
    }

    #[test]
    fn fragment_candidates_are_filtered_before_ranking() {
        let payload = serde_json::json!({
            "results": [
                {
                    "chunk_id": "fragment",
                    "tenant_id": "t",
                    "project_id": "p",
                    "text": "and then continued from a prior chunk",
                    "score": 10.0,
                    "chunk_type": "summary",
                    "timestamp_created": 1,
                    "tags": ["chunk_index:1", "kind:finish"]
                }
            ]
        });
        let mut by_chunk = HashMap::new();
        let mut explanations = Vec::new();
        merge_payload_candidates(
            &mut by_chunk,
            &mut explanations,
            &payload,
            "project",
            "project_highlights",
            "project takeaways",
            CliQueryMode::FindHighlights,
        );
        assert!(by_chunk.is_empty());
        let explanation = explanations.first().expect("explanation");
        assert_eq!(explanation.filter_reason.as_deref(), Some("fragment_like"));
        assert!(explanation
            .quality_flags
            .iter()
            .any(|flag| flag == "fragment_like"));
    }

    #[test]
    fn conditional_sentences_are_not_filtered_as_fragments() {
        assert!(!is_fragment_like_candidate(
            &[],
            "When rerunning failed tasks, use --force to overwrite stale outputs."
        ));
        assert!(!is_fragment_like_candidate(
            &[],
            "While debugging CI, inspect the failing job log before changing code."
        ));
        assert!(!is_fragment_like_candidate(
            &[],
            "Because the release workflow publishes from main, push a release branch first."
        ));
        assert!(is_fragment_like_candidate(
            &[],
            "and then continued from a prior chunk"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_drains_large_output_while_waiting() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "i=0; while [ \"$i\" -lt 20000 ]; do printf 'dirty-file-%05d xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' \"$i\"; i=$((i + 1)); done",
        );

        let output = run_command_with_timeout(command, Duration::from_secs(5)).unwrap();

        assert!(!output.timed_out);
        assert!(output.status_success);
        assert!(output.stdout.len() > 128 * 1024);
    }

    #[test]
    fn expected_git_failure_checks_true_and_false_expectations() {
        let mut metrics = make_agent_metrics();
        metrics.git_state_present = false;
        assert_eq!(
            expected_git_failure(&metrics, true).as_deref(),
            Some("expected git state but git_state_present=false")
        );
        assert!(expected_git_failure(&metrics, false).is_none());

        metrics.git_state_present = true;
        assert_eq!(
            expected_git_failure(&metrics, false).as_deref(),
            Some("expected no git state but git_state_present=true")
        );
        assert!(expected_git_failure(&metrics, true).is_none());
    }

    #[test]
    fn startup_filters_collapse_duplicates_and_boilerplate_actions() {
        let mut takeaways = vec![
            make_takeaway(
                "keep",
                "Validation: MEMD_EMBED_DEVICE cpu override fixed GPU contention. Agent action: Use MEMD_EMBED_DEVICE=cpu when GPU contention blocks embedding.",
                vec!["topic:embed-device", "priority:9"],
                "summary",
            ),
            make_takeaway(
                "drop-duplicate",
                "Validation: MEMD_EMBED_DEVICE cuda override fixed GPU contention. Agent action: Use MEMD_EMBED_DEVICE when GPU contention blocks embedding.",
                vec!["topic:embed-device", "priority:8"],
                "summary",
            ),
            make_takeaway(
                "drop-boilerplate",
                "Routine note without a concrete action or category signal.",
                vec![],
                "summary",
            ),
        ];
        for (idx, takeaway) in takeaways.iter_mut().enumerate() {
            takeaway.priority = (90 - idx) as f32;
        }

        let suppressed = filter_startup_takeaways(&mut takeaways, "project");
        let ids = takeaways
            .iter()
            .map(|takeaway| takeaway.chunk_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["keep"]);
        assert_eq!(
            suppressed.get("drop-boilerplate").map(String::as_str),
            Some("boilerplate_action")
        );
        assert!(suppressed
            .get("drop-duplicate")
            .map(|reason| reason.starts_with("duplicate_topic:"))
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn memory_state_reports_project_scoped_unreadable_chunks() {
        let store = FakeHealthStore::new(2, Vec::new());
        let tenant = TenantId::new("t").unwrap();
        let state = collect_memory_state(&store, &tenant, Some("p")).await;
        assert_eq!(state.metadata_active_chunks, Some(2));
        assert_eq!(state.readable_active_chunks, Some(0));
        assert_eq!(state.unreadable_active_chunks, Some(2));

        let project_state = ProjectState {
            memory: state,
            ..make_project_state()
        };
        assert!(project_state_warnings(&project_state)
            .iter()
            .any(|warning| warning.contains("memory degraded: 2 active chunks")));
    }

    #[tokio::test]
    async fn memory_state_caps_large_readable_scan_and_warns_partial() {
        let store = FakeHealthStore::new(READABLE_SCAN_MAX_METADATA_ROWS + 5, Vec::new());
        let tenant = TenantId::new("t").unwrap();
        let state = collect_memory_state(&store, &tenant, Some("p")).await;
        assert_eq!(
            state.unreadable_active_chunks,
            Some(READABLE_SCAN_MAX_METADATA_ROWS)
        );
        assert!(state
            .scan_warning
            .as_deref()
            .unwrap_or_default()
            .contains("readable memory scan partial"));
    }

    #[test]
    fn render_memory_md_caps_long_takeaway_text() {
        let takeaway = Takeaway {
            chunk_id: "chunk-a".to_string(),
            tenant_id: "tenant-a".to_string(),
            project_id: Some("project-a".to_string()),
            text: "word ".repeat(120),
            score: 1.0,
            priority: 42.0,
            chunk_type: "summary".to_string(),
            timestamp_created: 0,
            tags: vec!["kind:finish".to_string()],
            sources: BTreeSet::from(["project_highlights".to_string()]),
            occurrences: 1,
        };
        let state = make_project_state();
        let rendered = render_memory_md(&state, &[], &[takeaway], &[]);
        assert!(rendered.contains("## Project Fact Library"));
        assert!(rendered.contains("## Agent Guidance"));
        assert!(rendered.contains("agent action: `Use this as evidence only after confirming"));
        assert!(rendered.contains("chunk-a"));
        assert!(rendered.contains("..."));
    }

    #[test]
    fn render_memory_md_omits_empty_global_section() {
        let takeaway = Takeaway {
            chunk_id: "chunk-a".to_string(),
            tenant_id: "tenant-a".to_string(),
            project_id: Some("project-a".to_string()),
            text: "validated project lesson".to_string(),
            score: 1.0,
            priority: 42.0,
            chunk_type: "summary".to_string(),
            timestamp_created: 0,
            tags: vec!["kind:finish".to_string()],
            sources: BTreeSet::from(["project_highlights".to_string()]),
            occurrences: 1,
        };
        let state = make_project_state();
        let rendered = render_memory_md(&state, &[], &[takeaway], &[]);
        assert!(rendered.contains("## Project Fact Library"));
        assert!(!rendered.contains("## Machine-Wide Fact Library"));
    }

    #[test]
    fn render_memory_md_groups_takeaways_by_signal_category() {
        let mut decision = make_takeaway(
            "decision",
            "Decision: keep project aliases explicit. Rationale: silent merging hides drift.",
            vec!["kind:decision"],
            "summary",
        );
        decision.priority = 80.0;
        decision.timestamp_created = 42;

        let mut fix = make_takeaway(
            "fix",
            "Validation: cargo test -p memd passed after the scoped startup change.",
            vec!["kind:finish"],
            "summary",
        );
        fix.priority = 70.0;

        let mut command = make_takeaway(
            "command",
            "Command: memd memory-md --project-dir . --output memory.md",
            vec!["kind:run"],
            "trace",
        );
        command.priority = 60.0;

        let state = make_project_state();
        let rendered = render_memory_md(&state, &[], &[command, fix, decision], &[]);

        assert!(rendered.contains("### Decisions"));
        assert!(rendered.contains("### Validated Fixes"));
        assert!(rendered.contains("### Commands/Paths"));
        assert!(rendered.contains("reason: `decision or rationale`"));
        assert!(rendered.contains("reason: `validated fix or result`"));
        assert!(rendered.contains("reason: `command, path, or parameter evidence`"));
        assert!(rendered.contains("agent action: `Apply this decision"));
        assert!(rendered.contains("agent action: `Reuse this validated fix"));
        assert!(rendered.contains("agent action: `Use or verify this command/path"));
        assert!(rendered.contains("created_unix_ms: `42`"));
    }

    #[test]
    fn memory_md_quality_report_scores_useful_items_and_wrappers() {
        let content = r#"# memory.md

## Project Takeaways

### Decisions

1. Decision: keep project aliases explicit.
   - reason: `decision or rationale`
   - agent action: `Apply this decision when the same scope appears: keep project aliases explicit.`

### Other Takeaways

1. Task digest status generated. Summary: Highlight library for p contains 2 ranked lessons.
   - reason: `ranked project takeaway`
   - tags: `task:status:generated, task:role:highlight_library`

2. Routine status update with no reason.

## Machine-Wide Takeaways

1. Decision outside project section should not be counted.
   - reason: `decision or rationale`
"#;
        let report = evaluate_memory_md_quality(content, 10);
        assert_eq!(report.displayed_count, 3);
        assert_eq!(report.useful_count, 1);
        assert_eq!(report.generated_wrapper_count, 1);
        assert_eq!(report.missing_reason_count, 1);
        assert_eq!(report.missing_action_count, 2);
        assert!((report.useful_ratio - (1.0 / 3.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn explicit_agent_action_overrides_category_template() {
        let takeaway = make_takeaway(
            "action",
            "Decision: keep cache keys scoped. Agent action: Verify tenant and project are present before reusing cached retrieval results.",
            vec!["kind:decision"],
            "summary",
        );

        let state = make_project_state();
        let rendered = render_memory_md(&state, &[], &[takeaway], &[]);

        assert!(rendered.contains(
            "agent action: `Verify tenant and project are present before reusing cached retrieval results`"
        ));
    }

    #[test]
    fn explicit_agent_action_skips_explanatory_marker_mentions() {
        let takeaway = make_takeaway(
            "action",
            "Validation: memory quality gate passed. Docs mention Agent action: sentence. Agent action: Write every high-priority durable memory with a concrete action sentence that tells future agents what to verify or reuse.",
            vec!["kind:finish"],
            "summary",
        );

        let state = make_project_state();
        let rendered = render_memory_md(&state, &[], &[takeaway], &[]);

        assert!(rendered.contains(
            "agent action: `Write every high-priority durable memory with a concrete action sentence that tells future agents what to verify or reuse`"
        ));
    }

    #[test]
    fn explicit_agent_action_keeps_paths_and_versions() {
        let takeaway = make_takeaway(
            "action",
            "Installed skill bundle. Agent action: Verify future agent sessions read the refreshed ~/.agents/skills/memd skill and use memd 0.61.0 before diagnosing memory-quality behavior.",
            vec!["kind:finish"],
            "summary",
        );

        let state = make_project_state();
        let rendered = render_memory_md(&state, &[], &[takeaway], &[]);

        assert!(rendered.contains(
            "agent action: `Verify future agent sessions read the refreshed ~/.agents/skills/memd skill and use memd 0.61.0 before diagnosing memory-quality behavior`"
        ));
    }

    fn make_project_state() -> ProjectState {
        ProjectState {
            generated_unix_ms: 123,
            tenant_id: "tenant-a".to_string(),
            project_id: Some("project-a".to_string()),
            configured_project_dir: Some("/tmp/project-a".to_string()),
            resolved_project_dir: "/tmp/project-a".to_string(),
            scope_warnings: Vec::new(),
            git: GitState {
                available: true,
                not_git_repo: false,
                branch: Some("main".to_string()),
                clean: Some(true),
                changed_entries: 0,
                summary: "branch `main`; clean".to_string(),
                warning: None,
            },
            latest_task: Some(StateSignal {
                source_path: "tasks/todo.md".to_string(),
                line: Some(1),
                heading: Some("Current Work".to_string()),
                text: "open action: run validation".to_string(),
            }),
            latest_handoff: None,
            latest_vcs: None,
            next_actions: vec![NextAction {
                source_path: "tasks/todo.md".to_string(),
                line: 2,
                heading: Some("Current Work".to_string()),
                text: "run validation".to_string(),
            }],
            memory: MemoryState::default(),
            collection_warnings: Vec::new(),
        }
    }

    fn make_agent_metrics() -> AgentUsefulnessMetrics {
        AgentUsefulnessMetrics {
            latest_project_state_present: true,
            scope_present: true,
            git_state_present: true,
            git_state_present_or_not_git_repo: true,
            latest_work_present: true,
            next_action_present: true,
            no_open_tasks_detected: false,
            scope_health_present: true,
            memory_degraded_warning_present: false,
            fragment_count: 0,
            duplicate_cluster_count: 0,
            boilerplate_action_count: 0,
            unrelated_machine_items: 0,
            source_backed_next_actions: true,
            answerability_passed: true,
        }
    }

    struct FakeHealthStore {
        active_chunks: usize,
        readable_chunks: Vec<MemoryChunk>,
    }

    impl FakeHealthStore {
        fn new(active_chunks: usize, readable_chunks: Vec<MemoryChunk>) -> Self {
            Self {
                active_chunks,
                readable_chunks,
            }
        }
    }

    #[async_trait::async_trait]
    impl Store for FakeHealthStore {
        async fn add(&self, chunk: MemoryChunk) -> Result<ChunkId> {
            Ok(chunk.chunk_id)
        }

        async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>> {
            Ok(chunks.into_iter().map(|chunk| chunk.chunk_id).collect())
        }

        async fn get(
            &self,
            _tenant_id: &TenantId,
            _chunk_id: &ChunkId,
        ) -> Result<Option<MemoryChunk>> {
            Ok(None)
        }

        async fn search(
            &self,
            _tenant_id: &TenantId,
            _query: &str,
            _k: usize,
        ) -> Result<Vec<MemoryChunk>> {
            Ok(Vec::new())
        }

        async fn list_chunks_for_project(
            &self,
            _tenant_id: &TenantId,
            _project_id: Option<&str>,
            limit: usize,
            offset: usize,
        ) -> Result<Vec<MemoryChunk>> {
            Ok(self
                .readable_chunks
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        async fn delete(&self, _tenant_id: &TenantId, _chunk_id: &ChunkId) -> Result<bool> {
            Ok(false)
        }

        async fn stats(&self, _tenant_id: &TenantId) -> Result<StoreStats> {
            Ok(StoreStats {
                active_chunks: self.active_chunks,
                ..StoreStats::default()
            })
        }

        async fn health_snapshot(
            &self,
            _tenant_id: &TenantId,
            _project_id: Option<&str>,
            _duplicate_limit: usize,
        ) -> Result<Option<StoreHealthSnapshot>> {
            Ok(Some(StoreHealthSnapshot {
                counts: HealthCounts {
                    active_chunks: self.active_chunks,
                    total_chunks: self.active_chunks,
                    ..HealthCounts::default()
                },
                chunk_types_active: HashMap::new(),
                chunk_types_all: HashMap::new(),
                duplicates: DuplicateHealth::default(),
                index_coverage: IndexCoverageHealth::default(),
                payload: PayloadHealth::default(),
            }))
        }
    }

    fn make_takeaway(chunk_id: &str, text: &str, tags: Vec<&str>, chunk_type: &str) -> Takeaway {
        Takeaway {
            chunk_id: chunk_id.to_string(),
            tenant_id: "t".to_string(),
            project_id: Some("p".to_string()),
            text: text.to_string(),
            score: 0.0,
            priority: 0.0,
            chunk_type: chunk_type.to_string(),
            timestamp_created: 0,
            tags: tags.into_iter().map(str::to_string).collect(),
            sources: BTreeSet::new(),
            occurrences: 1,
        }
    }

    #[test]
    fn success_traces_are_not_filed_under_known_failures() {
        // A fully successful trace mentioning "0 failures" used to be
        // filed under Known Failures with fabricated avoid-guidance —
        // an active meaning inversion.
        let success = make_takeaway(
            "success",
            "Trace: ran alpha ETL end-to-end, 0 failures after retry patch was applied.",
            vec!["kind:run"],
            "trace",
        );
        let cat = takeaway_category(&success);
        assert_ne!(cat.heading, "Known Failures", "got: {}", cat.heading);
        assert_eq!(cat.heading, "Validated Fixes");

        // Arrival via a *_failures retrieval query alone is not
        // failure evidence either.
        let mut via_query = make_takeaway(
            "via-query",
            "Benchmark: HNSW recall comparable to IVFFlat at this corpus size.",
            vec![],
            "summary",
        );
        via_query.sources.insert("project_failures".to_string());
        assert_ne!(takeaway_category(&via_query).heading, "Known Failures");
    }

    #[test]
    fn real_failures_still_classify_as_known_failures() {
        let tagged = make_takeaway(
            "tagged",
            "Ingest run aborted on schema mismatch.",
            vec!["kind:failure"],
            "trace",
        );
        assert_eq!(takeaway_category(&tagged).heading, "Known Failures");

        let root_cause = make_takeaway(
            "root-cause",
            "Root cause: NFS stall truncated the segment write; job failed because fsync never returned.",
            vec![],
            "summary",
        );
        assert_eq!(takeaway_category(&root_cause).heading, "Known Failures");
    }

    #[test]
    fn extract_covered_task_ids_parses_summary_footer() {
        let text = "Highlight library for foo contains 3 ranked lessons.\nCovers tasks: task:id:T1, task:id:T2, task:id:T3";
        let ids = extract_covered_task_ids(text);
        assert_eq!(
            ids,
            vec!["T1".to_string(), "T2".to_string(), "T3".to_string()]
        );
    }

    #[test]
    fn task_finish_suppressed_when_covered_by_highlight() {
        let mut takeaways = vec![
            make_takeaway(
                "digest1",
                "Highlight library contains lessons.\nCovers tasks: task:id:T1, task:id:T2",
                vec!["task:role:highlight_library", "task:status:generated"],
                "summary",
            ),
            make_takeaway(
                "raw1",
                "Task T1 finished successfully.",
                vec!["task:kind:task_finish", "task:id:T1"],
                "summary",
            ),
            make_takeaway(
                "raw2",
                "Task T3 finished — not covered.",
                vec!["task:kind:task_finish", "task:id:T3"],
                "summary",
            ),
        ];
        suppress_finishes_covered_by_libraries(&mut takeaways);
        let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
        assert!(ids.contains(&"digest1"));
        assert!(!ids.contains(&"raw1"), "covered finish should be dropped");
        assert!(ids.contains(&"raw2"), "uncovered finish should survive");
    }

    #[test]
    fn user_explicit_priority_high_survives_suppression() {
        let mut takeaways = vec![
            make_takeaway(
                "digest1",
                "Covers tasks: task:id:T1",
                vec!["task:role:highlight_library", "task:status:generated"],
                "summary",
            ),
            make_takeaway(
                "raw1",
                "Task T1 finished — but operator marked it high priority.",
                vec!["task:kind:task_finish", "task:id:T1", "priority:9"],
                "summary",
            ),
        ];
        suppress_finishes_covered_by_libraries(&mut takeaways);
        let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
        assert!(ids.contains(&"raw1"), "priority>=8 finish must survive");
    }

    #[test]
    fn unverified_role_tag_does_not_suppress() {
        // A user-authored chunk that spoofs the role tag but lacks
        // `task:status:generated` must not suppress real finishes.
        let mut takeaways = vec![
            make_takeaway(
                "spoof",
                "Covers tasks: task:id:T1",
                vec!["task:role:highlight_library"],
                "summary",
            ),
            make_takeaway(
                "raw1",
                "Task T1 finished successfully.",
                vec!["task:kind:task_finish", "task:id:T1"],
                "summary",
            ),
        ];
        suppress_finishes_covered_by_libraries(&mut takeaways);
        let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
        assert!(ids.contains(&"raw1"), "spoofed digest must not suppress");
    }

    #[test]
    fn cross_project_finish_is_not_suppressed() {
        // Digest belongs to project p; the finish belongs to a
        // different project and shares the task id. It must survive.
        let mut digest = make_takeaway(
            "digest_p",
            "Covers tasks: task:id:T1",
            vec!["task:role:highlight_library", "task:status:generated"],
            "summary",
        );
        digest.project_id = Some("project-a".to_string());
        let mut raw = make_takeaway(
            "raw_other",
            "Task T1 finished in a different project.",
            vec!["task:kind:task_finish", "task:id:T1"],
            "summary",
        );
        raw.project_id = Some("project-b".to_string());
        let mut takeaways = vec![digest, raw];
        suppress_finishes_covered_by_libraries(&mut takeaways);
        let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
        assert!(
            ids.contains(&"raw_other"),
            "finish from another project must not be suppressed"
        );
    }

    #[test]
    fn decimal_priority_survives_suppression() {
        let mut takeaways = vec![
            make_takeaway(
                "digest1",
                "Covers tasks: task:id:T1",
                vec!["task:role:highlight_library", "task:status:generated"],
                "summary",
            ),
            make_takeaway(
                "raw1",
                "Task T1 finished — operator set a decimal priority.",
                vec!["task:kind:task_finish", "task:id:T1", "priority:8.5"],
                "summary",
            ),
        ];
        suppress_finishes_covered_by_libraries(&mut takeaways);
        let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
        assert!(ids.contains(&"raw1"), "priority:8.5 finish must survive");
    }

    #[test]
    fn library_bonus_outranks_raw_chunk() {
        let counts = HashMap::new();
        let hit_stats = HashMap::new();
        let library = make_takeaway(
            "digest",
            "Highlight library text.",
            vec!["task:role:highlight_library", "kind:finish"],
            "summary",
        );
        let raw = make_takeaway("raw", "Plain finish.", vec!["kind:finish"], "summary");
        let lib_score = priority_score(&library, &counts, &hit_stats, 0);
        let raw_score = priority_score(&raw, &counts, &hit_stats, 0);
        assert!(
            lib_score > raw_score + 10.0,
            "library bonus should dominate: lib={lib_score}, raw={raw_score}"
        );
    }

    #[test]
    fn priority_score_boosts_frequently_hit_chunks() {
        let counts = HashMap::new();
        let cold = make_takeaway("cold", "text", vec!["kind:finish"], "summary");
        let hot = make_takeaway("hot", "text", vec!["kind:finish"], "summary");
        let mut hits = HashMap::new();
        hits.insert(
            "hot".to_string(),
            HitStats {
                hit_count: 7,
                selected_count: 7,
                last_ts_ms: 1,
            },
        );
        let cold_score = priority_score(&cold, &counts, &hits, 1);
        let hot_score = priority_score(&hot, &counts, &hits, 1);
        assert!(
            hot_score > cold_score + 5.0,
            "hot chunk should outrank cold: hot={hot_score}, cold={cold_score}"
        );
    }

    #[test]
    fn priority_score_demotes_unused_old_chunks() {
        let counts = HashMap::new();
        let hits: HashMap<String, HitStats> = HashMap::new();
        let recent = {
            let mut t = make_takeaway("recent", "text", vec!["kind:finish"], "summary");
            t.timestamp_created = 1_000_000;
            t
        };
        let stale = {
            let mut t = make_takeaway("stale", "text", vec!["kind:finish"], "summary");
            t.timestamp_created = 1_000_000; // very old
            t
        };
        // now: a few hours after `recent`; ~365 days after `stale`.
        let now_recent = 1_000_000 + 3 * 3_600_000;
        let now_stale = 1_000_000 + 365 * 86_400_000;
        let recent_score = priority_score(&recent, &counts, &hits, now_recent);
        let stale_score = priority_score(&stale, &counts, &hits, now_stale);
        assert!(
            recent_score > stale_score,
            "stale chunk must be demoted: recent={recent_score}, stale={stale_score}"
        );
    }

    #[test]
    fn generated_digest_candidates_are_skipped_from_takeaways() {
        let tags = vec![
            "task:status:generated".to_string(),
            "task:role:decision_library".to_string(),
        ];
        assert!(is_generated_digest_takeaway(&tags));
        assert!(!is_generated_digest_takeaway(&[
            "kind:decision".to_string(),
            "priority:9".to_string(),
        ]));

        let payload = serde_json::json!({
            "results": [
                {
                    "chunk_id": "digest",
                    "tenant_id": "t",
                    "project_id": "p",
                    "text": "Task digest status generated. Summary: Highlight library for p contains 2 ranked lessons.",
                    "score": 25.0,
                    "chunk_type": "summary",
                    "timestamp_created": 10,
                    "tags": tags
                },
                {
                    "chunk_id": "raw",
                    "tenant_id": "t",
                    "project_id": "p",
                    "text": "Validated fix: use the stable project scope when restoring the gateway.",
                    "score": 4.0,
                    "chunk_type": "summary",
                    "timestamp_created": 11,
                    "tags": ["kind:finish"]
                }
            ]
        });
        let mut by_chunk = HashMap::new();
        let mut explanations = Vec::new();
        merge_payload_candidates(
            &mut by_chunk,
            &mut explanations,
            &payload,
            "project",
            "project_highlights",
            "project takeaways",
            CliQueryMode::FindHighlights,
        );
        assert!(!by_chunk.contains_key("digest"));
        assert!(by_chunk.contains_key("raw"));
        assert_eq!(
            explanations
                .iter()
                .find(|item| item.chunk_id == "digest")
                .and_then(|item| item.filter_reason.as_deref()),
            Some("generated_digest_wrapper")
        );
        assert_eq!(
            explanations
                .iter()
                .find(|item| item.chunk_id == "raw")
                .map(|item| item.display_status.as_str()),
            Some("candidate")
        );
    }
}
