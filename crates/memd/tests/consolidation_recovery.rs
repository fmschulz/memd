use std::sync::Arc;

use memd::consolidate::journal::{ConsolidationState, LineageRelation};
use memd::consolidate::prompt::ConsolidatedEntry;
use memd::consolidate::service::{
    execute_consolidation, execute_consolidation_with_hook, execute_consolidation_with_identity,
    recover_consolidation_runs_before, review_consolidation_run, ConsolidationReviewDecision,
    ConsolidationStage,
};
use memd::consolidate::ConsolidatorIdentity;
use memd::error::MemdError;
use memd::index::SparseIndex;
use memd::store::metadata::MetadataStore;
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::types::{ChunkStatus, ChunkType, MemoryChunk, ProjectId, TenantId};

fn open_store(path: &std::path::Path) -> PersistentStore {
    PersistentStore::open(PersistentStoreConfig {
        data_dir: path.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })
    .unwrap()
}

fn open_sparse_store(path: &std::path::Path) -> PersistentStore {
    PersistentStore::open(PersistentStoreConfig {
        data_dir: path.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: true,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })
    .unwrap()
}

async fn seed_source(store: &PersistentStore, tenant: &TenantId) -> memd::types::ChunkId {
    seed_source_with_text(store, tenant, "durable source fact").await
}

async fn seed_source_with_text(
    store: &PersistentStore,
    tenant: &TenantId,
    text: &str,
) -> memd::types::ChunkId {
    store
        .add(
            MemoryChunk::new(tenant.clone(), text, ChunkType::Doc)
                .with_project(ProjectId::from("p")),
        )
        .await
        .unwrap()
}

fn entry(source_id: &memd::types::ChunkId) -> ConsolidatedEntry {
    ConsolidatedEntry {
        text: "durable consolidated fact".to_string(),
        supersedes: vec![source_id.to_string()],
        agent_action: "Reuse this durable fact after checking the recorded source.".to_string(),
        evidence: vec![source_id.to_string()],
        confidence: 1.0,
        priority: 8,
    }
}

fn stage_from_name(name: &str) -> ConsolidationStage {
    match name {
        "journal_planned" => ConsolidationStage::JournalPlanned,
        "candidate_wal_appended" => ConsolidationStage::CandidateWalAppended,
        "candidate_metadata_inserted" => ConsolidationStage::CandidateMetadataInserted,
        "candidate_persisted" => ConsolidationStage::CandidatePersisted,
        "candidates_recorded" => ConsolidationStage::CandidatesRecorded,
        "validated" => ConsolidationStage::Validated,
        "promoted" => ConsolidationStage::Promoted,
        "sparse_cleanup_finished" => ConsolidationStage::SparseCleanupFinished,
        other => panic!("unknown consolidation stage {other}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn consolidation_sigkill_helper() {
    let Ok(data_dir) = std::env::var("MEMD_SIGKILL_DATA_DIR") else {
        return;
    };
    let source_id = memd::types::ChunkId::parse(
        &std::env::var("MEMD_SIGKILL_SOURCE_ID").expect("helper source id"),
    )
    .unwrap();
    let kill_at =
        stage_from_name(&std::env::var("MEMD_SIGKILL_STAGE").expect("helper consolidation stage"));
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(std::path::Path::new(&data_dir));
    let _ = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "sigkill-test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == kill_at {
                let mut killer = std::process::Command::new("kill")
                    .args(["-9", &std::process::id().to_string()])
                    .spawn()
                    .expect("spawn kill -9");
                let _ = killer.wait();
                loop {
                    std::thread::park();
                }
            }
            Ok(())
        },
    )
    .await;
    panic!("SIGKILL failpoint {kill_at:?} did not terminate helper");
}

#[cfg(unix)]
#[tokio::test]
async fn real_sigkill_at_every_durable_boundary_recovers_safely() {
    use std::os::unix::process::ExitStatusExt;

    for stage_name in [
        "journal_planned",
        "candidate_wal_appended",
        "candidate_metadata_inserted",
        "candidate_persisted",
        "candidates_recorded",
        "validated",
        "promoted",
        "sparse_cleanup_finished",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let tenant = TenantId::new("t").unwrap();
        let store = open_store(temp.path());
        let source_id = seed_source(&store, &tenant).await;
        drop(store);

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "consolidation_sigkill_helper",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MEMD_SIGKILL_DATA_DIR", temp.path())
            .env("MEMD_SIGKILL_SOURCE_ID", source_id.to_string())
            .env("MEMD_SIGKILL_STAGE", stage_name)
            .output()
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(9),
            "helper was not SIGKILLed at {stage_name}: status={} stderr={} stdout={}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );

        let reopened = open_store(temp.path());
        recover_consolidation_runs_before(&reopened, 100, i64::MAX)
            .await
            .unwrap();
        assert!(reopened
            .metadata()
            .list_recoverable_consolidation_runs(100)
            .unwrap()
            .is_empty());
        assert_eq!(
            reopened
                .metadata()
                .health_snapshot(&tenant, None, 10)
                .unwrap()
                .counts
                .candidate_chunks,
            0,
            "{stage_name} left a candidate visible to diagnostics"
        );

        let source = reopened
            .metadata()
            .get(&tenant, &source_id)
            .unwrap()
            .unwrap();
        let committed_summaries = reopened
            .metadata()
            .list(&tenant, 100, 0)
            .unwrap()
            .into_iter()
            .filter(|row| row.chunk_type == ChunkType::Summary && row.status == ChunkStatus::Final)
            .count();
        match source.status {
            ChunkStatus::Final => assert_eq!(committed_summaries, 0, "{stage_name}"),
            ChunkStatus::Superseded => assert_eq!(committed_summaries, 1, "{stage_name}"),
            other => panic!("{stage_name} left source in {other}"),
        }
    }
}

#[tokio::test]
async fn every_staged_failure_reopens_to_old_or_fully_committed_state() {
    for fail_at in [
        ConsolidationStage::JournalPlanned,
        ConsolidationStage::CandidateWalAppended,
        ConsolidationStage::CandidateMetadataInserted,
        ConsolidationStage::CandidatePersisted,
        ConsolidationStage::CandidatesRecorded,
        ConsolidationStage::Validated,
        ConsolidationStage::Promoted,
        ConsolidationStage::SparseCleanupFinished,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let tenant = TenantId::new("t").unwrap();
        let store = open_store(temp.path());
        let source_id = seed_source(&store, &tenant).await;
        let result = execute_consolidation_with_hook(
            &store,
            &tenant,
            Some("p"),
            &[entry(&source_id)],
            LineageRelation::Supersedes,
            "test",
            &[],
            "prompt",
            "response",
            |stage| {
                if stage == fail_at {
                    Err(MemdError::StorageError(format!(
                        "simulated crash at {stage:?}"
                    )))
                } else {
                    Ok(())
                }
            },
        )
        .await;
        assert!(result.is_err(), "failpoint {fail_at:?} did not fire");

        let source_before = store.metadata().get(&tenant, &source_id).unwrap().unwrap();
        let visible_before = store
            .search(&tenant, "durable consolidated fact", 10)
            .await
            .unwrap();
        if source_before.status == ChunkStatus::Superseded {
            assert_eq!(visible_before.len(), 1, "{fail_at:?} lost replacement");
        } else {
            assert_eq!(source_before.status, ChunkStatus::Final);
            assert!(visible_before.is_empty(), "{fail_at:?} exposed candidate");
        }

        drop(store);
        let reopened = open_store(temp.path());
        recover_consolidation_runs_before(&reopened, 100, i64::MAX)
            .await
            .unwrap();
        assert!(reopened
            .metadata()
            .list_recoverable_consolidation_runs(100)
            .unwrap()
            .is_empty());
        assert_eq!(
            reopened
                .metadata()
                .health_snapshot(&tenant, None, 10)
                .unwrap()
                .counts
                .candidate_chunks,
            0,
            "{fail_at:?} left a staged candidate"
        );

        let source_after = reopened
            .metadata()
            .get(&tenant, &source_id)
            .unwrap()
            .unwrap();
        let rows = reopened.metadata().list(&tenant, 100, 0).unwrap();
        let committed_summaries = rows
            .iter()
            .filter(|row| row.chunk_type == ChunkType::Summary && row.status == ChunkStatus::Final)
            .count();
        match source_after.status {
            ChunkStatus::Final => assert_eq!(committed_summaries, 0),
            ChunkStatus::Superseded => assert_eq!(committed_summaries, 1),
            other => panic!("{fail_at:?} left source in {other}"),
        }
    }
}

#[tokio::test]
async fn exact_source_set_reuses_committed_run_after_source_is_superseded() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let entries = vec![entry(&source_id)];

    let first = execute_consolidation(
        &store,
        &tenant,
        Some("p"),
        &entries,
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
    )
    .await
    .unwrap();
    let second = execute_consolidation(
        &store,
        &tenant,
        Some("p"),
        &entries,
        LineageRelation::Supersedes,
        "test",
        &[],
        "new prompt that must not duplicate",
        "different response that must not duplicate",
    )
    .await
    .unwrap();

    assert_eq!(first.state, ConsolidationState::Committed);
    assert_eq!(second.state, ConsolidationState::Committed);
    assert_eq!(
        store
            .get(&tenant, &first.candidate_chunk_ids[0])
            .await
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final,
        "payload status must reflect the authoritative promoted overlay"
    );
    assert_eq!(first.run_id, second.run_id);
    assert_eq!(first.candidate_chunk_ids, second.candidate_chunk_ids);
    assert!(second.reused_existing_run);
}

#[tokio::test]
async fn staged_run_stays_hidden_until_review_and_records_bounded_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let identity = ConsolidatorIdentity {
        adapter: "test-adapter".to_string(),
        command: Some("test-cli --model fixed".to_string()),
        model: Some("fixed-model".to_string()),
        version: Some("1.2.3".to_string()),
    };
    let raw_response = "x".repeat(300_000);

    let staged = execute_consolidation_with_identity(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        &identity,
        &[],
        "prompt",
        &raw_response,
        false,
    )
    .await
    .unwrap();
    assert_eq!(staged.state, ConsolidationState::Validated);
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &source_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
    assert!(store
        .search(&tenant, "durable consolidated fact", 10)
        .await
        .unwrap()
        .is_empty());

    let run = store
        .metadata()
        .get_consolidation_run(&staged.run_id)
        .unwrap()
        .unwrap();
    assert!(!run.promotion_requested);
    assert_eq!(run.consolidator_command, identity.command);
    assert_eq!(run.consolidator_model, identity.model);
    assert_eq!(run.consolidator_version, identity.version);
    let audit_path = temp.path().join(run.audit_artifact_path.unwrap());
    let audit: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&audit_path).unwrap()).unwrap();
    assert_eq!(audit["original_bytes"], raw_response.len());
    assert!(audit["stored_bytes"].as_u64().unwrap() < (256 * 1024) as u64);
    assert_eq!(audit["truncated"], true);
    assert!(std::fs::metadata(&audit_path).unwrap().len() <= (256 * 1024) as u64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&audit_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let recovery = recover_consolidation_runs_before(&store, 100, i64::MAX)
        .await
        .unwrap();
    assert_eq!(recovery.inspected, 0, "staged runs are not auto-promoted");
    let accepted =
        review_consolidation_run(&store, &staged.run_id, ConsolidationReviewDecision::Accept)
            .await
            .unwrap();
    assert_eq!(accepted.state, ConsolidationState::Committed);
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &source_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Superseded
    );
}

#[tokio::test]
async fn rejected_staged_run_never_hides_its_source() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let identity = ConsolidatorIdentity::internal("test");
    let staged = execute_consolidation_with_identity(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        &identity,
        &[],
        "prompt",
        "response",
        false,
    )
    .await
    .unwrap();
    let rejected =
        review_consolidation_run(&store, &staged.run_id, ConsolidationReviewDecision::Reject)
            .await
            .unwrap();
    assert_eq!(rejected.state, ConsolidationState::Rejected);
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &source_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
}

#[tokio::test]
async fn concurrent_accept_and_reject_report_exactly_one_winner() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = Arc::new(open_store(temp.path()));
    let source_id = seed_source(&store, &tenant).await;
    let staged = execute_consolidation_with_identity(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        &ConsolidatorIdentity::internal("test"),
        &[],
        "prompt",
        "response",
        false,
    )
    .await
    .unwrap();
    let accept_store = Arc::clone(&store);
    let accept_id = staged.run_id.clone();
    let accept = tokio::spawn(async move {
        review_consolidation_run(
            &accept_store,
            &accept_id,
            ConsolidationReviewDecision::Accept,
        )
        .await
    });
    let reject_store = Arc::clone(&store);
    let reject_id = staged.run_id.clone();
    let reject = tokio::spawn(async move {
        review_consolidation_run(
            &reject_store,
            &reject_id,
            ConsolidationReviewDecision::Reject,
        )
        .await
    });
    let accept = accept.await.unwrap();
    let reject = reject.await.unwrap();
    assert_ne!(accept.is_ok(), reject.is_ok());
    let final_state = store
        .metadata()
        .get_consolidation_run(&staged.run_id)
        .unwrap()
        .unwrap()
        .state;
    assert!(matches!(
        final_state,
        ConsolidationState::Committed | ConsolidationState::Rejected
    ));
}

#[tokio::test]
async fn transient_audit_read_failure_keeps_candidate_written_recoverable() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let interrupted = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == ConsolidationStage::CandidatesRecorded {
                Err(MemdError::StorageError(
                    "simulated interruption".to_string(),
                ))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(interrupted.is_err());
    let run = store
        .metadata()
        .list_recoverable_consolidation_runs(10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(run.state, ConsolidationState::CandidateWritten);
    let audit = temp.path().join(run.audit_artifact_path.as_ref().unwrap());
    let unavailable = audit.with_extension("unavailable");
    std::fs::rename(&audit, &unavailable).unwrap();
    let recovery = recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();
    assert_eq!(recovery.failed_recoverable, 1);
    assert_eq!(
        store
            .metadata()
            .get_consolidation_run(&run.run_id)
            .unwrap()
            .unwrap()
            .state,
        ConsolidationState::CandidateWritten
    );
    std::fs::rename(&unavailable, &audit).unwrap();
    recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();
    assert_eq!(
        store
            .metadata()
            .get_consolidation_run(&run.run_id)
            .unwrap()
            .unwrap()
            .state,
        ConsolidationState::Committed
    );
}

#[tokio::test]
async fn tampered_audit_body_blocks_explicit_promotion() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let staged = execute_consolidation_with_identity(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        &ConsolidatorIdentity::internal("test"),
        &[],
        "prompt",
        "response",
        false,
    )
    .await
    .unwrap();
    let run = store
        .metadata()
        .get_consolidation_run(&staged.run_id)
        .unwrap()
        .unwrap();
    let audit_path = temp.path().join(run.audit_artifact_path.unwrap());
    let mut audit: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&audit_path).unwrap()).unwrap();
    audit["raw_response"] = serde_json::Value::String("tampered".to_string());
    std::fs::write(&audit_path, serde_json::to_vec_pretty(&audit).unwrap()).unwrap();

    assert!(
        review_consolidation_run(&store, &staged.run_id, ConsolidationReviewDecision::Accept,)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &source_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
}

#[tokio::test]
async fn malformed_audit_json_is_terminally_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let interrupted = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == ConsolidationStage::CandidatesRecorded {
                Err(MemdError::StorageError(
                    "simulated interruption".to_string(),
                ))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(interrupted.is_err());
    let run = store
        .metadata()
        .list_recoverable_consolidation_runs(10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let audit_path = temp.path().join(run.audit_artifact_path.as_ref().unwrap());
    std::fs::write(audit_path, b"{not-json").unwrap();

    let recovery = recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();
    assert_eq!(recovery.rejected, 1);
    let rejected = store
        .metadata()
        .get_consolidation_run(&run.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(rejected.state, ConsolidationState::Rejected);
    assert!(rejected
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("not valid JSON"));
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &source_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
}

#[tokio::test]
async fn concurrent_exact_promotions_share_one_run_and_one_result() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = Arc::new(open_store(temp.path()));
    let source_id = seed_source(&store, &tenant).await;
    let entries = Arc::new(vec![entry(&source_id)]);

    let first_store = Arc::clone(&store);
    let first_tenant = tenant.clone();
    let first_entries = Arc::clone(&entries);
    let first = tokio::spawn(async move {
        execute_consolidation(
            &first_store,
            &first_tenant,
            Some("p"),
            &first_entries,
            LineageRelation::Supersedes,
            "test",
            &[],
            "prompt",
            "response",
        )
        .await
    });
    let second_store = Arc::clone(&store);
    let second_tenant = tenant.clone();
    let second_entries = Arc::clone(&entries);
    let second = tokio::spawn(async move {
        execute_consolidation(
            &second_store,
            &second_tenant,
            Some("p"),
            &second_entries,
            LineageRelation::Supersedes,
            "test",
            &[],
            "prompt",
            "response",
        )
        .await
    });
    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    assert_eq!(first.run_id, second.run_id);
    assert_eq!(first.state, ConsolidationState::Committed);
    assert_eq!(second.state, ConsolidationState::Committed);

    recover_consolidation_runs_before(&store, 100, i64::MAX)
        .await
        .unwrap();
    let runs = store
        .metadata()
        .get_consolidation_run(&first.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(runs.state, ConsolidationState::Committed);
    let rows = store.metadata().list(&tenant, 100, 0).unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| {
                row.chunk_type == ChunkType::Summary && row.status == ChunkStatus::Final
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn poisoned_run_does_not_block_later_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let poison_source = seed_source_with_text(&store, &tenant, "poison recovery source").await;
    let healthy_source = seed_source_with_text(&store, &tenant, "healthy recovery source").await;

    let poison = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&poison_source)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "poison prompt",
        "poison response",
        |stage| {
            if stage == ConsolidationStage::CandidatePersisted {
                Err(MemdError::StorageError("simulated crash".to_string()))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(poison.is_err());
    let poison_run = store
        .metadata()
        .list_recoverable_consolidation_runs(10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let poison_candidate = store
        .metadata()
        .get_consolidation_entries(&poison_run.run_id)
        .unwrap()[0]
        .candidate_chunk_id
        .clone()
        .unwrap();

    let healthy = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&healthy_source)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "healthy prompt",
        "healthy response",
        |stage| {
            if stage == ConsolidationStage::JournalPlanned {
                Err(MemdError::StorageError("simulated crash".to_string()))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(healthy.is_err());
    let runs = store
        .metadata()
        .list_recoverable_consolidation_runs(10)
        .unwrap();
    let healthy_run = runs
        .iter()
        .find(|run| run.run_id != poison_run.run_id)
        .unwrap()
        .clone();

    let connection = rusqlite::Connection::open(temp.path().join("metadata.db")).unwrap();
    connection
        .execute(
            "UPDATE chunks SET segment_id = 999999 WHERE chunk_id = ?1",
            [poison_candidate.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE consolidation_runs SET updated_at_ms = CASE run_id WHEN ?1 THEN 0 ELSE 1 END",
            [poison_run.run_id.to_string()],
        )
        .unwrap();
    drop(connection);

    let recovery = recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();
    assert_eq!(recovery.inspected, 2);
    assert_eq!(recovery.failed_recoverable, 1);
    assert_eq!(recovery.rolled_back, 1);
    let poison_after = store
        .metadata()
        .get_consolidation_run(&poison_run.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(poison_after.state, ConsolidationState::Planned);
    assert!(poison_after.error.is_some());
    assert_eq!(
        store
            .metadata()
            .get_consolidation_run(&healthy_run.run_id)
            .unwrap()
            .unwrap()
            .state,
        ConsolidationState::RolledBack
    );
}

#[tokio::test]
async fn permanently_invalid_validated_run_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let result = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == ConsolidationStage::Validated {
                Err(MemdError::StorageError("simulated crash".to_string()))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(result.is_err());
    let run = store
        .metadata()
        .list_recoverable_consolidation_runs(10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let candidate_id = store
        .metadata()
        .get_consolidation_entries(&run.run_id)
        .unwrap()[0]
        .candidate_chunk_id
        .clone()
        .unwrap();
    store.delete(&tenant, &source_id).await.unwrap();

    let recovery = recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();
    assert_eq!(recovery.rejected, 1);
    assert_eq!(
        store
            .metadata()
            .get_consolidation_run(&run.run_id)
            .unwrap()
            .unwrap()
            .state,
        ConsolidationState::Rejected
    );
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &candidate_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Error
    );
    assert!(store
        .metadata()
        .list_hard_purge_candidates(&tenant, Some("p"), i64::MAX, 10)
        .unwrap()
        .iter()
        .any(|row| row.chunk_id == candidate_id));
}

#[tokio::test]
async fn partial_multi_entry_payload_set_rolls_back_as_one_unit() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let first_source = seed_source_with_text(&store, &tenant, "first source").await;
    let second_source = seed_source_with_text(&store, &tenant, "second source").await;
    let entries = vec![entry(&first_source), entry(&second_source)];
    let result = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &entries,
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == ConsolidationStage::CandidatePersisted {
                Err(MemdError::StorageError("simulated crash".to_string()))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(result.is_err());

    let recovery = recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();
    assert_eq!(recovery.rolled_back, 1);
    assert_eq!(
        store
            .metadata()
            .health_snapshot(&tenant, None, 10)
            .unwrap()
            .counts
            .candidate_chunks,
        0
    );
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &first_source)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &second_source)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
}

#[tokio::test]
async fn failed_recoverable_run_retries_to_commit() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let result = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == ConsolidationStage::Validated {
                Err(MemdError::StorageError("simulated crash".to_string()))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(result.is_err());
    let run = store
        .metadata()
        .list_recoverable_consolidation_runs(10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(store
        .metadata()
        .transition_consolidation_run(
            &run.run_id,
            ConsolidationState::Validated,
            ConsolidationState::FailedRecoverable,
            0,
            None,
            Some("transient promotion failure"),
        )
        .unwrap());

    let recovery = recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();
    assert_eq!(recovery.committed, 1);
    assert_eq!(
        store
            .metadata()
            .get_consolidation_run(&run.run_id)
            .unwrap()
            .unwrap()
            .state,
        ConsolidationState::Committed
    );
}

#[tokio::test]
async fn fresh_planned_run_is_not_claimed_by_default_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let result = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == ConsolidationStage::JournalPlanned {
                Err(MemdError::StorageError("simulated pause".to_string()))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(result.is_err());
    let recovery = memd::consolidate::service::recover_consolidation_runs(&store, 10)
        .await
        .unwrap();
    assert_eq!(recovery.inspected, 0);
    assert_eq!(
        store
            .metadata()
            .list_recoverable_consolidation_runs(10)
            .unwrap()[0]
            .state,
        ConsolidationState::Planned
    );
}

#[tokio::test]
async fn candidate_written_after_terminal_rollback_is_hidden_on_next_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;
    let result = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == ConsolidationStage::JournalPlanned {
                Err(MemdError::StorageError("simulated pause".to_string()))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(result.is_err());
    let run = store
        .metadata()
        .list_recoverable_consolidation_runs(10)
        .unwrap()[0]
        .clone();
    let candidate_id = store
        .metadata()
        .get_consolidation_entries(&run.run_id)
        .unwrap()[0]
        .candidate_chunk_id
        .clone()
        .unwrap();
    recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();

    let mut late_candidate = MemoryChunk::new(
        tenant.clone(),
        "late candidate after rollback",
        ChunkType::Summary,
    )
    .with_project(ProjectId::from("p"))
    .with_status(ChunkStatus::Candidate);
    late_candidate.chunk_id = candidate_id.clone();
    store
        .add_consolidation_candidate(late_candidate)
        .await
        .unwrap();
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &candidate_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Candidate
    );

    recover_consolidation_runs_before(&store, 10, i64::MAX)
        .await
        .unwrap();
    assert_eq!(
        store
            .metadata()
            .get(&tenant, &candidate_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Error
    );
}

#[tokio::test]
async fn dense_only_commit_marks_sparse_cleanup_complete_when_no_index_exists() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = seed_source(&store, &tenant).await;

    let execution = execute_consolidation(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
    )
    .await
    .unwrap();

    assert!(
        store
            .metadata()
            .get_consolidation_run(&execution.run_id)
            .unwrap()
            .unwrap()
            .sparse_cleanup_done
    );
    assert!(store
        .metadata()
        .list_consolidation_runs_pending_sparse_cleanup(10)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn empty_project_scope_reuses_tenant_wide_run() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(temp.path());
    let source_id = store
        .add(MemoryChunk::new(
            tenant.clone(),
            "tenant-wide source",
            ChunkType::Doc,
        ))
        .await
        .unwrap();

    let first = execute_consolidation(
        &store,
        &tenant,
        None,
        &[entry(&source_id)],
        LineageRelation::DerivesFrom,
        "test",
        &[],
        "prompt",
        "response",
    )
    .await
    .unwrap();
    let second = execute_consolidation(
        &store,
        &tenant,
        Some(""),
        &[entry(&source_id)],
        LineageRelation::DerivesFrom,
        "test",
        &[],
        "different prompt",
        "different response",
    )
    .await
    .unwrap();

    assert_eq!(second.run_id, first.run_id);
    assert!(second.reused_existing_run);
}

#[tokio::test]
async fn sparse_cleanup_removes_source_and_drains_pending_journal() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_sparse_store(temp.path());
    let source_id = seed_source_with_text(&store, &tenant, "lexical cleanup sentinel").await;
    let sparse = store.sparse_index().expect("sparse index");
    sparse
        .insert(
            &tenant,
            &source_id,
            &["lexical cleanup sentinel".to_string()],
        )
        .unwrap();
    assert_eq!(sparse.search(&tenant, "sentinel", 5).unwrap().len(), 1);

    let execution = execute_consolidation(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
    )
    .await
    .unwrap();

    assert_eq!(execution.state, ConsolidationState::Committed);
    assert!(
        store
            .metadata()
            .get_consolidation_run(&execution.run_id)
            .unwrap()
            .unwrap()
            .sparse_cleanup_done
    );
    assert!(store
        .metadata()
        .list_consolidation_runs_pending_sparse_cleanup(10)
        .unwrap()
        .is_empty());
    assert!(sparse.search(&tenant, "sentinel", 5).unwrap().is_empty());
}

#[tokio::test]
async fn session_start_runs_bounded_recovery_before_context_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    let project_dir = temp.path().join("project");
    std::fs::create_dir_all(project_dir.join(".memd")).unwrap();
    std::fs::write(
        project_dir.join(".memd/project_scope.json"),
        serde_json::to_vec(&serde_json::json!({
            "tenant_id": "t",
            "project_id": "p",
            "interface": "cli",
            "cli_command": "memd",
            "agent_context_output": ".memd/context.md",
            "project_dir": project_dir,
        }))
        .unwrap(),
    )
    .unwrap();
    let tenant = TenantId::new("t").unwrap();
    let store = open_store(&data_dir);
    let source_id = seed_source(&store, &tenant).await;
    let result = execute_consolidation_with_hook(
        &store,
        &tenant,
        Some("p"),
        &[entry(&source_id)],
        LineageRelation::Supersedes,
        "test",
        &[],
        "prompt",
        "response",
        |stage| {
            if stage == ConsolidationStage::CandidatePersisted {
                Err(MemdError::StorageError("simulated crash".to_string()))
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(result.is_err());
    drop(store);

    // Production recovery deliberately ignores very fresh Planned runs to
    // avoid racing live candidate writes. Age this simulated crashed run so
    // session-start may claim it immediately.
    let connection = rusqlite::Connection::open(data_dir.join("metadata.db")).unwrap();
    connection
        .execute("UPDATE consolidation_runs SET updated_at_ms = 0", [])
        .unwrap();
    drop(connection);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_memd"))
        .arg("--data-dir")
        .arg(&data_dir)
        .args([
            "--search-variant",
            "bm25-only",
            "session-start",
            "--project-dir",
        ])
        .arg(&project_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "session-start failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        response.pointer("/consolidation_recovery/committed"),
        Some(&serde_json::json!(1)),
        "unexpected session-start response: {response}"
    );

    let reopened = open_store(&data_dir);
    assert_eq!(
        reopened
            .metadata()
            .get(&tenant, &source_id)
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Superseded
    );
}
