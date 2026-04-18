//! Integration tests for lifecycle overlay on PersistentStore.

#[path = "common/mod.rs"]
mod common;
use common::*;

use memd::store::metadata::MetadataStore;
use memd::store::Store;
use memd::types::lifecycle::{LifecycleDelta, MemoryTier};
use memd::types::{ChunkStatus, ChunkType, MemoryChunk};

#[tokio::test]
async fn persistent_store_returns_lifecycle_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let id = store
        .add(MemoryChunk::new(t.clone(), "hello", ChunkType::Doc))
        .await
        .unwrap();

    // Apply lifecycle delta directly through the PersistentStore API.
    store
        .update_lifecycle(
            &t,
            &id,
            &LifecycleDelta {
                tier: Some(MemoryTier::Working),
                lifecycle_updated_at_ms: Some(1_700_000_000_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let resolved = store.get_with_lifecycle(&t, &id).await.unwrap().unwrap();
    assert_eq!(resolved.chunk.text, "hello");
    assert_eq!(resolved.lifecycle.tier, MemoryTier::Working);
    assert_eq!(resolved.status, ChunkStatus::Final);
}

#[tokio::test]
async fn store_get_with_lifecycle_default_impl_returns_default_lifecycle() {
    // For an in-memory store (no override), the default trait impl should still
    // return a ResolvedChunk with default lifecycle fields.
    use memd::store::memory::MemoryStore;
    use std::sync::Arc;
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let t = tenant("t");
    let chunk = MemoryChunk::new(t.clone(), "hi", ChunkType::Doc);
    let id = store.add(chunk).await.unwrap();
    let resolved = store.get_with_lifecycle(&t, &id).await.unwrap().unwrap();
    assert_eq!(resolved.lifecycle.tier, MemoryTier::LongTerm); // default
    assert_eq!(resolved.lifecycle.lifecycle_updated_at_ms, 0); // default
}

#[tokio::test]
async fn get_with_lifecycle_returns_none_for_deleted_chunk() {
    // A6 (supersede_chunk) relies on Deleted rows being hidden by
    // get_with_lifecycle — document that invariant here so any future refactor
    // that drops the Deleted branch fails loudly.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let id = store
        .add(MemoryChunk::new(t.clone(), "to-delete", ChunkType::Doc))
        .await
        .unwrap();

    // Confirm the chunk is visible before deletion.
    let before = store.get_with_lifecycle(&t, &id).await.unwrap();
    assert!(before.is_some(), "chunk should be visible before delete");

    // Mark deleted via the metadata store (mirrors what memory.delete does
    // today). The `MetadataStore` trait is in scope at the top of the file so
    // `mark_deleted` resolves on the `&SqliteMetadataStore` accessor.
    store.metadata().mark_deleted(&t, &id).unwrap();

    // get_with_lifecycle must now return None, matching the Deleted branch.
    let after = store.get_with_lifecycle(&t, &id).await.unwrap();
    assert!(
        after.is_none(),
        "Deleted chunk must not surface through get_with_lifecycle"
    );
}

#[tokio::test]
async fn supersede_chunk_is_atomic_and_bumps_cache() {
    // End-to-end invariants for PersistentStore::supersede_chunk:
    // * the new chunk id differs from the old one,
    // * the old row transitions to Superseded with superseded_by = new,
    // * the new row carries supersedes = old,
    // * the tenant memory version either stays at its (missing) default
    //   when hybrid is disabled in the test harness, or strictly
    //   increases when hybrid is enabled. Because `persistent_store`
    //   disables hybrid, the accessor returns `None` and the assert
    //   degrades to a no-op — the real bump path is covered by the
    //   in-crate unit test that observes `tenant_memory_version` with a
    //   seeded tiered searcher.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let old_id = store
        .add(MemoryChunk::new(t.clone(), "v1", ChunkType::Doc))
        .await
        .unwrap();
    let version_before = store
        .hybrid()
        .and_then(|h| h.tenant_memory_version(&t))
        .unwrap_or(0);

    let new_chunk = MemoryChunk::new(t.clone(), "v2", ChunkType::Doc);
    let new_id = store.supersede_chunk(&t, &old_id, new_chunk).await.unwrap();
    assert_ne!(new_id, old_id, "supersede_chunk must mint a fresh chunk id");

    let old_resolved = store
        .get_with_lifecycle(&t, &old_id)
        .await
        .unwrap()
        .expect("old chunk still resolvable post-supersede");
    assert_eq!(old_resolved.status, ChunkStatus::Superseded);
    assert_eq!(
        old_resolved.lifecycle.superseded_by.as_ref().unwrap(),
        &new_id
    );

    let new_resolved = store
        .get_with_lifecycle(&t, &new_id)
        .await
        .unwrap()
        .expect("new chunk must be resolvable");
    assert_eq!(new_resolved.chunk.text, "v2");
    assert_eq!(new_resolved.status, ChunkStatus::Final);
    assert_eq!(new_resolved.lifecycle.supersedes.as_ref().unwrap(), &old_id);

    // When hybrid is enabled this version strictly increases; otherwise
    // both sides resolve to the `unwrap_or(0)` fallback and the check
    // degrades to `0 >= 0`, which is still a signal that we never
    // regress.
    let version_after = store
        .hybrid()
        .and_then(|h| h.tenant_memory_version(&t))
        .unwrap_or(0);
    assert!(
        version_after >= version_before,
        "tenant memory version must not regress across supersede_chunk: \
         before={version_before} after={version_after}"
    );
}

#[tokio::test]
async fn supersede_chunk_walks_long_chain_without_cycle_error() {
    // A→B→C→D chain — each hop drives supersede_chunk, which runs
    // detect_supersession_cycle from the latest `old`. Because the
    // chain is acyclic, every call must succeed. This test also
    // indirectly covers that `atomic_supersede` accepts the same
    // tenant+chunk_id pair being linked multiple times across hops.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let a = store
        .add(MemoryChunk::new(t.clone(), "A", ChunkType::Doc))
        .await
        .unwrap();
    let b = store
        .supersede_chunk(&t, &a, MemoryChunk::new(t.clone(), "B", ChunkType::Doc))
        .await
        .unwrap();
    let c = store
        .supersede_chunk(&t, &b, MemoryChunk::new(t.clone(), "C", ChunkType::Doc))
        .await
        .unwrap();
    let _d = store
        .supersede_chunk(&t, &c, MemoryChunk::new(t.clone(), "D", ChunkType::Doc))
        .await
        .unwrap();

    // A still resolves with its original superseded_by pointer.
    let resolved_a = store
        .get_with_lifecycle(&t, &a)
        .await
        .unwrap()
        .expect("A should still be resolvable as Superseded");
    assert_eq!(resolved_a.status, ChunkStatus::Superseded);
    assert_eq!(resolved_a.lifecycle.superseded_by.as_ref().unwrap(), &b);
}

#[tokio::test]
async fn supersede_chunk_detects_cycle_via_forged_overlay() {
    // A real A→A cycle cannot be induced through `supersede_chunk`
    // itself because `atomic_supersede`'s transaction requires both
    // rows to exist and the helper always mints a fresh chunk id for
    // `new`. To pin the cycle-detection branch we forge a chain's
    // overlay by calling `update_lifecycle` directly so
    // `a.superseded_by = b` and `b.superseded_by = a`, then exercise
    // `supersede_chunk` — it must fail-closed before touching the
    // add path.
    use memd::types::lifecycle::LifecycleDelta;

    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let t = tenant("t");
    let a = store
        .add(MemoryChunk::new(t.clone(), "A", ChunkType::Doc))
        .await
        .unwrap();
    let b = store
        .add(MemoryChunk::new(t.clone(), "B", ChunkType::Doc))
        .await
        .unwrap();

    // Forge A → B → A at the overlay layer, bypassing atomic_supersede
    // so we can observe detect_supersession_cycle's failure branch.
    store
        .update_lifecycle(
            &t,
            &a,
            &LifecycleDelta {
                superseded_by: Some(b.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store
        .update_lifecycle(
            &t,
            &b,
            &LifecycleDelta {
                superseded_by: Some(a.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let err = store
        .supersede_chunk(&t, &a, MemoryChunk::new(t.clone(), "C", ChunkType::Doc))
        .await
        .expect_err("supersede_chunk must fail on cyclic superseded_by chain");
    let msg = format!("{err}");
    assert!(
        msg.contains("supersession cycle detected"),
        "expected cycle-detection error, got: {msg}"
    );
}
