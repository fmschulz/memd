use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::args::{CliQueryMode, ProjectScopeConfig};
use super::paths::absolutize_project_dir;
use super::search::cli_search_payload;
use crate::error::{MemdError, Result};
use crate::hit_stats::{aggregate_hits_in, HitStats, DEFAULT_SUMMARY_TTL_MS};
use crate::store::Store;
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

#[derive(Debug)]
pub(super) struct MemoryMdOptions {
    pub(super) tenant_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) output: PathBuf,
    pub(super) project_limit: usize,
    pub(super) global_limit: usize,
    pub(super) candidate_k: usize,
    pub(super) cross_tenant: bool,
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

pub(super) async fn refresh_memory_md<S: Store>(
    store: &S,
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

    // Aggregate retrieval hits once per refresh; the same `HitStats`
    // map is shared with every `priority_score` call so we don't
    // re-read the JSONL log per chunk.
    let hit_stats = aggregate_hits_in(&project_dir, HIT_WINDOW_DAYS, DEFAULT_SUMMARY_TTL_MS);
    let project_takeaways = if project_limit == 0 {
        Vec::new()
    } else {
        collect_ranked_takeaways(
            store,
            tenant.as_str(),
            project_id.as_deref(),
            PROJECT_QUERIES,
            candidate_k,
            project_limit,
            &hit_stats,
        )
        .await?
    };
    let global_takeaways = if global_limit == 0 {
        Vec::new()
    } else {
        collect_ranked_takeaways(
            store,
            tenant.as_str(),
            None,
            GLOBAL_QUERIES,
            candidate_k,
            global_limit,
            &hit_stats,
        )
        .await?
    };

    let cross_tenant_takeaways = if options.cross_tenant {
        collect_cross_tenant_takeaways(store, tenant.as_str(), candidate_k, &hit_stats).await?
    } else {
        Vec::new()
    };

    let output_path = if options.output.is_absolute() {
        options.output
    } else {
        project_dir.join(options.output)
    };
    let rendered = render_memory_md(
        tenant.as_str(),
        project_id.as_deref(),
        &project_takeaways,
        &global_takeaways,
        &cross_tenant_takeaways,
    );
    std::fs::write(&output_path, rendered)?;

    Ok(json!({
        "tenant_id": tenant.to_string(),
        "project_id": project_id,
        "output": output_path,
        "project_takeaways": project_takeaways.len(),
        "global_takeaways": global_takeaways.len(),
        "cross_tenant_takeaways": cross_tenant_takeaways.len(),
        "candidate_k": candidate_k
    }))
}

async fn collect_ranked_takeaways<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: Option<&str>,
    queries: &[(CliQueryMode, &str, &str)],
    candidate_k: usize,
    limit: usize,
    hit_stats: &HashMap<String, HitStats>,
) -> Result<Vec<Takeaway>> {
    let mut by_chunk: HashMap<String, Takeaway> = HashMap::new();

    for (mode, source, query) in queries {
        let payload = cli_search_payload(
            store,
            tenant_id.to_string(),
            project_id.map(str::to_string),
            (*query).to_string(),
            candidate_k,
            true,
            Some(6_000),
            *mode,
            false,
            false,
            false,
        )
        .await?;
        merge_payload_candidates(&mut by_chunk, &payload, source);
    }

    let tag_counts = recurring_tag_counts(by_chunk.values());
    let now_ms = now_ms() as i64;
    let mut takeaways = by_chunk
        .into_values()
        .map(|mut takeaway| {
            takeaway.priority = priority_score(&takeaway, &tag_counts, hit_stats, now_ms);
            takeaway
        })
        .collect::<Vec<_>>();
    suppress_finishes_covered_by_libraries(&mut takeaways);
    takeaways.sort_by(|left, right| {
        right
            .priority
            .partial_cmp(&left.priority)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
    });
    takeaways.truncate(limit);
    Ok(takeaways)
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
fn suppress_finishes_covered_by_libraries(takeaways: &mut Vec<Takeaway>) {
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
        return;
    }
    takeaways.retain(|takeaway| !is_suppressible_finish(takeaway, &covered));
}

/// True only for system-generated library digests. The
/// `task:status:generated` requirement guards against a user-authored
/// chunk spoofing a `task:role:*` tag to suppress real finishes.
fn is_library_digest(tags: &[String]) -> bool {
    let generated = tags.iter().any(|tag| tag == "task:status:generated");
    let role = tags.iter().any(|tag| {
        tag == "task:role:highlight_library" || tag == "task:role:project_brief"
    });
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
    payload: &Value,
    source: &str,
) {
    let Some(results) = payload.get("results").and_then(Value::as_array) else {
        return;
    };
    for result in results {
        let Some(chunk_id) = result.get("chunk_id").and_then(Value::as_str) else {
            continue;
        };
        let text = result
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if text.is_empty() {
            continue;
        }
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
        if is_empty_generated_digest(text, &tags) {
            continue;
        }
        // Defence-in-depth: the lifecycle visibility filter already
        // hides superseded chunks, but skip anything still carrying a
        // `kind:superseded` tag so consolidated output never competes
        // with the raw chunks it replaced.
        if tags.iter().any(|tag| tag.starts_with("kind:superseded")) {
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

fn priority_score(
    takeaway: &Takeaway,
    tag_counts: &HashMap<String, usize>,
    hit_stats: &HashMap<String, HitStats>,
    now_ms: i64,
) -> f32 {
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

    explicit
        + kind_weight
        + type_weight
        + recurrence
        + multi_query
        + search_score
        + library_bonus
        + utility
        + staleness_penalty
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

fn is_empty_generated_digest(text: &str, tags: &[String]) -> bool {
    let generated_digest = tags.iter().any(|tag| tag == "task:status:generated")
        && tags
            .iter()
            .any(|tag| tag.starts_with("task:role:") || tag.starts_with("task:digest:"));
    if !generated_digest {
        return false;
    }
    let lowered = text.to_ascii_lowercase();
    lowered.contains(" contains 0 ") || lowered.contains(" has 0 ")
}

/// Cross-tenant takeaways: pull `kind:consolidated` lessons with
/// `priority>=8` from every tenant under the store data root, dedupe
/// by the first 100 chars of their normalised text, and surface the
/// strongest few.
///
/// This is opt-in via `--cross-tenant` because reading across tenants
/// crosses the default privacy boundary; the caller controls when it
/// happens.
async fn collect_cross_tenant_takeaways<S: Store>(
    store: &S,
    home_tenant_id: &str,
    candidate_k: usize,
    hit_stats: &HashMap<String, HitStats>,
) -> Result<Vec<Takeaway>> {
    let Ok(tenants) = store.list_tenants().await else {
        return Ok(Vec::new());
    };
    let mut by_text: HashMap<String, Takeaway> = HashMap::new();
    for tenant in tenants {
        let tid = tenant.as_str();
        if tid == home_tenant_id {
            // Already represented in the project/machine-wide
            // sections; the cross-tenant section is about *other*
            // tenants.
            continue;
        }
        let candidates = collect_ranked_takeaways(
            store,
            tid,
            None,
            GLOBAL_QUERIES,
            candidate_k,
            candidate_k,
            hit_stats,
        )
        .await
        .unwrap_or_default();
        for takeaway in candidates {
            if !is_cross_tenant_eligible(&takeaway) {
                continue;
            }
            let key = dedupe_key(&takeaway.text);
            // Higher-priority duplicate wins.
            by_text
                .entry(key)
                .and_modify(|existing| {
                    if takeaway.priority > existing.priority {
                        *existing = takeaway.clone();
                    }
                })
                .or_insert(takeaway);
        }
    }
    let mut takeaways: Vec<Takeaway> = by_text.into_values().collect();
    takeaways.sort_by(|a, b| {
        b.priority
            .partial_cmp(&a.priority)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.timestamp_created.cmp(&a.timestamp_created))
    });
    takeaways.truncate(5);
    Ok(takeaways)
}

/// True if a takeaway should appear in the cross-tenant section: it
/// must be a `kind:consolidated` lesson with priority>=8.
fn is_cross_tenant_eligible(takeaway: &Takeaway) -> bool {
    let consolidated = takeaway
        .tags
        .iter()
        .any(|t| t.starts_with("kind:consolidated"));
    let high_priority = takeaway.tags.iter().any(|t| {
        let value = t
            .strip_prefix("priority:")
            .or_else(|| t.strip_prefix("importance:"));
        match value.and_then(|v| v.parse::<f32>().ok()) {
            Some(n) => n >= 8.0,
            None => false,
        }
    });
    consolidated && high_priority
}

/// Normalised dedupe key: lowercased, whitespace-collapsed first 100
/// characters of the takeaway text. Stable enough to drop near-dupes
/// without merging genuinely distinct lessons.
fn dedupe_key(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.to_ascii_lowercase().chars().take(100).collect()
}

fn render_memory_md(
    tenant_id: &str,
    project_id: Option<&str>,
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
    cross_tenant_takeaways: &[Takeaway],
) -> String {
    let mut out = String::new();
    out.push_str("# memory.md\n\n");
    out.push_str("Generated by `memd memory-md`.\n\n");
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
    out.push_str("## Scoring\n\n");
    out.push_str("- Explicit `priority:N` or `importance:N` tags dominate when present.\n");
    out.push_str("- Decisions, finishes, evidence, recurring tags, multi-query matches, and search score increase priority.\n");
    out.push_str("- Repeated lessons should be recorded again with a higher `priority:N` tag when they keep mattering.\n\n");

    render_section(&mut out, "Project Takeaways", project_takeaways);
    render_section(&mut out, "Machine-Wide Takeaways", global_takeaways);
    if !cross_tenant_takeaways.is_empty() {
        render_section(&mut out, "Cross-Tenant Takeaways", cross_tenant_takeaways);
    }
    out
}

fn render_section(out: &mut String, title: &str, takeaways: &[Takeaway]) {
    out.push_str(&format!("## {title}\n\n"));
    if takeaways.is_empty() {
        out.push_str("- No takeaways found yet.\n\n");
        return;
    }

    for (idx, takeaway) in takeaways.iter().enumerate() {
        out.push_str(&format!(
            "{}. {}\n",
            idx + 1,
            summarize_text(&takeaway.text, 320)
        ));
        out.push_str(&format!(
            "   - priority: `{:.1}`; chunk: `{}`; type: `{}`; tenant: `{}`",
            takeaway.priority, takeaway.chunk_id, takeaway.chunk_type, takeaway.tenant_id
        ));
        if let Some(project_id) = takeaway.project_id.as_deref() {
            out.push_str(&format!("; project: `{project_id}`"));
        }
        out.push('\n');
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
    out.push('\n');
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
        let rendered = render_memory_md("tenant-a", Some("project-a"), &[takeaway], &[], &[]);
        assert!(rendered.contains("## Project Takeaways"));
        assert!(rendered.contains("chunk-a"));
        assert!(rendered.contains("..."));
    }

    fn make_takeaway(
        chunk_id: &str,
        text: &str,
        tags: Vec<&str>,
        chunk_type: &str,
    ) -> Takeaway {
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
    fn extract_covered_task_ids_parses_summary_footer() {
        let text = "Highlight library for foo contains 3 ranked lessons.\nCovers tasks: task:id:T1, task:id:T2, task:id:T3";
        let ids = extract_covered_task_ids(text);
        assert_eq!(ids, vec!["T1".to_string(), "T2".to_string(), "T3".to_string()]);
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
        let raw = make_takeaway(
            "raw",
            "Plain finish.",
            vec!["kind:finish"],
            "summary",
        );
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
    fn empty_generated_digest_placeholders_are_skipped() {
        let tags = vec![
            "task:status:generated".to_string(),
            "task:role:decision_library".to_string(),
        ];
        assert!(is_empty_generated_digest(
            "Task digest status generated. Summary: Decision library contains 0 explicit decisions.",
            &tags,
        ));
        assert!(!is_empty_generated_digest(
            "Useful decision with concrete operational value.",
            &tags,
        ));
    }
}
