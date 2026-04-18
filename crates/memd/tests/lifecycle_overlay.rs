//! Integration tests for lifecycle overlay on PersistentStore.

#[path = "common/mod.rs"]
mod common;
use common::*;

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
