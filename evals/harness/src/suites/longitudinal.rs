//! Frozen repeated-task evaluation for admission, consolidation, exposure, and outcomes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use memd::consolidate::journal::{ConsolidationState, LineageRelation};
use memd::consolidate::prompt::ConsolidatedEntry;
use memd::consolidate::service::{
    execute_consolidation_with_identity, review_consolidation_run, ConsolidationReviewDecision,
};
use memd::consolidate::ConsolidatorIdentity;
use memd::ops::{handle_memory_search, SearchParams};
use memd::store::{
    OutcomeEvent, OutcomeKind, OutcomeVerifier, RankingPolicyMode, RetrievalEpisodeId,
    RetrievalEpisodeItem, Store,
};
use memd::types::{ChunkId, ChunkType, IngestionMode, MemoryChunk, ProjectId, TenantId};
use memd::write_service::{prepare_write, PrepareWriteRequest};
use memd::{PersistentStore, PersistentStoreConfig};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::TestResult;

const MAIN_PROJECT: &str = "longitudinal-main";
const FOREIGN_PROJECT: &str = "longitudinal-foreign";

#[derive(Debug, Clone)]
pub struct LongitudinalConfig {
    pub protocol_path: PathBuf,
    pub fixtures_path: PathBuf,
    pub output_root: PathBuf,
    pub crash_gate_evidence: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Protocol {
    schema_version: String,
    protocol_id: String,
    random_seed: u64,
    bootstrap: BootstrapConfig,
    retrieval: RetrievalConfig,
    timeline: TimelineConfig,
    exposure_compatibility: ExposureCompatibility,
    treatments: Vec<Treatment>,
    promotion_gates: PromotionGateConfig,
}

#[derive(Debug, Deserialize)]
struct BootstrapConfig {
    iterations: usize,
    confidence_level: f64,
}

#[derive(Debug, Deserialize)]
struct RetrievalConfig {
    k: usize,
    candidate_multiplier: usize,
    ranking_policy_version: String,
    production_serve_enabled: bool,
}

#[derive(Debug, Deserialize)]
struct TimelineConfig {
    pre_feedback_rounds: usize,
    post_feedback_rounds: usize,
    consolidation_after_round: usize,
}

#[derive(Debug, Deserialize)]
struct ExposureCompatibility {
    adjustment_per_prior_render: f32,
    max_prior_renders: usize,
    max_adjustment: f32,
    product_policy: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Treatment {
    id: String,
    admission: String,
    consolidation: String,
    ranking: String,
}

#[derive(Debug, Deserialize)]
struct PromotionGateConfig {
    full_loop_minus_raw_success_ci_lower_gt: f64,
    full_loop_stale_recurrence_lte_raw: bool,
    full_loop_minus_raw_harmful_rate_ci_upper_lte: f64,
    full_loop_recall_at_3_gte_raw: bool,
    full_loop_mrr_gte_raw: bool,
    scope_violation_count_eq: usize,
    crash_recovery_violation_count_eq: usize,
}

#[derive(Debug, Deserialize)]
struct Fixtures {
    schema_version: String,
    fixture_id: String,
    fact_template: FactTemplate,
    task_variants: Vec<String>,
    write_order: Vec<String>,
    clusters: Vec<FixtureCluster>,
}

#[derive(Debug, Deserialize)]
struct FactTemplate {
    correct: String,
    correction: String,
    stale: String,
    distractor: String,
}

#[derive(Debug, Clone, Deserialize)]
struct FixtureCluster {
    id: String,
    subsystem: String,
}

#[derive(Debug, Clone)]
struct ClusterMemory {
    fixture: FixtureCluster,
    correct_ids: HashSet<String>,
    harmful_ids: HashSet<String>,
    source_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TrialRow {
    treatment: String,
    cluster_id: String,
    round: usize,
    phase: String,
    query_hash: String,
    retrieval_episode_id: Option<String>,
    served_top_k: Vec<String>,
    shadow_top_k: Vec<String>,
    simulated_top_k: Vec<String>,
    selected_chunk_id: Option<String>,
    success: bool,
    harmful_memory: bool,
    recall_at_3: f64,
    reciprocal_rank: f64,
    rendered_bytes: usize,
    token_proxy: usize,
    latency_ms: f64,
    outcome_recorded: bool,
    attribution_missed: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ConsolidationCounters {
    staged: usize,
    promoted: usize,
    rejected: usize,
    rolled_back: usize,
    recoverable: usize,
    expected_sources: usize,
    covered_sources: usize,
    validated_candidates: usize,
    factual_candidates: usize,
    elapsed_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
struct TreatmentSummary {
    treatment: String,
    post_task_success: f64,
    verifier_pass_rate: f64,
    stale_error_recurrence: f64,
    harmful_memory_rate: f64,
    recall_at_3: f64,
    mrr: f64,
    served_shadow_change_rate: f64,
    mean_rendered_bytes: f64,
    mean_token_proxy: f64,
    mean_latency_ms: f64,
    store_growth_bytes: u64,
    active_chunks: usize,
    scope_violation_count: usize,
    consolidation_source_coverage: f64,
    consolidation_factuality: f64,
    consolidation: ConsolidationCounters,
    cluster_post_success: BTreeMap<String, f64>,
    cluster_harmful_rate: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize)]
struct ConfidenceInterval {
    mean: f64,
    lower: f64,
    upper: f64,
    unit_count: usize,
    iterations: usize,
    seed: u64,
}

#[derive(Debug, Clone, Serialize)]
struct GateDecision {
    id: String,
    passed: bool,
    observed: Value,
    threshold: Value,
}

#[derive(Debug, Serialize)]
struct RunSummary {
    schema_version: &'static str,
    run_id: String,
    protocol_id: String,
    fixture_id: String,
    treatment_summaries: Vec<TreatmentSummary>,
    full_minus_raw_success: ConfidenceInterval,
    full_minus_raw_harmful_rate: ConfidenceInterval,
    gates: Vec<GateDecision>,
    promotion_allowed: bool,
}

#[derive(Debug, Serialize)]
struct CounterfactualArtifact {
    schema_version: &'static str,
    run_id: String,
    rows: Vec<CounterfactualRow>,
}

#[derive(Debug, Clone, Serialize)]
struct CounterfactualRow {
    treatment: String,
    cluster_id: String,
    round: usize,
    retrieval_episode_id: String,
    served_top_k: Vec<String>,
    shadow_top_k: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RunManifest {
    schema_version: &'static str,
    run_id: String,
    generated_unix_ms: u128,
    exact_argv: Vec<String>,
    protocol_path: String,
    protocol_sha256: String,
    fixture_path: String,
    fixture_sha256: String,
    crash_gate_evidence_path: String,
    crash_gate_evidence_sha256: String,
    crash_gate_evidence_artifact: String,
    source_repository: String,
    source_commit: String,
    dirty_patch_sha256: String,
    binary_path: String,
    binary_sha256: String,
    evaluator_binary_path: String,
    evaluator_binary_sha256: String,
    cargo_lock_sha256: String,
    rustc_version: String,
    os: String,
    architecture: String,
    available_parallelism: usize,
    environment_allowlist: BTreeMap<String, String>,
    row_count: usize,
    treatment_count: usize,
    cluster_count: usize,
}

#[derive(Debug, Deserialize)]
struct CrashGateEvidence {
    test_count: usize,
    passed: usize,
    failed: usize,
    ignored: usize,
    real_sigkill_boundary_test: String,
    result: String,
}

struct SearchObservation {
    episode_id: String,
    items: Vec<RetrievalEpisodeItem>,
    served_top_k: Vec<String>,
    shadow_top_k: Vec<String>,
    latency_ms: f64,
}

pub fn run(memd_binary: &Path, config: LongitudinalConfig) -> Vec<TestResult> {
    let start = Instant::now();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return vec![TestResult::fail(
                "L1_longitudinal_policy_gate",
                &format!("create Tokio runtime: {error}"),
            )];
        }
    };
    match runtime.block_on(run_async(memd_binary, config)) {
        Ok((run_id, promotion_allowed)) if promotion_allowed => {
            vec![TestResult::pass_with_duration(
                &format!("L1_longitudinal_policy_gate:{run_id}"),
                start,
            )]
        }
        Ok((run_id, _)) => vec![TestResult::fail_with_duration(
            &format!("L1_longitudinal_policy_gate:{run_id}"),
            "one or more frozen promotion gates failed; outcome serving stays disabled",
            start,
        )],
        Err(error) => vec![TestResult::fail_with_duration(
            "L1_longitudinal_policy_gate",
            &error,
            start,
        )],
    }
}

async fn run_async(
    memd_binary: &Path,
    config: LongitudinalConfig,
) -> Result<(String, bool), String> {
    let protocol_bytes = fs::read(&config.protocol_path)
        .map_err(|error| format!("read protocol {}: {error}", config.protocol_path.display()))?;
    let fixture_bytes = fs::read(&config.fixtures_path)
        .map_err(|error| format!("read fixtures {}: {error}", config.fixtures_path.display()))?;
    let crash_bytes = fs::read(&config.crash_gate_evidence).map_err(|error| {
        format!(
            "read crash-gate evidence {}: {error}",
            config.crash_gate_evidence.display()
        )
    })?;
    let protocol: Protocol = serde_json::from_slice(&protocol_bytes)
        .map_err(|error| format!("parse longitudinal protocol: {error}"))?;
    let fixtures: Fixtures = serde_json::from_slice(&fixture_bytes)
        .map_err(|error| format!("parse longitudinal fixtures: {error}"))?;
    let crash_evidence: CrashGateEvidence = serde_json::from_slice(&crash_bytes)
        .map_err(|error| format!("parse crash-gate evidence: {error}"))?;
    validate_inputs(&protocol, &fixtures)?;
    validate_crash_evidence(&crash_evidence)?;

    let protocol_hash = sha256_bytes(&protocol_bytes);
    let run_id = format!(
        "{}-{}",
        now_ms(),
        &protocol_hash[..12.min(protocol_hash.len())]
    );
    let run_dir = config.output_root.join(&run_id);
    if run_dir.exists() {
        return Err(format!(
            "refusing to overwrite longitudinal run {}",
            run_dir.display()
        ));
    }
    fs::create_dir_all(&run_dir)
        .map_err(|error| format!("create run directory {}: {error}", run_dir.display()))?;

    let tenant = TenantId::new("longitudinal_v1").map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    let mut counterfactual_rows = Vec::new();
    let mut treatment_summaries = Vec::new();
    for treatment in &protocol.treatments {
        let result = run_treatment(&protocol, &fixtures, treatment, &tenant).await?;
        counterfactual_rows.extend(result.counterfactual_rows);
        rows.extend(result.rows);
        treatment_summaries.push(result.summary);
    }

    let raw = treatment_summaries
        .iter()
        .find(|summary| summary.treatment == "raw_memory")
        .ok_or_else(|| "protocol is missing raw_memory treatment".to_string())?;
    let full = treatment_summaries
        .iter()
        .find(|summary| summary.treatment == "full_loop")
        .ok_or_else(|| "protocol is missing full_loop treatment".to_string())?;
    let success_deltas =
        paired_cluster_deltas(&raw.cluster_post_success, &full.cluster_post_success)?;
    let harmful_deltas =
        paired_cluster_deltas(&raw.cluster_harmful_rate, &full.cluster_harmful_rate)?;
    let success_ci = seeded_bootstrap_ci(
        &success_deltas,
        protocol.bootstrap.iterations,
        protocol.random_seed,
        protocol.bootstrap.confidence_level,
    );
    let harmful_ci = seeded_bootstrap_ci(
        &harmful_deltas,
        protocol.bootstrap.iterations,
        protocol.random_seed.wrapping_add(1),
        protocol.bootstrap.confidence_level,
    );
    let scope_violations = treatment_summaries
        .iter()
        .map(|summary| summary.scope_violation_count)
        .sum::<usize>();
    let gates = evaluate_gates(
        &protocol.promotion_gates,
        raw,
        full,
        &success_ci,
        &harmful_ci,
        scope_violations,
        0,
    );
    let promotion_allowed = gates.iter().all(|gate| gate.passed);
    let summary = RunSummary {
        schema_version: "memd.longitudinal.summary.v1",
        run_id: run_id.clone(),
        protocol_id: protocol.protocol_id.clone(),
        fixture_id: fixtures.fixture_id.clone(),
        treatment_summaries,
        full_minus_raw_success: success_ci,
        full_minus_raw_harmful_rate: harmful_ci,
        gates,
        promotion_allowed,
    };

    let rows_path = run_dir.join(format!("rows.{run_id}.jsonl"));
    let mut rows_text = String::new();
    for row in &rows {
        rows_text.push_str(&serde_json::to_string(row).map_err(|error| error.to_string())?);
        rows_text.push('\n');
    }
    fs::write(&rows_path, rows_text).map_err(|error| error.to_string())?;
    let summary_path = run_dir.join(format!("summary.{run_id}.json"));
    write_pretty_json(&summary_path, &summary)?;
    let counterfactual_path = run_dir.join(format!("counterfactual.{run_id}.json"));
    write_pretty_json(
        &counterfactual_path,
        &CounterfactualArtifact {
            schema_version: "memd.longitudinal.counterfactual.v1",
            run_id: run_id.clone(),
            rows: counterfactual_rows,
        },
    )?;
    let crash_artifact_name = format!("crash-gate-evidence.{run_id}.json");
    fs::write(run_dir.join(&crash_artifact_name), &crash_bytes)
        .map_err(|error| format!("copy crash-gate evidence: {error}"))?;

    let manifest = build_manifest(
        memd_binary,
        &config,
        &protocol,
        &fixtures,
        &run_id,
        &protocol_hash,
        &sha256_bytes(&fixture_bytes),
        &sha256_bytes(&crash_bytes),
        &crash_artifact_name,
        rows.len(),
    )?;
    let manifest_path = run_dir.join(format!("manifest.{run_id}.json"));
    write_pretty_json(&manifest_path, &manifest)?;
    let inventory_path = run_dir.join(format!("inventory.{run_id}.sha256"));
    write_inventory(&run_dir, &inventory_path)?;
    Ok((run_id, promotion_allowed))
}

struct TreatmentResult {
    rows: Vec<TrialRow>,
    counterfactual_rows: Vec<CounterfactualRow>,
    summary: TreatmentSummary,
}

async fn run_treatment(
    protocol: &Protocol,
    fixtures: &Fixtures,
    treatment: &Treatment,
    tenant: &TenantId,
) -> Result<TreatmentResult, String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let data_dir = temp.path().join("store");
    let store = PersistentStore::open(PersistentStoreConfig {
        data_dir: data_dir.clone(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    let bytes_before = directory_bytes(&data_dir)?;
    let mut memories = Vec::new();
    let mut timestamp = now_ms() as i64 - 10_000_000;
    if treatment.id != "no_memory" {
        for fixture in &fixtures.clusters {
            memories.push(
                seed_cluster(
                    &store,
                    tenant,
                    fixture,
                    fixtures,
                    treatment.admission == "shared_write_service",
                    &mut timestamp,
                )
                .await?,
            );
        }
    } else {
        memories.extend(
            fixtures
                .clusters
                .iter()
                .cloned()
                .map(|fixture| ClusterMemory {
                    fixture,
                    correct_ids: HashSet::new(),
                    harmful_ids: HashSet::new(),
                    source_ids: Vec::new(),
                }),
        );
    }
    seed_foreign_scope_probe(&store, tenant, &mut timestamp).await?;

    let mut rows = Vec::new();
    let mut counterfactual_rows = Vec::new();
    let mut exposure_counts = HashMap::<String, usize>::new();
    let mut consolidation = ConsolidationCounters::default();
    let total_rounds =
        protocol.timeline.pre_feedback_rounds + protocol.timeline.post_feedback_rounds;
    for round in 0..total_rounds {
        for memory in &mut memories {
            let query = render_template(
                &fixtures.task_variants[round % fixtures.task_variants.len()],
                &memory.fixture.subsystem,
            );
            let phase = if round < protocol.timeline.pre_feedback_rounds {
                "pre_feedback"
            } else {
                "post_feedback"
            };
            if treatment.id == "no_memory" {
                rows.push(TrialRow {
                    treatment: treatment.id.clone(),
                    cluster_id: memory.fixture.id.clone(),
                    round,
                    phase: phase.to_string(),
                    query_hash: memd::store::stable_query_hash(&query),
                    retrieval_episode_id: None,
                    served_top_k: Vec::new(),
                    shadow_top_k: Vec::new(),
                    simulated_top_k: Vec::new(),
                    selected_chunk_id: None,
                    success: false,
                    harmful_memory: false,
                    recall_at_3: 0.0,
                    reciprocal_rank: 0.0,
                    rendered_bytes: 0,
                    token_proxy: 0,
                    latency_ms: 0.0,
                    outcome_recorded: false,
                    attribution_missed: false,
                });
                continue;
            }

            let observation = search_episode(&store, tenant, &query, protocol).await?;
            let simulated_top_k = match treatment.ranking.as_str() {
                "shadow_replay" => observation.shadow_top_k.clone(),
                "offline_rendered_count_compatibility" => exposure_ranked_ids(
                    &observation.items,
                    &exposure_counts,
                    protocol.retrieval.k,
                    &protocol.exposure_compatibility,
                ),
                _ => observation.served_top_k.clone(),
            };
            for chunk_id in &simulated_top_k {
                *exposure_counts.entry(chunk_id.clone()).or_default() += 1;
            }
            let selected = simulated_top_k.iter().find(|chunk_id| {
                memory.correct_ids.contains(*chunk_id) || memory.harmful_ids.contains(*chunk_id)
            });
            let selected_chunk_id = selected.cloned();
            let success = selected.is_some_and(|id| memory.correct_ids.contains(id));
            let harmful_memory = selected.is_some_and(|id| memory.harmful_ids.contains(id));
            let recall_at_3 = if simulated_top_k
                .iter()
                .any(|id| memory.correct_ids.contains(id))
            {
                1.0
            } else {
                0.0
            };
            let reciprocal_rank = simulated_top_k
                .iter()
                .position(|id| memory.correct_ids.contains(id))
                .map(|rank| 1.0 / (rank + 1) as f64)
                .unwrap_or(0.0);
            let rendered_bytes = rendered_bytes(&store, tenant, &simulated_top_k).await?;
            let mut outcome_recorded = false;
            let mut attribution_missed = false;
            if treatment.ranking == "shadow_replay" {
                if let Some(selected_id) = selected_chunk_id.as_deref() {
                    let rendered = observation
                        .items
                        .iter()
                        .any(|item| item.chunk_id.to_string() == selected_id && item.rendered);
                    if rendered {
                        let episode_id = RetrievalEpisodeId::parse(&observation.episode_id)
                            .map_err(|error| error.to_string())?;
                        let chunk_id =
                            ChunkId::parse(selected_id).map_err(|error| error.to_string())?;
                        let event = if success {
                            OutcomeEvent::new(
                                episode_id,
                                OutcomeKind::Passed,
                                OutcomeVerifier::AutomatedTest,
                                vec![chunk_id],
                                Vec::new(),
                                Some(format!(
                                    "longitudinal:{}:{}:{round}:pass",
                                    treatment.id, memory.fixture.id
                                )),
                                now_ms() as i64,
                            )
                        } else if harmful_memory {
                            OutcomeEvent::new(
                                episode_id,
                                OutcomeKind::Failed,
                                OutcomeVerifier::AutomatedTest,
                                Vec::new(),
                                vec![chunk_id],
                                Some(format!(
                                    "longitudinal:{}:{}:{round}:fail",
                                    treatment.id, memory.fixture.id
                                )),
                                now_ms() as i64,
                            )
                        } else {
                            OutcomeEvent::new(
                                episode_id,
                                OutcomeKind::Abandoned,
                                OutcomeVerifier::AutomatedTest,
                                Vec::new(),
                                Vec::new(),
                                Some(format!(
                                    "longitudinal:{}:{}:{round}:unattributed",
                                    treatment.id, memory.fixture.id
                                )),
                                now_ms() as i64,
                            )
                        };
                        store
                            .record_outcome(tenant, event)
                            .await
                            .map_err(|error| error.to_string())?;
                        outcome_recorded = true;
                    } else {
                        attribution_missed = true;
                    }
                }
            }
            if observation.served_top_k != observation.shadow_top_k {
                counterfactual_rows.push(CounterfactualRow {
                    treatment: treatment.id.clone(),
                    cluster_id: memory.fixture.id.clone(),
                    round,
                    retrieval_episode_id: observation.episode_id.clone(),
                    served_top_k: observation.served_top_k.clone(),
                    shadow_top_k: observation.shadow_top_k.clone(),
                });
            }
            rows.push(TrialRow {
                treatment: treatment.id.clone(),
                cluster_id: memory.fixture.id.clone(),
                round,
                phase: phase.to_string(),
                query_hash: memd::store::stable_query_hash(&query),
                retrieval_episode_id: Some(observation.episode_id),
                served_top_k: observation.served_top_k,
                shadow_top_k: observation.shadow_top_k,
                simulated_top_k,
                selected_chunk_id,
                success,
                harmful_memory,
                recall_at_3,
                reciprocal_rank,
                rendered_bytes,
                token_proxy: rendered_bytes.div_ceil(4),
                latency_ms: observation.latency_ms,
                outcome_recorded,
                attribution_missed,
            });
        }

        if round + 1 == protocol.timeline.consolidation_after_round
            && treatment.consolidation == "stage_validate_explicit_promote"
        {
            for memory in &mut memories {
                consolidate_cluster(&store, tenant, memory, &mut consolidation).await?;
            }
        }
    }

    let scope_violation_count = scope_probe(&store, tenant, protocol).await?;
    let bytes_after = directory_bytes(&data_dir)?;
    let stats = store
        .stats(tenant)
        .await
        .map_err(|error| error.to_string())?;
    let summary = summarize_treatment(
        treatment,
        &rows,
        bytes_after.saturating_sub(bytes_before),
        stats.active_chunks,
        scope_violation_count,
        consolidation,
    );
    Ok(TreatmentResult {
        rows,
        counterfactual_rows,
        summary,
    })
}

async fn seed_cluster(
    store: &PersistentStore,
    tenant: &TenantId,
    fixture: &FixtureCluster,
    fixtures: &Fixtures,
    admission: bool,
    timestamp: &mut i64,
) -> Result<ClusterMemory, String> {
    let mut correct_ids = HashSet::new();
    let mut harmful_ids = HashSet::new();
    let mut source_ids = Vec::new();
    for kind in &fixtures.write_order {
        let (template, tags, correct, harmful, chunk_type) = match kind.as_str() {
            "correct" => (
                &fixtures.fact_template.correct,
                vec!["kind:decision", "state:correct"],
                true,
                false,
                ChunkType::Decision,
            ),
            "correction" => (
                &fixtures.fact_template.correction,
                vec!["kind:evidence", "state:correction"],
                true,
                false,
                ChunkType::Summary,
            ),
            "stale" => (
                &fixtures.fact_template.stale,
                vec!["kind:decision", "state:stale"],
                false,
                true,
                ChunkType::Decision,
            ),
            "distractor" => (
                &fixtures.fact_template.distractor,
                vec!["kind:evidence", "state:distractor"],
                false,
                false,
                ChunkType::Summary,
            ),
            other => return Err(format!("unsupported fixture write kind {other}")),
        };
        *timestamp += 1;
        let mut tags = tags.into_iter().map(str::to_string).collect::<Vec<_>>();
        tags.push(format!("topic:{}", fixture.id));
        let text = render_template(template, &fixture.subsystem);
        let chunk_id = add_fixture_chunk(
            store,
            tenant,
            MAIN_PROJECT,
            text,
            chunk_type,
            tags,
            admission,
            *timestamp,
        )
        .await?;
        if correct {
            correct_ids.insert(chunk_id.clone());
        }
        if harmful {
            harmful_ids.insert(chunk_id.clone());
        }
        source_ids.push(chunk_id);
    }

    *timestamp += 1;
    let _ = add_fixture_chunk(
        store,
        tenant,
        MAIN_PROJECT,
        format!("starting to inspect files for {}", fixture.subsystem),
        ChunkType::Summary,
        vec!["kind:progress".to_string(), format!("topic:{}", fixture.id)],
        admission,
        *timestamp,
    )
    .await;

    Ok(ClusterMemory {
        fixture: fixture.clone(),
        correct_ids,
        harmful_ids,
        source_ids,
    })
}

#[allow(clippy::too_many_arguments)]
async fn add_fixture_chunk(
    store: &PersistentStore,
    tenant: &TenantId,
    project: &str,
    text: String,
    chunk_type: ChunkType,
    tags: Vec<String>,
    admission: bool,
    timestamp: i64,
) -> Result<String, String> {
    let mut chunk = if admission {
        let prepared = prepare_write(PrepareWriteRequest {
            chunk_type,
            text: &text,
            tags: &tags,
            ingestion_mode: IngestionMode::Conversation,
            expires_at_ms: None,
            review_after_ms: None,
        });
        if prepared.is_rejected() {
            return Err(format!(
                "fixture write rejected: {}",
                prepared.outcome.reason
            ));
        }
        prepared.apply_to_chunk(MemoryChunk::new(tenant.clone(), text, chunk_type))
    } else {
        MemoryChunk::new(tenant.clone(), text, chunk_type).with_tags(tags)
    };
    chunk = chunk.with_project(ProjectId::from(project));
    chunk.timestamp_created = timestamp;
    store
        .add(chunk)
        .await
        .map(|id| id.to_string())
        .map_err(|error| error.to_string())
}

async fn consolidate_cluster(
    store: &PersistentStore,
    tenant: &TenantId,
    memory: &mut ClusterMemory,
    counters: &mut ConsolidationCounters,
) -> Result<(), String> {
    let start = Instant::now();
    let text = format!(
        "For {}, cache keys must use tenant scope.",
        memory.fixture.subsystem
    );
    let entry = ConsolidatedEntry {
        text: text.clone(),
        supersedes: memory.source_ids.clone(),
        agent_action: format!(
            "Use tenant-scoped cache keys when changing {}.",
            memory.fixture.subsystem
        ),
        evidence: memory.source_ids.clone(),
        confidence: 1.0,
        priority: 7,
    };
    counters.expected_sources += memory.source_ids.len();
    counters.covered_sources += entry.evidence.len();
    let raw_response = serde_json::json!([{
        "text": entry.text,
        "agent_action": entry.agent_action,
        "evidence": entry.evidence,
        "supersedes": entry.supersedes,
        "kind": "consolidated",
        "confidence": entry.confidence,
        "priority": entry.priority
    }])
    .to_string();
    let execution = execute_consolidation_with_identity(
        store,
        tenant,
        Some(MAIN_PROJECT),
        std::slice::from_ref(&entry),
        LineageRelation::Supersedes,
        &ConsolidatorIdentity {
            adapter: "longitudinal-deterministic".to_string(),
            command: Some("internal-evaluator".to_string()),
            model: Some("fixture-oracle-v1".to_string()),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        },
        &[format!("topic:{}", memory.fixture.id)],
        "frozen-longitudinal-prompt-v1",
        &raw_response,
        false,
    )
    .await
    .map_err(|error| error.to_string())?;
    if execution.state != ConsolidationState::Validated {
        return Err(format!(
            "expected staged consolidation, got {}",
            execution.state
        ));
    }
    counters.staged += execution.candidate_chunk_ids.len();
    let promoted = review_consolidation_run(
        store,
        &execution.run_id,
        ConsolidationReviewDecision::Accept,
    )
    .await
    .map_err(|error| error.to_string())?;
    match promoted.state {
        ConsolidationState::Committed => {
            counters.promoted += promoted.candidate_chunk_ids.len();
            for candidate_id in &promoted.candidate_chunk_ids {
                counters.validated_candidates += 1;
                let candidate = store
                    .get(tenant, candidate_id)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("promoted candidate {candidate_id} is unreadable"))?;
                let lowered = candidate.text.to_ascii_lowercase();
                if lowered.contains(&memory.fixture.subsystem.to_ascii_lowercase())
                    && lowered.contains("tenant scope")
                    && !lowered.contains("must use process scope")
                {
                    counters.factual_candidates += 1;
                }
            }
            memory
                .correct_ids
                .extend(promoted.candidate_chunk_ids.iter().map(ToString::to_string));
        }
        ConsolidationState::Rejected => counters.rejected += 1,
        ConsolidationState::RolledBack => counters.rolled_back += 1,
        ConsolidationState::FailedRecoverable => counters.recoverable += 1,
        state => return Err(format!("unexpected consolidation state {state}")),
    }
    counters.elapsed_ms += start.elapsed().as_secs_f64() * 1000.0;
    Ok(())
}

async fn search_episode(
    store: &PersistentStore,
    tenant: &TenantId,
    query: &str,
    protocol: &Protocol,
) -> Result<SearchObservation, String> {
    let start = Instant::now();
    let response = handle_memory_search(
        store,
        SearchParams {
            tenant_id: tenant.to_string(),
            project_id: Some(MAIN_PROJECT.to_string()),
            query: query.to_string(),
            k: protocol.retrieval.k,
            dedupe_by_source: true,
            ranking_policy: Some(RankingPolicyMode::Shadow),
            candidate_multiplier: Some(protocol.retrieval.candidate_multiplier),
            task_id: Some("longitudinal-eval".to_string()),
            suppress_usage_event: true,
            ..Default::default()
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
    let payload = unwrap_operation_payload(response)?;
    let episode_id = payload
        .get("retrieval_episode_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "search returned no retrieval episode".to_string())?
        .to_string();
    let parsed = RetrievalEpisodeId::parse(&episode_id).map_err(|error| error.to_string())?;
    let (_, items) = store
        .get_retrieval_episode(tenant, &parsed)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("retrieval episode {episode_id} disappeared"))?;
    let served_top_k = ranked_ids(&items, protocol.retrieval.k, RankKind::Served);
    let shadow_top_k = ranked_ids(&items, protocol.retrieval.k, RankKind::Shadow);
    Ok(SearchObservation {
        episode_id,
        items,
        served_top_k,
        shadow_top_k,
        latency_ms,
    })
}

enum RankKind {
    Served,
    Shadow,
}

fn ranked_ids(items: &[RetrievalEpisodeItem], k: usize, kind: RankKind) -> Vec<String> {
    let mut ranked = items
        .iter()
        .filter_map(|item| {
            let rank = match kind {
                RankKind::Served => item.served_rank,
                RankKind::Shadow => item.shadow_rank,
            }?;
            Some((rank, item))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(rank, _)| *rank);
    source_dedup_ids(ranked.into_iter().map(|(_, item)| item), k)
}

fn exposure_ranked_ids(
    items: &[RetrievalEpisodeItem],
    exposure_counts: &HashMap<String, usize>,
    k: usize,
    config: &ExposureCompatibility,
) -> Vec<String> {
    let mut ranked = items.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        let adjustment = |item: &RetrievalEpisodeItem| {
            let count = exposure_counts
                .get(&item.chunk_id.to_string())
                .copied()
                .unwrap_or(0)
                .min(config.max_prior_renders);
            (count as f32 * config.adjustment_per_prior_render).min(config.max_adjustment)
        };
        (right.original_score + adjustment(right))
            .partial_cmp(&(left.original_score + adjustment(left)))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.original_rank.cmp(&right.original_rank))
    });
    source_dedup_ids(ranked.into_iter(), k)
}

fn source_dedup_ids<'a>(
    items: impl Iterator<Item = &'a RetrievalEpisodeItem>,
    k: usize,
) -> Vec<String> {
    let mut groups = HashSet::new();
    let mut ids = Vec::new();
    for item in items {
        if let Some(group) = item.source_dedup_group.as_deref() {
            if !groups.insert(group) {
                continue;
            }
        }
        ids.push(item.chunk_id.to_string());
        if ids.len() >= k {
            break;
        }
    }
    ids
}

async fn rendered_bytes(
    store: &PersistentStore,
    tenant: &TenantId,
    ids: &[String],
) -> Result<usize, String> {
    let mut bytes = 0usize;
    for id in ids {
        let id = ChunkId::parse(id).map_err(|error| error.to_string())?;
        if let Some(chunk) = store
            .get(tenant, &id)
            .await
            .map_err(|error| error.to_string())?
        {
            bytes = bytes.saturating_add(chunk.text.len());
        }
    }
    Ok(bytes)
}

async fn seed_foreign_scope_probe(
    store: &PersistentStore,
    tenant: &TenantId,
    timestamp: &mut i64,
) -> Result<(), String> {
    *timestamp += 1;
    add_fixture_chunk(
        store,
        tenant,
        FOREIGN_PROJECT,
        "foreign-scope-sentinel cache keys use planet scope".to_string(),
        ChunkType::Decision,
        vec!["kind:decision".to_string()],
        false,
        *timestamp,
    )
    .await
    .map(|_| ())
}

async fn scope_probe(
    store: &PersistentStore,
    tenant: &TenantId,
    protocol: &Protocol,
) -> Result<usize, String> {
    let observation = search_episode(
        store,
        tenant,
        "foreign-scope-sentinel planet scope",
        protocol,
    )
    .await?;
    let mut violations = 0;
    for id in observation.served_top_k {
        let id = ChunkId::parse(&id).map_err(|error| error.to_string())?;
        if let Some(chunk) = store
            .get(tenant, &id)
            .await
            .map_err(|error| error.to_string())?
        {
            if chunk.project_id.as_option() == Some(FOREIGN_PROJECT) {
                violations += 1;
            }
        }
    }
    Ok(violations)
}

fn summarize_treatment(
    treatment: &Treatment,
    rows: &[TrialRow],
    store_growth_bytes: u64,
    active_chunks: usize,
    scope_violation_count: usize,
    consolidation: ConsolidationCounters,
) -> TreatmentSummary {
    let post = rows
        .iter()
        .filter(|row| row.phase == "post_feedback")
        .collect::<Vec<_>>();
    let mean = |values: Vec<f64>| {
        if values.is_empty() {
            0.0
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        }
    };
    let mut by_cluster = BTreeMap::<String, Vec<&TrialRow>>::new();
    for row in &post {
        by_cluster
            .entry(row.cluster_id.clone())
            .or_default()
            .push(row);
    }
    let cluster_post_success = by_cluster
        .iter()
        .map(|(id, rows)| {
            (
                id.clone(),
                mean(rows.iter().map(|row| row.success as u8 as f64).collect()),
            )
        })
        .collect();
    let cluster_harmful_rate = by_cluster
        .iter()
        .map(|(id, rows)| {
            (
                id.clone(),
                mean(
                    rows.iter()
                        .map(|row| row.harmful_memory as u8 as f64)
                        .collect(),
                ),
            )
        })
        .collect();
    let source_coverage = if consolidation.expected_sources == 0 {
        0.0
    } else {
        consolidation.covered_sources as f64 / consolidation.expected_sources as f64
    };
    let consolidation_factuality = if consolidation.validated_candidates == 0 {
        0.0
    } else {
        consolidation.factual_candidates as f64 / consolidation.validated_candidates as f64
    };
    TreatmentSummary {
        treatment: treatment.id.clone(),
        post_task_success: mean(post.iter().map(|row| row.success as u8 as f64).collect()),
        verifier_pass_rate: mean(post.iter().map(|row| row.success as u8 as f64).collect()),
        stale_error_recurrence: mean(
            post.iter()
                .map(|row| row.harmful_memory as u8 as f64)
                .collect(),
        ),
        harmful_memory_rate: mean(
            post.iter()
                .map(|row| row.harmful_memory as u8 as f64)
                .collect(),
        ),
        recall_at_3: mean(post.iter().map(|row| row.recall_at_3).collect()),
        mrr: mean(post.iter().map(|row| row.reciprocal_rank).collect()),
        served_shadow_change_rate: mean(
            post.iter()
                .map(|row| (row.served_top_k != row.shadow_top_k) as u8 as f64)
                .collect(),
        ),
        mean_rendered_bytes: mean(post.iter().map(|row| row.rendered_bytes as f64).collect()),
        mean_token_proxy: mean(post.iter().map(|row| row.token_proxy as f64).collect()),
        mean_latency_ms: mean(post.iter().map(|row| row.latency_ms).collect()),
        store_growth_bytes,
        active_chunks,
        scope_violation_count,
        consolidation_source_coverage: source_coverage,
        consolidation_factuality,
        consolidation,
        cluster_post_success,
        cluster_harmful_rate,
    }
}

fn paired_cluster_deltas(
    baseline: &BTreeMap<String, f64>,
    candidate: &BTreeMap<String, f64>,
) -> Result<Vec<f64>, String> {
    if baseline.keys().ne(candidate.keys()) {
        return Err("paired cluster IDs differ between treatments".to_string());
    }
    Ok(baseline
        .iter()
        .map(|(id, value)| candidate[id] - value)
        .collect())
}

fn seeded_bootstrap_ci(
    values: &[f64],
    iterations: usize,
    seed: u64,
    confidence_level: f64,
) -> ConfidenceInterval {
    if values.is_empty() || iterations == 0 {
        return ConfidenceInterval {
            mean: 0.0,
            lower: 0.0,
            upper: 0.0,
            unit_count: values.len(),
            iterations,
            seed,
        };
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let mut rng = StdRng::seed_from_u64(seed);
    let mut means = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let sample_mean = (0..values.len())
            .map(|_| values[rng.gen_range(0..values.len())])
            .sum::<f64>()
            / values.len() as f64;
        means.push(sample_mean);
    }
    means.sort_by(|left, right| left.total_cmp(right));
    let alpha = 1.0 - confidence_level.clamp(0.0, 1.0);
    let lower_index = ((alpha / 2.0) * iterations as f64).floor() as usize;
    let upper_index = ((1.0 - alpha / 2.0) * iterations as f64).ceil() as usize - 1;
    ConfidenceInterval {
        mean,
        lower: means[lower_index.min(iterations - 1)],
        upper: means[upper_index.min(iterations - 1)],
        unit_count: values.len(),
        iterations,
        seed,
    }
}

fn evaluate_gates(
    config: &PromotionGateConfig,
    raw: &TreatmentSummary,
    full: &TreatmentSummary,
    success_ci: &ConfidenceInterval,
    harmful_ci: &ConfidenceInterval,
    scope_violations: usize,
    crash_violations: usize,
) -> Vec<GateDecision> {
    vec![
        GateDecision {
            id: "success_improvement".to_string(),
            passed: success_ci.lower > config.full_loop_minus_raw_success_ci_lower_gt,
            observed: serde_json::json!(success_ci.lower),
            threshold: serde_json::json!({"strictly_greater_than": config.full_loop_minus_raw_success_ci_lower_gt}),
        },
        GateDecision {
            id: "stale_recurrence_nonincrease".to_string(),
            passed: !config.full_loop_stale_recurrence_lte_raw
                || full.stale_error_recurrence <= raw.stale_error_recurrence,
            observed: serde_json::json!({"raw": raw.stale_error_recurrence, "full": full.stale_error_recurrence}),
            threshold: serde_json::json!({"full_lte_raw": true}),
        },
        GateDecision {
            id: "harmful_memory_noninferiority".to_string(),
            passed: harmful_ci.upper <= config.full_loop_minus_raw_harmful_rate_ci_upper_lte,
            observed: serde_json::json!(harmful_ci.upper),
            threshold: serde_json::json!({"lte": config.full_loop_minus_raw_harmful_rate_ci_upper_lte}),
        },
        GateDecision {
            id: "recall_nonregression".to_string(),
            passed: !config.full_loop_recall_at_3_gte_raw || full.recall_at_3 >= raw.recall_at_3,
            observed: serde_json::json!({"raw": raw.recall_at_3, "full": full.recall_at_3}),
            threshold: serde_json::json!({"full_gte_raw": true}),
        },
        GateDecision {
            id: "mrr_nonregression".to_string(),
            passed: !config.full_loop_mrr_gte_raw || full.mrr >= raw.mrr,
            observed: serde_json::json!({"raw": raw.mrr, "full": full.mrr}),
            threshold: serde_json::json!({"full_gte_raw": true}),
        },
        GateDecision {
            id: "scope_invariants".to_string(),
            passed: scope_violations == config.scope_violation_count_eq,
            observed: serde_json::json!(scope_violations),
            threshold: serde_json::json!({"eq": config.scope_violation_count_eq}),
        },
        GateDecision {
            id: "crash_recovery_invariants".to_string(),
            passed: crash_violations == config.crash_recovery_violation_count_eq,
            observed: serde_json::json!(crash_violations),
            threshold: serde_json::json!({"eq": config.crash_recovery_violation_count_eq}),
        },
    ]
}

fn validate_inputs(protocol: &Protocol, fixtures: &Fixtures) -> Result<(), String> {
    if protocol.schema_version != "memd.longitudinal.protocol.v1"
        || fixtures.schema_version != "memd.longitudinal.fixtures.v1"
    {
        return Err("unsupported longitudinal schema version".to_string());
    }
    if protocol.retrieval.ranking_policy_version != memd::store::OUTCOME_POLICY_VERSION {
        return Err(format!(
            "protocol policy {} does not match product {}",
            protocol.retrieval.ranking_policy_version,
            memd::store::OUTCOME_POLICY_VERSION
        ));
    }
    if protocol.retrieval.production_serve_enabled || protocol.exposure_compatibility.product_policy
    {
        return Err("frozen protocol must not enable outcome or exposure serving".to_string());
    }
    if protocol.timeline.pre_feedback_rounds != 1
        || protocol.timeline.post_feedback_rounds == 0
        || fixtures.task_variants.len()
            < protocol.timeline.pre_feedback_rounds + protocol.timeline.post_feedback_rounds
        || fixtures.clusters.len() < 20
    {
        return Err("longitudinal timeline or cluster count violates v1 contract".to_string());
    }
    let required = [
        "no_memory",
        "raw_memory",
        "admission_only",
        "staged_consolidation",
        "exposure_compat",
        "outcome_only",
        "full_loop",
    ];
    let present = protocol
        .treatments
        .iter()
        .map(|treatment| treatment.id.as_str())
        .collect::<HashSet<_>>();
    if required.iter().any(|required| !present.contains(required)) {
        return Err("protocol is missing a required treatment".to_string());
    }
    Ok(())
}

fn validate_crash_evidence(evidence: &CrashGateEvidence) -> Result<(), String> {
    if evidence.result != "passed"
        || evidence.failed != 0
        || evidence.ignored != 0
        || evidence.test_count == 0
        || evidence.passed != evidence.test_count
        || evidence.real_sigkill_boundary_test
            != "real_sigkill_at_every_durable_boundary_recovers_safely"
    {
        return Err("crash-gate evidence does not prove the full SIGKILL gate passed".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_manifest(
    memd_binary: &Path,
    config: &LongitudinalConfig,
    protocol: &Protocol,
    fixtures: &Fixtures,
    run_id: &str,
    protocol_sha256: &str,
    fixture_sha256: &str,
    crash_sha256: &str,
    crash_artifact_name: &str,
    row_count: usize,
) -> Result<RunManifest, String> {
    let source_commit = command_output("git", &["rev-parse", "HEAD"]);
    let source_repository = command_output("git", &["config", "--get", "remote.origin.url"]);
    let dirty_patch = Command::new("git")
        .args(["diff", "--binary", "HEAD"])
        .output()
        .map_err(|error| error.to_string())?
        .stdout;
    let mut environment_allowlist = BTreeMap::new();
    for key in ["RUST_BACKTRACE", "MEMD_EMBED_DEVICE"] {
        if let Ok(value) = std::env::var(key) {
            environment_allowlist.insert(key.to_string(), value);
        }
    }
    let evaluator_binary = std::env::current_exe()
        .map_err(|error| format!("resolve evaluator executable: {error}"))?;
    Ok(RunManifest {
        schema_version: "memd.longitudinal.manifest.v1",
        run_id: run_id.to_string(),
        generated_unix_ms: now_ms(),
        exact_argv: std::env::args().collect(),
        protocol_path: config.protocol_path.display().to_string(),
        protocol_sha256: protocol_sha256.to_string(),
        fixture_path: config.fixtures_path.display().to_string(),
        fixture_sha256: fixture_sha256.to_string(),
        crash_gate_evidence_path: config.crash_gate_evidence.display().to_string(),
        crash_gate_evidence_sha256: crash_sha256.to_string(),
        crash_gate_evidence_artifact: crash_artifact_name.to_string(),
        source_repository,
        source_commit,
        dirty_patch_sha256: sha256_bytes(&dirty_patch),
        binary_path: memd_binary.display().to_string(),
        binary_sha256: sha256_file(memd_binary)?,
        evaluator_binary_path: evaluator_binary.display().to_string(),
        evaluator_binary_sha256: sha256_file(&evaluator_binary)?,
        cargo_lock_sha256: sha256_file(Path::new("Cargo.lock"))?,
        rustc_version: command_output("rustc", &["-Vv"]),
        os: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        available_parallelism: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        environment_allowlist,
        row_count,
        treatment_count: protocol.treatments.len(),
        cluster_count: fixtures.clusters.len(),
    })
}

fn unwrap_operation_payload(value: Value) -> Result<Value, String> {
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return Ok(value);
    };
    let text = content
        .iter()
        .find_map(|item| item.get("text").and_then(Value::as_str))
        .ok_or_else(|| "operation response content had no text payload".to_string())?;
    serde_json::from_str(text).map_err(|error| format!("parse operation payload: {error}"))
}

fn render_template(template: &str, subsystem: &str) -> String {
    template.replace("{subsystem}", subsystem)
}

fn directory_bytes(path: &Path) -> Result<u64, String> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let metadata = entry.metadata().map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_bytes(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn write_pretty_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

fn write_inventory(run_dir: &Path, inventory_path: &Path) -> Result<(), String> {
    let mut entries = fs::read_dir(run_dir)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.retain(|path| path != inventory_path && path.is_file());
    entries.sort();
    let mut output = String::new();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "non-UTF8 artifact name".to_string())?;
        output.push_str(&format!("{}  {}\n", sha256_file(&path)?, name));
    }
    fs::write(inventory_path, output).map_err(|error| error.to_string())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    fs::read(path)
        .map(|bytes| sha256_bytes(&bytes))
        .map_err(|error| format!("hash {}: {error}", path.display()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
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
    fn seeded_bootstrap_is_reproducible() {
        let values = [0.0, 0.5, 1.0, 1.0];
        assert_eq!(
            serde_json::to_value(seeded_bootstrap_ci(&values, 500, 42, 0.95)).unwrap(),
            serde_json::to_value(seeded_bootstrap_ci(&values, 500, 42, 0.95)).unwrap()
        );
    }

    #[test]
    fn exposure_adjustment_is_capped() {
        let config = ExposureCompatibility {
            adjustment_per_prior_render: 0.05,
            max_prior_renders: 4,
            max_adjustment: 0.2,
            product_policy: false,
        };
        let episode = RetrievalEpisodeId::new();
        let tenant = TenantId::new("t").unwrap();
        let make = |id: &str, score: f32, rank: usize| RetrievalEpisodeItem {
            episode_id: episode.clone(),
            chunk_id: ChunkId::parse(id).unwrap(),
            origin_tenant_id: tenant.clone(),
            origin_project_id: None,
            original_rank: rank,
            original_score: score,
            lane_scores_json: "{}".to_string(),
            outcome_adjustment: 0.0,
            served_rank: Some(rank),
            shadow_rank: Some(rank),
            rendered: true,
            source_dedup_group: None,
        };
        let older = "01900000-0000-7000-8000-000000000001";
        let newer = "01900000-0000-7000-8000-000000000002";
        let items = vec![make(newer, 1.0, 0), make(older, 0.9, 1)];
        let counts = HashMap::from([(older.to_string(), 100)]);
        assert_eq!(exposure_ranked_ids(&items, &counts, 2, &config)[0], older);
    }
}
