use super::*;
use crate::embeddings::MockEmbedder;
use crate::retrieval::{RerankerConfig, RerankerMode};
use crate::store::dense::{DenseSearchConfig, DenseSearcher};
use crate::store::hybrid::{HybridConfig, HybridSearcher};
use crate::store::metadata::MetadataStore;
use crate::store::Store;
use crate::task_memory::{build_task_projections, TaskArtifact, TaskSearchFilters};
use crate::types::{ChunkType, ProjectId};
use rusqlite::Connection;
use std::fs;
use std::sync::Arc;
use tempfile::tempdir;

#[tokio::test]
async fn sparse_only_store_searches_without_a_dense_index() {
    let dir = tempdir().unwrap();
    let store = PersistentStore::open(PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: true,
        enable_tiered_search: false,
        hybrid_config: Some(HybridConfig {
            dense_k: 0,
            enable_sparse: true,
            enable_rerank: false,
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    assert!(store.dense_searcher.is_none());
    assert!(store.hybrid_searcher.is_some());

    let tenant = TenantId::new("sparse_only").unwrap();
    let chunk_id = store
        .add(MemoryChunk::new(
            tenant.clone(),
            "lexical zanzibar sentinel",
            ChunkType::Doc,
        ))
        .await
        .unwrap();
    let results = store.search(&tenant, "zanzibar", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, chunk_id);
}

#[test]
fn sparse_only_store_fails_when_the_sparse_index_cannot_open() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("sparse_index"), b"not a directory").unwrap();
    let result = PersistentStore::open(PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: true,
        enable_tiered_search: false,
        hybrid_config: Some(HybridConfig {
            dense_k: 0,
            enable_sparse: true,
            enable_rerank: false,
            ..Default::default()
        }),
        ..Default::default()
    });
    let error = match result {
        Ok(_) => panic!("invalid sparse index path must fail closed"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("sparse-only search requires a readable BM25 index"));
}

#[tokio::test]
async fn sparse_only_read_only_missing_store_is_empty_without_creating_data_dir() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("missing");
    let store = PersistentStore::open(PersistentStoreConfig {
        data_dir: data_dir.clone(),
        read_only: true,
        enable_dense_search: false,
        enable_hybrid_search: true,
        enable_tiered_search: false,
        hybrid_config: Some(HybridConfig {
            dense_k: 0,
            enable_sparse: true,
            enable_rerank: false,
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();

    let tenant = TenantId::new("missing_sparse_only").unwrap();
    assert!(store
        .search(&tenant, "anything", 10)
        .await
        .unwrap()
        .is_empty());
    assert!(!data_dir.exists());
}

#[tokio::test]
async fn promoted_candidate_is_refreshed_into_a_cold_dense_index() {
    let dir = tempdir().unwrap();
    let mut store = PersistentStore::open(PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_tiered_search: false,
        backfill_hnsw_on_startup: false,
        ..Default::default()
    })
    .unwrap();
    let tenant = TenantId::new("refresh_promoted").unwrap();
    let candidate_id = store
        .add_consolidation_candidate(
            MemoryChunk::new(
                tenant.clone(),
                "promoted candidate dense refresh",
                ChunkType::Summary,
            )
            .with_status(ChunkStatus::Candidate),
        )
        .await
        .unwrap();

    let dense = Arc::new(DenseSearcher::with_embedder(
        Arc::new(MockEmbedder::new()),
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    store.dense_searcher = Some(Arc::clone(&dense));
    store
        .metadata
        .update_lifecycle(
            &tenant,
            &candidate_id,
            &LifecycleDelta {
                status: Some(ChunkStatus::Final),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!dense.contains_chunk(&tenant, &candidate_id));

    store
        .refresh_promoted_chunks(&tenant, std::slice::from_ref(&candidate_id))
        .await
        .unwrap();
    assert!(dense.contains_chunk(&tenant, &candidate_id));
    assert_eq!(
        store
            .get(&tenant, &candidate_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ChunkStatus::Final
    );
}

#[tokio::test]
async fn dense_hybrid_search_never_returns_physically_indexed_candidate() {
    let dir = tempdir().unwrap();
    let mut store = PersistentStore::open(PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_tiered_search: false,
        backfill_hnsw_on_startup: false,
        ..Default::default()
    })
    .unwrap();
    let dense = Arc::new(DenseSearcher::with_embedder(
        Arc::new(MockEmbedder::new()),
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let hybrid = HybridSearcher::new(
        Arc::clone(&dense),
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            ..Default::default()
        },
    );
    store.dense_searcher = Some(Arc::clone(&dense));
    store.hybrid_searcher = Some(Arc::new(hybrid));
    let tenant = TenantId::new("candidate_dense").unwrap();
    let visible_id = store
        .add(MemoryChunk::new(
            tenant.clone(),
            "dense candidate visibility control",
            ChunkType::Doc,
        ))
        .await
        .unwrap();
    let candidate_id = store
        .add_consolidation_candidate(
            MemoryChunk::new(
                tenant.clone(),
                "dense candidate visibility staged",
                ChunkType::Summary,
            )
            .with_status(ChunkStatus::Candidate),
        )
        .await
        .unwrap();
    assert!(dense.contains_chunk(&tenant, &candidate_id));

    let results = store
        .search_with_scores(&tenant, "dense candidate visibility", 10)
        .await
        .unwrap();
    let ids = results
        .into_iter()
        .map(|(chunk, _)| chunk.chunk_id)
        .collect::<Vec<_>>();
    assert!(ids.contains(&visible_id));
    assert!(!ids.contains(&candidate_id));
}

#[test]
fn warm_worker_defaults_enable_async_indexing_only_when_env_unset() {
    // Env unset → the worker turns async indexing on for availability.
    let mut config = PersistentStoreConfig {
        enable_async_indexing: false,
        ..Default::default()
    };
    config.apply_warm_worker_availability_defaults(None);
    assert!(config.enable_async_indexing);

    // An explicit operator setting wins, in both directions.
    let mut config = PersistentStoreConfig {
        enable_async_indexing: false,
        ..Default::default()
    };
    config.apply_warm_worker_availability_defaults(Some("0"));
    assert!(!config.enable_async_indexing);

    let mut config = PersistentStoreConfig {
        enable_async_indexing: true,
        ..Default::default()
    };
    config.apply_warm_worker_availability_defaults(Some("1"));
    assert!(config.enable_async_indexing);
}

fn make_test_store() -> (PersistentStore, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 10,
        enable_dense_search: false, // Disable for unit tests
        enable_hybrid_search: false,
        // Keep the shared single-flight slot free at open() so probe/repair
        // tests are deterministic (a startup backfill would otherwise hold
        // the in-flight flag until the first runtime poll).
        backfill_hnsw_on_startup: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    (store, dir)
}

fn make_tenant() -> TenantId {
    TenantId::new("test_tenant").unwrap()
}

fn make_chunk(tenant: &TenantId, text: &str) -> MemoryChunk {
    MemoryChunk::new(tenant.clone(), text, ChunkType::Doc)
}

#[test]
fn ensure_writable_bumps_write_epoch() {
    let (store, _dir) = make_test_store();
    let before = store.write_epoch.load(std::sync::atomic::Ordering::Acquire);

    store.ensure_writable("test_write").unwrap();
    let after_first = store.write_epoch.load(std::sync::atomic::Ordering::Acquire);
    store.ensure_writable("test_write").unwrap();
    let after_second = store.write_epoch.load(std::sync::atomic::Ordering::Acquire);

    assert!(after_first > before);
    assert!(after_second > after_first);
}

#[test]
fn ensure_writable_read_only_does_not_bump_write_epoch() {
    let dir = tempdir().unwrap();
    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        drop(store);
    }

    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        read_only: true,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    let before = store.write_epoch.load(std::sync::atomic::Ordering::Acquire);
    let error = store.ensure_writable("test_write").unwrap_err();
    let after = store.write_epoch.load(std::sync::atomic::Ordering::Acquire);

    assert!(matches!(error, MemdError::ReadOnlyStore { .. }));
    assert_eq!(after, before);
}

#[tokio::test]
async fn probe_reports_own_writes_not_external() {
    let (store, _dir) = make_test_store();
    let tenant = make_tenant();

    assert_eq!(
        store.probe_external_mutation().await,
        ExternalMutationOutcome::Clean
    );
    store
        .add(make_chunk(&tenant, "probe own write"))
        .await
        .unwrap();

    assert_eq!(
        store.probe_external_mutation().await,
        ExternalMutationOutcome::OwnWrites
    );
    assert_eq!(
        store.probe_external_mutation().await,
        ExternalMutationOutcome::Clean
    );
    let stats = store.ryw_probe_stats().unwrap();
    assert_eq!(stats.checks, 3);
    assert_eq!(stats.external_detected, 0);
    assert_eq!(stats.repairs, 0);
}

#[tokio::test]
async fn probe_detects_external_connection_write() {
    let (store, dir) = make_test_store();

    assert_eq!(
        store.probe_external_mutation().await,
        ExternalMutationOutcome::Clean
    );

    let conn = Connection::open(dir.path().join("metadata.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ryw_probe_external_test(x INTEGER)",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO ryw_probe_external_test(x) VALUES (1)", [])
        .unwrap();
    drop(conn);

    // The repair is now scheduled as a store-owned background task rather
    // than awaited in the foreground. With dense search disabled the
    // backfill is an instant no-op, so it finishes within the foreground
    // budget and reports `repaired: true`; `repairs` is counted on completion.
    assert_eq!(
        store.probe_external_mutation().await,
        ExternalMutationOutcome::External { repaired: true }
    );
    let stats = store.ryw_probe_stats().unwrap();
    assert_eq!(stats.checks, 2);
    assert_eq!(stats.external_detected, 1);
    assert_eq!(stats.repairs, 1);
    assert!(!stats.repair_in_progress);
}

#[tokio::test]
async fn probe_baseline_is_after_startup_wal_recovery() {
    let dir = tempdir().unwrap();
    let config = || PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        min_finalize_chunks: 256,
        wal_checkpoint_interval: 0,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let tenant = make_tenant();
    {
        let store = PersistentStore::open(config()).unwrap();
        store
            .add(make_chunk(&tenant, "chunk recovered before probe baseline"))
            .await
            .unwrap();
    }

    let store = PersistentStore::open(config()).unwrap();

    assert_eq!(
        store.probe_external_mutation().await,
        ExternalMutationOutcome::Clean
    );
    let stats = store.ryw_probe_stats().unwrap();
    assert_eq!(stats.checks, 1);
    assert_eq!(stats.external_detected, 0);
    assert_eq!(stats.repairs, 0);
}

#[tokio::test]
async fn read_only_store_probe_is_unavailable() {
    let dir = tempdir().unwrap();
    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        drop(store);
    }

    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        read_only: true,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();

    assert_eq!(
        store.probe_external_mutation().await,
        ExternalMutationOutcome::Unavailable
    );
    assert_eq!(store.ryw_probe_stats(), None);
}

#[tokio::test]
async fn hnsw_repair_is_single_flight() {
    let (store, _dir) = make_test_store();

    // The first schedule wins the single-flight race and spawns the
    // repair; a second schedule issued before the task is polled sees the
    // in-flight flag and schedules nothing. (Current-thread test runtime:
    // the spawned task does not run until the await below.)
    let first = store.schedule_hnsw_repair(RepairKind::Probe);
    let second = store.schedule_hnsw_repair(RepairKind::Probe);
    assert!(matches!(first, RepairSchedule::Scheduled(_)));
    assert!(matches!(second, RepairSchedule::AlreadyInFlight));

    // Draining the first repair resets the flag, so a later schedule wins
    // again — the guard is not stuck after completion.
    if let RepairSchedule::Scheduled(rx) = first {
        assert!(matches!(rx.await, Ok(true)));
    }
    assert!(!store.repair_state.repair_in_progress());
    let third = store.schedule_hnsw_repair(RepairKind::Probe);
    assert!(matches!(third, RepairSchedule::Scheduled(_)));
}

#[tokio::test]
async fn probe_does_not_block_when_repair_in_flight() {
    let (store, dir) = make_test_store();

    // Occupy the single-flight slot with a repair that has not been polled
    // yet (holding its handle keeps the in-flight flag set on the
    // current-thread runtime until we await).
    let _held = store.schedule_hnsw_repair(RepairKind::Probe);
    assert!(store.repair_state.repair_in_progress());

    // Make metadata.db look externally mutated.
    let conn = Connection::open(dir.path().join("metadata.db")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ryw_probe_inflight_test(x INTEGER)",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO ryw_probe_inflight_test(x) VALUES (1)", [])
        .unwrap();
    drop(conn);

    // The probe detects the external mutation but a repair is already
    // running, so it serves immediately as `InFlight` instead of
    // scheduling or blocking on another backfill.
    assert_eq!(
        store.probe_external_mutation().await,
        ExternalMutationOutcome::External { repaired: false }
    );
    assert!(store.ryw_probe_stats().unwrap().repair_in_progress);
}

#[test]
fn repair_bookkeeping_rearms_pending_then_releases() {
    let state = HnswRepairState::default();

    // First owner claims the single-flight slot.
    assert!(state.try_begin_or_arm(RepairKind::Probe));
    assert!(state.repair_in_progress());

    // A probe arriving mid-repair is coalesced but arms a follow-up pass
    // (this is the regression guard: the signal must not be dropped).
    assert!(!state.try_begin_or_arm(RepairKind::Probe));

    // The running repair sees `pending` and must do another pass; the slot
    // stays held so no second task can start.
    assert!(state.finish_or_continue());
    assert!(state.repair_in_progress());

    // No further arming -> the next finish releases the slot.
    assert!(!state.finish_or_continue());
    assert!(!state.repair_in_progress());

    // A startup backfill losing the race does NOT arm a follow-up pass
    // (only externally-observed writes need one).
    assert!(state.try_begin_or_arm(RepairKind::Probe));
    assert!(!state.try_begin_or_arm(RepairKind::Startup));
    assert!(!state.finish_or_continue());
    assert!(!state.repair_in_progress());
}

fn make_long_document() -> String {
    let sentence = "This is a long test sentence that should trigger document chunking behavior. ";
    sentence.repeat(40)
}

fn segment_payload_path(
    base_dir: &std::path::Path,
    tenant: &TenantId,
    segment_id: u64,
) -> std::path::PathBuf {
    base_dir
        .join("tenants")
        .join(tenant.as_str())
        .join("segments")
        .join(format!("seg_{:06}", segment_id))
        .join("payload.bin")
}

fn segment_index_path(
    base_dir: &std::path::Path,
    tenant: &TenantId,
    segment_id: u64,
) -> std::path::PathBuf {
    base_dir
        .join("tenants")
        .join(tenant.as_str())
        .join("segments")
        .join(format!("seg_{:06}", segment_id))
        .join("payload.idx")
}

fn corrupt_segment_payload(base_dir: &std::path::Path, tenant: &TenantId, segment_id: u64) {
    let payload_path = segment_payload_path(base_dir, tenant, segment_id);
    let mut bytes = fs::read(&payload_path).unwrap();
    assert!(!bytes.is_empty(), "payload file must not be empty");
    bytes[0] ^= 0xFF;
    fs::write(payload_path, bytes).unwrap();
}

#[test]
fn default_config_has_valid_async_indexing_settings() {
    let config = PersistentStoreConfig::default();
    assert!(config.async_index_batch_size > 0);
    assert!(config.async_index_poll_ms > 0);
}

#[tokio::test]
async fn async_indexer_scaffold_is_created_when_enabled() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_async_indexing: true,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    assert!(store.async_indexing_enabled());
}

#[tokio::test]
async fn add_and_get() {
    let (store, _dir) = make_test_store();
    let tenant = make_tenant();
    let chunk = make_chunk(&tenant, "hello persistent");

    let chunk_id = store.add(chunk).await.unwrap();
    let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().text, "hello persistent");
}

#[tokio::test]
async fn add_marks_indexed_when_async_indexing_disabled() {
    let (store, _dir) = make_test_store();
    let tenant = make_tenant();

    store
        .add(make_chunk(&tenant, "indexed state check"))
        .await
        .unwrap();

    let (pending, indexed, failed) = store.metadata.count_by_index_state(&tenant).unwrap();
    assert_eq!(pending, 0);
    assert_eq!(indexed, 1);
    assert_eq!(failed, 0);
}

#[tokio::test]
async fn add_async_eventually_marks_indexed() {
    let dir = tempdir().unwrap();
    let tenant = make_tenant();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_async_indexing: true,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();

    store
        .add(make_chunk(&tenant, "pending state check"))
        .await
        .unwrap();

    // Async worker runs out-of-band; allow a short settle window.
    let mut saw_pending = false;
    let mut saw_indexed = false;
    for _ in 0..20 {
        let (pending, indexed, failed) = store.metadata.count_by_index_state(&tenant).unwrap();
        assert_eq!(failed, 0);
        if pending > 0 {
            saw_pending = true;
        }
        if indexed > 0 {
            saw_indexed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        saw_pending || saw_indexed,
        "chunk should appear in pending or indexed states"
    );
    assert!(
        saw_indexed,
        "async worker should eventually mark chunk indexed"
    );
}

#[tokio::test]
async fn pending_chunks_are_recovered_by_worker_sweep() {
    let dir = tempdir().unwrap();
    let tenant = make_tenant();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_async_indexing: true,
        async_index_poll_ms: 25,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    let chunk_id = store
        .add(make_chunk(&tenant, "sweep pending recovery"))
        .await
        .unwrap();

    // Wait until initial async indexing completes.
    for _ in 0..20 {
        let (_, indexed, failed) = store.metadata.count_by_index_state(&tenant).unwrap();
        assert_eq!(failed, 0);
        if indexed > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    store
        .metadata
        .mark_index_pending(&tenant, std::slice::from_ref(&chunk_id), current_time_ms())
        .unwrap();

    let mut recovered = false;
    for _ in 0..25 {
        let (pending, indexed, failed) = store.metadata.count_by_index_state(&tenant).unwrap();
        assert_eq!(failed, 0);
        if pending == 0 && indexed > 0 {
            recovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(recovered, "worker sweep should re-index pending chunks");
}

#[tokio::test]
async fn tenant_isolation() {
    let (store, _dir) = make_test_store();
    let tenant_a = TenantId::new("tenant_a").unwrap();
    let tenant_b = TenantId::new("tenant_b").unwrap();

    let chunk = make_chunk(&tenant_a, "secret");
    let chunk_id = store.add(chunk).await.unwrap();

    // Tenant B cannot see tenant A's chunk
    let result = store.get(&tenant_b, &chunk_id).await.unwrap();
    assert!(result.is_none());

    // Search isolation
    let results = store.search(&tenant_b, "secret", 10).await.unwrap();
    assert!(results.is_empty());
}

/// Bug B defense-in-depth: if `tenant.segments` drops a finalized entry
/// for any reason (observed in prod, unreliable to reproduce from a
/// rollover race), `get_chunk` must still serve the read by opening the
/// segment on demand AND it must repopulate the cache for next time.
#[tokio::test]
async fn get_chunk_recovers_when_segments_cache_loses_entry() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1, // force rollover so the first chunk finalizes
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    let tenant = make_tenant();

    // With segment_max_chunks=1, the second add triggers rollover, which
    // finalizes the first chunk's segment and registers its reader in
    // `tenant.segments`.
    let finalized_id = store
        .add(make_chunk(
            &tenant,
            "finalized bytes that must survive cache drift",
        ))
        .await
        .unwrap();
    let _ = store
        .add(make_chunk(&tenant, "subsequent chunk forces rollover"))
        .await
        .unwrap();

    // Find the tenant store and the segment id that holds the first chunk.
    let meta = store
        .metadata
        .get(&tenant, &finalized_id)
        .unwrap()
        .expect("metadata row for finalized chunk");
    let tenant_store = store
        .tenants
        .read()
        .get(tenant.as_str())
        .cloned()
        .expect("tenant store must exist after an add");

    // Simulate the observed production failure: the reader for this
    // segment disappears from the cache.
    {
        let mut segments = tenant_store.segments.write();
        let removed = segments.remove(&meta.segment_id);
        assert!(
            removed.is_some(),
            "expected finalized reader for segment {} to be cached before removal",
            meta.segment_id
        );
    }

    // The read must still succeed — on-demand open from disk.
    let recovered = store
        .get(&tenant, &finalized_id)
        .await
        .expect("get_chunk must succeed via on-demand open");
    assert!(
        recovered.is_some(),
        "get_chunk returned None despite segment files existing on disk"
    );

    // And the cache must be repopulated so subsequent reads skip the
    // on-demand open.
    assert!(
        tenant_store.segments.read().contains_key(&meta.segment_id),
        "on-demand open must repopulate tenant.segments"
    );
}

#[tokio::test]
async fn soft_delete() {
    let (store, _dir) = make_test_store();
    let tenant = make_tenant();
    let chunk = make_chunk(&tenant, "to delete");

    let chunk_id = store.add(chunk).await.unwrap();
    let deleted = store.delete(&tenant, &chunk_id).await.unwrap();
    assert!(deleted);

    // Chunk no longer retrievable
    let result = store.get(&tenant, &chunk_id).await.unwrap();
    assert!(result.is_none());

    // Not in search results
    let results = store.search(&tenant, "delete", 10).await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn text_search_skips_crc_corrupted_active_chunk() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1,
        // Match the test's intent: it explicitly forces rollover
        // via segment_max_chunks=1 so each shutdown should finalize
        // the active segment. Opt out of the default 256-chunk gate
        // so the test exercises the rollover/finalize path it cares
        // about, not the segment-proliferation guard.
        min_finalize_chunks: 1,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    let tenant = make_tenant();

    let healthy_id = store
        .add(make_chunk(&tenant, "healthy finalized chunk"))
        .await
        .unwrap();
    let corrupted_id = store
        .add(make_chunk(&tenant, "corrupted active chunk"))
        .await
        .unwrap();

    let warmup = store.get(&tenant, &corrupted_id).await.unwrap();
    assert!(warmup.is_some());

    let corrupt_meta = store
        .metadata
        .get(&tenant, &corrupted_id)
        .unwrap()
        .expect("corrupted chunk metadata");
    corrupt_segment_payload(dir.path(), &tenant, corrupt_meta.segment_id);

    let results = store.search(&tenant, "chunk", 10).await.unwrap();
    let result_ids = results
        .into_iter()
        .map(|chunk| chunk.chunk_id)
        .collect::<Vec<_>>();

    assert!(result_ids.contains(&healthy_id));
    assert!(!result_ids.contains(&corrupted_id));
}

#[tokio::test]
async fn list_chunks_skips_unreadable_finalized_segment() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1,
        // Match the test's intent: it explicitly forces rollover
        // via segment_max_chunks=1 so each shutdown should finalize
        // the active segment. Opt out of the default 256-chunk gate
        // so the test exercises the rollover/finalize path it cares
        // about, not the segment-proliferation guard.
        min_finalize_chunks: 1,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let tenant = make_tenant();
    let unreadable_id;
    let healthy_id;
    let unreadable_segment_id;

    {
        let store = PersistentStore::open(config.clone()).unwrap();
        unreadable_id = store
            .add(make_chunk(&tenant, "unreadable context chunk"))
            .await
            .unwrap();
        healthy_id = store
            .add(make_chunk(&tenant, "healthy context chunk"))
            .await
            .unwrap();
        unreadable_segment_id = store
            .metadata
            .get(&tenant, &unreadable_id)
            .unwrap()
            .expect("unreadable chunk metadata")
            .segment_id;
    }

    fs::remove_file(segment_index_path(
        dir.path(),
        &tenant,
        unreadable_segment_id,
    ))
    .unwrap();
    fs::write(
        dir.path()
            .join("tenants")
            .join(tenant.as_str())
            .join("wal.log"),
        [],
    )
    .unwrap();

    let store = PersistentStore::open(config).unwrap();
    let chunks = store.list_chunks(&tenant, 10, 0).await.unwrap();
    let chunk_ids = chunks
        .into_iter()
        .map(|chunk| chunk.chunk_id)
        .collect::<Vec<_>>();

    assert!(chunk_ids.contains(&healthy_id));
    assert!(!chunk_ids.contains(&unreadable_id));
}

#[tokio::test]
async fn list_chunks_for_project_filters_metadata_before_payload_reads() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1,
        // Match the test's intent: it explicitly forces rollover
        // via segment_max_chunks=1 so each shutdown should finalize
        // the active segment. Opt out of the default 256-chunk gate
        // so the test exercises the rollover/finalize path it cares
        // about, not the segment-proliferation guard.
        min_finalize_chunks: 1,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let tenant = make_tenant();
    let wanted_id;
    let unrelated_id;

    {
        let store = PersistentStore::open(config.clone()).unwrap();
        wanted_id = store
            .add(
                make_chunk(&tenant, "wanted project context")
                    .with_project(ProjectId::new(Some("wanted"))),
            )
            .await
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        unrelated_id = store
            .add(
                make_chunk(&tenant, "newer unrelated context")
                    .with_project(ProjectId::new(Some("other"))),
            )
            .await
            .unwrap();
    }

    let unrelated_segment_id = {
        let store = PersistentStore::open(config.clone()).unwrap();
        store
            .metadata
            .get(&tenant, &unrelated_id)
            .unwrap()
            .expect("unrelated chunk metadata")
            .segment_id
    };
    fs::remove_file(segment_index_path(
        dir.path(),
        &tenant,
        unrelated_segment_id,
    ))
    .unwrap();

    let store = PersistentStore::open(config).unwrap();
    let chunks = store
        .list_chunks_for_project(&tenant, Some("wanted"), 1, 0)
        .await
        .unwrap();

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk_id, wanted_id);
}

#[tokio::test]
async fn text_search_skips_crc_corrupted_finalized_chunk() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1,
        // Match the test's intent: it explicitly forces rollover
        // via segment_max_chunks=1 so each shutdown should finalize
        // the active segment. Opt out of the default 256-chunk gate
        // so the test exercises the rollover/finalize path it cares
        // about, not the segment-proliferation guard.
        min_finalize_chunks: 1,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let tenant = make_tenant();
    let store = PersistentStore::open(config).unwrap();
    let corrupted_id = store
        .add(make_chunk(&tenant, "corrupted finalized chunk"))
        .await
        .unwrap();
    let healthy_id = store
        .add(make_chunk(&tenant, "healthy finalized chunk"))
        .await
        .unwrap();
    let corrupted_segment_id = store
        .metadata
        .get(&tenant, &corrupted_id)
        .unwrap()
        .expect("corrupted chunk metadata")
        .segment_id;

    corrupt_segment_payload(dir.path(), &tenant, corrupted_segment_id);
    let results = store.search(&tenant, "chunk", 10).await.unwrap();
    let result_ids = results
        .into_iter()
        .map(|chunk| chunk.chunk_id)
        .collect::<Vec<_>>();

    assert!(result_ids.contains(&healthy_id));
    assert!(!result_ids.contains(&corrupted_id));
}

#[tokio::test]
async fn hybrid_search_skips_crc_corrupted_active_chunk() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1,
        // Match the test's intent: it explicitly forces rollover
        // via segment_max_chunks=1 so each shutdown should finalize
        // the active segment. Opt out of the default 256-chunk gate
        // so the test exercises the rollover/finalize path it cares
        // about, not the segment-proliferation guard.
        min_finalize_chunks: 1,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let mut store = PersistentStore::open(config).unwrap();
    let tenant = make_tenant();

    let embedder = Arc::new(MockEmbedder::new());
    let dense = Arc::new(DenseSearcher::with_embedder(
        embedder,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let hybrid = HybridSearcher::new(
        dense,
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            reranker: RerankerConfig {
                mode: RerankerMode::Feature,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    store.hybrid_searcher = Some(Arc::new(hybrid));

    let healthy_id = store
        .add(make_chunk(&tenant, "healthy hybrid retrieval chunk"))
        .await
        .unwrap();
    let corrupted_id = store
        .add(make_chunk(&tenant, "corrupted hybrid retrieval chunk"))
        .await
        .unwrap();

    let warmup = store.get(&tenant, &corrupted_id).await.unwrap();
    assert!(warmup.is_some());

    let corrupt_meta = store
        .metadata
        .get(&tenant, &corrupted_id)
        .unwrap()
        .expect("corrupted chunk metadata");
    corrupt_segment_payload(dir.path(), &tenant, corrupt_meta.segment_id);

    let results = store.search(&tenant, "retrieval", 10).await.unwrap();
    let result_ids = results
        .into_iter()
        .map(|chunk| chunk.chunk_id)
        .collect::<Vec<_>>();

    assert!(result_ids.contains(&healthy_id));
    assert!(!result_ids.contains(&corrupted_id));
}

#[tokio::test]
async fn persistence_across_restarts() {
    let dir = tempdir().unwrap();
    let tenant = make_tenant();
    let chunk_id;

    // First session: add chunk
    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        let chunk = make_chunk(&tenant, "persistent data");
        chunk_id = store.add(chunk).await.unwrap();

        // Drop triggers finalization
        drop(store);
    }

    // Second session: retrieve chunk
    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 10,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

        // Chunk survives restart
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().text, "persistent data");
    }
}

#[tokio::test]
async fn wal_recovery_after_crash() {
    let dir = tempdir().unwrap();
    let tenant = make_tenant();
    let chunk_id;

    // First session: add chunk but simulate crash (no finalization)
    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 10,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        let chunk = make_chunk(&tenant, "crash test data");
        chunk_id = store.add(chunk).await.unwrap();

        // Leave the active segment below the finalize threshold so
        // the next writer must recover from WAL, while still dropping
        // the store to release the process-wide writer flock.
        drop(store);
    }

    // Second session: should recover from WAL
    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

        // Chunk recovered from WAL
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().text, "crash test data");
    }
}

#[tokio::test]
async fn wal_recovery_rebuilds_task_side_tables() {
    let dir = tempdir().unwrap();
    let tenant = make_tenant();
    let metadata_path = dir.path().join("metadata.db");
    let task_id;
    let artifact_id;

    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();

        let mut artifact = TaskArtifact::new_task_start(tenant.clone());
        artifact.project_id = ProjectId::new(Some("project_alpha".to_string()));
        artifact.goal = Some("Map the perturbation-responsive genes".to_string());
        artifact.dataset_refs = vec![crate::task_memory::DatasetRef {
            name: "rna_seq".to_string(),
            version: Some("v1".to_string()),
            description: None,
        }];
        task_id = artifact.task_id.clone();
        artifact_id = artifact.artifact_id.clone();

        store
            .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
            .await
            .unwrap();

        let conn = Connection::open(&metadata_path).unwrap();
        conn.execute("DELETE FROM artifact_links", []).unwrap();
        conn.execute("DELETE FROM task_datasets", []).unwrap();
        conn.execute("DELETE FROM task_entities", []).unwrap();
        conn.execute("DELETE FROM task_events", []).unwrap();
        conn.execute("DELETE FROM tasks", []).unwrap();
        conn.execute("DELETE FROM task_artifacts", []).unwrap();
        drop(conn);

        drop(store);
    }

    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();

        let recovered = store
            .get_task_artifact(&tenant, &artifact_id)
            .await
            .unwrap();
        assert!(recovered.is_some());

        let artifacts = store.list_task_artifacts(&tenant, &task_id).await.unwrap();
        assert_eq!(artifacts.len(), 1);

        let chunk_ids = store
            .search_task_projection_chunk_ids(
                &tenant,
                &TaskSearchFilters {
                    task_id: Some(task_id.clone()),
                    ..Default::default()
                },
                10,
            )
            .await
            .unwrap();
        assert!(!chunk_ids.is_empty());
    }
}

#[tokio::test]
async fn rerank_chunks_for_query_uses_hybrid_reranker_for_candidate_set() {
    let (mut store, _dir) = make_test_store();
    let tenant = make_tenant();
    let now_ms = current_time_ms();

    let mut older = make_chunk(&tenant, "alpha beta exact lexical match");
    older.timestamp_created = now_ms - 30 * 24 * 60 * 60 * 1000;
    let older_id = store.add(older).await.unwrap();

    let mut newer = make_chunk(&tenant, "alpha parameter note");
    newer.timestamp_created = now_ms;
    let newer_id = store.add(newer).await.unwrap();

    let embedder = Arc::new(MockEmbedder::new());
    let dense = Arc::new(DenseSearcher::with_embedder(
        embedder,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let hybrid = HybridSearcher::new(
        dense,
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            reranker: RerankerConfig {
                mode: RerankerMode::Feature,
                rrf_weight: 0.0,
                recency_weight: 1.0,
                recency_half_life_days: 7.0,
                project_weight: 0.0,
                type_weight: 0.0,
                query_text_weight: 0.0,
                cross_encoder_weight: 0.0,
            },
            ..Default::default()
        },
    );
    store.hybrid_searcher = Some(Arc::new(hybrid));

    let ranked = store
        .rerank_chunks_for_query(&tenant, "alpha beta", &[older_id, newer_id.clone()], 2)
        .await
        .unwrap();

    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].0.chunk_id, newer_id);

    let candidate_ids = ranked
        .iter()
        .map(|(chunk, _)| chunk.chunk_id.clone())
        .collect::<Vec<_>>();
    let fixed =
        Store::rerank_chunks_for_query_at(&store, &tenant, "alpha beta", &candidate_ids, 2, now_ms)
            .await
            .unwrap();
    let replayed =
        Store::rerank_chunks_for_query_at(&store, &tenant, "alpha beta", &candidate_ids, 2, now_ms)
            .await
            .unwrap();
    let later = Store::rerank_chunks_for_query_at(
        &store,
        &tenant,
        "alpha beta",
        &candidate_ids,
        2,
        now_ms + 365 * 24 * 60 * 60 * 1000,
    )
    .await
    .unwrap();

    let identity = |rows: &[(MemoryChunk, f32)]| {
        rows.iter()
            .map(|(chunk, score)| (chunk.chunk_id.clone(), score.to_bits()))
            .collect::<Vec<_>>()
    };
    assert_eq!(identity(&fixed), identity(&replayed));
    assert_ne!(identity(&fixed), identity(&later));
}

#[tokio::test]
async fn fixed_ranking_time_matches_standard_and_tier_debug_paths() {
    let (mut store, _dir) = make_test_store();
    let tenant = make_tenant();
    let ranking_time_ms = 1_700_000_000_000i64;
    let embedder = Arc::new(MockEmbedder::new());
    let dense = Arc::new(DenseSearcher::with_embedder(
        embedder,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let hybrid = HybridSearcher::new(
        Arc::clone(&dense),
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            ..Default::default()
        },
    );
    store.dense_searcher = Some(dense);
    store.hybrid_searcher = Some(Arc::new(hybrid));

    let mut older = make_chunk(&tenant, "car workshop classic restoration");
    older.timestamp_created = ranking_time_ms - 30 * 24 * 60 * 60 * 1_000;
    Store::add(&store, older).await.unwrap();
    let mut newer = make_chunk(&tenant, "car workshop custom engineering");
    newer.timestamp_created = ranking_time_ms - 24 * 60 * 60 * 1_000;
    Store::add(&store, newer).await.unwrap();

    let standard = store
        .search_with_scores_at(&tenant, "car workshop", 1, ranking_time_ms)
        .await
        .unwrap();
    let (debug, _) =
        Store::search_with_tier_info_at(&store, &tenant, "car workshop", 1, ranking_time_ms)
            .await
            .unwrap();

    let identity = |rows: &[(MemoryChunk, f32)]| {
        rows.iter()
            .map(|(chunk, score)| (chunk.chunk_id.clone(), score.to_bits()))
            .collect::<Vec<_>>()
    };
    assert_eq!(standard.len(), 1);
    assert_eq!(identity(&standard), identity(&debug));
}

#[tokio::test]
async fn stats() {
    let (store, _dir) = make_test_store();
    let tenant = make_tenant();

    store.add(make_chunk(&tenant, "doc 1")).await.unwrap();
    store.add(make_chunk(&tenant, "doc 2")).await.unwrap();
    let to_delete = store.add(make_chunk(&tenant, "doc 3")).await.unwrap();

    store.delete(&tenant, &to_delete).await.unwrap();

    let stats = store.stats(&tenant).await.unwrap();
    assert_eq!(stats.total_chunks, 3);
    assert_eq!(stats.deleted_chunks, 1);
}

#[tokio::test]
async fn stats_counts_chunk_types_without_list_cap() {
    let (store, _dir) = make_test_store();
    let tenant = make_tenant();
    let mut rows = Vec::with_capacity(10_050);

    for i in 0..10_050usize {
        rows.push(ChunkMetadata {
            chunk_id: ChunkId::new(),
            tenant_id: tenant.clone(),
            project_id: None,
            segment_id: i as u64,
            ordinal: 0,
            chunk_type: if i % 2 == 0 {
                ChunkType::Doc
            } else {
                ChunkType::Summary
            },
            status: if i < 5 {
                ChunkStatus::Deleted
            } else {
                ChunkStatus::Final
            },
            timestamp_created: i as i64,
            hash: format!("hash-{i}"),
            source_uri: None,
            lifecycle: crate::types::LifecycleMetadata::default(),
            canonical_text: Some(format!("stats row {i}")),
            ingestion_mode: crate::types::IngestionMode::Document,
        });
    }

    store.metadata.insert_many(&rows).unwrap();

    let stats = store.stats(&tenant).await.unwrap();
    assert_eq!(stats.total_chunks, 10_050);
    assert_eq!(stats.deleted_chunks, 5);
    assert_eq!(stats.active_chunks, 10_045);
    assert_eq!(
        stats.chunk_types_all.values().sum::<usize>(),
        stats.total_chunks
    );
    assert_eq!(
        stats.chunk_types_active.values().sum::<usize>(),
        stats.active_chunks
    );
    assert_eq!(
        stats.chunk_types_deleted.values().sum::<usize>(),
        stats.deleted_chunks
    );
    assert_eq!(stats.chunk_types, stats.chunk_types_active);
}

#[tokio::test]
async fn add_long_document_splits_into_multiple_chunks() {
    let (store, _dir) = make_test_store();
    let tenant = make_tenant();
    let long_text = make_long_document();

    let _chunk_id = store.add(make_chunk(&tenant, &long_text)).await.unwrap();

    let stats = store.stats(&tenant).await.unwrap();
    assert!(stats.total_chunks > 1);
}

#[tokio::test]
async fn feedback_adjusts_scores_in_persistent_store() {
    let (store, _dir) = make_test_store();
    let tenant = make_tenant();

    let older = store
        .add(make_chunk(&tenant, "alpha retrieval note"))
        .await
        .unwrap();
    let newer = store
        .add(make_chunk(&tenant, "beta retrieval note"))
        .await
        .unwrap();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    store
        .add_feedback(FeedbackEntry::new(
            tenant.clone(),
            "retrieval note",
            older.clone(),
            crate::store::RelevanceLabel::Relevant,
            now_ms,
        ))
        .await
        .unwrap();
    store
        .add_feedback(FeedbackEntry::new(
            tenant.clone(),
            "retrieval note",
            older.clone(),
            crate::store::RelevanceLabel::Relevant,
            now_ms,
        ))
        .await
        .unwrap();
    store
        .add_feedback(FeedbackEntry::new(
            tenant.clone(),
            "retrieval note",
            newer.clone(),
            crate::store::RelevanceLabel::Irrelevant,
            now_ms,
        ))
        .await
        .unwrap();
    store
        .add_feedback(FeedbackEntry::new(
            tenant.clone(),
            "retrieval note",
            newer.clone(),
            crate::store::RelevanceLabel::Irrelevant,
            now_ms,
        ))
        .await
        .unwrap();

    let ranked = store
        .search_with_scores(&tenant, "retrieval note", 10)
        .await
        .unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].0.chunk_id, older);
}

/// Bug A: HNSW backfill. Simulates the production cold-start condition
/// where metadata has chunks but the in-memory HNSW is empty (because
/// the previous daemon never saved it). After calling
/// `backfill_hnsw_for_cold_tenants`, the search must find chunks whose
/// embeddings were previously missing.
#[tokio::test]
async fn backfill_hnsw_for_cold_tenants_reindexes_stranded_chunks() {
    use crate::embeddings::MockEmbedder;
    use crate::store::dense::{DenseSearchConfig, DenseSearcher};
    use crate::store::hybrid::{HybridConfig, HybridSearcher};

    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 0,
        enable_dense_search: true,
        enable_hybrid_search: true,
        enable_tiered_search: false, // keep the test simple
        ..Default::default()
    };
    let mut store = PersistentStore::open(config).unwrap();
    let embedder = Arc::new(MockEmbedder::new());
    let dense_searcher = Arc::new(DenseSearcher::with_embedder(
        Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let hybrid = HybridSearcher::new(
        Arc::clone(&dense_searcher),
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            ..Default::default()
        },
    );
    store.dense_searcher = Some(Arc::clone(&dense_searcher));
    store.hybrid_searcher = Some(Arc::new(hybrid));

    let tenant = make_tenant();
    let texts = [
        "alpha lifecycle overlay prototype",
        "bravo wal recovery idempotent replay",
        "charlie tiered cache invalidation",
    ];
    for text in &texts {
        Store::add(&store, make_chunk(&tenant, text)).await.unwrap();
    }
    // Sanity: search works while HNSW is warm.
    assert_eq!(dense_searcher.index_len(&tenant), texts.len());

    // Simulate the cold-start condition: swap in a brand new empty
    // dense searcher + hybrid. Metadata is unchanged; segments are on
    // disk; but HNSW has no entries — exactly the state we observed
    // on the production daemon after restart.
    let cold_dense = Arc::new(DenseSearcher::with_embedder(
        Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let cold_hybrid = HybridSearcher::new(
        Arc::clone(&cold_dense),
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            ..Default::default()
        },
    );
    store.dense_searcher = Some(Arc::clone(&cold_dense));
    store.hybrid_searcher = Some(Arc::new(cold_hybrid));

    // Before backfill: searching the cold HNSW returns nothing.
    assert_eq!(
        cold_dense.index_len(&tenant),
        0,
        "fresh dense searcher must start empty"
    );

    // Act.
    let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();

    // After backfill: HNSW has all three chunks, search finds them.
    assert!(
        stats.chunks_indexed >= texts.len(),
        "backfill must reindex all stranded chunks, got stats {:?}",
        stats
    );
    assert_eq!(stats.tenants_backfilled, 1);
    assert_eq!(cold_dense.index_len(&tenant), texts.len());

    let scored = store
        .search_with_scores(&tenant, "lifecycle", 10)
        .await
        .unwrap();
    assert!(
        !scored.is_empty(),
        "semantic search must return results after backfill"
    );
}

#[tokio::test]
async fn backfill_hnsw_is_noop_when_dense_disabled() {
    let (store, _dir) = make_test_store();
    // make_test_store disables dense search; backfill must return
    // cleanly with zero work done.
    let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();
    assert_eq!(stats.tenants_backfilled, 0);
    assert_eq!(stats.chunks_indexed, 0);
}

/// Codex-reviewed regression: count-only heuristics (`index_len >=
/// active_count`) silently skip stale tenants because HNSW's
/// `next_id` counter never decrements on delete. Simulate:
/// add 3 chunks, delete 2 (HNSW count still 3, active metadata 1),
/// then add 2 new chunks while HNSW is empty (simulates a
/// cold-restart mid-lifecycle). The stale tenant must still be
/// backfilled.
#[tokio::test]
async fn backfill_hnsw_detects_staleness_via_per_chunk_membership_not_counts() {
    use crate::embeddings::MockEmbedder;
    use crate::store::dense::{DenseSearchConfig, DenseSearcher};
    use crate::store::hybrid::{HybridConfig, HybridSearcher};

    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 0,
        enable_dense_search: true,
        enable_hybrid_search: true,
        enable_tiered_search: false,
        ..Default::default()
    };
    let mut store = PersistentStore::open(config).unwrap();
    let embedder = Arc::new(MockEmbedder::new());
    let dense = Arc::new(DenseSearcher::with_embedder(
        Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let hybrid = HybridSearcher::new(
        Arc::clone(&dense),
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            ..Default::default()
        },
    );
    store.dense_searcher = Some(Arc::clone(&dense));
    store.hybrid_searcher = Some(Arc::new(hybrid));

    let tenant = make_tenant();
    let id1 = Store::add(&store, make_chunk(&tenant, "first old chunk"))
        .await
        .unwrap();
    let id2 = Store::add(&store, make_chunk(&tenant, "second old chunk"))
        .await
        .unwrap();
    let _id3 = Store::add(&store, make_chunk(&tenant, "third old chunk"))
        .await
        .unwrap();

    // Soft-delete two of the chunks. HNSW's mapping.next_id stays at
    // 3, but metadata only has one active row.
    assert!(store.delete(&tenant, &id1).await.unwrap());
    assert!(store.delete(&tenant, &id2).await.unwrap());

    // Now simulate a cold restart: swap in a fresh empty HNSW state.
    let cold_dense = Arc::new(DenseSearcher::with_embedder(
        Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let cold_hybrid = HybridSearcher::new(
        Arc::clone(&cold_dense),
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            ..Default::default()
        },
    );
    store.dense_searcher = Some(Arc::clone(&cold_dense));
    store.hybrid_searcher = Some(Arc::new(cold_hybrid));

    // Add one more chunk post-"restart" — this one lands in HNSW
    // normally. With the naive count heuristic, `hnsw_count = 1` and
    // `active_count = 2` so backfill WOULD run, but only because
    // the skew is in our favor. Before per-chunk membership the
    // heuristic could also have failed the other way.
    let id4 = Store::add(&store, make_chunk(&tenant, "new post-restart chunk"))
        .await
        .unwrap();
    assert_eq!(cold_dense.index_len(&tenant), 1);
    assert!(cold_dense.contains_chunk(&tenant, &id4));

    // The surviving-from-old-era chunk is id3. It must be missing
    // from HNSW currently.
    let _ = _id3; // only referenced for assertion symmetry

    // Act.
    let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();

    // Exactly the one surviving pre-restart chunk should have been
    // re-indexed; id4 is already there and gets skipped by the
    // per-chunk membership test.
    assert_eq!(
        stats.chunks_indexed, 1,
        "backfill should re-index only the one missing chunk, got {:?}",
        stats
    );
    assert_eq!(stats.tenants_backfilled, 1);
}

#[tokio::test]
async fn chunks_missing_embeddings_is_cache_aware() {
    // The backfill's cold signal is cache-aware membership: a chunk counts
    // as "missing" only when it has no live cached embedding. Indexed
    // chunks (mapping + cache) are NOT missing; unindexed chunk ids ARE;
    // and a tenant whose index is not loaded reports every id missing.
    use crate::embeddings::MockEmbedder;
    use crate::store::dense::{DenseSearchConfig, DenseSearcher};

    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        enable_dense_search: true,
        enable_hybrid_search: false,
        enable_tiered_search: false,
        ..Default::default()
    };
    let mut store = PersistentStore::open(config).unwrap();
    let embedder = Arc::new(MockEmbedder::new());
    let dense = Arc::new(DenseSearcher::with_embedder(
        Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    store.dense_searcher = Some(Arc::clone(&dense));

    let tenant = make_tenant();
    let id1 = Store::add(&store, make_chunk(&tenant, "indexed one"))
        .await
        .unwrap();
    let id2 = Store::add(&store, make_chunk(&tenant, "indexed two"))
        .await
        .unwrap();

    // Both added chunks have live cached embeddings → neither is missing.
    assert!(
        dense
            .chunks_missing_embeddings(&tenant, &[id1.clone(), id2.clone()])
            .is_empty(),
        "indexed chunks must not be reported missing"
    );

    // A fabricated id that was never indexed IS missing.
    let ghost = ChunkId::new();
    assert_eq!(
        dense.chunks_missing_embeddings(&tenant, &[id1.clone(), ghost.clone()]),
        vec![ghost],
        "only the unindexed chunk is missing"
    );

    // A tenant with no loaded index reports every id missing (the cold
    // case the backfill resolves by `ensure_index_loaded` first).
    let other = TenantId::new("other_tenant").unwrap();
    assert_eq!(
        dense.chunks_missing_embeddings(&other, std::slice::from_ref(&id1)),
        vec![id1],
        "unloaded tenant reports all ids missing"
    );
}

#[tokio::test]
async fn backfill_hnsw_skips_tenants_whose_index_is_already_warm() {
    use crate::embeddings::MockEmbedder;
    use crate::store::dense::{DenseSearchConfig, DenseSearcher};
    use crate::store::hybrid::{HybridConfig, HybridSearcher};

    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 0,
        enable_dense_search: true,
        enable_hybrid_search: true,
        enable_tiered_search: false,
        ..Default::default()
    };
    let mut store = PersistentStore::open(config).unwrap();
    let embedder = Arc::new(MockEmbedder::new());
    let dense = Arc::new(DenseSearcher::with_embedder(
        Arc::clone(&embedder) as Arc<dyn crate::embeddings::Embedder>,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let hybrid = HybridSearcher::new(
        Arc::clone(&dense),
        None,
        HybridConfig {
            enable_sparse: false,
            enable_tiered: false,
            ..Default::default()
        },
    );
    store.dense_searcher = Some(Arc::clone(&dense));
    store.hybrid_searcher = Some(Arc::new(hybrid));

    let tenant = make_tenant();
    Store::add(&store, make_chunk(&tenant, "already indexed"))
        .await
        .unwrap();

    let before = dense.index_len(&tenant);
    let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();
    let after = dense.index_len(&tenant);

    assert_eq!(before, after, "warm tenant must not be re-indexed");
    assert_eq!(stats.tenants_backfilled, 0);
    assert_eq!(stats.chunks_indexed, 0);
}

/// Regression test for the `next_segment_id` scan.
///
/// Before the fix, `next_segment_id` only consulted loaded
/// finalized segments, so a crash that left behind an unfinalized
/// `seg_N/` directory (no `meta` file) was invisible to the next
/// rotation. The segment writer would then call `create_dir_all` +
/// `truncate(true)` on the same id and silently destroy the crashed
/// segment's payload bytes.
#[tokio::test]
async fn next_segment_id_skips_over_orphan_directories() {
    let (store, temp) = make_test_store();
    let tenant = make_tenant();

    // Force creation of a real segment via a normal write. Go
    // through the `Store` trait explicitly so type inference picks
    // the right method.
    Store::add(&store, make_chunk(&tenant, "seed chunk"))
        .await
        .unwrap();
    let tenant_arc = store.get_or_create_tenant(tenant.as_str()).unwrap();

    let initial_id = tenant_arc.next_segment_id();

    // Manually create an orphan segment directory without a `meta`
    // file — this is exactly the state a mid-write crash leaves.
    let orphan_id = initial_id + 5;
    let segments_dir = temp
        .path()
        .join("tenants")
        .join(tenant.as_str())
        .join("segments");
    let orphan_dir = segments_dir.join(format!("seg_{:06}", orphan_id));
    fs::create_dir_all(&orphan_dir).unwrap();
    fs::write(orphan_dir.join("payload.bin"), b"crashed mid-write").unwrap();

    // The next id must be strictly greater than the orphan's id so
    // the next rotation cannot reuse (and overwrite) it.
    let next_id = tenant_arc.next_segment_id();
    assert!(
        next_id > orphan_id,
        "next_segment_id must skip over orphan dirs: got {} but orphan is {}",
        next_id,
        orphan_id
    );
}

/// Codex Phase 3 coverage gap: verify that a task/artifact write
/// bumps the per-tenant warm-tier `memory_version`. The public
/// write path is `Store::add_task_artifact`, which threads through
/// the same `hybrid.index_batch` site that `Phase 3.5` hooked with
/// `bump_tenant_memory_version`. If a future refactor accidentally
/// takes artifact writes off the hybrid indexing path, the cache
/// invalidation invariant silently breaks — this test pins it.
#[tokio::test]
async fn add_task_artifact_bumps_tenant_memory_version() {
    use crate::embeddings::MockEmbedder;
    use crate::retrieval::{RerankerConfig, RerankerMode};
    use crate::store::dense::{DenseSearchConfig, DenseSearcher};
    use crate::store::hybrid::{HybridConfig, HybridSearcher};
    use crate::task_memory::{build_task_projections, TaskArtifact};

    let (store, _dir) = make_test_store_hybrid_tiered();
    let hybrid = store.hybrid_searcher.as_ref().unwrap().clone();
    let tenant = make_tenant();

    // Seed the tiered searcher by issuing a search — this builds
    // the per-tenant warm tier lazily, which is the version
    // counter we want to observe.
    store
        .search_with_scores(&tenant, "seed probe", 1)
        .await
        .unwrap();

    let before = hybrid
        .tenant_memory_version(&tenant)
        .expect("tiered searcher must exist after a search call");

    // Drive a real task artifact write through Store::add_task_artifact.
    let mut artifact = TaskArtifact::new_task_start(tenant.clone());
    artifact.goal = Some("pin version-bump invariant for task writes".to_string());
    let projections = build_task_projections(&artifact);
    <PersistentStore as Store>::add_task_artifact(&store, artifact.clone(), projections)
        .await
        .unwrap();

    let after = hybrid
        .tenant_memory_version(&tenant)
        .expect("tiered searcher must still exist");

    assert!(
        after > before,
        "add_task_artifact must bump per-tenant memory_version: \
         before={} after={}",
        before,
        after
    );
    // Suppress unused-assignment warning.
    let _ = hybrid;

    // Avoid dead-code warning on the `store` shadow below.
    drop(store);

    fn make_test_store_hybrid_tiered() -> (PersistentStore, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: true,
            enable_hybrid_search: true,
            enable_tiered_search: true,
            ..Default::default()
        };
        let mut store = PersistentStore::open(config).unwrap();

        let embedder = Arc::new(MockEmbedder::new());
        let dense = Arc::new(DenseSearcher::with_embedder(
            embedder,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let hybrid = HybridSearcher::new(
            dense,
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: true,
                reranker: RerankerConfig {
                    mode: RerankerMode::Feature,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        store.hybrid_searcher = Some(Arc::new(hybrid));
        (store, dir)
    }
}

/// Track C6: `PersistentStore::update_lifecycle_if_exists` must
/// bump `tenant_memory_version` when the row was found AND hybrid
/// is enabled, and MUST NOT bump when the row didn't exist. Pins
/// the cache-invalidation contract that `memory.set_expiry`
/// depends on so a later refactor can't silently take it off the
/// bump path.
#[tokio::test]
async fn update_lifecycle_if_exists_bumps_only_on_match() {
    use crate::embeddings::MockEmbedder;
    use crate::retrieval::{RerankerConfig, RerankerMode};
    use crate::store::dense::{DenseSearchConfig, DenseSearcher};
    use crate::store::hybrid::{HybridConfig, HybridSearcher};
    use crate::types::LifecycleDelta;

    fn hybrid_store() -> (PersistentStore, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0,
            enable_dense_search: true,
            enable_hybrid_search: true,
            enable_tiered_search: true,
            ..Default::default()
        };
        let mut store = PersistentStore::open(config).unwrap();
        let embedder = Arc::new(MockEmbedder::new());
        let dense = Arc::new(DenseSearcher::with_embedder(
            embedder,
            DenseSearchConfig {
                persist: false,
                ..Default::default()
            },
        ));
        let hybrid = HybridSearcher::new(
            dense,
            None,
            HybridConfig {
                enable_sparse: false,
                enable_tiered: true,
                reranker: RerankerConfig {
                    mode: RerankerMode::Feature,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        store.hybrid_searcher = Some(Arc::new(hybrid));
        (store, dir)
    }

    let (store, _dir) = hybrid_store();
    let hybrid = store.hybrid_searcher.as_ref().unwrap().clone();
    let tenant = make_tenant();

    // Seed the tiered warm tier so tenant_memory_version is live.
    store
        .search_with_scores(&tenant, "seed probe", 1)
        .await
        .unwrap();

    // Add a chunk we can update.
    let id = <PersistentStore as Store>::add(
        &store,
        MemoryChunk::new(tenant.clone(), "target", ChunkType::Doc),
    )
    .await
    .unwrap();

    let v_after_add = hybrid
        .tenant_memory_version(&tenant)
        .expect("tiered searcher must exist after an add");

    // Matched update must return true AND bump the version.
    let updated = store
        .update_lifecycle_if_exists(
            &tenant,
            &id,
            &LifecycleDelta {
                expires_at_ms: Some(Some(1_i64)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(updated, "existing row must report updated=true");
    let v_after_update = hybrid.tenant_memory_version(&tenant).unwrap_or(0);
    assert!(
        v_after_update > v_after_add,
        "matched update must bump tenant_memory_version: \
         before={v_after_add} after={v_after_update}"
    );

    // Unmatched update must return false AND leave the version alone.
    let bogus = ChunkId::new();
    let updated = store
        .update_lifecycle_if_exists(
            &tenant,
            &bogus,
            &LifecycleDelta {
                expires_at_ms: Some(Some(1_i64)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!updated, "nonexistent row must report updated=false");
    let v_after_noop = hybrid.tenant_memory_version(&tenant).unwrap_or(0);
    assert_eq!(
        v_after_noop, v_after_update,
        "nonexistent-row update must NOT bump tenant_memory_version"
    );
}

/// Codex-review regression (v0.3.1) for the WAL recovery durability
/// hole: recovery used to replay chunks into a fresh active
/// `SegmentWriter`, insert metadata rows, then truncate the WAL —
/// without ever finalizing the replayed active segment. A second
/// crash after recovery but before the next rotation would strand
/// metadata pointing at an unfinalized directory (no `meta` file,
/// so startup skipped it) while the WAL was already empty. The
/// recovery path now calls `finalize_active_segment()` before the
/// truncate so everything the WAL described is durable.
///
/// We emulate the failure mode by: (1) writing a chunk and letting
/// the store rotate + shut down normally, (2) verifying that a
/// second `PersistentStore::open` of the same directory can still
/// read the chunk — i.e. the recovery-to-finalize path produced a
/// real loadable segment.
#[tokio::test]
async fn recovery_finalizes_active_segment_before_wal_truncate() {
    let dir = tempdir().unwrap();

    // Phase A: write, then shut down cleanly.
    let tenant;
    let chunk_id;
    {
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 0, // safety valve default
            enable_dense_search: false,
            enable_hybrid_search: false,
            ..Default::default()
        };
        let store = PersistentStore::open(config).unwrap();
        tenant = make_tenant();
        chunk_id = Store::add(&store, make_chunk(&tenant, "durability sentinel"))
            .await
            .unwrap();
        store.shutdown().unwrap();
    }

    // Phase B: reopen the store. If recovery had truncated the WAL
    // without finalizing the active segment, the chunk's metadata
    // would be unreadable. This should succeed.
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 0,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();

    let recovered = Store::get(&store, &tenant, &chunk_id).await.unwrap();
    assert!(
        recovered.is_some(),
        "after reopen, the durability sentinel must still be readable; \
         an unfinalized recovery segment would have lost it"
    );
    assert_eq!(recovered.unwrap().text, "durability sentinel");
}

#[tokio::test]
async fn sparse_backfill_rebuilds_empty_index_for_active_tenant() {
    // The degraded state a crash leaves behind: active metadata rows
    // and payloads on disk, but an empty sparse index (the tantivy
    // directory was lost and silently recreated empty on open). The
    // backfill must detect the cold sparse side and rebuild it from
    // surviving payloads.
    let (mut store, dir) = make_test_store();
    let tenant = make_tenant();

    for text in [
        "the zanzibar expedition left in june",
        "unrelated second memory about compilers",
        "a third note mentioning harbor logistics",
    ] {
        let chunk = MemoryChunk::new(tenant.clone(), text.to_string(), ChunkType::Doc);
        store.add(chunk).await.unwrap();
    }

    // Inject searchers AFTER the writes so nothing was sparse-indexed
    // at add time — the same observable state as a lost tantivy dir.
    let sparse = Arc::new(Bm25Index::with_path(Some(dir.path().join("sparse_index"))).unwrap());
    let embedder = Arc::new(MockEmbedder::new());
    let dense = Arc::new(DenseSearcher::with_embedder(
        embedder,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    ));
    let hybrid = HybridSearcher::new(
        dense.clone(),
        Some(sparse.clone()),
        HybridConfig {
            enable_tiered: false,
            reranker: RerankerConfig {
                mode: RerankerMode::Feature,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    store.dense_searcher = Some(dense);
    store.hybrid_searcher = Some(Arc::new(hybrid));

    assert_eq!(sparse.doc_count(&tenant).unwrap(), 0);
    assert!(
        store.any_tenant_sparse_cold(),
        "active rows + empty sparse index must register as cold"
    );

    let stats = store.backfill_hnsw_for_cold_tenants().await.unwrap();
    assert_eq!(stats.chunks_indexed, 3);

    assert_eq!(sparse.doc_count(&tenant).unwrap(), 3);
    assert!(!store.any_tenant_sparse_cold());
    let hits = sparse.search(&tenant, "zanzibar", 5).unwrap();
    assert!(
        !hits.is_empty(),
        "rebuilt sparse index must serve lexical hits"
    );
}
