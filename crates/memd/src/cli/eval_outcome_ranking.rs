//! Served-versus-shadow counterfactual evaluation for outcome-v1 ranking.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::paths::absolutize_project_dir;
use super::unwrap_content_payload;
use crate::error::{MemdError, Result};
use crate::mcp::handlers::{handle_memory_search, SearchParams};
use crate::store::{RankingPolicyMode, RetrievalEpisodeId, RetrievalEpisodeItem, Store};
use crate::types::TenantId;

#[derive(Debug, Clone)]
pub(super) struct EvalOutcomeRankingOptions {
    pub(super) tenant_id: String,
    pub(super) project_id: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) queries_path: PathBuf,
    pub(super) k: usize,
    pub(super) report_json: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
struct OutcomeBenchQuery {
    id: String,
    query: String,
    #[serde(default)]
    relevant_chunk_ids: Vec<String>,
    #[serde(default)]
    harmful_chunk_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PolicyMetrics {
    recall_at_k: f64,
    reciprocal_rank: f64,
    harmful_at_k: usize,
}

#[derive(Debug, Clone, Serialize)]
struct OutcomeCounterfactualRow {
    query_id: String,
    query_hash: String,
    retrieval_episode_id: String,
    served_top_k: Vec<String>,
    shadow_top_k: Vec<String>,
    order_changed: bool,
    served: PolicyMetrics,
    shadow: PolicyMetrics,
}

#[derive(Debug, Clone, Serialize)]
struct AggregatePolicyMetrics {
    mean_recall_at_k: f64,
    mean_reciprocal_rank: f64,
    mean_harmful_at_k: f64,
}

#[derive(Debug, Clone, Serialize)]
struct OutcomeCounterfactualSummary {
    query_count: usize,
    changed_query_count: usize,
    served: AggregatePolicyMetrics,
    shadow: AggregatePolicyMetrics,
    shadow_minus_served_recall_at_k: f64,
    shadow_minus_served_mrr: f64,
    shadow_minus_served_harmful_at_k: f64,
}

#[derive(Debug, Clone, Serialize)]
struct OutcomeCounterfactualReport {
    schema_version: &'static str,
    generated_unix_ms: u128,
    tenant_id: String,
    project_id: Option<String>,
    policy_version: &'static str,
    policy_mode: RankingPolicyMode,
    k: usize,
    queries_path: String,
    queries_sha256: String,
    summary: OutcomeCounterfactualSummary,
    rows: Vec<OutcomeCounterfactualRow>,
}

pub(super) async fn run_eval_outcome_ranking<S: Store>(
    store: &S,
    options: EvalOutcomeRankingOptions,
) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let queries_path = resolve_from(&project_dir, &options.queries_path);
    let report_json = resolve_from(&project_dir, &options.report_json);
    let report_markdown = report_json.with_extension("md");
    refuse_overwrite(&report_json)?;
    refuse_overwrite(&report_markdown)?;
    let tenant_id = TenantId::new(&options.tenant_id)?;
    let k = options.k.clamp(1, 50);
    let (queries, queries_sha256) = load_queries(&queries_path)?;
    if queries.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "no outcome-ranking queries found in {}",
            queries_path.display()
        )));
    }

    let mut rows = Vec::with_capacity(queries.len());
    for query in queries {
        let response = handle_memory_search(
            store,
            SearchParams {
                tenant_id: tenant_id.to_string(),
                project_id: options.project_id.clone(),
                query: query.query.clone(),
                k,
                dedupe_by_source: true,
                include_text: Some(false),
                ranking_policy: Some(RankingPolicyMode::Shadow),
                candidate_multiplier: Some(4),
                task_id: Some(format!("eval-outcome-ranking:{}", query.id)),
                suppress_usage_event: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| MemdError::ProtocolError(error.to_string()))?;
        let payload = unwrap_content_payload(response)?;
        let episode_id = payload
            .get("retrieval_episode_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                MemdError::ProtocolError(
                    "outcome-ranking evaluation search returned no retrieval episode".to_string(),
                )
            })?;
        let parsed_episode_id = RetrievalEpisodeId::parse(episode_id)?;
        let (_, items) = store
            .get_retrieval_episode(&tenant_id, &parsed_episode_id)
            .await?
            .ok_or_else(|| {
                MemdError::StorageError(format!(
                    "retrieval episode {episode_id} disappeared during evaluation"
                ))
            })?;
        let served_top_k = ranked_chunk_ids(&items, k, false);
        let shadow_top_k = ranked_chunk_ids(&items, k, true);
        let served = policy_metrics(
            &served_top_k,
            &query.relevant_chunk_ids,
            &query.harmful_chunk_ids,
        );
        let shadow = policy_metrics(
            &shadow_top_k,
            &query.relevant_chunk_ids,
            &query.harmful_chunk_ids,
        );
        rows.push(OutcomeCounterfactualRow {
            query_id: query.id,
            query_hash: crate::store::stable_query_hash(&query.query),
            retrieval_episode_id: episode_id.to_string(),
            order_changed: served_top_k != shadow_top_k,
            served_top_k,
            shadow_top_k,
            served,
            shadow,
        });
    }

    let summary = summarize(&rows);
    let report = OutcomeCounterfactualReport {
        schema_version: "memd.outcome_counterfactual.v1",
        generated_unix_ms: now_ms(),
        tenant_id: tenant_id.to_string(),
        project_id: options.project_id,
        policy_version: crate::store::OUTCOME_POLICY_VERSION,
        policy_mode: RankingPolicyMode::Shadow,
        k,
        queries_path: queries_path.display().to_string(),
        queries_sha256,
        summary,
        rows,
    };
    if let Some(parent) = report_json.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&report_json, serde_json::to_string_pretty(&report)?)?;
    std::fs::write(&report_markdown, render_markdown(&report))?;

    Ok(json!({
        "schema_version": report.schema_version,
        "queries": report.summary.query_count,
        "changed_queries": report.summary.changed_query_count,
        "shadow_minus_served_recall_at_k": report.summary.shadow_minus_served_recall_at_k,
        "shadow_minus_served_mrr": report.summary.shadow_minus_served_mrr,
        "shadow_minus_served_harmful_at_k": report.summary.shadow_minus_served_harmful_at_k,
        "report_json": report_json,
        "report_markdown": report_markdown,
    }))
}

fn resolve_from(project_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_dir.join(path)
    }
}

fn refuse_overwrite(path: &Path) -> Result<()> {
    if path.exists() {
        return Err(MemdError::ValidationError(format!(
            "refusing to overwrite outcome-ranking artifact {}",
            path.display()
        )));
    }
    Ok(())
}

fn load_queries(path: &Path) -> Result<(Vec<OutcomeBenchQuery>, String)> {
    let bytes = std::fs::read(path).map_err(|error| {
        MemdError::ValidationError(format!(
            "cannot read outcome-ranking queries {}: {error}",
            path.display()
        ))
    })?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let text = String::from_utf8(bytes).map_err(|error| {
        MemdError::ValidationError(format!(
            "outcome-ranking queries {} are not UTF-8: {error}",
            path.display()
        ))
    })?;
    let mut queries = Vec::new();
    let mut ids = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let query: OutcomeBenchQuery = serde_json::from_str(line).map_err(|error| {
            MemdError::ValidationError(format!(
                "invalid outcome-ranking JSON on line {} of {}: {error}",
                index + 1,
                path.display()
            ))
        })?;
        validate_query(&query, &mut ids, index + 1)?;
        queries.push(query);
    }
    Ok((queries, sha256))
}

fn validate_query(query: &OutcomeBenchQuery, ids: &mut HashSet<String>, line: usize) -> Result<()> {
    if query.id.trim().is_empty() || query.query.trim().is_empty() {
        return Err(MemdError::ValidationError(format!(
            "outcome-ranking line {line} requires non-empty id and query"
        )));
    }
    if !ids.insert(query.id.clone()) {
        return Err(MemdError::ValidationError(format!(
            "duplicate outcome-ranking query id {}",
            query.id
        )));
    }
    let relevant = query.relevant_chunk_ids.iter().collect::<HashSet<_>>();
    let harmful = query.harmful_chunk_ids.iter().collect::<HashSet<_>>();
    if relevant.len() != query.relevant_chunk_ids.len()
        || harmful.len() != query.harmful_chunk_ids.len()
        || relevant.iter().any(|id| harmful.contains(id))
    {
        return Err(MemdError::ValidationError(format!(
            "outcome-ranking query {} has duplicate or overlapping judgments",
            query.id
        )));
    }
    Ok(())
}

fn ranked_chunk_ids(items: &[RetrievalEpisodeItem], k: usize, shadow: bool) -> Vec<String> {
    let mut ranked = items
        .iter()
        .filter_map(|item| {
            let rank = if shadow {
                item.shadow_rank
            } else {
                item.served_rank
            }?;
            Some((rank, item))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(rank, _)| *rank);
    let mut source_groups = HashSet::new();
    let mut chunks = Vec::new();
    for (_, item) in ranked {
        if let Some(group) = item.source_dedup_group.as_deref() {
            if !source_groups.insert(group) {
                continue;
            }
        }
        chunks.push(item.chunk_id.to_string());
        if chunks.len() >= k {
            break;
        }
    }
    chunks
}

fn policy_metrics(order: &[String], relevant: &[String], harmful: &[String]) -> PolicyMetrics {
    let relevant = relevant.iter().collect::<HashSet<_>>();
    let harmful = harmful.iter().collect::<HashSet<_>>();
    let relevant_hits = order.iter().filter(|id| relevant.contains(id)).count();
    let recall_at_k = if relevant.is_empty() {
        0.0
    } else {
        relevant_hits as f64 / relevant.len() as f64
    };
    let reciprocal_rank = order
        .iter()
        .position(|id| relevant.contains(id))
        .map(|rank| 1.0 / (rank + 1) as f64)
        .unwrap_or(0.0);
    let harmful_at_k = order.iter().filter(|id| harmful.contains(id)).count();
    PolicyMetrics {
        recall_at_k,
        reciprocal_rank,
        harmful_at_k,
    }
}

fn summarize(rows: &[OutcomeCounterfactualRow]) -> OutcomeCounterfactualSummary {
    let query_count = rows.len();
    let mean = |values: Vec<f64>| values.iter().sum::<f64>() / query_count as f64;
    let aggregate = |shadow: bool| AggregatePolicyMetrics {
        mean_recall_at_k: mean(
            rows.iter()
                .map(|row| {
                    if shadow {
                        row.shadow.recall_at_k
                    } else {
                        row.served.recall_at_k
                    }
                })
                .collect(),
        ),
        mean_reciprocal_rank: mean(
            rows.iter()
                .map(|row| {
                    if shadow {
                        row.shadow.reciprocal_rank
                    } else {
                        row.served.reciprocal_rank
                    }
                })
                .collect(),
        ),
        mean_harmful_at_k: mean(
            rows.iter()
                .map(|row| {
                    if shadow {
                        row.shadow.harmful_at_k as f64
                    } else {
                        row.served.harmful_at_k as f64
                    }
                })
                .collect(),
        ),
    };
    let served = aggregate(false);
    let shadow = aggregate(true);
    OutcomeCounterfactualSummary {
        query_count,
        changed_query_count: rows.iter().filter(|row| row.order_changed).count(),
        shadow_minus_served_recall_at_k: shadow.mean_recall_at_k - served.mean_recall_at_k,
        shadow_minus_served_mrr: shadow.mean_reciprocal_rank - served.mean_reciprocal_rank,
        shadow_minus_served_harmful_at_k: shadow.mean_harmful_at_k - served.mean_harmful_at_k,
        served,
        shadow,
    }
}

fn render_markdown(report: &OutcomeCounterfactualReport) -> String {
    let mut output = String::new();
    output.push_str("# Outcome-ranking counterfactual\n\n");
    output.push_str(&format!(
        "- policy: `{}` (`shadow`)\n",
        report.policy_version
    ));
    output.push_str(&format!("- queries: `{}`\n", report.summary.query_count));
    output.push_str(&format!("- k: `{}`\n", report.k));
    output.push_str(&format!(
        "- changed queries: `{}`\n",
        report.summary.changed_query_count
    ));
    output.push_str(&format!(
        "- shadow - served recall@k: `{:.4}`\n",
        report.summary.shadow_minus_served_recall_at_k
    ));
    output.push_str(&format!(
        "- shadow - served MRR: `{:.4}`\n",
        report.summary.shadow_minus_served_mrr
    ));
    output.push_str(&format!(
        "- shadow - served harmful@k: `{:.4}`\n\n",
        report.summary.shadow_minus_served_harmful_at_k
    ));
    output.push_str("| query_id | changed | served recall | shadow recall | served harmful | shadow harmful |\n");
    output.push_str("|---|---:|---:|---:|---:|---:|\n");
    for row in &report.rows {
        output.push_str(&format!(
            "| {} | {} | {:.3} | {:.3} | {} | {} |\n",
            row.query_id.replace('|', "\\|"),
            row.order_changed,
            row.served.recall_at_k,
            row.shadow.recall_at_k,
            row.served.harmful_at_k,
            row.shadow.harmful_at_k,
        ));
    }
    output
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
    fn metrics_distinguish_relevant_and_harmful_ranks() {
        let metrics = policy_metrics(
            &["harm".to_string(), "ok".to_string()],
            &["ok".to_string()],
            &["harm".to_string()],
        );
        assert_eq!(metrics.recall_at_k, 1.0);
        assert_eq!(metrics.reciprocal_rank, 0.5);
        assert_eq!(metrics.harmful_at_k, 1);
    }

    #[test]
    fn shadow_order_applies_source_dedup() {
        let episode_id = RetrievalEpisodeId::new();
        let tenant_id = TenantId::new("t").unwrap();
        let make = |id: &str, rank: usize, group: Option<&str>| RetrievalEpisodeItem {
            episode_id: episode_id.clone(),
            chunk_id: crate::types::ChunkId::parse(id).unwrap(),
            origin_tenant_id: tenant_id.clone(),
            origin_project_id: None,
            original_rank: rank,
            original_score: 1.0,
            lane_scores_json: "{}".to_string(),
            outcome_adjustment: 0.0,
            served_rank: None,
            shadow_rank: Some(rank),
            rendered: false,
            source_dedup_group: group.map(str::to_string),
        };
        let items = vec![
            make("01900000-0000-7000-8000-000000000001", 0, Some("same")),
            make("01900000-0000-7000-8000-000000000002", 1, Some("same")),
            make("01900000-0000-7000-8000-000000000003", 2, None),
        ];
        assert_eq!(ranked_chunk_ids(&items, 2, true).len(), 2);
        assert_eq!(
            ranked_chunk_ids(&items, 2, true)[1],
            "01900000-0000-7000-8000-000000000003"
        );
    }
}
