//! In-memory store implementation
//!
//! Provides a working baseline store backed by a simple HashMap.
//! This is used for development and testing before persistent storage.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use super::{Store, StoreStats};
use crate::error::Result;
use crate::types::{ChunkId, ChunkStatus, MemoryChunk, TenantId};

/// In-memory store implementation
///
/// Thread-safe storage using RwLock for concurrent access.
/// Data is organized by tenant_id for isolation.
pub struct MemoryStore {
    /// Map of tenant_id -> (chunk_id -> chunk)
    chunks: RwLock<HashMap<String, HashMap<String, MemoryChunk>>>,
}

impl MemoryStore {
    /// Create a new empty in-memory store
    pub fn new() -> Self {
        Self {
            chunks: RwLock::new(HashMap::new()),
        }
    }

    /// Compute SHA-256 hash of text content for deduplication
    fn compute_hash(text: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        let result = hasher.finalize();
        format!("{:x}", result)
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn add(&self, mut chunk: MemoryChunk) -> Result<ChunkId> {
        // Generate a new UUIDv7 for the chunk_id (time-sortable)
        let chunk_id = ChunkId::new();
        chunk.chunk_id = chunk_id.clone();

        // Compute SHA-256 hash for deduplication
        chunk.hash = Self::compute_hash(&chunk.text);

        let tenant_str = chunk.tenant_id.to_string();

        debug!(
            tenant_id = %tenant_str,
            chunk_id = %chunk_id,
            chunk_type = %chunk.chunk_type,
            "adding chunk to store"
        );

        let mut store = self.chunks.write().unwrap();
        let tenant_chunks = store.entry(tenant_str).or_default();
        tenant_chunks.insert(chunk_id.to_string(), chunk);

        Ok(chunk_id)
    }

    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>> {
        let mut ids = Vec::with_capacity(chunks.len());

        info!(count = chunks.len(), "adding batch of chunks");

        for chunk in chunks {
            let id = self.add(chunk).await?;
            ids.push(id);
        }

        Ok(ids)
    }

    async fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<MemoryChunk>> {
        let tenant_str = tenant_id.to_string();
        let chunk_id_str = chunk_id.to_string();

        debug!(
            tenant_id = %tenant_str,
            chunk_id = %chunk_id_str,
            "getting chunk from store"
        );

        let store = self.chunks.read().unwrap();

        // Enforce tenant isolation: only return chunks from the requested tenant
        let chunk = store
            .get(&tenant_str)
            .and_then(|tenant_chunks| tenant_chunks.get(&chunk_id_str))
            .filter(|c| c.status != ChunkStatus::Deleted)
            .cloned();

        Ok(chunk)
    }

    async fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<MemoryChunk>> {
        let tenant_str = tenant_id.to_string();

        debug!(
            tenant_id = %tenant_str,
            query = %query,
            k = k,
            "searching chunks"
        );

        let store = self.chunks.read().unwrap();

        let results: Vec<MemoryChunk> = store
            .get(&tenant_str)
            .map(|tenant_chunks| {
                tenant_chunks
                    .values()
                    // Filter out deleted chunks
                    .filter(|chunk| chunk.status != ChunkStatus::Deleted)
                    // Basic text contains filter if query is non-empty
                    .filter(|chunk| {
                        query.is_empty()
                            || chunk.text.to_lowercase().contains(&query.to_lowercase())
                    })
                    .take(k)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        info!(
            tenant_id = %tenant_str,
            query = %query,
            results_count = results.len(),
            "search completed"
        );

        Ok(results)
    }

    async fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        let tenant_str = tenant_id.to_string();
        let chunk_id_str = chunk_id.to_string();

        debug!(
            tenant_id = %tenant_str,
            chunk_id = %chunk_id_str,
            "deleting chunk (soft delete)"
        );

        let mut store = self.chunks.write().unwrap();

        // Enforce tenant isolation: only delete chunks from the requested tenant
        if let Some(tenant_chunks) = store.get_mut(&tenant_str) {
            if let Some(chunk) = tenant_chunks.get_mut(&chunk_id_str) {
                if chunk.status == ChunkStatus::Deleted {
                    warn!(
                        tenant_id = %tenant_str,
                        chunk_id = %chunk_id_str,
                        "chunk already deleted"
                    );
                    return Ok(false);
                }

                chunk.status = ChunkStatus::Deleted;
                info!(
                    tenant_id = %tenant_str,
                    chunk_id = %chunk_id_str,
                    "chunk marked as deleted"
                );
                return Ok(true);
            }
        }

        warn!(
            tenant_id = %tenant_str,
            chunk_id = %chunk_id_str,
            "chunk not found for deletion"
        );
        Ok(false)
    }

    async fn stats(&self, tenant_id: &TenantId) -> Result<StoreStats> {
        let tenant_str = tenant_id.to_string();

        debug!(tenant_id = %tenant_str, "getting store stats");

        let store = self.chunks.read().unwrap();

        let stats = store
            .get(&tenant_str)
            .map(|tenant_chunks| {
                let mut chunk_types: HashMap<String, usize> = HashMap::new();
                let mut deleted_count = 0;

                for chunk in tenant_chunks.values() {
                    if chunk.status == ChunkStatus::Deleted {
                        deleted_count += 1;
                    }

                    *chunk_types.entry(chunk.chunk_type.to_string()).or_insert(0) += 1;
                }

                StoreStats {
                    total_chunks: tenant_chunks.len(),
                    deleted_chunks: deleted_count,
                    chunk_types,
                }
            })
            .unwrap_or_default();

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkType;

    fn make_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    fn make_chunk(tenant: &TenantId, text: &str, chunk_type: ChunkType) -> MemoryChunk {
        MemoryChunk::new(tenant.clone(), text, chunk_type)
    }

    #[tokio::test]
    async fn add_and_get_chunk() {
        let store = MemoryStore::new();
        let tenant = make_tenant();
        let chunk = make_chunk(&tenant, "hello world", ChunkType::Doc);

        let chunk_id = store.add(chunk).await.unwrap();
        let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.text, "hello world");
        assert_eq!(retrieved.chunk_type, ChunkType::Doc);
    }

    #[tokio::test]
    async fn chunk_id_is_uuidv7() {
        let store = MemoryStore::new();
        let tenant = make_tenant();
        let chunk = make_chunk(&tenant, "test", ChunkType::Doc);

        let chunk_id = store.add(chunk).await.unwrap();

        // UUIDv7 should be valid and parseable
        let uuid_str = chunk_id.to_string();
        assert!(uuid::Uuid::parse_str(&uuid_str).is_ok());
    }

    #[tokio::test]
    async fn content_hash_is_sha256() {
        let store = MemoryStore::new();
        let tenant = make_tenant();
        let text = "test content";
        let chunk = make_chunk(&tenant, text, ChunkType::Doc);

        let chunk_id = store.add(chunk).await.unwrap();
        let retrieved = store.get(&tenant, &chunk_id).await.unwrap().unwrap();

        // Verify hash is 64 hex chars (SHA-256)
        assert_eq!(retrieved.hash.len(), 64);
        assert!(retrieved.hash.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify hash matches expected SHA-256
        let expected = MemoryStore::compute_hash(text);
        assert_eq!(retrieved.hash, expected);
    }

    #[tokio::test]
    async fn tenant_isolation() {
        let store = MemoryStore::new();
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();

        let chunk = make_chunk(&tenant_a, "secret data", ChunkType::Doc);
        let chunk_id = store.add(chunk).await.unwrap();

        // Tenant A can access the chunk
        let from_a = store.get(&tenant_a, &chunk_id).await.unwrap();
        assert!(from_a.is_some());

        // Tenant B cannot access the chunk
        let from_b = store.get(&tenant_b, &chunk_id).await.unwrap();
        assert!(from_b.is_none());
    }

    #[tokio::test]
    async fn search_returns_matching_chunks() {
        let store = MemoryStore::new();
        let tenant = make_tenant();

        store
            .add(make_chunk(&tenant, "hello world", ChunkType::Doc))
            .await
            .unwrap();
        store
            .add(make_chunk(&tenant, "goodbye world", ChunkType::Doc))
            .await
            .unwrap();
        store
            .add(make_chunk(&tenant, "other content", ChunkType::Code))
            .await
            .unwrap();

        let results = store.search(&tenant, "world", 10).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn search_respects_k_limit() {
        let store = MemoryStore::new();
        let tenant = make_tenant();

        for i in 0..10 {
            store
                .add(make_chunk(&tenant, &format!("chunk {}", i), ChunkType::Doc))
                .await
                .unwrap();
        }

        let results = store.search(&tenant, "", 5).await.unwrap();
        assert_eq!(results.len(), 5);
    }

    #[tokio::test]
    async fn search_tenant_isolation() {
        let store = MemoryStore::new();
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();

        store
            .add(make_chunk(&tenant_a, "secret data", ChunkType::Doc))
            .await
            .unwrap();

        // Search as tenant B should return empty
        let results = store.search(&tenant_b, "secret", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn soft_delete() {
        let store = MemoryStore::new();
        let tenant = make_tenant();

        let chunk = make_chunk(&tenant, "to be deleted", ChunkType::Doc);
        let chunk_id = store.add(chunk).await.unwrap();

        // Delete the chunk
        let deleted = store.delete(&tenant, &chunk_id).await.unwrap();
        assert!(deleted);

        // Chunk no longer retrievable
        let retrieved = store.get(&tenant, &chunk_id).await.unwrap();
        assert!(retrieved.is_none());

        // Chunk doesn't appear in search
        let results = store.search(&tenant, "deleted", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn delete_tenant_isolation() {
        let store = MemoryStore::new();
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();

        let chunk = make_chunk(&tenant_a, "protected data", ChunkType::Doc);
        let chunk_id = store.add(chunk).await.unwrap();

        // Tenant B cannot delete tenant A's chunk
        let deleted = store.delete(&tenant_b, &chunk_id).await.unwrap();
        assert!(!deleted);

        // Chunk is still accessible to tenant A
        let retrieved = store.get(&tenant_a, &chunk_id).await.unwrap();
        assert!(retrieved.is_some());
    }

    #[tokio::test]
    async fn stats_counts_correctly() {
        let store = MemoryStore::new();
        let tenant = make_tenant();

        store
            .add(make_chunk(&tenant, "doc 1", ChunkType::Doc))
            .await
            .unwrap();
        store
            .add(make_chunk(&tenant, "doc 2", ChunkType::Doc))
            .await
            .unwrap();
        let code_id = store
            .add(make_chunk(&tenant, "code 1", ChunkType::Code))
            .await
            .unwrap();

        // Delete one chunk
        store.delete(&tenant, &code_id).await.unwrap();

        let stats = store.stats(&tenant).await.unwrap();
        assert_eq!(stats.total_chunks, 3);
        assert_eq!(stats.deleted_chunks, 1);
        assert_eq!(stats.chunk_types.get("doc"), Some(&2));
        assert_eq!(stats.chunk_types.get("code"), Some(&1));
    }

    #[tokio::test]
    async fn add_batch() {
        let store = MemoryStore::new();
        let tenant = make_tenant();

        let chunks = vec![
            make_chunk(&tenant, "batch 1", ChunkType::Doc),
            make_chunk(&tenant, "batch 2", ChunkType::Code),
            make_chunk(&tenant, "batch 3", ChunkType::Trace),
        ];

        let ids = store.add_batch(chunks).await.unwrap();
        assert_eq!(ids.len(), 3);

        // All chunks retrievable
        for id in ids {
            let chunk = store.get(&tenant, &id).await.unwrap();
            assert!(chunk.is_some());
        }
    }

    #[tokio::test]
    async fn empty_tenant_stats() {
        let store = MemoryStore::new();
        let tenant = make_tenant();

        let stats = store.stats(&tenant).await.unwrap();
        assert_eq!(stats.total_chunks, 0);
        assert_eq!(stats.deleted_chunks, 0);
        assert!(stats.chunk_types.is_empty());
    }
}
