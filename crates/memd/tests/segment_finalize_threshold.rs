//! Short CLI lifetimes must not accumulate thousands of tiny segment directories.
//! Shutdown seals payloads; writer open packs small segments with bounded copying.

#[path = "common/mod.rs"]
mod common;

use memd::store::hybrid::HybridConfig;
use memd::store::metadata::MetadataStore;
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::types::{ChunkType, MemoryChunk, TenantId};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::tempdir;

fn growth_config(path: &Path) -> PersistentStoreConfig {
    PersistentStoreConfig {
        data_dir: path.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: true,
        hybrid_config: Some(HybridConfig {
            dense_k: 0,
            enable_rerank: false,
            ..Default::default()
        }),
        backfill_hnsw_on_startup: false,
        ..Default::default()
    }
}

fn segment_dirs(root: &Path, tenant: &TenantId) -> Vec<PathBuf> {
    std::fs::read_dir(root.join("tenants").join(tenant.as_str()).join("segments"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect()
}

#[tokio::test]
async fn merge_retains_sources_until_metadata_checkpoint_finishes() {
    let tmp = tempdir().unwrap();
    let tenant = TenantId::new("merge_busy").unwrap();
    let store = PersistentStore::open(growth_config(tmp.path())).unwrap();
    let live = store
        .add(MemoryChunk::new(
            tenant.clone(),
            "live payload",
            ChunkType::Doc,
        ))
        .await
        .unwrap();
    let deleted = store
        .add(MemoryChunk::new(
            tenant.clone(),
            "deleted payload",
            ChunkType::Doc,
        ))
        .await
        .unwrap();
    store.checkpoint_tenant_wal(&tenant).unwrap();
    store.delete(&tenant, &deleted).await.unwrap();
    store.checkpoint_tenant_wal(&tenant).unwrap();
    let sources = segment_dirs(tmp.path(), &tenant);
    let reader = rusqlite::Connection::open(tmp.path().join("metadata.db")).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    reader
        .query_row("SELECT COUNT(*) FROM chunks", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert!(
        store.rewrite_segments_for_tenant(&tenant).is_err(),
        "blocked metadata checkpoint must prevent source removal"
    );
    assert!(sources.iter().all(|path| path.exists()));
    assert_eq!(
        store.get(&tenant, &live).await.unwrap().unwrap().text,
        "live payload"
    );
    reader.execute_batch("COMMIT").unwrap();
    store.shutdown().unwrap();
    drop(store);
    let store = PersistentStore::open(growth_config(tmp.path())).unwrap();
    assert!(sources.iter().all(|path| !path.exists()));
    assert_eq!(segment_dirs(tmp.path(), &tenant).len(), 1);
    assert!(store.get(&tenant, &deleted).await.unwrap().is_none());
    assert!(store.get(&tenant, &live).await.unwrap().is_some());
}

#[tokio::test]
async fn short_writer_lifetimes_bound_segments_and_amortize_copies() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempdir().unwrap();
    let tenant = TenantId::new("growth").unwrap();
    let mut ids = Vec::new();
    let mut locations = BTreeMap::new();
    let mut moves = BTreeMap::new();
    for i in 0..300 {
        let before: Vec<_> = if i == 0 {
            Vec::new()
        } else {
            segment_dirs(tmp.path(), &tenant)
                .into_iter()
                .map(|dir| {
                    let file = dir.join("payload.bin");
                    let meta = file.metadata().unwrap();
                    (
                        file.clone(),
                        meta.ino(),
                        meta.modified().unwrap(),
                        std::fs::read(file).unwrap(),
                    )
                })
                .collect()
        };
        let store = PersistentStore::open(growth_config(tmp.path())).unwrap();
        // Surviving durable payloads are immutable. Only a merge may replace them.
        for (file, ino, modified, bytes) in before {
            if file.exists() {
                let meta = file.metadata().unwrap();
                assert_eq!(meta.ino(), ino);
                assert_eq!(meta.modified().unwrap(), modified);
                assert_eq!(std::fs::read(file).unwrap(), bytes);
            }
        }
        for id in &ids {
            let row = store.metadata().get(&tenant, id).unwrap().unwrap();
            if locations
                .insert(id.clone(), row.segment_id)
                .is_some_and(|old| old != row.segment_id)
            {
                *moves.entry(id.clone()).or_insert(0usize) += 1;
            }
        }
        let id = store
            .add(MemoryChunk::new(
                tenant.clone(),
                format!("lifetime payload record{i}"),
                ChunkType::Doc,
            ))
            .await
            .unwrap();
        locations.insert(
            id.clone(),
            store
                .metadata()
                .get(&tenant, &id)
                .unwrap()
                .unwrap()
                .segment_id,
        );
        ids.push(id);
        store.shutdown().unwrap();
    }
    let dirs = segment_dirs(tmp.path(), &tenant);
    let store = PersistentStore::open(growth_config(tmp.path())).unwrap();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(
            store.get(&tenant, id).await.unwrap().unwrap().text,
            format!("lifetime payload record{i}")
        );
    }
    let found: HashSet<_> = store
        .search(&tenant, "lifetime", 300)
        .await
        .unwrap()
        .into_iter()
        .map(|chunk| chunk.chunk_id)
        .collect();
    assert_eq!(found, ids.into_iter().collect());
    // At most one segment per occupied binary tier for 299 chunks, plus the
    // just-sealed chunk: ceil(log2(300)) + 1 = 10. No merge reaches capacity.
    assert!(
        dirs.len() <= 10,
        "300 short writers left {} directories",
        dirs.len()
    );
    assert!(
        moves.values().all(|count| *count <= 9),
        "a copy must advance a size tier: {moves:?}"
    );
    store.shutdown().unwrap();
    drop(store);

    let snapshot = || {
        let mut files = BTreeMap::new();
        for dir in segment_dirs(tmp.path(), &tenant) {
            let path = dir.join("payload.bin");
            let meta = path.metadata().unwrap();
            files.insert(
                path.clone(),
                (
                    meta.ino(),
                    meta.modified().unwrap(),
                    std::fs::read(path).unwrap(),
                ),
            );
        }
        files
    };
    let before = snapshot();
    for _ in 0..5 {
        let store = PersistentStore::open(growth_config(tmp.path())).unwrap();
        store.shutdown().unwrap();
    }
    assert_eq!(
        snapshot(),
        before,
        "opens without new writes must do no merge work"
    );
}

#[tokio::test]
async fn writer_reclaims_purged_payload_and_interrupted_merge_orphan() {
    let tmp = tempdir().unwrap();
    let tenant = TenantId::new("reclaim").unwrap();
    let secret = "PURGED_PLAINTEXT_MARKER_2B";
    let store = PersistentStore::open(growth_config(tmp.path())).unwrap();
    let live = store
        .add(MemoryChunk::new(
            tenant.clone(),
            "keep lifetime payload",
            ChunkType::Doc,
        ))
        .await
        .unwrap();
    let purged = store
        .add(MemoryChunk::new(tenant.clone(), secret, ChunkType::Doc))
        .await
        .unwrap();
    store.checkpoint_tenant_wal(&tenant).unwrap();
    store.delete(&tenant, &purged).await.unwrap();
    store.checkpoint_tenant_wal(&tenant).unwrap();
    store
        .metadata()
        .hard_delete_chunks(&tenant, std::slice::from_ref(&purged))
        .unwrap();
    store.shutdown().unwrap();
    drop(store);
    // A sealed replacement written before its metadata transaction committed.
    let original = segment_dirs(tmp.path(), &tenant).pop().unwrap();
    let orphan = original.parent().unwrap().join("seg_009000");
    std::fs::create_dir(&orphan).unwrap();
    for entry in std::fs::read_dir(&original).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), orphan.join(entry.file_name())).unwrap();
    }
    let store = PersistentStore::open(growth_config(tmp.path())).unwrap();
    assert!(store.get(&tenant, &purged).await.unwrap().is_none());
    assert_eq!(
        store.get(&tenant, &live).await.unwrap().unwrap().text,
        "keep lifetime payload"
    );
    store.shutdown().unwrap();
    assert!(
        !orphan.exists(),
        "unreferenced finalized directory must be removed"
    );
    for dir in segment_dirs(tmp.path(), &tenant) {
        let bytes = std::fs::read(dir.join("payload.bin")).unwrap();
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "purged plaintext remains in {}",
            dir.display()
        );
    }
}

#[tokio::test]
async fn packing_deleted_only_segments_preserves_deletion_metadata() {
    let tmp = tempdir().unwrap();
    let tenant = TenantId::new("deleted_coordinates").unwrap();
    let config = || PersistentStoreConfig {
        enable_hybrid_search: false,
        ..growth_config(tmp.path())
    };
    let deleted;
    {
        let store = PersistentStore::open(config()).unwrap();
        deleted = store
            .add(MemoryChunk::new(tenant.clone(), "deleted", ChunkType::Doc))
            .await
            .unwrap();
        store.delete(&tenant, &deleted).await.unwrap();
        store.shutdown().unwrap();
    }
    for _ in 0..2 {
        let store = PersistentStore::open(config()).unwrap();
        store
            .add(MemoryChunk::new(tenant.clone(), "new live", ChunkType::Doc))
            .await
            .unwrap();
        assert_eq!(
            store.metadata().get_deleted_chunk_ids(&tenant).unwrap(),
            vec![deleted.clone()]
        );
        assert!(store.get(&tenant, &deleted).await.unwrap().is_none());
        store.shutdown().unwrap();
    }
}

#[tokio::test]
async fn shutdown_finalizes_tiny_segment_without_replay_rewrite() {
    let tmp = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        segment_max_chunks: 10_000,
        ..Default::default()
    };
    let store = Arc::new(PersistentStore::open(config).expect("persistent store"));
    let tenant = TenantId::new("t1".to_string()).unwrap();

    // Even a short CLI write must leave a finalized segment.
    for i in 0..3 {
        let chunk = MemoryChunk::new(tenant.clone(), format!("chunk {i}"), ChunkType::Doc);
        store.add(chunk).await.unwrap();
    }

    // Graceful shutdown — same path a CLI invocation hits when the
    // process exits cleanly.
    store.shutdown().unwrap();

    // Count finalized segment dirs (those with a `meta` file).
    let seg_dir = tmp.path().join("tenants").join("t1").join("segments");
    let mut finalized = 0;
    if seg_dir.exists() {
        for entry in std::fs::read_dir(&seg_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.path().join("meta").exists() {
                finalized += 1;
            }
        }
    }
    assert_eq!(
        finalized, 1,
        "shutdown must seal tiny segments so replay need not rewrite their payloads"
    );
}

#[tokio::test]
async fn repeated_shutdown_does_not_create_extra_segments() {
    let tmp = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        segment_max_chunks: 10_000,
        ..Default::default()
    };
    let store = Arc::new(PersistentStore::open(config).expect("persistent store"));
    let tenant = TenantId::new("t2".to_string()).unwrap();

    for i in 0..3 {
        let chunk = MemoryChunk::new(tenant.clone(), format!("chunk {i}"), ChunkType::Doc);
        store.add(chunk).await.unwrap();
    }

    store.shutdown().unwrap();
    store.shutdown().unwrap();

    let seg_dir = tmp.path().join("tenants").join("t2").join("segments");
    let mut finalized = 0;
    if seg_dir.exists() {
        for entry in std::fs::read_dir(&seg_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.path().join("meta").exists() {
                finalized += 1;
            }
        }
    }
    assert_eq!(finalized, 1, "expected exactly one finalized segment");
}

#[tokio::test]
async fn unfinalized_chunks_survive_restart_via_wal_recovery() {
    // Recreate the incomplete directory left by a process that died
    // before finalization. Its WAL remains the source for recovery.
    let tmp = tempdir().unwrap();
    let make_config = || PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        segment_max_chunks: 10_000,
        ..Default::default()
    };
    let tenant = TenantId::new("t3".to_string()).unwrap();

    let id;
    {
        let store = Arc::new(PersistentStore::open(make_config()).expect("persistent store"));
        let chunk = MemoryChunk::new(tenant.clone(), "must-survive-restart", ChunkType::Doc);
        id = store.add(chunk).await.unwrap();
        store.shutdown().unwrap();
        drop(store);
    }
    let old_segment = tmp.path().join("tenants/t3/segments/seg_000001");
    std::fs::remove_file(old_segment.join("meta")).unwrap();

    let store2 = Arc::new(PersistentStore::open(make_config()).expect("persistent store"));
    let recovered = store2.get(&tenant, &id).await.unwrap();
    assert!(
        recovered.is_some(),
        "chunk in an unfinalized active segment must be recovered via WAL replay"
    );
    assert_eq!(recovered.unwrap().text, "must-survive-restart");
    assert!(
        !old_segment.exists(),
        "recovery must remove obsolete unfinished bytes"
    );
}

#[tokio::test]
async fn checkpoint_enabled_shutdown_leaves_finalized_segments() {
    let tmp = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        segment_max_chunks: 10_000,
        // Non-zero interval activates the checkpoint path.
        wal_checkpoint_interval: 1,
        ..Default::default()
    };
    let store = Arc::new(PersistentStore::open(config).expect("persistent store"));
    let tenant = TenantId::new("t4".to_string()).unwrap();

    for i in 0..3 {
        let chunk = MemoryChunk::new(tenant.clone(), format!("chunk {i}"), ChunkType::Doc);
        store.add(chunk).await.unwrap();
    }

    store.shutdown().unwrap();

    let seg_dir = tmp.path().join("tenants").join("t4").join("segments");
    let mut finalized = 0;
    if seg_dir.exists() {
        for entry in std::fs::read_dir(&seg_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.path().join("meta").exists() {
                finalized += 1;
            }
        }
    }
    assert!(
        finalized >= 1,
        "checkpointed writes must remain finalized; got {finalized}"
    );
}

#[tokio::test]
async fn checkpoint_finalizes_active_segment_immediately() {
    // A WAL checkpoint claims "everything before me is durable in
    // finalized segments" — recovery drops those records and truncates
    // the WAL. If the checkpoint is appended while the active segment is
    // still unfinalized (no `meta` file), a crash before shutdown loses
    // the only durable copy of those adds. The checkpoint path must
    // therefore finalize BEFORE appending, not rely on graceful
    // shutdown to do it later.
    let tmp = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        segment_max_chunks: 10_000,
        wal_checkpoint_interval: 1,
        ..Default::default()
    };
    let store = Arc::new(PersistentStore::open(config).expect("persistent store"));
    let tenant = TenantId::new("t5".to_string()).unwrap();

    let chunk = MemoryChunk::new(tenant.clone(), "checkpointed".to_string(), ChunkType::Doc);
    store.add(chunk).await.unwrap();

    // No shutdown: the checkpoint written by the add itself must have
    // sealed the segment (meta file present) at this point.
    let seg_dir = tmp.path().join("tenants").join("t5").join("segments");
    let mut finalized = 0;
    if seg_dir.exists() {
        for entry in std::fs::read_dir(&seg_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.path().join("meta").exists() {
                finalized += 1;
            }
        }
    }
    assert!(
        finalized >= 1,
        "a written checkpoint requires an already-finalized segment; got {finalized}"
    );
}
