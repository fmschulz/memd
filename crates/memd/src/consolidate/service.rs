//! Crash-safe staged consolidation service.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;

use sha2::{Digest, Sha256};
use tracing::warn;

use super::journal::{
    ConsolidationEntryRecord, ConsolidationRun, ConsolidationRunId, ConsolidationState,
    LineageRelation, MemoryLineage, PromotionOutcome,
};
use super::prompt::ConsolidatedEntry;
use super::ConsolidatorIdentity;
use crate::error::{MemdError, Result};
use crate::index::SparseIndex;
use crate::store::metadata::MetadataStore;
use crate::store::persistent::{CandidatePersistenceStage, PersistentStore};
use crate::store::Store;
use crate::task_memory::TrustTier;
use crate::types::{
    ChunkId, ChunkStatus, ChunkType, IngestionMode, MemoryChunk, MemoryTier, ProjectId, TenantId,
};
use crate::write_service::{prepare_write, PrepareWriteRequest};

/// Grace period before a global recovery sweep may claim a run. Candidate
/// persistence is normally much faster than this, while the delay prevents
/// a concurrent session-start from mistaking live work for a crash.
const RECOVERY_MIN_AGE_MS: i64 = 30_000;
const ACTIVE_RUN_WAIT_ATTEMPTS: usize = 100;
const ACTIVE_RUN_WAIT_MS: u64 = 20;
const RAW_RESPONSE_AUDIT_MAX_BYTES: usize = 256 * 1024;

/// Observable boundaries used by crash-injection tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationStage {
    JournalPlanned,
    CandidateWalAppended,
    CandidateMetadataInserted,
    CandidatePersisted,
    CandidatesRecorded,
    Validated,
    Promoted,
    SparseCleanupFinished,
}

/// Result returned by a staged consolidation execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationExecution {
    pub run_id: ConsolidationRunId,
    pub state: ConsolidationState,
    pub candidate_chunk_ids: Vec<ChunkId>,
    pub source_count: usize,
    pub reused_existing_run: bool,
}

/// Summary of one bounded startup recovery pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsolidationRecovery {
    pub inspected: usize,
    pub committed: usize,
    pub rolled_back: usize,
    pub rejected: usize,
    pub failed_recoverable: usize,
    pub promoted_chunks: Vec<(TenantId, ChunkId)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationReviewDecision {
    Accept,
    Reject,
}

/// Apply an explicit decision to a validated staged run. Acceptance first
/// journals promotion intent and then enters the same recovery-safe promotion
/// path used after a crash. Rejection atomically hides candidates and records
/// the terminal journal state.
pub async fn review_consolidation_run(
    store: &PersistentStore,
    run_id: &ConsolidationRunId,
    decision: ConsolidationReviewDecision,
) -> Result<ConsolidationExecution> {
    let run = store
        .metadata()
        .get_consolidation_run(run_id)?
        .ok_or_else(|| MemdError::ValidationError(format!("unknown consolidation run {run_id}")))?;
    let source_count = store
        .metadata()
        .get_memory_lineage(run_id)?
        .into_iter()
        .map(|edge| edge.source_chunk_id)
        .collect::<BTreeSet<_>>()
        .len();

    match decision {
        ConsolidationReviewDecision::Accept => {
            if run.state == ConsolidationState::Committed {
                return execution_from_journal(store, run_id.clone(), source_count, true);
            }
            if run.state != ConsolidationState::Validated {
                return Err(MemdError::ValidationError(format!(
                    "consolidation run {run_id} cannot be accepted from state {}",
                    run.state
                )));
            }
            let requested_now = store
                .metadata()
                .request_consolidation_promotion(run_id, now_ms())?;
            if !requested_now {
                let current = store
                    .metadata()
                    .get_consolidation_run(run_id)?
                    .ok_or_else(|| {
                        MemdError::StorageError(format!("missing consolidation run {run_id}"))
                    })?;
                if current.state == ConsolidationState::Committed {
                    return execution_from_journal(store, run_id.clone(), source_count, true);
                }
                if current.state != ConsolidationState::Validated || !current.promotion_requested {
                    return Err(MemdError::ValidationError(format!(
                        "concurrent review prevented accepting run {run_id}; current state is {}",
                        current.state
                    )));
                }
            }
            let requested = store
                .metadata()
                .get_consolidation_run(run_id)?
                .ok_or_else(|| {
                    MemdError::StorageError(format!("missing consolidation run {run_id}"))
                })?;
            recover_one_run(store, &requested).await?;
            let execution = execution_from_journal(store, run_id.clone(), source_count, false)?;
            if execution.state != ConsolidationState::Committed {
                let current = store
                    .metadata()
                    .get_consolidation_run(run_id)?
                    .ok_or_else(|| {
                        MemdError::StorageError(format!("missing consolidation run {run_id}"))
                    })?;
                let detail = current
                    .error
                    .as_deref()
                    .map(|error| format!(": {error}"))
                    .unwrap_or_default();
                return Err(MemdError::ValidationError(format!(
                    "accepting consolidation run {run_id} failed; current state is {}{detail}",
                    execution.state,
                )));
            }
            return Ok(execution);
        }
        ConsolidationReviewDecision::Reject => {
            if run.state == ConsolidationState::Rejected {
                return execution_from_journal(store, run_id.clone(), source_count, true);
            }
            if run.state != ConsolidationState::Validated {
                return Err(MemdError::ValidationError(format!(
                    "consolidation run {run_id} cannot be rejected from state {}",
                    run.state
                )));
            }
            let rejected_now = store.metadata().terminate_consolidation_run(
                run_id,
                ConsolidationState::Validated,
                ConsolidationState::Rejected,
                now_ms(),
                "rejected by explicit review",
            )?;
            if !rejected_now {
                let current = store
                    .metadata()
                    .get_consolidation_run(run_id)?
                    .ok_or_else(|| {
                        MemdError::StorageError(format!("missing consolidation run {run_id}"))
                    })?;
                if current.state == ConsolidationState::Rejected {
                    return execution_from_journal(store, run_id.clone(), source_count, true);
                }
                return Err(MemdError::ValidationError(format!(
                    "concurrent review prevented rejecting run {run_id}; current state is {}",
                    current.state
                )));
            }
        }
    }
    execution_from_journal(store, run_id.clone(), source_count, false)
}

/// Execute consolidation using the durable prepare/validate/commit protocol.
#[allow(clippy::too_many_arguments)]
pub async fn execute_consolidation(
    store: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    entries: &[ConsolidatedEntry],
    relation: LineageRelation,
    consolidator: &str,
    inherited_ctx: &[String],
    prompt: &str,
    raw_response: &str,
) -> Result<ConsolidationExecution> {
    execute_consolidation_with_hook(
        store,
        tenant_id,
        project_id,
        entries,
        relation,
        consolidator,
        inherited_ctx,
        prompt,
        raw_response,
        |_| Ok(()),
    )
    .await
}

/// Execute with fully resolved consolidator provenance and explicit durable
/// promotion intent. CLI staging uses `promotion_requested=false`; legacy and
/// explicitly promoted callers pass true.
#[allow(clippy::too_many_arguments)]
pub async fn execute_consolidation_with_identity(
    store: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    entries: &[ConsolidatedEntry],
    relation: LineageRelation,
    identity: &ConsolidatorIdentity,
    inherited_ctx: &[String],
    prompt: &str,
    raw_response: &str,
    promotion_requested: bool,
) -> Result<ConsolidationExecution> {
    execute_consolidation_with_identity_and_hook(
        store,
        tenant_id,
        project_id,
        entries,
        relation,
        identity,
        inherited_ctx,
        prompt,
        raw_response,
        promotion_requested,
        |_| Ok(()),
    )
    .await
}

/// Same protocol as [`execute_consolidation`], with an injectable boundary
/// hook for deterministic crash/reopen tests.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn execute_consolidation_with_hook<F>(
    store: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    entries: &[ConsolidatedEntry],
    relation: LineageRelation,
    consolidator: &str,
    inherited_ctx: &[String],
    prompt: &str,
    raw_response: &str,
    hook: F,
) -> Result<ConsolidationExecution>
where
    F: FnMut(ConsolidationStage) -> Result<()>,
{
    let identity = ConsolidatorIdentity::internal(consolidator);
    execute_consolidation_with_identity_and_hook(
        store,
        tenant_id,
        project_id,
        entries,
        relation,
        &identity,
        inherited_ctx,
        prompt,
        raw_response,
        true,
        hook,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_consolidation_with_identity_and_hook<F>(
    store: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    entries: &[ConsolidatedEntry],
    relation: LineageRelation,
    identity: &ConsolidatorIdentity,
    inherited_ctx: &[String],
    prompt: &str,
    raw_response: &str,
    promotion_requested: bool,
    mut hook: F,
) -> Result<ConsolidationExecution>
where
    F: FnMut(ConsolidationStage) -> Result<()>,
{
    store.ensure_writable("execute_consolidation")?;
    let project_id = project_id.filter(|project_id| !project_id.is_empty());
    finish_pending_sparse_cleanup(store, 100);
    if entries.is_empty() {
        return Err(MemdError::ValidationError(
            "consolidation requires at least one validated entry".to_string(),
        ));
    }
    let now = now_ms();
    let source_ids = parse_unique_sources(entries)?;
    let run_id = ConsolidationRunId::new();
    let input_hash = consolidation_input_hash(tenant_id, project_id, relation, &source_ids);
    if let Some(mut existing) =
        store
            .metadata()
            .find_consolidation_run_by_input(tenant_id, project_id, &input_hash)?
    {
        if promotion_requested && !existing.promotion_requested {
            store
                .metadata()
                .request_consolidation_promotion(&existing.run_id, now_ms())?;
            existing = store
                .metadata()
                .get_consolidation_run(&existing.run_id)?
                .ok_or_else(|| {
                    MemdError::StorageError(
                        "consolidation run disappeared after promotion request".to_string(),
                    )
                })?;
        }
        return settle_existing_run(store, existing, source_ids.len()).await;
    }
    validate_sources(store, tenant_id, project_id, relation, &source_ids, now)?;

    let audit_artifact_path = write_raw_response_audit(store, &run_id, raw_response)?;

    let run = ConsolidationRun {
        run_id: run_id.clone(),
        tenant_id: tenant_id.clone(),
        project_id: project_id.map(str::to_string),
        input_hash,
        state: ConsolidationState::Planned,
        consolidator: identity.adapter.clone(),
        consolidator_command: identity.command.clone(),
        consolidator_model: identity.model.clone(),
        consolidator_version: identity.version.clone(),
        prompt_hash: Some(sha256_hex(prompt.as_bytes())),
        response_hash: Some(sha256_hex(raw_response.as_bytes())),
        audit_artifact_path: Some(audit_artifact_path),
        validation_result: None,
        error: None,
        sparse_cleanup_done: false,
        promotion_requested,
        created_at_ms: now,
        updated_at_ms: now,
    };

    let mut candidate_chunks = Vec::with_capacity(entries.len());
    let mut entry_records = Vec::with_capacity(entries.len());
    let mut lineage = Vec::new();
    for (entry_index, entry) in entries.iter().enumerate() {
        let rendered_text = entry.rendered_text();
        let mut tags = vec![
            "kind:consolidated".to_string(),
            format!("priority:{}", entry.priority),
            format!("confidence:{:.3}", entry.confidence),
            "trust:semantic_candidate".to_string(),
            format!("consolidator:{}", identity.adapter),
            format!("consolidation_run:{run_id}"),
            format!("consolidation_entry:{entry_index}"),
        ];
        tags.push(format!(
            "{}:{}",
            relation.as_str(),
            entry.supersedes.join(",")
        ));
        tags.extend(inherited_ctx.iter().cloned());

        let prepared = prepare_write(PrepareWriteRequest {
            chunk_type: ChunkType::Summary,
            text: &rendered_text,
            tags: &tags,
            ingestion_mode: IngestionMode::Document,
            expires_at_ms: None,
            review_after_ms: None,
        });
        if prepared.is_rejected() {
            return Err(MemdError::ValidationError(format!(
                "consolidation candidate {entry_index} rejected by write preparation: {}",
                prepared.outcome.reason
            )));
        }
        if prepared.trust_tier != TrustTier::SemanticCandidate {
            return Err(MemdError::ValidationError(
                "model-authored consolidation cannot exceed semantic-candidate trust".to_string(),
            ));
        }
        let mut chunk = prepared
            .apply_to_chunk(MemoryChunk::new(
                tenant_id.clone(),
                rendered_text,
                ChunkType::Summary,
            ))
            .with_status(ChunkStatus::Candidate);
        if let Some(project_id) = project_id {
            chunk = chunk.with_project(ProjectId::from(project_id));
        }
        let candidate_id = chunk.chunk_id.clone();
        let entry_sources = entry
            .supersedes
            .iter()
            .map(|source| ChunkId::parse(source))
            .collect::<Result<Vec<_>>>()?;
        entry_records.push(ConsolidationEntryRecord {
            run_id: run_id.clone(),
            entry_index,
            candidate_chunk_id: Some(candidate_id.clone()),
            source_set_hash: source_set_hash(&entry_sources),
            state: ConsolidationState::Planned,
            validation_error: None,
            created_at_ms: now,
            updated_at_ms: now,
        });
        for source_chunk_id in entry_sources {
            lineage.push(MemoryLineage {
                run_id: run_id.clone(),
                tenant_id: tenant_id.clone(),
                project_id: project_id.map(str::to_string),
                source_chunk_id,
                result_chunk_id: candidate_id.clone(),
                relation,
                created_at_ms: now,
            });
        }
        candidate_chunks.push(chunk);
    }

    let durable_run = match store
        .metadata()
        .begin_consolidation_run(&run, &entry_records, &lineage)
    {
        Ok(durable_run) => durable_run,
        Err(error) => {
            remove_raw_response_audit(store, run.audit_artifact_path.as_deref());
            return Err(error);
        }
    };
    let reused_existing_run = durable_run.run_id != run_id;
    if reused_existing_run {
        remove_raw_response_audit(store, run.audit_artifact_path.as_deref());
        return settle_existing_run(store, durable_run, source_ids.len()).await;
    }
    hook(ConsolidationStage::JournalPlanned)?;

    for chunk in candidate_chunks {
        store
            .add_consolidation_candidate_with_hook(chunk, |stage| match stage {
                CandidatePersistenceStage::WalAppended => {
                    hook(ConsolidationStage::CandidateWalAppended)
                }
                CandidatePersistenceStage::MetadataInserted => {
                    hook(ConsolidationStage::CandidateMetadataInserted)
                }
            })
            .await?;
        hook(ConsolidationStage::CandidatePersisted)?;
    }
    if !store.metadata().transition_consolidation_run(
        &run_id,
        ConsolidationState::Planned,
        ConsolidationState::CandidateWritten,
        now_ms(),
        None,
        None,
    )? {
        if store
            .metadata()
            .get_consolidation_run(&run_id)?
            .is_some_and(|current| current.state.is_terminal())
        {
            store
                .metadata()
                .hide_consolidation_candidates(&run_id, now_ms())?;
        }
        return Err(MemdError::StorageError(format!(
            "consolidation run {run_id} lost its planned-state guard"
        )));
    }
    hook(ConsolidationStage::CandidatesRecorded)?;

    if let Err(error) = validate_candidate_run(store, &run).await {
        if matches!(&error, MemdError::ValidationError(_)) {
            store.metadata().terminate_consolidation_run(
                &run_id,
                ConsolidationState::CandidateWritten,
                ConsolidationState::Rejected,
                now_ms(),
                &error.to_string(),
            )?;
        }
        return Err(error);
    }
    if !store.metadata().transition_consolidation_run(
        &run_id,
        ConsolidationState::CandidateWritten,
        ConsolidationState::Validated,
        now_ms(),
        Some("accepted"),
        None,
    )? {
        return Err(MemdError::StorageError(format!(
            "consolidation run {run_id} lost its candidate-written guard"
        )));
    }
    hook(ConsolidationStage::Validated)?;

    if !promotion_requested {
        return execution_from_journal(store, run_id, source_ids.len(), false);
    }

    if let Err(error) = promote_validated_run(store, &run).await {
        classify_promotion_failure(store, &run, &error).await?;
        return Err(error);
    }
    hook(ConsolidationStage::Promoted)?;
    if let Err(error) =
        finish_sparse_cleanup_for_run(store, &run_id, tenant_id, relation, &source_ids)
    {
        warn!(
            run_id = %run_id,
            error = %error,
            "consolidation committed but sparse cleanup remains pending"
        );
    }
    hook(ConsolidationStage::SparseCleanupFinished)?;

    execution_from_journal(store, run_id, source_ids.len(), false)
}

/// Reconcile a bounded set of nonterminal runs after opening the store.
pub async fn recover_consolidation_runs(
    store: &PersistentStore,
    limit: usize,
) -> Result<ConsolidationRecovery> {
    recover_consolidation_runs_before(store, limit, now_ms() - RECOVERY_MIN_AGE_MS).await
}

/// Recovery entry point with an explicit clock cutoff. Production callers
/// use the grace period above; deterministic crash tests pass `i64::MAX`.
#[doc(hidden)]
pub async fn recover_consolidation_runs_before(
    store: &PersistentStore,
    limit: usize,
    updated_before_ms: i64,
) -> Result<ConsolidationRecovery> {
    store.ensure_writable("recover_consolidation_runs")?;
    store
        .metadata()
        .hide_terminal_consolidation_candidates(now_ms())?;
    finish_pending_sparse_cleanup(store, limit);
    let runs = store
        .metadata()
        .list_recoverable_consolidation_runs_before(limit, updated_before_ms)?;
    let mut recovery = ConsolidationRecovery::default();
    for run in runs {
        recovery.inspected += 1;
        if let Err(error) = recover_one_run(store, &run).await {
            let post_state = store
                .metadata()
                .get_consolidation_run(&run.run_id)?
                .map(|current| current.state);
            match post_state {
                Some(ConsolidationState::Committed) => {
                    recovery.committed += 1;
                    recovery.promoted_chunks.extend(
                        store
                            .metadata()
                            .get_consolidation_entries(&run.run_id)?
                            .into_iter()
                            .filter_map(|entry| entry.candidate_chunk_id)
                            .map(|chunk_id| (run.tenant_id.clone(), chunk_id)),
                    );
                    warn!(
                        run_id = %run.run_id,
                        error = %error,
                        "consolidation committed but post-commit cleanup was deferred"
                    );
                    continue;
                }
                Some(ConsolidationState::Rejected) => {
                    recovery.rejected += 1;
                    continue;
                }
                Some(ConsolidationState::RolledBack) => {
                    recovery.rolled_back += 1;
                    continue;
                }
                _ => {}
            }
            warn!(
                run_id = %run.run_id,
                error = %error,
                "consolidation recovery deferred one poisoned run"
            );
            if let Err(record_error) = store.metadata().record_consolidation_recovery_error(
                &run.run_id,
                now_ms(),
                &error.to_string(),
            ) {
                warn!(
                    run_id = %run.run_id,
                    error = %record_error,
                    "could not rotate failed consolidation recovery row"
                );
            }
            recovery.failed_recoverable += 1;
            continue;
        }
        let state = store
            .metadata()
            .get_consolidation_run(&run.run_id)?
            .ok_or_else(|| {
                MemdError::StorageError(format!(
                    "consolidation run {} disappeared during recovery",
                    run.run_id
                ))
            })?
            .state;
        match state {
            ConsolidationState::Committed => {
                recovery.committed += 1;
                recovery.promoted_chunks.extend(
                    store
                        .metadata()
                        .get_consolidation_entries(&run.run_id)?
                        .into_iter()
                        .filter_map(|entry| entry.candidate_chunk_id)
                        .map(|chunk_id| (run.tenant_id.clone(), chunk_id)),
                );
            }
            ConsolidationState::RolledBack => recovery.rolled_back += 1,
            ConsolidationState::Rejected => recovery.rejected += 1,
            ConsolidationState::FailedRecoverable => recovery.failed_recoverable += 1,
            _ => {}
        }
    }
    Ok(recovery)
}

async fn recover_one_run(store: &PersistentStore, run: &ConsolidationRun) -> Result<()> {
    let mut current_run = store
        .metadata()
        .get_consolidation_run(&run.run_id)?
        .ok_or_else(|| {
            MemdError::StorageError(format!("missing consolidation run {}", run.run_id))
        })?;
    let mut state = current_run.state;

    if state == ConsolidationState::Planned {
        let entries = store.metadata().get_consolidation_entries(&run.run_id)?;
        let mut present = 0usize;
        for entry in &entries {
            let Some(candidate_id) = entry.candidate_chunk_id.as_ref() else {
                continue;
            };
            if store
                .metadata()
                .get(&run.tenant_id, candidate_id)?
                .is_some()
                && Store::get(store, &run.tenant_id, candidate_id)
                    .await?
                    .is_some()
            {
                present += 1;
            }
        }
        if present != entries.len() {
            store.metadata().terminate_consolidation_run(
                &run.run_id,
                ConsolidationState::Planned,
                ConsolidationState::RolledBack,
                now_ms(),
                "recovery found an incomplete candidate payload set",
            )?;
            return Ok(());
        }
        if !store.metadata().transition_consolidation_run(
            &run.run_id,
            ConsolidationState::Planned,
            ConsolidationState::CandidateWritten,
            now_ms(),
            None,
            None,
        )? {
            return Ok(());
        }
        state = ConsolidationState::CandidateWritten;
    }

    if state == ConsolidationState::CandidateWritten {
        if let Err(error) = validate_candidate_run(store, run).await {
            if matches!(&error, MemdError::ValidationError(_)) {
                store.metadata().terminate_consolidation_run(
                    &run.run_id,
                    ConsolidationState::CandidateWritten,
                    ConsolidationState::Rejected,
                    now_ms(),
                    &error.to_string(),
                )?;
                return Ok(());
            }
            return Err(error);
        }
        if !store.metadata().transition_consolidation_run(
            &run.run_id,
            ConsolidationState::CandidateWritten,
            ConsolidationState::Validated,
            now_ms(),
            Some("accepted during recovery"),
            None,
        )? {
            return Ok(());
        }
        state = ConsolidationState::Validated;
        current_run = store
            .metadata()
            .get_consolidation_run(&run.run_id)?
            .ok_or_else(|| {
                MemdError::StorageError(format!("missing consolidation run {}", run.run_id))
            })?;
    }

    if state == ConsolidationState::FailedRecoverable {
        if !store.metadata().transition_consolidation_run(
            &run.run_id,
            ConsolidationState::FailedRecoverable,
            ConsolidationState::Validated,
            now_ms(),
            Some("retrying promotion"),
            None,
        )? {
            return Ok(());
        }
        state = ConsolidationState::Validated;
        current_run = store
            .metadata()
            .get_consolidation_run(&run.run_id)?
            .ok_or_else(|| {
                MemdError::StorageError(format!("missing consolidation run {}", run.run_id))
            })?;
    }

    if state == ConsolidationState::Validated {
        if !current_run.promotion_requested {
            return Ok(());
        }
        match promote_validated_run(store, &current_run).await {
            Ok(PromotionOutcome::Committed | PromotionOutcome::AlreadyCommitted) => {
                let lineage = store.metadata().get_memory_lineage(&run.run_id)?;
                let relation = lineage.first().map(|edge| edge.relation).ok_or_else(|| {
                    MemdError::StorageError(format!(
                        "consolidation run {} has no lineage",
                        run.run_id
                    ))
                })?;
                let sources = lineage
                    .into_iter()
                    .map(|edge| edge.source_chunk_id)
                    .collect::<BTreeSet<_>>();
                finish_sparse_cleanup_for_run(
                    store,
                    &run.run_id,
                    &run.tenant_id,
                    relation,
                    &sources,
                )?;
            }
            Err(error) => {
                classify_promotion_failure(store, &current_run, &error).await?;
            }
        }
    }
    Ok(())
}

async fn validate_candidate_run(store: &PersistentStore, run: &ConsolidationRun) -> Result<()> {
    validate_raw_response_audit(store, run)?;
    let entries = store.metadata().get_consolidation_entries(&run.run_id)?;
    if entries.is_empty() {
        return Err(MemdError::ValidationError(
            "consolidation run has no candidate entries".to_string(),
        ));
    }
    for entry in entries {
        let candidate_id = entry.candidate_chunk_id.ok_or_else(|| {
            MemdError::ValidationError("consolidation entry has no candidate id".to_string())
        })?;
        let metadata = store
            .metadata()
            .get(&run.tenant_id, &candidate_id)?
            .ok_or_else(|| {
                MemdError::ValidationError(format!("candidate {candidate_id} metadata is missing"))
            })?;
        if metadata.status != ChunkStatus::Candidate || metadata.project_id != run.project_id {
            return Err(MemdError::ValidationError(format!(
                "candidate {candidate_id} metadata escaped its journal scope"
            )));
        }
        if Store::get(store, &run.tenant_id, &candidate_id)
            .await?
            .is_none()
        {
            return Err(MemdError::ValidationError(format!(
                "candidate {candidate_id} payload is missing"
            )));
        }
    }
    let lineage = store.metadata().get_memory_lineage(&run.run_id)?;
    let sources = lineage
        .iter()
        .map(|edge| edge.source_chunk_id.clone())
        .collect::<BTreeSet<_>>();
    validate_sources(
        store,
        &run.tenant_id,
        run.project_id.as_deref(),
        lineage.first().map(|edge| edge.relation).ok_or_else(|| {
            MemdError::ValidationError("consolidation run has no lineage".to_string())
        })?,
        &sources,
        now_ms(),
    )
}

fn validate_raw_response_audit(store: &PersistentStore, run: &ConsolidationRun) -> Result<()> {
    let relative_path = run.audit_artifact_path.as_deref().ok_or_else(|| {
        MemdError::ValidationError(
            "consolidation run has no raw-response audit artifact".to_string(),
        )
    })?;
    let relative = std::path::Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(MemdError::ValidationError(
            "consolidation audit artifact path is not a safe relative path".to_string(),
        ));
    }
    let bytes = std::fs::read(store.data_dir().join(relative))?;
    if bytes.len() > RAW_RESPONSE_AUDIT_MAX_BYTES {
        return Err(MemdError::ValidationError(
            "consolidation audit artifact exceeds its bounded size".to_string(),
        ));
    }
    let artifact: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        MemdError::ValidationError(format!(
            "consolidation audit artifact is not valid JSON: {error}"
        ))
    })?;
    let artifact_hash = artifact
        .get("response_sha256")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MemdError::ValidationError(
                "consolidation audit artifact has no response hash".to_string(),
            )
        })?;
    if run.response_hash.as_deref() != Some(artifact_hash) {
        return Err(MemdError::ValidationError(
            "consolidation audit response hash does not match its journal".to_string(),
        ));
    }
    let stored_response = artifact
        .get("raw_response")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MemdError::ValidationError(
                "consolidation audit artifact has no stored response".to_string(),
            )
        })?;
    let stored_hash = artifact
        .get("stored_response_sha256")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MemdError::ValidationError(
                "consolidation audit artifact has no stored-response hash".to_string(),
            )
        })?;
    if sha256_hex(stored_response.as_bytes()) != stored_hash {
        return Err(MemdError::ValidationError(
            "consolidation audit stored-response hash does not match its body".to_string(),
        ));
    }
    if artifact
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        && artifact_hash != stored_hash
    {
        return Err(MemdError::ValidationError(
            "untruncated consolidation audit body does not match the journal response hash"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_sources(
    store: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    relation: LineageRelation,
    source_ids: &BTreeSet<ChunkId>,
    now_ms: i64,
) -> Result<()> {
    for source_id in source_ids {
        let source = store
            .metadata()
            .get(tenant_id, source_id)?
            .ok_or_else(|| MemdError::ValidationError(format!("source {source_id} is missing")))?;
        if source.status != ChunkStatus::Final
            || source.lifecycle.superseded_by.is_some()
            || source.lifecycle.tier == MemoryTier::History
            || source
                .lifecycle
                .expires_at_ms
                .is_some_and(|expiry| expiry <= now_ms)
        {
            return Err(MemdError::ValidationError(format!(
                "source {source_id} is no longer a visible final head"
            )));
        }
        if relation == LineageRelation::Supersedes && source.project_id.as_deref() != project_id {
            return Err(MemdError::ValidationError(format!(
                "source {source_id} escaped project consolidation scope"
            )));
        }
    }
    Ok(())
}

async fn settle_existing_run(
    store: &PersistentStore,
    mut run: ConsolidationRun,
    source_count: usize,
) -> Result<ConsolidationExecution> {
    for _ in 0..ACTIVE_RUN_WAIT_ATTEMPTS {
        if run.state.is_terminal() {
            break;
        }
        if run.state == ConsolidationState::Validated {
            if run.promotion_requested {
                recover_one_run(store, &run).await?;
            }
            break;
        }
        if run.updated_at_ms <= now_ms() - RECOVERY_MIN_AGE_MS {
            recover_one_run(store, &run).await?;
            return execution_from_journal(store, run.run_id, source_count, true);
        }
        tokio::time::sleep(std::time::Duration::from_millis(ACTIVE_RUN_WAIT_MS)).await;
        run = store
            .metadata()
            .get_consolidation_run(&run.run_id)?
            .ok_or_else(|| {
                MemdError::StorageError(format!(
                    "consolidation run {} disappeared while waiting",
                    run.run_id
                ))
            })?;
    }
    execution_from_journal(store, run.run_id, source_count, true)
}

async fn promote_validated_run(
    store: &PersistentStore,
    run: &ConsolidationRun,
) -> Result<PromotionOutcome> {
    validate_raw_response_audit(store, run)?;
    let outcome = store
        .metadata()
        .atomic_promote_consolidation_run(&run.run_id, now_ms())?;
    let candidate_ids = store
        .metadata()
        .get_consolidation_entries(&run.run_id)?
        .into_iter()
        .filter_map(|entry| entry.candidate_chunk_id)
        .collect::<Vec<_>>();
    if let Err(error) = store
        .refresh_promoted_chunks(&run.tenant_id, &candidate_ids)
        .await
    {
        warn!(
            run_id = %run.run_id,
            error = %error,
            "committed consolidation remains visible through metadata/sparse search but dense refresh failed"
        );
    }
    Ok(outcome)
}

async fn classify_promotion_failure(
    store: &PersistentStore,
    run: &ConsolidationRun,
    promotion_error: &MemdError,
) -> Result<()> {
    match validate_candidate_run(store, run).await {
        Err(MemdError::ValidationError(reason)) => {
            store.metadata().terminate_consolidation_run(
                &run.run_id,
                ConsolidationState::Validated,
                ConsolidationState::Rejected,
                now_ms(),
                &format!("promotion rejected after source/candidate drift: {reason}"),
            )?;
        }
        validation_result => {
            let detail = match validation_result {
                Ok(()) => promotion_error.to_string(),
                Err(validation_error) => format!(
                    "promotion failed: {promotion_error}; validation retry failed: {validation_error}"
                ),
            };
            store.metadata().transition_consolidation_run(
                &run.run_id,
                ConsolidationState::Validated,
                ConsolidationState::FailedRecoverable,
                now_ms(),
                None,
                Some(&detail),
            )?;
        }
    }
    Ok(())
}

fn execution_from_journal(
    store: &PersistentStore,
    run_id: ConsolidationRunId,
    source_count: usize,
    reused_existing_run: bool,
) -> Result<ConsolidationExecution> {
    let run = store
        .metadata()
        .get_consolidation_run(&run_id)?
        .ok_or_else(|| MemdError::StorageError(format!("missing consolidation run {run_id}")))?;
    let candidate_chunk_ids = store
        .metadata()
        .get_consolidation_entries(&run_id)?
        .into_iter()
        .filter_map(|entry| entry.candidate_chunk_id)
        .collect();
    Ok(ConsolidationExecution {
        run_id,
        state: run.state,
        candidate_chunk_ids,
        source_count,
        reused_existing_run,
    })
}

fn cleanup_superseded_sparse_rows(
    store: &PersistentStore,
    tenant_id: &TenantId,
    relation: LineageRelation,
    source_ids: &BTreeSet<ChunkId>,
) -> bool {
    if relation != LineageRelation::Supersedes {
        return true;
    }
    let Some(sparse) = store.sparse_index() else {
        // A genuinely dense-only store has nothing physical to clean. A
        // recovery handle may also lack a sparse writer while an index exists
        // on disk; keep that run pending for a later hybrid-enabled writer.
        return !store.sparse_index_exists_on_disk();
    };
    let mut complete = true;
    for source_id in source_ids {
        if let Err(error) = sparse.delete(tenant_id, source_id) {
            complete = false;
            warn!(
                tenant_id = %tenant_id,
                chunk_id = %source_id,
                error = %error,
                "committed consolidation could not clean a sparse source row"
            );
        }
    }
    complete
}

fn finish_sparse_cleanup_for_run(
    store: &PersistentStore,
    run_id: &ConsolidationRunId,
    tenant_id: &TenantId,
    relation: LineageRelation,
    source_ids: &BTreeSet<ChunkId>,
) -> Result<()> {
    if cleanup_superseded_sparse_rows(store, tenant_id, relation, source_ids) {
        store
            .metadata()
            .mark_consolidation_sparse_cleanup_done(run_id, now_ms())?;
    }
    Ok(())
}

fn finish_pending_sparse_cleanup(store: &PersistentStore, limit: usize) {
    if store.sparse_index().is_none() && store.sparse_index_exists_on_disk() {
        return;
    }
    let runs = match store
        .metadata()
        .list_consolidation_runs_pending_sparse_cleanup(limit)
    {
        Ok(runs) => runs,
        Err(error) => {
            warn!(error = %error, "could not list pending consolidation sparse cleanup");
            return;
        }
    };
    for run in runs {
        let result = (|| -> Result<()> {
            let lineage = store.metadata().get_memory_lineage(&run.run_id)?;
            let relation = lineage.first().map(|edge| edge.relation).ok_or_else(|| {
                MemdError::StorageError(format!(
                    "committed consolidation run {} has no lineage",
                    run.run_id
                ))
            })?;
            let sources = lineage
                .into_iter()
                .map(|edge| edge.source_chunk_id)
                .collect::<BTreeSet<_>>();
            finish_sparse_cleanup_for_run(store, &run.run_id, &run.tenant_id, relation, &sources)
        })();
        if let Err(error) = result {
            warn!(run_id = %run.run_id, error = %error, "consolidation sparse cleanup remains pending");
        }
    }
}

fn parse_unique_sources(entries: &[ConsolidatedEntry]) -> Result<BTreeSet<ChunkId>> {
    let mut sources = BTreeSet::new();
    let mut ownership = HashMap::new();
    for (entry_index, entry) in entries.iter().enumerate() {
        if entry.text.trim().is_empty() || entry.supersedes.is_empty() {
            return Err(MemdError::ValidationError(
                "consolidation entries require text and source lineage".to_string(),
            ));
        }
        for source in &entry.supersedes {
            let source_id = ChunkId::parse(source)?;
            if let Some(previous) = ownership.insert(source_id.clone(), entry_index) {
                return Err(MemdError::ValidationError(format!(
                    "source {source_id} is claimed by entries {previous} and {entry_index}"
                )));
            }
            sources.insert(source_id);
        }
    }
    Ok(sources)
}

fn consolidation_input_hash(
    tenant_id: &TenantId,
    project_id: Option<&str>,
    relation: LineageRelation,
    source_ids: &BTreeSet<ChunkId>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"memd-consolidation-input-v1\0");
    hash_field(&mut hasher, tenant_id.as_str().as_bytes());
    hash_field(&mut hasher, project_id.unwrap_or("").as_bytes());
    hash_field(&mut hasher, relation.as_str().as_bytes());
    for source_id in source_ids {
        hash_field(&mut hasher, source_id.to_string().as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn source_set_hash(source_ids: &[ChunkId]) -> String {
    let mut sorted = source_ids.to_vec();
    sorted.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"memd-consolidation-source-set-v1\0");
    for source_id in sorted {
        hash_field(&mut hasher, source_id.to_string().as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn write_raw_response_audit(
    store: &PersistentStore,
    run_id: &ConsolidationRunId,
    raw_response: &str,
) -> Result<String> {
    let audit_dir = store.data_dir().join("consolidation-audit");
    std::fs::create_dir_all(&audit_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&audit_dir, std::fs::Permissions::from_mode(0o700))?;
    }

    let original_bytes = raw_response.len();
    let mut end = original_bytes.min(RAW_RESPONSE_AUDIT_MAX_BYTES);
    while !raw_response.is_char_boundary(end) {
        end -= 1;
    }
    let response_hash = sha256_hex(raw_response.as_bytes());
    let artifact = loop {
        let response = &raw_response[..end];
        let encoded = serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "run_id": run_id.to_string(),
            "original_bytes": original_bytes,
            "stored_bytes": response.len(),
            "truncated": response.len() != original_bytes,
            "response_sha256": response_hash.as_str(),
            "stored_response_sha256": sha256_hex(response.as_bytes()),
            "raw_response": response,
        }))?;
        if encoded.len() <= RAW_RESPONSE_AUDIT_MAX_BYTES {
            break encoded;
        }
        let shrink_by = encoded.len() - RAW_RESPONSE_AUDIT_MAX_BYTES + 128;
        end = end.saturating_sub(shrink_by);
        while !raw_response.is_char_boundary(end) {
            end -= 1;
        }
    };
    let file_name = format!("{run_id}.json");
    let path = audit_dir.join(&file_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(&artifact)?;
    file.sync_all()?;
    Ok(format!("consolidation-audit/{file_name}"))
}

fn remove_raw_response_audit(store: &PersistentStore, relative_path: Option<&str>) {
    let Some(relative_path) = relative_path else {
        return;
    };
    let _ = std::fs::remove_file(store.data_dir().join(relative_path));
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_hash_is_order_independent_but_scope_sensitive() {
        let tenant = TenantId::new("t").unwrap();
        let a = ChunkId::new();
        let b = ChunkId::new();
        let one = BTreeSet::from([a.clone(), b.clone()]);
        let two = BTreeSet::from([b, a]);
        assert_eq!(
            consolidation_input_hash(&tenant, Some("p"), LineageRelation::Supersedes, &one),
            consolidation_input_hash(&tenant, Some("p"), LineageRelation::Supersedes, &two)
        );
        assert_ne!(
            consolidation_input_hash(&tenant, Some("p"), LineageRelation::Supersedes, &one),
            consolidation_input_hash(&tenant, None, LineageRelation::DerivesFrom, &one)
        );
    }
}
