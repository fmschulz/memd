use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::args::{CliQueryMode, ProjectScopeConfig};
use super::paths::absolutize_project_dir;
use super::search::cli_search_payload;
use crate::error::{MemdError, Result};
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
        )
        .await?
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
    );
    std::fs::write(&output_path, rendered)?;

    Ok(json!({
        "tenant_id": tenant.to_string(),
        "project_id": project_id,
        "output": output_path,
        "project_takeaways": project_takeaways.len(),
        "global_takeaways": global_takeaways.len(),
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
        )
        .await?;
        merge_payload_candidates(&mut by_chunk, &payload, source);
    }

    let tag_counts = recurring_tag_counts(by_chunk.values());
    let mut takeaways = by_chunk
        .into_values()
        .map(|mut takeaway| {
            takeaway.priority = priority_score(&takeaway, &tag_counts);
            takeaway
        })
        .collect::<Vec<_>>();
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

fn priority_score(takeaway: &Takeaway, tag_counts: &HashMap<String, usize>) -> f32 {
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
    let search_score = takeaway.score.max(0.0).min(25.0) * 2.0;

    explicit + kind_weight + type_weight + recurrence + multi_query + search_score
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

fn render_memory_md(
    tenant_id: &str,
    project_id: Option<&str>,
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
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
        let rendered = render_memory_md("tenant-a", Some("project-a"), &[takeaway], &[]);
        assert!(rendered.contains("## Project Takeaways"));
        assert!(rendered.contains("chunk-a"));
        assert!(rendered.contains("..."));
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
