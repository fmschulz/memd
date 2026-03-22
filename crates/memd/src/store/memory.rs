//! In-memory store implementation
//!
//! Provides a working baseline store backed by a simple HashMap.
//! This is used for development and testing before persistent storage.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use super::{
    apply_feedback_scores, split_for_add, FeedbackConfig, FeedbackEntry, Store, StoreStats,
};
use crate::error::Result;
use crate::task_memory::{
    TaskArtifact, TaskArtifactWriteResult, TaskProjection, TaskSearchFilters,
};
use crate::types::{ChunkId, ChunkStatus, MemoryChunk, TenantId};

/// In-memory store implementation
///
/// Thread-safe storage using RwLock for concurrent access.
/// Data is organized by tenant_id for isolation.
pub struct MemoryStore {
    /// Map of tenant_id -> (chunk_id -> chunk)
    chunks: RwLock<HashMap<String, HashMap<String, MemoryChunk>>>,
    /// Canonical task artifacts keyed by tenant and artifact ID.
    task_artifacts: RwLock<HashMap<String, HashMap<String, TaskArtifact>>>,
    /// Projection links keyed by tenant then artifact ID.
    task_projection_links: RwLock<HashMap<String, HashMap<String, Vec<ChunkId>>>>,
    /// Reverse lookup of projection chunk ID -> artifact ID.
    projection_to_artifact: RwLock<HashMap<String, HashMap<String, String>>>,
    /// Per-tenant relevance feedback log.
    feedback: RwLock<HashMap<String, Vec<FeedbackEntry>>>,
}

impl MemoryStore {
    /// Create a new empty in-memory store
    pub fn new() -> Self {
        Self {
            chunks: RwLock::new(HashMap::new()),
            task_artifacts: RwLock::new(HashMap::new()),
            task_projection_links: RwLock::new(HashMap::new()),
            projection_to_artifact: RwLock::new(HashMap::new()),
            feedback: RwLock::new(HashMap::new()),
        }
    }

    /// Compute SHA-256 hash of text content for deduplication
    fn compute_hash(text: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        let result = hasher.finalize();
        format!("{:x}", result)
    }

    async fn add_single_chunk(&self, mut chunk: MemoryChunk) -> Result<ChunkId> {
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
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn add(&self, chunk: MemoryChunk) -> Result<ChunkId> {
        let mut chunks = split_for_add(chunk);
        if chunks.len() == 1 {
            return self
                .add_single_chunk(chunks.pop().ok_or_else(|| {
                    crate::error::MemdError::StorageError("no chunk to add".into())
                })?)
                .await;
        }

        let mut chunk_ids = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            chunk_ids.push(self.add_single_chunk(chunk).await?);
        }

        chunk_ids
            .into_iter()
            .next()
            .ok_or_else(|| crate::error::MemdError::StorageError("no chunk id produced".into()))
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

    async fn add_feedback(&self, feedback: FeedbackEntry) -> Result<()> {
        let tenant = feedback.tenant_id.to_string();
        let mut store = self.feedback.write().unwrap();
        store.entry(tenant).or_default().push(feedback);
        Ok(())
    }

    async fn add_task_artifact(
        &self,
        artifact: TaskArtifact,
        projections: Vec<TaskProjection>,
    ) -> Result<TaskArtifactWriteResult> {
        let projection_chunk_ids = self
            .add_batch(
                projections
                    .into_iter()
                    .map(|projection| projection.chunk)
                    .collect(),
            )
            .await?
            .into_iter()
            .map(|chunk_id| chunk_id.to_string())
            .collect::<Vec<_>>();

        let tenant = artifact.tenant_id.to_string();
        let artifact_id = artifact.artifact_id.clone();
        let task_id = artifact.task_id.clone();
        let projection_ids = projection_chunk_ids
            .iter()
            .filter_map(|id| ChunkId::parse(id).ok())
            .collect::<Vec<_>>();

        let mut task_store = self.task_artifacts.write().unwrap();
        task_store
            .entry(tenant.clone())
            .or_default()
            .insert(artifact_id.clone(), artifact);
        drop(task_store);

        let mut projection_store = self.task_projection_links.write().unwrap();
        projection_store
            .entry(tenant.clone())
            .or_default()
            .insert(artifact_id.clone(), projection_ids);
        drop(projection_store);

        let mut reverse = self.projection_to_artifact.write().unwrap();
        let reverse_map = reverse.entry(tenant.clone()).or_default();
        for projection_chunk_id in &projection_chunk_ids {
            reverse_map.insert(projection_chunk_id.clone(), artifact_id.clone());
        }

        Ok(TaskArtifactWriteResult {
            task_id,
            artifact_id,
            projection_chunk_ids,
        })
    }

    async fn get_task_artifact(
        &self,
        tenant_id: &TenantId,
        artifact_id: &str,
    ) -> Result<Option<TaskArtifact>> {
        let task_store = self.task_artifacts.read().unwrap();
        Ok(task_store
            .get(tenant_id.as_str())
            .and_then(|artifacts| artifacts.get(artifact_id))
            .cloned())
    }

    async fn list_task_artifacts(
        &self,
        tenant_id: &TenantId,
        task_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        let task_store = self.task_artifacts.read().unwrap();
        let mut artifacts = task_store
            .get(tenant_id.as_str())
            .map(|artifacts| {
                artifacts
                    .values()
                    .filter(|artifact| artifact.task_id == task_id)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        artifacts.sort_by(|left, right| {
            left.timestamp_created
                .cmp(&right.timestamp_created)
                .then_with(|| left.artifact_id.cmp(&right.artifact_id))
        });
        Ok(artifacts)
    }

    async fn list_thread_artifacts(
        &self,
        tenant_id: &TenantId,
        thread_id: &str,
    ) -> Result<Vec<TaskArtifact>> {
        let task_store = self.task_artifacts.read().unwrap();
        let mut artifacts = task_store
            .get(tenant_id.as_str())
            .map(|artifacts| {
                artifacts
                    .values()
                    .filter(|artifact| artifact.thread_key() == thread_id)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        artifacts.sort_by(|left, right| {
            left.timestamp_created
                .cmp(&right.timestamp_created)
                .then_with(|| left.artifact_id.cmp(&right.artifact_id))
        });
        Ok(artifacts)
    }

    async fn search_task_projection_chunk_ids(
        &self,
        tenant_id: &TenantId,
        filters: &TaskSearchFilters,
        limit: usize,
    ) -> Result<Vec<ChunkId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let task_store = self.task_artifacts.read().unwrap();
        let link_store = self.task_projection_links.read().unwrap();
        let mut chunk_ids = Vec::new();

        let Some(artifacts) = task_store.get(tenant_id.as_str()) else {
            return Ok(Vec::new());
        };
        let links = link_store.get(tenant_id.as_str());

        let mut matching = artifacts
            .values()
            .filter(|artifact| artifact_matches_filters(artifact, filters))
            .cloned()
            .collect::<Vec<_>>();
        matching.sort_by_key(|artifact| std::cmp::Reverse(artifact.timestamp_created));

        for artifact in matching {
            if let Some(chunk_list) =
                links.and_then(|tenant_links| tenant_links.get(&artifact.artifact_id))
            {
                for chunk_id in chunk_list {
                    chunk_ids.push(chunk_id.clone());
                    if chunk_ids.len() >= limit {
                        return Ok(chunk_ids);
                    }
                }
            }
        }

        Ok(chunk_ids)
    }

    async fn resolve_artifacts_for_chunks(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
    ) -> Result<HashMap<String, TaskArtifact>> {
        let reverse = self.projection_to_artifact.read().unwrap();
        let task_store = self.task_artifacts.read().unwrap();
        let mut resolved = HashMap::new();

        let Some(tenant_reverse) = reverse.get(tenant_id.as_str()) else {
            return Ok(resolved);
        };
        let Some(tenant_artifacts) = task_store.get(tenant_id.as_str()) else {
            return Ok(resolved);
        };

        for chunk_id in chunk_ids {
            if let Some(artifact_id) = tenant_reverse.get(&chunk_id.to_string()) {
                if let Some(artifact) = tenant_artifacts.get(artifact_id) {
                    resolved.insert(chunk_id.to_string(), artifact.clone());
                }
            }
        }

        Ok(resolved)
    }

    async fn list_feedback(
        &self,
        tenant_id: &TenantId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FeedbackEntry>> {
        let tenant = tenant_id.to_string();
        let normalized = super::normalize_query(query);
        let store = self.feedback.read().unwrap();
        let entries = store.get(&tenant).cloned().unwrap_or_default();
        let mut filtered: Vec<FeedbackEntry> = entries
            .into_iter()
            .filter(|entry| super::normalize_query(&entry.query) == normalized)
            .collect();
        filtered.sort_by(|a, b| b.timestamp_ms.cmp(&a.timestamp_ms));
        filtered.truncate(limit);
        Ok(filtered)
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

    async fn search_with_scores(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        let chunks = self.search(tenant_id, query, k).await?;
        let scored: Vec<(MemoryChunk, f32)> =
            chunks.into_iter().map(|chunk| (chunk, 1.0)).collect();
        let feedback = self.list_feedback(tenant_id, query, 512).await?;
        Ok(apply_feedback_scores(
            scored,
            query,
            &feedback,
            current_time_ms(),
            &FeedbackConfig::default(),
        ))
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

    async fn list_chunks(
        &self,
        tenant_id: &TenantId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MemoryChunk>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let tenant_str = tenant_id.to_string();
        let store = self.chunks.read().unwrap();
        let mut chunks: Vec<MemoryChunk> = store
            .get(&tenant_str)
            .map(|tenant_chunks| {
                tenant_chunks
                    .values()
                    .filter(|chunk| chunk.status != ChunkStatus::Deleted)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        chunks.sort_by_key(|chunk| std::cmp::Reverse(chunk.timestamp_created));
        if offset >= chunks.len() {
            return Ok(Vec::new());
        }

        Ok(chunks.into_iter().skip(offset).take(limit).collect())
    }
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn artifact_matches_filters(artifact: &TaskArtifact, filters: &TaskSearchFilters) -> bool {
    if let Some(task_id) = filters.task_id.as_deref() {
        if artifact.task_id != task_id {
            return false;
        }
    }
    if let Some(kind) = filters.artifact_kind {
        if artifact.artifact_kind != kind {
            return false;
        }
    }
    if let Some(status) = filters.status.as_deref() {
        if artifact.status.as_deref() != Some(status) {
            return false;
        }
    }
    if let Some(challenge_id) = filters.challenge_id.as_deref() {
        if artifact.challenge_id.as_deref() != Some(challenge_id) {
            return false;
        }
    }
    if let Some(thread_id) = filters.thread_id.as_deref() {
        if artifact.thread_key() != thread_id {
            return false;
        }
    }
    if let Some(reply_to_artifact_id) = filters.reply_to_artifact_id.as_deref() {
        if artifact.reply_to_artifact_id.as_deref() != Some(reply_to_artifact_id) {
            return false;
        }
    }
    if let Some(artifact_role) = filters.artifact_role.as_deref() {
        if artifact.artifact_role.as_deref() != Some(artifact_role) {
            return false;
        }
    }
    if let Some(project_id) = filters.project_id.as_deref() {
        if artifact.project_id.as_option() != Some(project_id) {
            return false;
        }
    }
    if let Some(agent_id) = filters.agent_id.as_deref() {
        if artifact.agent_id.as_deref() != Some(agent_id) {
            return false;
        }
    }
    if let Some(session_id) = filters.session_id.as_deref() {
        if artifact.session_id.as_deref() != Some(session_id) {
            return false;
        }
    }
    if let Some(tool_name) = filters.tool_name.as_deref() {
        if artifact.tool_name.as_deref() != Some(tool_name)
            && artifact.provenance.tool_name.as_deref() != Some(tool_name)
        {
            return false;
        }
    }
    if let Some(requested_action) = filters.requested_action.as_deref() {
        if artifact.requested_action.as_deref() != Some(requested_action) {
            return false;
        }
    }
    if let Some(verification_status) = filters.verification_status.as_deref() {
        if artifact.verification_status.as_deref() != Some(verification_status) {
            return false;
        }
    }
    if let Some(dataset_name) = filters.dataset_name.as_deref() {
        let has_match = artifact.dataset_refs.iter().any(|dataset| {
            dataset.name == dataset_name
                && filters
                    .dataset_version
                    .as_deref()
                    .map(|version| dataset.version.as_deref() == Some(version))
                    .unwrap_or(true)
        });
        if !has_match {
            return false;
        }
    } else if let Some(dataset_version) = filters.dataset_version.as_deref() {
        if !artifact
            .dataset_refs
            .iter()
            .any(|dataset| dataset.version.as_deref() == Some(dataset_version))
        {
            return false;
        }
    }
    if let Some(entity_name) = filters.entity_name.as_deref() {
        let has_match = artifact.entity_refs.iter().any(|entity| {
            entity.name == entity_name
                && filters
                    .entity_type
                    .as_deref()
                    .map(|entity_type| entity.entity_type == entity_type)
                    .unwrap_or(true)
        });
        if !has_match {
            return false;
        }
    } else if let Some(entity_type) = filters.entity_type.as_deref() {
        if !artifact
            .entity_refs
            .iter()
            .any(|entity| entity.entity_type == entity_type)
        {
            return false;
        }
    }
    true
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

    fn make_long_document() -> String {
        let sentence =
            "This is a long test sentence that should trigger document chunking behavior. ";
        sentence.repeat(40)
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
    async fn add_long_document_splits_into_multiple_chunks() {
        let store = MemoryStore::new();
        let tenant = make_tenant();
        let long_text = make_long_document();

        let _chunk_id = store
            .add(make_chunk(&tenant, &long_text, ChunkType::Doc))
            .await
            .unwrap();

        let stats = store.stats(&tenant).await.unwrap();
        assert!(stats.total_chunks > 1);
    }

    #[tokio::test]
    async fn feedback_adjusts_search_scores() {
        let store = MemoryStore::new();
        let tenant = make_tenant();

        let alpha = store
            .add(make_chunk(&tenant, "alpha parser notes", ChunkType::Doc))
            .await
            .unwrap();
        let beta = store
            .add(make_chunk(&tenant, "beta parser notes", ChunkType::Doc))
            .await
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        store
            .add_feedback(FeedbackEntry::new(
                tenant.clone(),
                "parser notes",
                alpha.clone(),
                crate::store::RelevanceLabel::Relevant,
                now_ms,
            ))
            .await
            .unwrap();
        store
            .add_feedback(FeedbackEntry::new(
                tenant.clone(),
                "parser notes",
                alpha.clone(),
                crate::store::RelevanceLabel::Relevant,
                now_ms,
            ))
            .await
            .unwrap();
        store
            .add_feedback(FeedbackEntry::new(
                tenant.clone(),
                "parser notes",
                beta.clone(),
                crate::store::RelevanceLabel::Irrelevant,
                now_ms,
            ))
            .await
            .unwrap();
        store
            .add_feedback(FeedbackEntry::new(
                tenant.clone(),
                "parser notes",
                beta.clone(),
                crate::store::RelevanceLabel::Irrelevant,
                now_ms,
            ))
            .await
            .unwrap();

        let ranked = store
            .search_with_scores(&tenant, "parser notes", 10)
            .await
            .unwrap();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0.chunk_id, alpha);
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
