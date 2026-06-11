use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

use super::args::{CliQueryMode, ProjectScopeConfig};
use super::paths::absolutize_project_dir;
use super::report::memory_health_lines;
use super::search::cli_search_payload;
use crate::error::{MemdError, Result};
use crate::hit_stats::{aggregate_hits_in, HitStats, DEFAULT_SUMMARY_TTL_MS};
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
        .or_else(|| scope.and_then(|scope| scope.project_id));
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
    // re-read the JSONL log per chunk.
    let hit_stats = aggregate_hits_in(&project_dir, HIT_WINDOW_DAYS, DEFAULT_SUMMARY_TTL_MS);
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
    let output_path = if options.output.is_absolute() {
        options.output
    } else {
        project_dir.join(options.output)
    };
    let rendered = render_memory_md(
        tenant.as_str(),
        project_id.as_deref(),
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
            "generated_unix_ms": now_ms(),
            "candidate_k": candidate_k,
            "limits": {
                "project": project_limit,
                "machine_wide": global_limit,
            },
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
        "output": output_path,
        "explain_output": explain_output,
        "project_takeaways": project_takeaways.len(),
        "global_takeaways": global_takeaways.len(),
        "candidate_k": candidate_k
    }))
}

pub(super) async fn run_memory_md_eval<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: MemoryMdEvalOptions,
) -> Result<Value> {
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
            global_limit: 0,
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

    let payload = json!({
        "passed": failures.is_empty(),
        "output": rendered_path,
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
    let suppressed_ids = suppress_finishes_covered_by_libraries(&mut takeaways);
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
        &suppressed_ids,
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
            Some(n) => n >= threshold as f32,
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
        generated_digest: is_generated_digest_takeaway(tags)
            || text
                .to_ascii_lowercase()
                .contains("task digest status generated"),
        tags: tags.to_vec(),
        matched_sources: vec![source.to_string()],
    }
}

fn finalize_candidate_explanations(
    explanations: &mut [MemoryMdCandidateExplanation],
    scored_takeaways: &[Takeaway],
    suppressed_ids: &BTreeSet<String>,
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
        }
        if suppressed_ids.contains(&explanation.chunk_id) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("covered_by_library".to_string());
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
        if parsed <= 10.0 {
            Some(parsed * 10.0)
        } else {
            Some(parsed.min(100.0))
        }
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

fn render_memory_md(
    tenant_id: &str,
    project_id: Option<&str>,
    health_lines: &[String],
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
) -> String {
    let mut out = String::new();
    out.push_str("# memory.md\n\n");
    out.push_str("Generated by `memd memory-md`.\n\n");
    if !health_lines.is_empty() {
        out.push_str("## Memory health\n\n");
        for line in health_lines {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    out.push_str("## Scope\n\n");
    out.push_str(&format!("- tenant_id: `{tenant_id}`\n"));
    out.push_str(&format!(
        "- project_id: `{}`\n",
        project_id.unwrap_or("<none>")
    ));
    out.push_str(&format!("- generated_unix_ms: `{}`\n\n", now_ms()));
    out.push_str("## Session-Start Use\n\n");
    out.push_str("- Read this file before task-specific retrieval.\n");
    out.push_str("- Refresh it at the start of substantive sessions with `memd memory-md`.\n");
    out.push_str("- Then run task-specific `memd agent-context` or `memd search`.\n\n");
    out.push_str("## Agent Guidance\n\n");
    out.push_str("- Each displayed takeaway includes `agent action`: a concrete instruction derived from the stored memory.\n");
    out.push_str("- Treat the action as a starting rule, then verify it against current files, logs, or tests before applying it.\n\n");
    out.push_str("## Scoring\n\n");
    out.push_str("- Explicit `priority:N` or `importance:N` tags dominate when present.\n");
    out.push_str("- Decisions, finishes, evidence, recurring tags, multi-query matches, and search score increase priority.\n");
    out.push_str("- Repeated lessons should be recorded again with a higher `priority:N` tag when they keep mattering.\n\n");

    render_section(&mut out, "Project Takeaways", project_takeaways);
    if !global_takeaways.is_empty() {
        render_section(&mut out, "Machine-Wide Takeaways", global_takeaways);
    }
    out
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
            in_project = line.trim() == "## Project Takeaways";
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

    #[test]
    fn explicit_priority_scales_small_values_and_caps_large_values() {
        assert_eq!(explicit_priority(&["priority:7".to_string()]), Some(70.0));
        assert_eq!(
            explicit_priority(&["importance:120".to_string()]),
            Some(100.0)
        );
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
        let rendered = render_memory_md("tenant-a", Some("project-a"), &[], &[takeaway], &[]);
        assert!(rendered.contains("## Project Takeaways"));
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
        let rendered = render_memory_md("tenant-a", Some("project-a"), &[], &[takeaway], &[]);
        assert!(rendered.contains("## Project Takeaways"));
        assert!(!rendered.contains("## Machine-Wide Takeaways"));
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

        let rendered = render_memory_md(
            "tenant-a",
            Some("project-a"),
            &[],
            &[command, fix, decision],
            &[],
        );

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

        let rendered = render_memory_md("tenant-a", Some("project-a"), &[], &[takeaway], &[]);

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

        let rendered = render_memory_md("tenant-a", Some("project-a"), &[], &[takeaway], &[]);

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

        let rendered = render_memory_md("tenant-a", Some("project-a"), &[], &[takeaway], &[]);

        assert!(rendered.contains(
            "agent action: `Verify future agent sessions read the refreshed ~/.agents/skills/memd skill and use memd 0.61.0 before diagnosing memory-quality behavior`"
        ));
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
