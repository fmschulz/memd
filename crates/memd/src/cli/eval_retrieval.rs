//! Fixed retrieval-quality evals for project memory.
//!
//! Each JSONL query names one or more known-useful chunk IDs. The
//! runner searches once per query, scores the full top-k list, and
//! reports counterfactual filtered views that remove generated digest
//! wrappers or keep only durable/consolidated rows.

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

const DEFAULT_QUERIES_PATH: &str = "evals/bench/queries/retrieval_queries.jsonl";
const REPORTS_DIR: &str = "evals/bench/reports";

#[derive(Debug, Clone)]
pub(super) struct EvalRetrievalOptions {
    pub(super) tenant_id: String,
    pub(super) project_id: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) queries_path: Option<PathBuf>,
    pub(super) k: usize,
    pub(super) min_precision_at_k: f64,
    pub(super) min_hit_rate_at_k: f64,
    pub(super) min_known_recall_at_k: f64,
    pub(super) min_mrr: f64,
}

#[derive(Debug, Deserialize)]
struct RetrievalEvalCase {
    query: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    useful_chunk_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct EvalRow {
    chunk_id: String,
    tags: Vec<String>,
    chunk_type: String,
}

#[derive(Debug, Clone)]
struct VariantMetrics {
    name: &'static str,
    precision_at_k: f64,
    known_recall_at_k: f64,
    reciprocal_rank: f64,
    max_possible_precision_at_k: f64,
    hit: bool,
    relevant_hits: usize,
    returned: usize,
    ranked_chunk_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct QueryEval {
    label: Option<String>,
    query: String,
    useful_chunk_ids: Vec<String>,
    variants: Vec<VariantMetrics>,
}

pub(super) async fn run_eval_retrieval<S: Store>(
    store: &S,
    options: EvalRetrievalOptions,
) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let tenant = TenantId::new(&options.tenant_id)?;
    let k = options.k.clamp(1, 50);
    let min_precision_at_k = options.min_precision_at_k.clamp(0.0, 1.0);
    let min_hit_rate_at_k = options.min_hit_rate_at_k.clamp(0.0, 1.0);
    let min_known_recall_at_k = options.min_known_recall_at_k.clamp(0.0, 1.0);
    let min_mrr = options.min_mrr.clamp(0.0, 1.0);

    let queries_path = options
        .queries_path
        .clone()
        .unwrap_or_else(|| project_dir.join(DEFAULT_QUERIES_PATH));
    let cases = load_cases(&queries_path)?;
    if cases.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "eval-retrieval: no queries found in {}",
            queries_path.display()
        )));
    }

    let mut evals = Vec::with_capacity(cases.len());
    for case in cases {
        let rows = search_rows(
            store,
            tenant.as_str(),
            &options.project_id,
            &case.query,
            k.saturating_mul(4).max(k),
        )
        .await?;
        let useful = case
            .useful_chunk_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let variants = vec![
            variant_metrics("full", rows.iter(), &useful, k),
            variant_metrics(
                "without_generated_digests",
                rows.iter().filter(|row| !is_generated_digest(row)),
                &useful,
                k,
            ),
            variant_metrics(
                "durable_or_consolidated",
                rows.iter().filter(|row| is_durable_or_consolidated(row)),
                &useful,
                k,
            ),
        ];
        evals.push(QueryEval {
            label: case.label,
            query: case.query,
            useful_chunk_ids: case.useful_chunk_ids,
            variants,
        });
    }

    let full = aggregate_variant(&evals, "full", k);
    let without_generated = aggregate_variant(&evals, "without_generated_digests", k);
    let durable = aggregate_variant(&evals, "durable_or_consolidated", k);
    let report_path = write_report(&project_dir, &evals, k)?;

    let coverage_failures = precision_judgment_coverage_failures(&evals, k, min_precision_at_k);
    let mut failures = Vec::new();
    failures.extend(coverage_failures.iter().cloned());
    if full["mean_precision_at_k"].as_f64().unwrap_or(0.0) + f64::EPSILON < min_precision_at_k {
        failures.push(format!(
            "precision_at_{k} {:.3} below threshold {:.3}",
            full["mean_precision_at_k"].as_f64().unwrap_or(0.0),
            min_precision_at_k
        ));
    }
    if full["hit_rate_at_k"].as_f64().unwrap_or(0.0) + f64::EPSILON < min_hit_rate_at_k {
        failures.push(format!(
            "hit_rate_at_{k} {:.3} below threshold {:.3}",
            full["hit_rate_at_k"].as_f64().unwrap_or(0.0),
            min_hit_rate_at_k
        ));
    }
    if full["mean_known_recall_at_k"].as_f64().unwrap_or(0.0) + f64::EPSILON < min_known_recall_at_k
    {
        failures.push(format!(
            "known_recall_at_{k} {:.3} below threshold {:.3}",
            full["mean_known_recall_at_k"].as_f64().unwrap_or(0.0),
            min_known_recall_at_k
        ));
    }
    if full["mean_reciprocal_rank"].as_f64().unwrap_or(0.0) + f64::EPSILON < min_mrr {
        failures.push(format!(
            "mean_reciprocal_rank {:.3} below threshold {:.3}",
            full["mean_reciprocal_rank"].as_f64().unwrap_or(0.0),
            min_mrr
        ));
    }

    let payload = json!({
        "passed": failures.is_empty(),
        "tenant_id": options.tenant_id,
        "project_id": options.project_id,
        "queries": evals.len(),
        "k": k,
        "thresholds": {
            "min_precision_at_k": min_precision_at_k,
            "min_hit_rate_at_k": min_hit_rate_at_k,
            "min_known_recall_at_k": min_known_recall_at_k,
            "min_mrr": min_mrr,
        },
        "variants": {
            "full": full,
            "without_generated_digests": without_generated,
            "durable_or_consolidated": durable,
        },
        "report": report_path,
        "judgment_coverage_failures": coverage_failures,
        "failures": failures,
    });

    if !failures.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "retrieval eval thresholds failed: {}",
            serde_json::to_string(&payload)?
        )));
    }

    Ok(payload)
}

fn load_cases(path: &std::path::Path) -> Result<Vec<RetrievalEvalCase>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        MemdError::ValidationError(format!(
            "eval-retrieval: cannot read queries file {}: {e}",
            path.display()
        ))
    })?;
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parsed: RetrievalEvalCase = serde_json::from_str(trimmed).map_err(|e| {
            MemdError::ValidationError(format!(
                "eval-retrieval: invalid JSON on line {} of {}: {e}",
                idx + 1,
                path.display()
            ))
        })?;
        if parsed.query.trim().is_empty() {
            continue;
        }
        if parsed.useful_chunk_ids.is_empty() {
            return Err(MemdError::ValidationError(format!(
                "eval-retrieval: line {} of {} has no useful_chunk_ids",
                idx + 1,
                path.display()
            )));
        }
        out.push(parsed);
    }
    Ok(out)
}

async fn search_rows<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: &Option<String>,
    query: &str,
    k: usize,
) -> Result<Vec<EvalRow>> {
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
                .filter_map(|row| {
                    let chunk_id = row.get("chunk_id").and_then(Value::as_str)?.to_string();
                    let tags = row
                        .get("tags")
                        .and_then(Value::as_array)
                        .map(|tags| {
                            tags.iter()
                                .filter_map(Value::as_str)
                                .map(ToString::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    let chunk_type = row
                        .get("chunk_type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some(EvalRow {
                        chunk_id,
                        tags,
                        chunk_type,
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

fn variant_metrics<'a>(
    name: &'static str,
    rows: impl Iterator<Item = &'a EvalRow>,
    useful: &HashSet<String>,
    k: usize,
) -> VariantMetrics {
    let ranked_chunk_ids = rows
        .take(k)
        .map(|row| row.chunk_id.clone())
        .collect::<Vec<_>>();
    let first_relevant_rank = ranked_chunk_ids
        .iter()
        .position(|chunk_id| useful.contains(chunk_id));
    let relevant_hits = ranked_chunk_ids
        .iter()
        .filter(|chunk_id| useful.contains(*chunk_id))
        .count();
    let useful_count = useful.len().max(1);
    VariantMetrics {
        name,
        precision_at_k: relevant_hits as f64 / k as f64,
        known_recall_at_k: relevant_hits as f64 / useful_count as f64,
        reciprocal_rank: first_relevant_rank
            .map(|rank| 1.0 / (rank + 1) as f64)
            .unwrap_or(0.0),
        max_possible_precision_at_k: useful.len().min(k) as f64 / k as f64,
        hit: relevant_hits > 0,
        relevant_hits,
        returned: ranked_chunk_ids.len(),
        ranked_chunk_ids,
    }
}

fn aggregate_variant(evals: &[QueryEval], name: &str, k: usize) -> Value {
    let mut precision_sum = 0.0;
    let mut known_recall_sum = 0.0;
    let mut reciprocal_rank_sum = 0.0;
    let mut max_possible_precision_sum = 0.0;
    let mut hit_count = 0usize;
    let mut returned_sum = 0usize;
    for eval in evals {
        let Some(metrics) = eval.variants.iter().find(|variant| variant.name == name) else {
            continue;
        };
        precision_sum += metrics.precision_at_k;
        known_recall_sum += metrics.known_recall_at_k;
        reciprocal_rank_sum += metrics.reciprocal_rank;
        max_possible_precision_sum += metrics.max_possible_precision_at_k;
        hit_count += usize::from(metrics.hit);
        returned_sum += metrics.returned;
    }
    let denom = evals.len().max(1) as f64;
    json!({
        "mean_precision_at_k": precision_sum / denom,
        "mean_known_recall_at_k": known_recall_sum / denom,
        "mean_reciprocal_rank": reciprocal_rank_sum / denom,
        "mean_max_possible_precision_at_k": max_possible_precision_sum / denom,
        "hit_rate_at_k": hit_count as f64 / denom,
        "mean_returned_at_k": returned_sum as f64 / denom,
        "k": k,
    })
}

fn precision_judgment_coverage_failures(
    evals: &[QueryEval],
    k: usize,
    min_precision_at_k: f64,
) -> Vec<String> {
    if min_precision_at_k <= 0.0 {
        return Vec::new();
    }
    evals
        .iter()
        .filter_map(|eval| {
            let unique_useful = eval
                .useful_chunk_ids
                .iter()
                .collect::<HashSet<_>>()
                .len();
            let max_possible_precision_at_k = unique_useful.min(k) as f64 / k as f64;
            if max_possible_precision_at_k + f64::EPSILON < min_precision_at_k {
                let label = eval.label.as_deref().unwrap_or("<unlabeled>");
                Some(format!(
                    "query {label} has {unique_useful} judged useful chunk IDs; max_possible_precision_at_{k} {max_possible_precision_at_k:.3} below threshold {min_precision_at_k:.3}"
                ))
            } else {
                None
            }
        })
        .collect()
}

fn is_generated_digest(row: &EvalRow) -> bool {
    row.tags.iter().any(|tag| {
        tag == "task:status:generated"
            || tag.starts_with("task:digest:")
            || tag.starts_with("task:role:")
    })
}

fn is_durable_or_consolidated(row: &EvalRow) -> bool {
    if matches!(row.chunk_type.as_str(), "decision" | "plan" | "research") {
        return true;
    }
    row.tags.iter().any(|tag| {
        matches!(
            tag.as_str(),
            "kind:decision"
                | "kind:evidence"
                | "kind:finish"
                | "kind:consolidated"
                | "retention:durable"
                | "validated:true"
                | "supports:true"
        ) || tag.starts_with("priority:")
            || tag.starts_with("importance:")
    })
}

fn write_report(project_dir: &std::path::Path, evals: &[QueryEval], k: usize) -> Result<PathBuf> {
    let reports_dir = project_dir.join(REPORTS_DIR);
    std::fs::create_dir_all(&reports_dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = reports_dir.join(format!("retrieval_{stamp}.json"));
    let query_reports = evals
        .iter()
        .map(|eval| {
            json!({
                "label": eval.label,
                "query": eval.query,
                "useful_chunk_ids": eval.useful_chunk_ids,
                "variants": eval.variants.iter().map(|variant| json!({
                    "name": variant.name,
                    "precision_at_k": variant.precision_at_k,
                    "known_recall_at_k": variant.known_recall_at_k,
                    "reciprocal_rank": variant.reciprocal_rank,
                    "max_possible_precision_at_k": variant.max_possible_precision_at_k,
                    "hit": variant.hit,
                    "relevant_hits": variant.relevant_hits,
                    "returned": variant.returned,
                    "ranked_chunk_ids": variant.ranked_chunk_ids,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "k": k,
            "queries": query_reports,
        }))?,
    )?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_cases_requires_useful_chunk_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queries.jsonl");
        std::fs::write(&path, "{\"query\":\"alpha\"}\n").unwrap();
        let err = load_cases(&path).unwrap_err().to_string();
        assert!(err.contains("no useful_chunk_ids"), "{err}");
    }

    #[test]
    fn generated_and_durable_filters_classify_expected_rows() {
        let generated = EvalRow {
            chunk_id: "g".to_string(),
            tags: vec![
                "task:status:generated".to_string(),
                "task:role:highlight_library".to_string(),
            ],
            chunk_type: "summary".to_string(),
        };
        let durable = EvalRow {
            chunk_id: "d".to_string(),
            tags: vec!["kind:evidence".to_string()],
            chunk_type: "summary".to_string(),
        };
        let low_signal = EvalRow {
            chunk_id: "p".to_string(),
            tags: vec!["kind:progress".to_string()],
            chunk_type: "summary".to_string(),
        };

        assert!(is_generated_digest(&generated));
        assert!(is_durable_or_consolidated(&durable));
        assert!(!is_durable_or_consolidated(&low_signal));
    }

    #[test]
    fn variant_metrics_uses_fixed_k_denominator() {
        let rows = [
            EvalRow {
                chunk_id: "a".to_string(),
                tags: Vec::new(),
                chunk_type: "summary".to_string(),
            },
            EvalRow {
                chunk_id: "b".to_string(),
                tags: Vec::new(),
                chunk_type: "summary".to_string(),
            },
        ];
        let useful = ["a".to_string(), "missing".to_string()]
            .into_iter()
            .collect();
        let metrics = variant_metrics("full", rows.iter(), &useful, 5);
        assert_eq!(metrics.relevant_hits, 1);
        assert_eq!(metrics.returned, 2);
        assert_eq!(metrics.precision_at_k, 0.2);
        assert_eq!(metrics.known_recall_at_k, 0.5);
        assert_eq!(metrics.reciprocal_rank, 1.0);
        assert_eq!(metrics.max_possible_precision_at_k, 0.4);
    }

    #[test]
    fn precision_coverage_flags_under_judged_queries() {
        let eval = QueryEval {
            label: Some("under_judged".to_string()),
            query: "alpha".to_string(),
            useful_chunk_ids: vec!["a".to_string()],
            variants: Vec::new(),
        };
        let failures = precision_judgment_coverage_failures(&[eval], 5, 0.6);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("max_possible_precision_at_5 0.200"));
    }

    #[test]
    fn precision_coverage_is_report_only_when_threshold_disabled() {
        let eval = QueryEval {
            label: Some("sparse_judgment".to_string()),
            query: "alpha".to_string(),
            useful_chunk_ids: vec!["a".to_string()],
            variants: Vec::new(),
        };
        let failures = precision_judgment_coverage_failures(&[eval], 5, 0.0);
        assert!(failures.is_empty());
    }
}
