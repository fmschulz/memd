//! Bug B reproduction: runtime-rolled-over segments are not readable via
//! `get_chunk` after the rollover, even though the data is on disk and the
//! metadata row is correctly written.
//!
//! Observed in production: memory.add returns a chunk_id, the row lands in
//! metadata with status=final and segment_id=N+1, the segment files exist on
//! disk, but memory.get returns null. A daemon restart repairs it because
//! `load_segments()` registers all finalized readers from disk at startup.
//!
//! Hypothesis: the in-loop rollover in `add_chunks_internal` /
//! `add_task_artifact` — triggered mid-batch when segment_max_chunks is hit —
//! is losing reader registrations or producing an inconsistent (segment_id,
//! ordinal) pair vs metadata.

use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::types::{ChunkType, MemoryChunk, TenantId};
use tempfile::tempdir;

fn make_chunk(tenant: &TenantId, text: &str) -> MemoryChunk {
    MemoryChunk::new(tenant.clone(), text, ChunkType::Doc)
}

#[tokio::test]
async fn get_chunk_works_for_each_chunk_in_batch_that_spans_rollover() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1, // force rollover between each chunk
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    let tenant = TenantId::new("bug_b_tenant").unwrap();

    // Add a batch of 3 chunks. With max_chunks=1, rollover happens between
    // each. Chunks 0 and 1 are in finalized segments; chunk 2 is in the
    // active segment.
    let chunks = vec![
        make_chunk(&tenant, "alpha chunk content"),
        make_chunk(&tenant, "bravo chunk content"),
        make_chunk(&tenant, "charlie chunk content"),
    ];
    let ids = store.add_batch(chunks).await.unwrap();
    assert_eq!(ids.len(), 3);

    // All three MUST be retrievable by id.
    for (idx, id) in ids.iter().enumerate() {
        let result = store.get(&tenant, id).await.unwrap();
        assert!(
            result.is_some(),
            "get_chunk returned None for chunk {} (id={})",
            idx,
            id
        );
    }
}

/// Pins the observability invariant: a read whose metadata points at a
/// finalized segment should still succeed after a process restart. In prod,
/// the daemon restart fixed exactly this symptom because `load_segments()`
/// rehydrates the cache. This test runs the same round-trip in-process so
/// we catch any regression in segment load / on-demand recovery.
#[tokio::test]
async fn finalized_chunk_is_recoverable_across_store_restarts() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let tenant = TenantId::new("bug_b_restart").unwrap();

    let finalized_id = {
        let store = PersistentStore::open(config.clone()).unwrap();
        let a = store
            .add(make_chunk(&tenant, "finalized-bytes"))
            .await
            .unwrap();
        // Add a second chunk so the first definitely finalizes
        // (segment_max_chunks=1 forces rollover on the second add).
        let _ = store
            .add(make_chunk(&tenant, "active-bytes"))
            .await
            .unwrap();
        assert!(store.get(&tenant, &a).await.unwrap().is_some());
        a
    };

    // New store — simulates a fresh daemon process. `load_segments()` must
    // register the finalized segment, or on-demand open must kick in.
    let store = PersistentStore::open(config).unwrap();
    let recovered = store.get(&tenant, &finalized_id).await.unwrap();
    assert!(
        recovered.is_some(),
        "restart must recover finalized chunk via load_segments or on-demand open"
    );
}

#[tokio::test]
async fn get_chunk_works_after_separate_add_calls_span_rollover() {
    let dir = tempdir().unwrap();
    let config = PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 1,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(config).unwrap();
    let tenant = TenantId::new("bug_b_tenant_seq").unwrap();

    // Separate add calls — mirrors MCP clients adding chunks one at a time.
    let first_id = store.add(make_chunk(&tenant, "first post")).await.unwrap();
    let second_id = store
        .add(make_chunk(&tenant, "second post"))
        .await
        .unwrap();
    let third_id = store.add(make_chunk(&tenant, "third post")).await.unwrap();

    // Active segment: third's. Finalized: first's, second's. All must read.
    assert!(
        store.get(&tenant, &first_id).await.unwrap().is_some(),
        "first chunk unreadable after being rolled over"
    );
    assert!(
        store.get(&tenant, &second_id).await.unwrap().is_some(),
        "second chunk unreadable after being rolled over"
    );
    assert!(
        store.get(&tenant, &third_id).await.unwrap().is_some(),
        "third chunk unreadable (still in active segment)"
    );
}
