//! `memd eval-counterfactual` — measures whether `kind:consolidated`
//! lessons are actually load-bearing in retrieval (Phase 3).
//!
//! For each query in the benchmark file, run a search twice:
//!   (a) full bank — every chunk visible;
//!   (b) consolidated-filtered — `kind:consolidated` hits removed.
//! Compute retrieval@5 overlap and the average rank shift between
//! the two result sets. A positive overlap-loss means the
//! consolidated lessons are doing real work; a zero loss means they
//! are decoration.
//!
//! The report is a small Markdown file under
//! `evals/bench/reports/` that can be diffed across runs.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use super::paths::absolutize_project_dir;
use super::search::cli_search_payload_silent;
use crate::error::{MemdError, Result};
use crate::store::Store;
use crate::types::TenantId;

const DEFAULT_QUERIES_PATH: &str = "evals/bench/queries/counterfactual_queries.jsonl";
const REPORTS_DIR: &str = "evals/bench/reports";

#[derive(Debug, Clone)]
pub(super) struct EvalCounterfactualOptions {
    pub(super) tenant_id: String,
    pub(super) project_id: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) queries_path: Option<PathBuf>,
    pub(super) k: usize,
}

#[derive(Debug, Deserialize)]
struct BenchQuery {
    query: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Clone)]
struct QueryEval {
    query: String,
    label: Option<String>,
    full: Vec<String>,
    filtered: Vec<String>,
    overlap_at_k: usize,
    avg_rank_shift: f64,
}

pub(super) async fn run_eval_counterfactual<S: Store>(
    store: &S,
    options: EvalCounterfactualOptions,
) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let tenant = TenantId::new(&options.tenant_id)?;
    let k = options.k.clamp(1, 50);

    let queries_path = options
        .queries_path
        .clone()
        .unwrap_or_else(|| project_dir.join(DEFAULT_QUERIES_PATH));
    let queries = load_queries(&queries_path)?;
    if queries.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "no queries found in {}",
            queries_path.display()
        )));
    }

    let mut evals = Vec::with_capacity(queries.len());
    for q in queries {
        // Single wider-k search so the filtered baseline is derived
        // from the same ranking pass as the full baseline — no second
        // search, no per-chunk store.get round-trips.
        let scan_k = k.saturating_mul(4).max(k);
        let scored = scored_for_query(store, tenant.as_str(), &options.project_id, &q.query, scan_k)
            .await?;
        let full: Vec<String> = scored
            .iter()
            .take(k)
            .map(|r| r.chunk_id.clone())
            .collect();
        let filtered: Vec<String> = scored
            .iter()
            .filter(|r| !r.is_consolidated)
            .take(k)
            .map(|r| r.chunk_id.clone())
            .collect();

        let overlap_at_k = count_overlap(&full, &filtered);
        let avg_rank_shift = avg_rank_shift(&full, &filtered);
        evals.push(QueryEval {
            query: q.query,
            label: q.label,
            full,
            filtered,
            overlap_at_k,
            avg_rank_shift,
        });
    }

    let report_path = write_report(&project_dir, &evals, k)?;
    let mean_overlap_loss = evals
        .iter()
        .map(|e| overlap_loss(e, k) as f64)
        .sum::<f64>()
        / evals.len() as f64;
    let mean_rank_shift =
        evals.iter().map(|e| e.avg_rank_shift).sum::<f64>() / evals.len() as f64;

    Ok(json!({
        "tenant_id": options.tenant_id,
        "project_id": options.project_id,
        "queries": evals.len(),
        "k": k,
        "mean_overlap_loss_at_k": mean_overlap_loss,
        "mean_rank_shift": mean_rank_shift,
        "report": report_path,
    }))
}

fn load_queries(path: &std::path::Path) -> Result<Vec<BenchQuery>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        MemdError::ValidationError(format!(
            "eval-counterfactual: cannot read queries file {}: {e}",
            path.display()
        ))
    })?;
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parsed: BenchQuery = serde_json::from_str(trimmed).map_err(|e| {
            MemdError::ValidationError(format!(
                "eval-counterfactual: invalid JSON on line {} of {}: {e}",
                idx + 1,
                path.display()
            ))
        })?;
        if parsed.query.trim().is_empty() {
            continue;
        }
        out.push(parsed);
    }
    Ok(out)
}

#[derive(Debug, Clone)]
struct ScoredResult {
    chunk_id: String,
    is_consolidated: bool,
}

/// Run one search and return ranked results with a precomputed
/// "is consolidated" flag taken from each row's own `tags` field —
/// so the filtered baseline is the same ranking pass with consolidated
/// rows removed, not a separate query.
async fn scored_for_query<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: &Option<String>,
    query: &str,
    k: usize,
) -> Result<Vec<ScoredResult>> {
    let payload = cli_search_payload_silent(
        store,
        tenant_id.to_string(),
        project_id.clone(),
        query.to_string(),
        k,
        true,
        Some(16_000),
        super::args::CliQueryMode::Generic,
        false,
        false,
        false,
    )
    .await?;
    Ok(payload
        .get("results")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let chunk_id = r.get("chunk_id").and_then(Value::as_str)?.to_string();
                    if chunk_id.is_empty() {
                        return None;
                    }
                    let is_consolidated = r
                        .get("tags")
                        .and_then(Value::as_array)
                        .map(|tags| {
                            tags.iter()
                                .any(|t| t.as_str() == Some("kind:consolidated"))
                        })
                        .unwrap_or(false);
                    Some(ScoredResult {
                        chunk_id,
                        is_consolidated,
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

fn count_overlap(full: &[String], filtered: &[String]) -> usize {
    let set: HashSet<&String> = filtered.iter().collect();
    full.iter().filter(|id| set.contains(id)).count()
}

/// Overlap loss normalised by the smaller of `k` and the actual top-k
/// size — avoids inflating the metric when the corpus has fewer than
/// `k` chunks.
fn overlap_loss(eval: &QueryEval, k: usize) -> usize {
    let denom = k.min(eval.full.len()).max(eval.overlap_at_k);
    denom.saturating_sub(eval.overlap_at_k)
}

/// Average absolute rank shift for chunks present in both sets.
/// 0 means the consolidated filter did not move anything; larger
/// values mean the consolidated chunks were reshuffling ranks.
fn avg_rank_shift(full: &[String], filtered: &[String]) -> f64 {
    let mut shifts = Vec::new();
    for (rank_a, id) in full.iter().enumerate() {
        if let Some(rank_b) = filtered.iter().position(|x| x == id) {
            shifts.push((rank_a as i64 - rank_b as i64).unsigned_abs() as f64);
        }
    }
    if shifts.is_empty() {
        0.0
    } else {
        shifts.iter().sum::<f64>() / shifts.len() as f64
    }
}

fn write_report(
    project_dir: &std::path::Path,
    evals: &[QueryEval],
    k: usize,
) -> Result<PathBuf> {
    let reports_dir = project_dir.join(REPORTS_DIR);
    std::fs::create_dir_all(&reports_dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = reports_dir.join(format!("counterfactual_{stamp}.md"));

    let mut out = String::new();
    out.push_str("# Counterfactual Retrieval Eval\n\n");
    out.push_str(&format!("- queries: {}\n", evals.len()));
    out.push_str(&format!("- k: {}\n\n", k));

    let mean_overlap_loss = evals
        .iter()
        .map(|e| overlap_loss(e, k) as f64)
        .sum::<f64>()
        / evals.len() as f64;
    out.push_str(&format!(
        "- mean overlap loss @ k: {:.2} / {k} (normalized to actual top-k size)\n",
        mean_overlap_loss
    ));
    let mean_rank_shift =
        evals.iter().map(|e| e.avg_rank_shift).sum::<f64>() / evals.len() as f64;
    out.push_str(&format!("- mean abs rank shift: {:.2}\n\n", mean_rank_shift));

    out.push_str("| # | label | query | overlap@k | rank-shift | full top-k | filtered top-k |\n");
    out.push_str("|---|-------|-------|-----------|------------|------------|----------------|\n");
    for (i, e) in evals.iter().enumerate() {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.2} | {} | {} |\n",
            i + 1,
            e.label.as_deref().unwrap_or(""),
            truncate(&e.query, 40),
            e.overlap_at_k,
            e.avg_rank_shift,
            join_short(&e.full),
            join_short(&e.filtered),
        ));
    }
    std::fs::write(&path, out)?;
    Ok(path)
}

fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(limit.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn join_short(ids: &[String]) -> String {
    ids.iter()
        .map(|id| id.chars().take(8).collect::<String>())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_overlap_full_overlap() {
        let a = vec!["a".into(), "b".into(), "c".into()];
        let b = vec!["c".into(), "b".into(), "a".into()];
        assert_eq!(count_overlap(&a, &b), 3);
    }

    #[test]
    fn count_overlap_partial() {
        let a = vec!["a".into(), "b".into(), "c".into()];
        let b = vec!["x".into(), "b".into(), "y".into()];
        assert_eq!(count_overlap(&a, &b), 1);
    }

    #[test]
    fn rank_shift_zero_when_identical() {
        let a = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(avg_rank_shift(&a, &a), 0.0);
    }

    #[test]
    fn rank_shift_counts_reorder() {
        // a: [x, y]
        // b: [y, x]   → both shifted by 1
        let a = vec!["x".into(), "y".into()];
        let b = vec!["y".into(), "x".into()];
        assert_eq!(avg_rank_shift(&a, &b), 1.0);
    }

    #[test]
    fn load_queries_parses_jsonl_and_skips_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.jsonl");
        std::fs::write(
            &path,
            "{\"query\":\"alpha\"}\n\n# comment\n{\"query\":\"beta\",\"label\":\"l\"}\n",
        )
        .unwrap();
        let queries = load_queries(&path).unwrap();
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[1].label.as_deref(), Some("l"));
    }
}
