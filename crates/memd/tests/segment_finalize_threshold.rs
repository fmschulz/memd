//! Verify that a CLI-style shutdown does NOT seal an active segment
//! that holds fewer than `min_finalize_chunks` chunks.
//!
//! Bug being pinned here: `finalize_active_segment` runs on every
//! graceful shutdown / Drop, so a CLI invocation that wrote only 1-2
//! chunks would seal a tiny segment on every call. Real tenants
//! accumulated thousands of <100-chunk segment dirs. With the
//! `min_finalize_chunks` gate at the shutdown call sites, small active
//! segments are left unfinalized between invocations and grow across
//! runs until they cross the threshold.

#[path = "common/mod.rs"]
mod common;

use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::types::{ChunkType, MemoryChunk, TenantId};
use std::sync::Arc;
use tempfile::tempdir;

#[tokio::test]
async fn shutdown_does_not_finalize_tiny_segment() {
    let tmp = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        segment_max_chunks: 10_000,
        min_finalize_chunks: 100,
        ..Default::default()
    };
    let store = Arc::new(PersistentStore::open(config).expect("persistent store"));
    let tenant = TenantId::new("t1".to_string()).unwrap();

    // Insert 3 chunks (far below min_finalize_chunks).
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
        finalized, 0,
        "shutdown finalized a tiny segment (3 chunks < min_finalize_chunks=100)"
    );
}

#[tokio::test]
async fn shutdown_finalizes_segment_at_or_above_threshold() {
    // Symmetric guard: when the active segment IS at the threshold we
    // must still finalize on shutdown so it becomes searchable across
    // restarts the normal way.
    let tmp = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        segment_max_chunks: 10_000,
        min_finalize_chunks: 3,
        ..Default::default()
    };
    let store = Arc::new(PersistentStore::open(config).expect("persistent store"));
    let tenant = TenantId::new("t2".to_string()).unwrap();

    for i in 0..3 {
        let chunk = MemoryChunk::new(tenant.clone(), format!("chunk {i}"), ChunkType::Doc);
        store.add(chunk).await.unwrap();
    }

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
    // The whole point of the gate: between calls, unfinalized chunks
    // must still be recoverable on the next startup. WAL replay handles
    // this — recover_from_wal flushes them into a new active segment
    // and (via the unconditional finalize_active_segment in the WAL
    // durability barrier) seals it before truncating the WAL.
    let tmp = tempdir().unwrap();
    let make_config = || PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        segment_max_chunks: 10_000,
        min_finalize_chunks: 100,
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

    let store2 = Arc::new(PersistentStore::open(make_config()).expect("persistent store"));
    let recovered = store2.get(&tenant, &id).await.unwrap();
    assert!(
        recovered.is_some(),
        "chunk in an unfinalized active segment must be recovered via WAL replay"
    );
    assert_eq!(recovered.unwrap().text, "must-survive-restart");
}

#[tokio::test]
async fn checkpoint_enabled_forces_finalize_even_below_threshold() {
    // Codex review HIGH for Phase 3: with wal_checkpoint_interval > 0,
    // the WAL may be truncated past records that would otherwise be
    // needed to recover an unfinalized active segment. The gate must
    // disable itself in that mode or data is lost. Verify that the
    // shutdown finalizes the segment regardless of min_finalize_chunks
    // when checkpointing is on.
    let tmp = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        segment_max_chunks: 10_000,
        min_finalize_chunks: 1_000_000,
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
        "with wal_checkpoint_interval>0 the gate must disable itself \
         to avoid data loss; expected at least 1 finalized segment, got {finalized}"
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
        backfill_canonical_text_on_startup: false,
        segment_max_chunks: 10_000,
        min_finalize_chunks: 1_000_000,
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
