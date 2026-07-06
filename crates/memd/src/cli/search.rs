use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use tracing::info;

use crate::error::{MemdError, Result};
use crate::hit_stats::{query_mode_label, record_hits, record_hits_to_data_dir, HitRecord};
use crate::mcp::handlers::{handle_memory_search, SearchParams};
use crate::store::usage::{UsageEvent, UsageOp};
use crate::store::Store;

use super::args::{CliQueryMode, ExportFormat, SearchReranker, SearchRerankerOptions};
use super::unwrap_content_payload;

pub(super) fn export_format_name(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Markdown => "markdown",
        ExportFormat::Json => "json",
        ExportFormat::Jsonl => "jsonl",
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn cli_search_payload<S: Store>(
    store: &S,
    tenant_id: String,
    project_id: Option<String>,
    query: String,
    k: usize,
    compact: bool,
    dedupe_by_source: bool,
    token_budget: Option<usize>,
    mode: CliQueryMode,
    no_text: bool,
    include_artifact: bool,
    include_superseded: bool,
) -> Result<Value> {
    cli_search_payload_inner(
        store,
        tenant_id,
        project_id,
        query,
        k,
        compact,
        dedupe_by_source,
        token_budget,
        mode,
        no_text,
        include_artifact,
        include_superseded,
        true,
        false,
    )
    .await
}

/// Variant of [`cli_search_payload`] that suppresses hit logging.
/// Used by internal probes (eval-counterfactual, future health
/// checks) so synthetic queries do not pollute the retrieval-success
/// signal that feeds `priority_score`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn cli_search_payload_silent<S: Store>(
    store: &S,
    tenant_id: String,
    project_id: Option<String>,
    query: String,
    k: usize,
    compact: bool,
    token_budget: Option<usize>,
    mode: CliQueryMode,
    no_text: bool,
    include_artifact: bool,
    include_superseded: bool,
) -> Result<Value> {
    cli_search_payload_inner(
        store,
        tenant_id,
        project_id,
        query,
        k,
        compact,
        false,
        token_budget,
        mode,
        no_text,
        include_artifact,
        include_superseded,
        false,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn cli_search_payload_inner<S: Store>(
    store: &S,
    tenant_id: String,
    project_id: Option<String>,
    query: String,
    k: usize,
    compact: bool,
    dedupe_by_source: bool,
    token_budget: Option<usize>,
    mode: CliQueryMode,
    no_text: bool,
    include_artifact: bool,
    include_superseded: bool,
    log_hits: bool,
    suppress_usage_event: bool,
) -> Result<Value> {
    let payload = direct_memory_search_payload(
        store,
        tenant_id.as_str(),
        project_id.as_deref(),
        query.as_str(),
        k,
        compact,
        dedupe_by_source,
        token_budget,
        mode,
        no_text,
        include_artifact,
        include_superseded,
        suppress_usage_event,
    )
    .await?;
    let result_count = payload
        .get("results")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    info!(count = result_count, "search complete");
    if log_hits {
        log_search_hits(store, &payload, &tenant_id, project_id.as_deref(), mode);
    }
    Ok(payload)
}

/// Append one [`HitRecord`] per chunk in `payload["results"]` to the
/// central store hit log (resolved from the persistent store's
/// `data_dir`), falling back to the cwd-relative log only for an
/// in-memory store. Best-effort: every IO error inside the hit-stats
/// writer is swallowed so retrieval never fails for this.
fn log_search_hits<S: Store>(
    store: &S,
    payload: &Value,
    tenant_id: &str,
    project_id: Option<&str>,
    mode: CliQueryMode,
) {
    let Some(results) = payload.get("results").and_then(Value::as_array) else {
        return;
    };
    if results.is_empty() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let query_mode = query_mode_label(&format!("{mode:?}"));
    let records: Vec<HitRecord> = results
        .iter()
        .enumerate()
        .filter_map(|(rank, result)| {
            let chunk_id = result.get("chunk_id").and_then(Value::as_str)?;
            if chunk_id.is_empty() {
                return None;
            }
            let score = result.get("score").and_then(Value::as_f64).unwrap_or(0.0);
            // Compact-shaped payloads drop chunks past the token
            // budget; what remains in `results` *was* rendered.
            let selected = true;
            Some(HitRecord {
                ts_ms: now,
                chunk_id: chunk_id.to_string(),
                tenant_id: tenant_id.to_string(),
                project_id: project_id.map(str::to_string),
                query_mode: query_mode.clone(),
                rank,
                score,
                selected,
            })
        })
        .collect();
    // Route hits to the central store data_dir so they aggregate in one
    // ledger regardless of the process cwd. In-memory stores have no
    // data_dir, so fall back to the cwd-relative log (harmless for tests).
    match store.as_persistent() {
        Some(ps) => record_hits_to_data_dir(ps.data_dir(), &records),
        None => record_hits(&records),
    }
}

const MEMRERANKER_HELPER: &str = r#"
import json
import re
import sys
import time


def token_count(text):
    return len(re.findall(r"[A-Za-z0-9_]+", text or ""))


def emit(payload, code=0):
    print(json.dumps(payload, ensure_ascii=False))
    raise SystemExit(code)


def main():
    request = json.load(sys.stdin)
    query = request.get("query") or ""
    results = request.get("results") or []
    model_id = request.get("model") or "IAAR-Shanghai/MemReranker-4B"
    device = (request.get("device") or "auto").strip()
    batch_size = max(1, int(request.get("batch_size") or 1))

    try:
        import torch
    except Exception as exc:
        emit({"ok": False, "error": f"import torch failed: {exc}"}, 2)

    if device == "auto":
        if torch.cuda.is_available():
            device = "cuda"
        else:
            emit({"ok": False, "fallback_reason": "CUDA is not available"})
    elif device.startswith("cuda") and not torch.cuda.is_available():
        emit({"ok": False, "fallback_reason": f"requested device {device} but CUDA is not available"})

    try:
        from sentence_transformers import CrossEncoder
    except Exception as exc:
        emit({"ok": False, "error": f"import sentence_transformers.CrossEncoder failed: {exc}"}, 2)

    pairs = [(query, str(item.get("text") or "")) for item in results]
    if not pairs:
        emit({"ok": True, "scores": [], "metadata": {"model": model_id, "device": device, "pair_count": 0}})

    load_start = time.perf_counter()
    try:
        model = CrossEncoder(model_id, device=device, trust_remote_code=True)
    except Exception as exc:
        emit({"ok": False, "error": f"load CrossEncoder failed: {exc}"}, 2)
    load_seconds = time.perf_counter() - load_start

    rerank_start = time.perf_counter()
    try:
        raw_scores = model.predict(pairs, batch_size=batch_size)
    except Exception as exc:
        emit({"ok": False, "error": f"CrossEncoder prediction failed: {exc}"}, 2)
    rerank_seconds = time.perf_counter() - rerank_start

    scores = [float(score) for score in raw_scores]
    doc_tokens = sum(token_count(item.get("text") or "") for item in results)
    query_tokens = token_count(query)
    metadata = {
        "model": model_id,
        "device": device,
        "batch_size": batch_size,
        "pair_count": len(pairs),
        "load_seconds": round(load_seconds, 3),
        "rerank_seconds": round(rerank_seconds, 3),
        "avg_rerank_seconds_per_pair": round(rerank_seconds / max(1, len(pairs)), 6),
        "estimated_doc_tokens": doc_tokens,
        "estimated_query_tokens_once": query_tokens,
        "estimated_query_tokens_repeated": query_tokens * len(pairs),
        "estimated_pair_tokens": doc_tokens + query_tokens * len(pairs),
    }
    if device.startswith("cuda"):
        try:
            metadata["cuda_device_name"] = torch.cuda.get_device_name(torch.cuda.current_device())
        except Exception:
            pass
    emit({"ok": True, "scores": scores, "metadata": metadata})


main()
"#;

pub(super) fn apply_search_reranker(
    payload: Value,
    query: &str,
    options: &SearchRerankerOptions,
) -> Result<Value> {
    if options.reranker == SearchReranker::None {
        return Ok(payload);
    }

    let results = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if results.is_empty() {
        return Ok(attach_reranker_fallback(
            payload,
            "no results to rerank",
            options,
        ));
    }

    let has_text = results.iter().any(|result| {
        result
            .get("text")
            .and_then(Value::as_str)
            .map(|text| !text.trim().is_empty())
            .unwrap_or(false)
    });
    if !has_text {
        return fallback_or_error(payload, "search results do not include text", options);
    }

    if memreranker_needs_cuda(&options.device) && !cuda_probe_available() {
        return fallback_or_error(payload, "CUDA GPU is not visible to the CLI", options);
    }

    let helper_input = json!({
        "query": query,
        "results": results
            .iter()
            .map(|result| json!({
                "chunk_id": result.get("chunk_id").and_then(Value::as_str).unwrap_or(""),
                "text": result.get("text").and_then(Value::as_str).unwrap_or(""),
            }))
            .collect::<Vec<_>>(),
        "model": &options.model,
        "device": &options.device,
        "batch_size": options.batch_size.max(1),
    });

    match run_memreranker_helper(&helper_input, options) {
        Ok(helper_output) => {
            if helper_output
                .get("ok")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                apply_memreranker_output(payload, helper_output, options)
            } else {
                let reason = helper_output
                    .get("fallback_reason")
                    .or_else(|| helper_output.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or("MemReranker helper did not apply");
                fallback_or_error(payload, reason, options)
            }
        }
        Err(error) => fallback_or_error(payload, &error.to_string(), options),
    }
}

fn fallback_or_error(
    payload: Value,
    reason: &str,
    options: &SearchRerankerOptions,
) -> Result<Value> {
    if options.reranker == SearchReranker::Auto {
        Ok(attach_reranker_fallback(payload, reason, options))
    } else {
        Err(MemdError::ValidationError(format!(
            "MemReranker-4B requested but unavailable: {reason}"
        )))
    }
}

fn attach_reranker_fallback(
    mut payload: Value,
    reason: impl Into<String>,
    options: &SearchRerankerOptions,
) -> Value {
    payload["reranker"] = json!({
        "requested": options.reranker,
        "applied": false,
        "fallback": "built_in_search_order",
        "reason": reason.into(),
        "model": &options.model,
        "device": &options.device,
    });
    payload
}

fn apply_memreranker_output(
    mut payload: Value,
    helper_output: Value,
    options: &SearchRerankerOptions,
) -> Result<Value> {
    let scores = helper_output
        .get("scores")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MemdError::ProtocolError("MemReranker helper returned no scores".to_string())
        })?;
    let results = payload
        .get_mut("results")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| MemdError::ProtocolError("search payload has no results".to_string()))?;
    if scores.len() != results.len() {
        return Err(MemdError::ProtocolError(format!(
            "MemReranker returned {} scores for {} results",
            scores.len(),
            results.len()
        )));
    }

    for (result, score) in results.iter_mut().zip(scores) {
        let score = score.as_f64().ok_or_else(|| {
            MemdError::ProtocolError("MemReranker score is not numeric".to_string())
        })?;
        let old_score = result.get("score").cloned().unwrap_or(Value::Null);
        if let Some(object) = result.as_object_mut() {
            object.insert("pre_rerank_score".to_string(), old_score);
            object.insert("reranker_score".to_string(), json!(score));
            object.insert("score".to_string(), json!(score));
        }
    }
    results.sort_by(|left, right| {
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut metadata = helper_output
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.insert("requested".to_string(), json!(options.reranker));
        object.insert("applied".to_string(), json!(true));
        object.insert("fallback".to_string(), Value::Null);
    }
    payload["reranker"] = metadata;
    Ok(payload)
}

fn run_memreranker_helper(input: &Value, options: &SearchRerankerOptions) -> Result<Value> {
    let timeout = format!("{}s", options.timeout_seconds.max(1));
    let mut child = Command::new("timeout")
        .arg(timeout)
        .arg(&options.python)
        .arg("-c")
        .arg(MEMRERANKER_HELPER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| MemdError::ProtocolError(format!("start MemReranker helper: {err}")))?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            MemdError::ProtocolError("MemReranker helper stdin unavailable".to_string())
        })?;
        stdin.write_all(serde_json::to_string(input)?.as_bytes())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| MemdError::ProtocolError(format!("wait for MemReranker helper: {err}")))?;
    if !output.status.success() {
        if !output.stdout.is_empty() {
            if let Ok(value) = serde_json::from_slice(&output.stdout) {
                return Ok(value);
            }
        }
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(MemdError::ProtocolError(format!(
            "MemReranker helper exited with {code}: stdout: {}; stderr: {}",
            trim_for_error(&stdout),
            trim_for_error(&stderr)
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|err| {
        MemdError::ProtocolError(format!(
            "parse MemReranker helper output: {err}; stderr: {}",
            trim_for_error(&String::from_utf8_lossy(&output.stderr))
        ))
    })
}

fn memreranker_needs_cuda(device: &str) -> bool {
    let device = device.trim().to_ascii_lowercase();
    device == "auto" || device.starts_with("cuda")
}

fn cuda_probe_available() -> bool {
    Command::new("nvidia-smi")
        .arg("-L")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn trim_for_error(text: &str) -> String {
    const MAX_LEN: usize = 1600;
    let text = text.trim();
    if text.chars().count() <= MAX_LEN {
        text.to_string()
    } else {
        let tail: String = text
            .chars()
            .rev()
            .take(MAX_LEN)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("...{tail}")
    }
}

pub(super) async fn cli_agent_context_payload<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: Option<&str>,
    queries: &[String],
    k: usize,
    token_budget: usize,
    mode: CliQueryMode,
    no_text: bool,
    include_artifact: bool,
) -> Result<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut merged_results = Vec::new();
    let mut query_summaries = Vec::new();
    let mut scope_status = Value::Null;

    for query in queries {
        let payload = direct_memory_search_payload(
            store,
            tenant_id,
            project_id,
            query,
            k,
            true,
            false,
            Some(token_budget),
            mode,
            no_text,
            include_artifact,
            false,
            true,
        )
        .await?;
        log_search_hits(store, &payload, tenant_id, project_id, mode);
        // Per-query scope_status entries agree on tenant/project/mode;
        // keep the one with the most signal (warnings or a widen hint).
        if let Some(status) = payload.get("scope_status") {
            if scope_status.is_null()
                || status.get("widen_hint").is_some()
                || status.get("warnings").is_some()
            {
                scope_status = status.clone();
            }
        }
        let results = payload
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for result in &results {
            let Some(chunk_id) = result.get("chunk_id").and_then(Value::as_str) else {
                continue;
            };
            if seen.insert(chunk_id.to_string()) {
                merged_results.push(result.clone());
            }
        }
        query_summaries.push(json!({
            "query": query,
            "result_count": results.len(),
            "budget_info": payload.get("budget_info").cloned().unwrap_or(Value::Null),
        }));
    }

    let n = merged_results.len();
    store.record_usage_event(UsageEvent {
        op: UsageOp::AgentContext,
        tenant: Some(tenant_id.to_string()),
        project: project_id.map(ToString::to_string),
        outcome: if n == 0 {
            "zero_hits".to_string()
        } else {
            format!("hits:{n}")
        },
        chunk_count: Some(n as i64),
        bytes: None,
        detail: Some(json!({"queries": queries.len(), "k": k}).to_string()),
    });

    Ok(json!({
        "tool": "memd.agent_context",
        "interface": "cli_prefetch",
        "retrieval_backend": "direct_store",
        "tenant_id": tenant_id,
        "project_id": project_id,
        "queries": query_summaries,
        "k_per_query": k,
        "token_budget_per_query": token_budget,
        "result_count": merged_results.len(),
        "scope_status": scope_status,
        "results": merged_results,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn direct_memory_search_payload<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: Option<&str>,
    query: &str,
    k: usize,
    compact: bool,
    dedupe_by_source: bool,
    token_budget: Option<usize>,
    mode: CliQueryMode,
    no_text: bool,
    include_artifact: bool,
    include_superseded: bool,
    suppress_usage_event: bool,
) -> Result<Value> {
    let params = SearchParams {
        tenant_id: tenant_id.to_string(),
        query: query.to_string(),
        project_id: project_id.map(ToString::to_string),
        k,
        mode: Some(mode.into()),
        compact,
        dedupe_by_source,
        token_budget,
        include_text: no_text.then_some(false),
        include_artifact: include_artifact.then_some(true),
        include_superseded: include_superseded.then_some(true),
        suppress_usage_event,
        ..Default::default()
    };
    let mcp_value = handle_memory_search(store, params)
        .await
        .map_err(|e| MemdError::ProtocolError(e.to_string()))?;
    unwrap_content_payload(mcp_value)
}
