//! Synthetic write-quality eval for bounded memory growth.
//!
//! The eval opens an isolated temporary persistent store, runs a tiny
//! agent-session write fixture through the public memory.add handler,
//! and checks admission, dedupe, storage growth, and retrieval.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::args::CliQueryMode;
use super::paths::absolutize_project_dir;
use super::render::unwrap_content_payload;
use super::search::cli_search_payload_silent;
use crate::compaction::{CompactionConfig, CompactionRunner};
use crate::embeddings::MockEmbedder;
use crate::error::{MemdError, Result};
use crate::ops::{handle_memory_add, AddParams};
use crate::store::dense::{DenseSearchConfig, DenseSearcher};
use crate::store::persistent::{PersistentStore, PersistentStoreConfig};
use crate::store::Store;
use crate::types::{ChunkId, ChunkStatus, MemoryTier, TenantId};

const REPORTS_DIR: &str = "evals/bench/reports";
const SYNTHETIC_PROJECT_ID: &str = "synthetic_session";

#[derive(Debug, Clone)]
pub(super) struct EvalWriteQualityOptions {
    pub(super) project_dir: PathBuf,
    pub(super) min_rejection_or_downgrade_rate: f64,
    pub(super) min_duplicate_reuse_rate: f64,
    pub(super) max_total_chunks: usize,
    pub(super) max_disk_bytes: u64,
    pub(super) require_retention_compaction: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedOutcome {
    Reject,
    Ephemeral,
    Durable,
    DuplicateReused,
}

impl ExpectedOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reject => "reject",
            Self::Ephemeral => "ephemeral",
            Self::Durable => "durable",
            Self::DuplicateReused => "duplicate_reused",
        }
    }

    fn low_value(self) -> bool {
        matches!(self, Self::Reject | Self::Ephemeral)
    }

    fn duplicate_candidate(self) -> bool {
        matches!(self, Self::DuplicateReused)
    }
}

#[derive(Debug)]
struct SyntheticWrite {
    label: &'static str,
    expected: ExpectedOutcome,
    params: AddParams,
}

#[derive(Debug, Clone)]
struct AttemptReport {
    label: &'static str,
    expected: &'static str,
    outcome: String,
    chunk_id: Option<String>,
    admission_decision: Option<String>,
    admission_reason: Option<String>,
    dedupe_decision: Option<String>,
    lifecycle_tier: Option<String>,
    expires_at_ms: Option<i64>,
    review_after_ms: Option<i64>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct RetentionCompactionReport {
    expired_count: usize,
    promoted_count: usize,
    pre_compaction_durable_retrieval_hit: bool,
    post_compaction_durable_retrieval_hit: bool,
    expired_chunk_hidden_after_compaction: bool,
    expired_chunk_status_after_compaction: Option<String>,
}

pub(super) async fn run_eval_write_quality(options: EvalWriteQualityOptions) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let min_rejection_or_downgrade_rate = options.min_rejection_or_downgrade_rate.clamp(0.0, 1.0);
    let min_duplicate_reuse_rate = options.min_duplicate_reuse_rate.clamp(0.0, 1.0);
    let max_total_chunks = options.max_total_chunks;
    let max_disk_bytes = options.max_disk_bytes;

    let temp_store = TempStoreDir::new()?;
    let store = PersistentStore::open(PersistentStoreConfig {
        data_dir: temp_store.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_tiered_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })?;

    let tenant = TenantId::new("eval_write_quality")?;
    let before_stats = store.stats(&tenant).await?;
    let before_disk_bytes = dir_size(temp_store.path())?;
    let writes = synthetic_writes();

    let mut attempts = Vec::with_capacity(writes.len());
    let mut expected_mismatches = Vec::new();
    let mut low_value_attempts = 0usize;
    let mut rejected_or_downgraded = 0usize;
    let mut duplicate_attempts = 0usize;
    let mut duplicate_reused = 0usize;
    let mut durable_chunk_ids = Vec::new();
    let mut ephemeral_chunk_ids = Vec::new();
    let mut expired_chunk_ids = Vec::new();
    let mut progress_retention_chunk_ids = Vec::new();

    for write in writes {
        if write.expected.low_value() {
            low_value_attempts += 1;
        }
        if write.expected.duplicate_candidate() {
            duplicate_attempts += 1;
        }

        let expected = write.expected;
        let label = write.label;
        let attempt = run_write(&store, write).await?;
        if matches!(
            expected,
            ExpectedOutcome::Reject | ExpectedOutcome::Ephemeral
        ) && matches!(attempt.outcome.as_str(), "rejected" | "ephemeral")
        {
            rejected_or_downgraded += 1;
        }
        if expected == ExpectedOutcome::DuplicateReused && attempt.outcome == "duplicate_reused" {
            duplicate_reused += 1;
        }
        if label == "durable_decision" && matches!(attempt.outcome.as_str(), "durable") {
            if let Some(chunk_id) = &attempt.chunk_id {
                durable_chunk_ids.push(chunk_id.clone());
            }
        }
        if attempt.outcome == "ephemeral" {
            if let Some(chunk_id) = &attempt.chunk_id {
                ephemeral_chunk_ids.push(chunk_id.clone());
            }
        }
        if label == "expired_run_trace" {
            if let Some(chunk_id) = &attempt.chunk_id {
                expired_chunk_ids.push(chunk_id.clone());
            }
        }
        if label == "ordinary_progress_summary_ttl" {
            if let Some(chunk_id) = &attempt.chunk_id {
                progress_retention_chunk_ids.push(chunk_id.clone());
            }
        }
        if attempt.outcome != expected_outcome_name(expected) {
            expected_mismatches.push(format!(
                "{} expected {} but got {}",
                label,
                expected.as_str(),
                attempt.outcome
            ));
        }
        attempts.push(attempt);
    }

    let after_stats = store.stats(&tenant).await?;
    let after_disk_bytes = dir_size(temp_store.path())?;
    let rejection_or_downgrade_rate = rate(rejected_or_downgraded, low_value_attempts);
    let duplicate_reuse_rate = rate(duplicate_reused, duplicate_attempts);
    let total_chunks_delta = after_stats
        .total_chunks
        .saturating_sub(before_stats.total_chunks);
    let active_chunks_delta = after_stats
        .active_chunks
        .saturating_sub(before_stats.active_chunks);
    let disk_bytes_delta = after_disk_bytes.saturating_sub(before_disk_bytes);
    let pre_compaction_durable_retrieval_hit =
        durable_retrieval_check(&store, &tenant, &durable_chunk_ids).await?;
    let ephemeral_hidden_from_default_search =
        ephemeral_hidden_check(&store, &tenant, &ephemeral_chunk_ids).await?;
    let progress_summary_ttl_applied =
        default_retention_check(&store, &tenant, &progress_retention_chunk_ids).await?;
    let retention_compaction = run_retention_compaction_check(
        &store,
        &tenant,
        &durable_chunk_ids,
        &expired_chunk_ids,
        pre_compaction_durable_retrieval_hit,
    )
    .await?;

    let report_path = write_report(
        &project_dir,
        &attempts,
        rejection_or_downgrade_rate,
        duplicate_reuse_rate,
        total_chunks_delta,
        active_chunks_delta,
        disk_bytes_delta,
        &retention_compaction,
        ephemeral_hidden_from_default_search,
        progress_summary_ttl_applied,
    )?;

    let mut failures = expected_mismatches;
    if rejection_or_downgrade_rate + f64::EPSILON < min_rejection_or_downgrade_rate {
        failures.push(format!(
            "rejection_or_downgrade_rate {:.3} below threshold {:.3}",
            rejection_or_downgrade_rate, min_rejection_or_downgrade_rate
        ));
    }
    if duplicate_reuse_rate + f64::EPSILON < min_duplicate_reuse_rate {
        failures.push(format!(
            "duplicate_reuse_rate {:.3} below threshold {:.3}",
            duplicate_reuse_rate, min_duplicate_reuse_rate
        ));
    }
    if total_chunks_delta > max_total_chunks {
        failures.push(format!(
            "total_chunks_delta {total_chunks_delta} above maximum {max_total_chunks}"
        ));
    }
    if disk_bytes_delta > max_disk_bytes {
        failures.push(format!(
            "disk_bytes_delta {disk_bytes_delta} above maximum {max_disk_bytes}"
        ));
    }
    if !retention_compaction.pre_compaction_durable_retrieval_hit {
        failures
            .push("durable synthetic chunks were not retrievable before compaction".to_string());
    }
    if options.require_retention_compaction && retention_compaction.expired_count == 0 {
        failures.push("retention compaction expired zero synthetic chunks".to_string());
    }
    if !retention_compaction.post_compaction_durable_retrieval_hit {
        failures.push(
            "durable synthetic chunks were not retrievable after retention compaction".to_string(),
        );
    }
    if !retention_compaction.expired_chunk_hidden_after_compaction {
        failures.push(
            "expired synthetic trace appeared in default search after compaction".to_string(),
        );
    }
    if !ephemeral_hidden_from_default_search {
        failures.push("ephemeral synthetic progress appeared in default search".to_string());
    }
    if !progress_summary_ttl_applied {
        failures
            .push("ordinary synthetic progress summary did not receive default TTL".to_string());
    }

    let payload = json!({
        "passed": failures.is_empty(),
        "attempts": attempts.len(),
        "low_value_attempts": low_value_attempts,
        "rejected_or_downgraded": rejected_or_downgraded,
        "rejection_or_downgrade_rate": rejection_or_downgrade_rate,
        "duplicate_attempts": duplicate_attempts,
        "duplicate_reused": duplicate_reused,
        "duplicate_reuse_rate": duplicate_reuse_rate,
        "storage": {
            "total_chunks_delta": total_chunks_delta,
            "active_chunks_delta": active_chunks_delta,
            "disk_bytes_delta": disk_bytes_delta,
            "max_total_chunks": max_total_chunks,
            "max_disk_bytes": max_disk_bytes,
        },
        "retrieval": {
            "durable_retrieval_hit": retention_compaction.pre_compaction_durable_retrieval_hit,
            "post_compaction_durable_retrieval_hit": retention_compaction.post_compaction_durable_retrieval_hit,
            "ephemeral_hidden_from_default_search": ephemeral_hidden_from_default_search,
            "expired_chunk_hidden_after_compaction": retention_compaction.expired_chunk_hidden_after_compaction,
            "expired_chunk_status_after_compaction": retention_compaction.expired_chunk_status_after_compaction,
        },
        "retention": {
            "progress_summary_ttl_applied": progress_summary_ttl_applied,
        },
        "compaction": {
            "expired_count": retention_compaction.expired_count,
            "promoted_count": retention_compaction.promoted_count,
            "required": options.require_retention_compaction,
        },
        "thresholds": {
            "min_rejection_or_downgrade_rate": min_rejection_or_downgrade_rate,
            "min_duplicate_reuse_rate": min_duplicate_reuse_rate,
            "max_total_chunks": max_total_chunks,
            "max_disk_bytes": max_disk_bytes,
            "require_retention_compaction": options.require_retention_compaction,
        },
        "report": report_path,
        "failures": failures,
    });

    if !failures.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "write-quality eval thresholds failed: {}",
            serde_json::to_string(&payload)?
        )));
    }

    Ok(payload)
}

async fn run_write(store: &PersistentStore, write: SyntheticWrite) -> Result<AttemptReport> {
    match handle_memory_add(store, None, write.params).await {
        Ok(value) => {
            let payload = unwrap_content_payload(value)?;
            let admission_decision = payload
                .get("admission_decision")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let dedupe_decision = payload
                .get("dedupe_decision")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let lifecycle_tier = payload
                .get("lifecycle_tier")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let expires_at_ms = payload.get("expires_at_ms").and_then(Value::as_i64);
            let review_after_ms = payload.get("review_after_ms").and_then(Value::as_i64);
            let chunk_id = payload
                .get("chunk_id")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let outcome = if dedupe_decision.as_deref() == Some("reused_existing_exact_content") {
                "duplicate_reused"
            } else if admission_decision.as_deref() == Some("ephemeral") {
                "ephemeral"
            } else {
                "durable"
            };
            Ok(AttemptReport {
                label: write.label,
                expected: write.expected.as_str(),
                outcome: outcome.to_string(),
                chunk_id,
                admission_decision,
                admission_reason: payload
                    .get("admission_reason")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                dedupe_decision,
                lifecycle_tier,
                expires_at_ms,
                review_after_ms,
                error: None,
            })
        }
        Err(err) => Ok(AttemptReport {
            label: write.label,
            expected: write.expected.as_str(),
            outcome: "rejected".to_string(),
            chunk_id: None,
            admission_decision: Some("reject".to_string()),
            admission_reason: Some(err.message().to_string()),
            dedupe_decision: None,
            lifecycle_tier: None,
            expires_at_ms: None,
            review_after_ms: None,
            error: Some(err.to_string()),
        }),
    }
}

async fn durable_retrieval_check(
    store: &PersistentStore,
    tenant: &TenantId,
    durable_chunk_ids: &[String],
) -> Result<bool> {
    let expected = durable_chunk_ids
        .first()
        .ok_or_else(|| MemdError::ValidationError("no durable synthetic chunk IDs".to_string()))?;
    let results =
        search_chunk_ids(store, tenant, "synthetic write quality eval durable", 5).await?;
    Ok(results.iter().any(|chunk_id| chunk_id == expected))
}

async fn ephemeral_hidden_check(
    store: &PersistentStore,
    tenant: &TenantId,
    ephemeral_chunk_ids: &[String],
) -> Result<bool> {
    let Some(ephemeral_id) = ephemeral_chunk_ids.first() else {
        return Ok(false);
    };
    let results = search_chunk_ids(store, tenant, "starting to inspect the files", 10).await?;
    Ok(results.iter().all(|chunk_id| chunk_id != ephemeral_id))
}

async fn default_retention_check(
    store: &PersistentStore,
    tenant: &TenantId,
    chunk_ids: &[String],
) -> Result<bool> {
    let Some(chunk_id) = chunk_ids.first() else {
        return Ok(false);
    };
    let chunk_id = ChunkId::parse(chunk_id)?;
    let Some(resolved) = store.get_with_lifecycle(tenant, &chunk_id).await? else {
        return Ok(false);
    };
    Ok(resolved.lifecycle.tier == MemoryTier::LongTerm
        && resolved.lifecycle.expires_at_ms.is_some()
        && resolved.lifecycle.review_after_ms == resolved.lifecycle.expires_at_ms)
}

async fn run_retention_compaction_check(
    store: &PersistentStore,
    tenant: &TenantId,
    durable_chunk_ids: &[String],
    expired_chunk_ids: &[String],
    pre_compaction_durable_retrieval_hit: bool,
) -> Result<RetentionCompactionReport> {
    let runner = CompactionRunner::new(CompactionConfig::default());
    let embedder = Arc::new(MockEmbedder::new());
    let dense = DenseSearcher::with_embedder(
        embedder,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    );
    let result = runner.run_compaction(tenant, store.metadata(), &dense, None, None, None)?;
    let post_compaction_durable_retrieval_hit =
        durable_retrieval_check(store, tenant, durable_chunk_ids).await?;
    let expired_chunk_hidden_after_compaction =
        expired_hidden_check(store, tenant, expired_chunk_ids).await?;
    let expired_chunk_status_after_compaction =
        first_chunk_status(store, tenant, expired_chunk_ids).await?;
    Ok(RetentionCompactionReport {
        expired_count: result.expired_count,
        promoted_count: result.promoted_count,
        pre_compaction_durable_retrieval_hit,
        post_compaction_durable_retrieval_hit,
        expired_chunk_hidden_after_compaction,
        expired_chunk_status_after_compaction,
    })
}

async fn expired_hidden_check(
    store: &PersistentStore,
    tenant: &TenantId,
    expired_chunk_ids: &[String],
) -> Result<bool> {
    let Some(expired_id) = expired_chunk_ids.first() else {
        return Ok(false);
    };
    let results = search_chunk_ids(
        store,
        tenant,
        "obsolete synthetic run trace should expire",
        10,
    )
    .await?;
    Ok(results.iter().all(|chunk_id| chunk_id != expired_id))
}

async fn first_chunk_status(
    store: &PersistentStore,
    tenant: &TenantId,
    chunk_ids: &[String],
) -> Result<Option<String>> {
    let Some(chunk_id) = chunk_ids.first() else {
        return Ok(None);
    };
    let chunk_id = ChunkId::parse(chunk_id)?;
    Ok(store
        .get_with_lifecycle(tenant, &chunk_id)
        .await?
        .map(|resolved| status_name(resolved.status).to_string()))
}

async fn search_chunk_ids(
    store: &PersistentStore,
    tenant: &TenantId,
    query: &str,
    k: usize,
) -> Result<Vec<String>> {
    let payload = cli_search_payload_silent(
        store,
        tenant.as_str().to_string(),
        Some(SYNTHETIC_PROJECT_ID.to_string()),
        query.to_string(),
        k,
        true,
        Some(4000),
        CliQueryMode::Generic,
        true,
        false,
        false,
    )
    .await?;
    Ok(payload
        .get("results")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("chunk_id").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default())
}

fn synthetic_writes() -> Vec<SyntheticWrite> {
    let project_id = Some(SYNTHETIC_PROJECT_ID.to_string());
    vec![
        SyntheticWrite {
            label: "low_signal_document_progress",
            expected: ExpectedOutcome::Reject,
            params: add_params(
                "starting to inspect the files",
                "summary",
                project_id.clone(),
                vec!["kind:progress"],
                None,
            ),
        },
        SyntheticWrite {
            label: "low_signal_conversation_progress",
            expected: ExpectedOutcome::Ephemeral,
            params: add_params(
                "starting to inspect the files",
                "summary",
                project_id.clone(),
                vec!["kind:progress"],
                Some("conversation"),
            ),
        },
        SyntheticWrite {
            label: "generated_digest_wrapper",
            expected: ExpectedOutcome::Reject,
            params: add_params(
                "Task digest status generated. Summary: Highlight library for synthetic_session contains 0 ranked lessons.",
                "summary",
                project_id.clone(),
                vec!["task:status:generated", "task:role:highlight_library"],
                None,
            ),
        },
        SyntheticWrite {
            label: "ordinary_progress_summary_ttl",
            expected: ExpectedOutcome::Durable,
            params: add_params(
                "Mapped synthetic eval touchpoints; next step is validating bounded progress retention.",
                "summary",
                project_id.clone(),
                vec!["kind:progress"],
                None,
            ),
        },
        SyntheticWrite {
            label: "durable_decision",
            expected: ExpectedOutcome::Durable,
            params: add_params(
                "Decision: keep synthetic write quality eval durable. Rationale: validated root cause and command evidence should survive startup context.",
                "decision",
                project_id.clone(),
                vec!["kind:decision"],
                None,
            ),
        },
        SyntheticWrite {
            label: "exact_duplicate_decision",
            expected: ExpectedOutcome::DuplicateReused,
            params: add_params(
                "Decision: keep synthetic write quality eval durable. Rationale: validated root cause and command evidence should survive startup context.",
                "decision",
                project_id.clone(),
                vec!["kind:decision"],
                None,
            ),
        },
        SyntheticWrite {
            label: "durable_path_evidence",
            expected: ExpectedOutcome::Durable,
            params: add_params(
                "Validation: command `cargo test -p memd eval_write_quality` passed. Path: crates/memd/src/cli/eval_write_quality.rs.",
                "summary",
                project_id.clone(),
                vec!["kind:evidence"],
                None,
            ),
        },
        SyntheticWrite {
            label: "ordinary_run_trace_ttl",
            expected: ExpectedOutcome::Durable,
            params: add_params(
                "Run result: synthetic write quality eval executed cargo test and passed threshold checks.",
                "trace",
                project_id.clone(),
                vec!["kind:run"],
                None,
            ),
        },
        SyntheticWrite {
            label: "expired_run_trace",
            expected: ExpectedOutcome::Durable,
            params: {
                let mut params = add_params(
                    "Run result: obsolete synthetic run trace should expire during retention compaction.",
                    "trace",
                    project_id,
                    vec!["kind:run"],
                    None,
                );
                params.expires_at_ms = Some(1);
                params
            },
        },
    ]
}

fn add_params(
    text: &str,
    chunk_type: &str,
    project_id: Option<String>,
    tags: Vec<&str>,
    mode: Option<&str>,
) -> AddParams {
    AddParams {
        tenant_id: "eval_write_quality".to_string(),
        text: text.to_string(),
        chunk_type: chunk_type.to_string(),
        project_id,
        tags: tags.into_iter().map(ToString::to_string).collect(),
        mode: mode.map(ToString::to_string),
        ..Default::default()
    }
}

fn expected_outcome_name(expected: ExpectedOutcome) -> &'static str {
    match expected {
        ExpectedOutcome::Reject => "rejected",
        ExpectedOutcome::Ephemeral => "ephemeral",
        ExpectedOutcome::Durable => "durable",
        ExpectedOutcome::DuplicateReused => "duplicate_reused",
    }
}

fn rate(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn status_name(status: ChunkStatus) -> &'static str {
    match status {
        ChunkStatus::Draft => "draft",
        ChunkStatus::Candidate => "candidate",
        ChunkStatus::Error => "error",
        ChunkStatus::Final => "final",
        ChunkStatus::Superseded => "superseded",
        ChunkStatus::Expired => "expired",
        ChunkStatus::Deleted => "deleted",
    }
}

// The report schema intentionally names each measured scalar at this boundary.
#[allow(clippy::too_many_arguments)]
fn write_report(
    project_dir: &Path,
    attempts: &[AttemptReport],
    rejection_or_downgrade_rate: f64,
    duplicate_reuse_rate: f64,
    total_chunks_delta: usize,
    active_chunks_delta: usize,
    disk_bytes_delta: u64,
    retention_compaction: &RetentionCompactionReport,
    ephemeral_hidden_from_default_search: bool,
    progress_summary_ttl_applied: bool,
) -> Result<PathBuf> {
    let reports_dir = project_dir.join(REPORTS_DIR);
    fs::create_dir_all(&reports_dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = reports_dir.join(format!("write_quality_{stamp}.json"));
    let attempt_reports = attempts
        .iter()
        .map(|attempt| {
            json!({
                "label": attempt.label,
                "expected": attempt.expected,
                "outcome": attempt.outcome,
                "chunk_id": attempt.chunk_id,
                "admission_decision": attempt.admission_decision,
                "admission_reason": attempt.admission_reason,
                "dedupe_decision": attempt.dedupe_decision,
                "lifecycle_tier": attempt.lifecycle_tier,
                "expires_at_ms": attempt.expires_at_ms,
                "review_after_ms": attempt.review_after_ms,
                "error": attempt.error,
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "attempts": attempt_reports,
            "rejection_or_downgrade_rate": rejection_or_downgrade_rate,
            "duplicate_reuse_rate": duplicate_reuse_rate,
            "storage": {
                "total_chunks_delta": total_chunks_delta,
                "active_chunks_delta": active_chunks_delta,
                "disk_bytes_delta": disk_bytes_delta,
            },
            "retrieval": {
                "durable_retrieval_hit": retention_compaction.pre_compaction_durable_retrieval_hit,
                "post_compaction_durable_retrieval_hit": retention_compaction.post_compaction_durable_retrieval_hit,
                "ephemeral_hidden_from_default_search": ephemeral_hidden_from_default_search,
                "expired_chunk_hidden_after_compaction": retention_compaction.expired_chunk_hidden_after_compaction,
                "expired_chunk_status_after_compaction": retention_compaction.expired_chunk_status_after_compaction,
            },
            "retention": {
                "progress_summary_ttl_applied": progress_summary_ttl_applied,
            },
            "compaction": {
                "expired_count": retention_compaction.expired_count,
                "promoted_count": retention_compaction.promoted_count,
            },
        }))?,
    )?;
    Ok(path)
}

fn dir_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        total = total.saturating_add(dir_size(&entry.path())?);
    }
    Ok(total)
}

struct TempStoreDir {
    path: PathBuf,
}

impl TempStoreDir {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("memd-eval-write-quality-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempStoreDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(project_dir: PathBuf) -> EvalWriteQualityOptions {
        EvalWriteQualityOptions {
            project_dir,
            min_rejection_or_downgrade_rate: 1.0,
            min_duplicate_reuse_rate: 1.0,
            max_total_chunks: 6,
            max_disk_bytes: 5_000_000,
            require_retention_compaction: true,
        }
    }

    #[tokio::test]
    async fn write_quality_eval_passes_synthetic_session() {
        let dir = tempfile::tempdir().unwrap();
        let payload = run_eval_write_quality(options(dir.path().to_path_buf()))
            .await
            .unwrap();
        assert_eq!(payload["passed"], true);
        assert_eq!(payload["rejection_or_downgrade_rate"], 1.0);
        assert_eq!(payload["duplicate_reuse_rate"], 1.0);
        assert_eq!(payload["storage"]["total_chunks_delta"], 6);
        assert_eq!(payload["retention"]["progress_summary_ttl_applied"], true);
        assert_eq!(payload["retrieval"]["durable_retrieval_hit"], true);
        assert_eq!(
            payload["retrieval"]["post_compaction_durable_retrieval_hit"],
            true
        );
        assert_eq!(
            payload["retrieval"]["expired_chunk_hidden_after_compaction"],
            true
        );
        assert_eq!(
            payload["retrieval"]["expired_chunk_status_after_compaction"],
            "expired"
        );
        assert_eq!(payload["compaction"]["expired_count"], 1);
        assert_eq!(
            payload["retrieval"]["ephemeral_hidden_from_default_search"],
            true
        );
    }

    #[tokio::test]
    async fn write_quality_eval_fails_when_storage_threshold_is_too_low() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = options(dir.path().to_path_buf());
        opts.max_total_chunks = 0;
        let err = run_eval_write_quality(opts).await.unwrap_err().to_string();
        assert!(err.contains("total_chunks_delta"), "{err}");
    }
}
