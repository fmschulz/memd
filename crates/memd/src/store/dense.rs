//! Dense vector search coordinator
//!
//! Combines embeddings and HNSW index for semantic search.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::compaction::hnsw_rebuild::{HnswRebuilder, RebuildResult};
use crate::embeddings::{CandleEmbedder, CandleModel, Embedder, EmbeddingConfig};
use crate::error::{MemdError, Result};
use crate::index::{HnswConfig, HnswIndex};
use crate::types::{ChunkId, TenantId};

/// Result of a dense search with chunk content
#[derive(Debug, Clone)]
pub struct DenseSearchResult {
    /// Chunk ID
    pub chunk_id: ChunkId,
    /// Cosine similarity score (0.0 to 1.0)
    pub score: f32,
}

/// Configuration for dense search
#[derive(Debug, Clone)]
pub struct DenseSearchConfig {
    /// HNSW configuration
    pub hnsw: HnswConfig,
    /// Whether to persist index
    pub persist: bool,
    /// Candle embedder model to load (selected via `--embedding-model`).
    pub model: CandleModel,
}

impl Default for DenseSearchConfig {
    fn default() -> Self {
        Self {
            hnsw: HnswConfig::default(),
            persist: true,
            model: CandleModel::default(),
        }
    }
}

fn embedding_batch_size() -> usize {
    std::env::var("MEMD_EMBED_BATCH_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(EmbeddingConfig::default().batch_size)
}

/// Dense search coordinator for a tenant
pub struct DenseSearcher {
    /// Embedding model (shared across tenants)
    embedder: Arc<dyn Embedder>,
    /// Per-tenant HNSW indices
    indices: RwLock<HashMap<String, Arc<HnswIndex>>>,
    /// Base path for index persistence
    base_path: Option<PathBuf>,
    /// Disable disk writes and index mutations.
    read_only: bool,
    /// Why the embedding model cannot load, once an embedding call found
    /// out. From then on this searcher does not call the embedder again:
    /// indexing skips the dense side and search reports the failure.
    unavailable: OnceLock<String>,
    /// Whether this process has already recorded in the rebuild marker that
    /// some chunks lack vectors. The lock also keeps two skipping writes
    /// from writing the marker at once.
    rebuild_recorded: Mutex<bool>,
    /// Configuration
    config: DenseSearchConfig,
}

impl DenseSearcher {
    pub(crate) fn model(&self) -> CandleModel {
        self.config.model
    }

    /// Create a new dense searcher with Candle embedder.
    ///
    /// The embedder loads its model on the first embedding call, so opening a
    /// store for a command that never embeds costs no model load.
    pub fn new(mut config: DenseSearchConfig) -> Self {
        let embedding_config = EmbeddingConfig {
            batch_size: embedding_batch_size(),
            ..EmbeddingConfig::default()
        };
        let embedder = Arc::new(CandleEmbedder::with_config_and_model(
            embedding_config,
            config.model,
        ));

        // Update HNSW config dimension to match model
        config.hnsw.dimension = embedder.dimension();

        Self::with_embedder(embedder, config)
    }

    /// Create with custom embedder (for testing with MockEmbedder)
    pub fn with_embedder(embedder: Arc<dyn Embedder>, config: DenseSearchConfig) -> Self {
        Self {
            embedder,
            indices: RwLock::new(HashMap::new()),
            base_path: None,
            read_only: false,
            unavailable: OnceLock::new(),
            rebuild_recorded: Mutex::new(false),
            config,
        }
    }

    /// Why the embedding model cannot load, if an embedding call has found
    /// out. `None` means the model works or has not been needed yet.
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.get().map(String::as_str)
    }

    /// Indexing skipped because the model cannot load. The rows are marked
    /// indexed by the caller but hold no vector, so the first skip in this
    /// process makes that durable before the write returns: the embedding
    /// revision marker moves to `rebuilding`, and the next writer whose model
    /// loads re-embeds the chunks that lack a vector. If the marker cannot be
    /// written, the error reaches the caller and the rows stay index failures,
    /// which a later pass retries.
    fn skip_without_model(&self) -> Result<()> {
        let Some(data_dir) = &self.base_path else {
            return Ok(());
        };
        let mut recorded = self.rebuild_recorded.lock();
        if !*recorded {
            super::embedding_revision::mark_rebuilding(data_dir)?;
            *recorded = true;
        }
        Ok(())
    }

    /// The embedding result, remembering a model that cannot load.
    fn note_model_failure<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(MemdError::ModelUnavailable(cause)) = &result {
            let _ = self.unavailable.set(cause.clone());
        }
        result
    }

    /// Set base path for index persistence
    pub fn with_base_path(mut self, path: PathBuf) -> Self {
        self.base_path = Some(path);
        self
    }

    /// Configure read-only mode for persisted warm indexes.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Get or create index for a tenant
    fn get_or_create_index(&self, tenant_id: &TenantId) -> Result<Arc<HnswIndex>> {
        let tenant_str = tenant_id.to_string();

        // Fast path: read lock
        {
            let indices = self.indices.read();
            if let Some(index) = indices.get(&tenant_str) {
                return Ok(Arc::clone(index));
            }
        }

        // Slow path: write lock + create
        let mut indices = self.indices.write();

        // Double-check
        if let Some(index) = indices.get(&tenant_str) {
            return Ok(Arc::clone(index));
        }

        let index = if self.config.persist {
            if let Some(ref base_path) = self.base_path {
                let index_path = base_path
                    .join("tenants")
                    .join(&tenant_str)
                    .join("warm_index");
                if self.read_only {
                    HnswIndex::with_persistence_read_only(self.config.hnsw.clone(), index_path)?
                } else {
                    HnswIndex::with_persistence(self.config.hnsw.clone(), index_path)?
                }
            } else {
                HnswIndex::new(self.config.hnsw.clone())
            }
        } else {
            HnswIndex::new(self.config.hnsw.clone())
        };

        let index = Arc::new(index);
        indices.insert(tenant_str, Arc::clone(&index));

        Ok(index)
    }

    /// Index a chunk embedding
    pub async fn index_chunk(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        text: &str,
    ) -> Result<()> {
        if self.read_only {
            return Err(MemdError::ReadOnlyStore {
                op: "dense_index_chunk".to_string(),
            });
        }
        if self.unavailable.get().is_some() {
            return self.skip_without_model();
        }
        // Documents are embedded verbatim via embed_texts; embed_query would
        // apply a retrieval query prefix (BGE), corrupting stored vectors.
        let embedded = self.note_model_failure(self.embedder.embed_texts(&[text]).await);
        let embedding = match embedded {
            Ok(vectors) => vectors.into_iter().next().ok_or_else(|| {
                MemdError::EmbeddingError("embed_texts returned no embedding".to_string())
            })?,
            // The model cannot load: the dense side waits for a repair pass.
            Err(MemdError::ModelUnavailable(_)) => return self.skip_without_model(),
            Err(error) => return Err(error),
        };
        let index = self.get_or_create_index(tenant_id)?;
        index.insert(chunk_id, &embedding)?;

        tracing::debug!(
            tenant_id = %tenant_id,
            chunk_id = %chunk_id,
            "indexed chunk in HNSW"
        );

        Ok(())
    }

    /// Index multiple chunks in batch
    pub async fn index_batch(
        &self,
        tenant_id: &TenantId,
        chunks: &[(ChunkId, String)],
    ) -> Result<()> {
        if self.read_only {
            return Err(MemdError::ReadOnlyStore {
                op: "dense_index_batch".to_string(),
            });
        }
        if chunks.is_empty() {
            return Ok(());
        }

        // A model that cannot load leaves the dense side unindexed, as when a
        // store has no dense searcher. The rows wait for a repair pass that
        // has the model; the caller can tell from `unavailable_reason`, and
        // the rebuild marker tells the next writer.
        if self.unavailable.get().is_some() {
            return self.skip_without_model();
        }

        // Batch embed all texts
        let texts: Vec<&str> = chunks.iter().map(|(_, text)| text.as_str()).collect();
        let embeddings = match self.note_model_failure(self.embedder.embed_texts(&texts).await) {
            Ok(embeddings) => embeddings,
            Err(MemdError::ModelUnavailable(_)) => return self.skip_without_model(),
            Err(error) => return Err(error),
        };

        // Insert into index
        let index = self.get_or_create_index(tenant_id)?;
        let items: Vec<(ChunkId, Vec<f32>)> = chunks
            .iter()
            .zip(embeddings)
            .map(|((chunk_id, _), emb)| (chunk_id.clone(), emb))
            .collect();

        index.insert_batch(&items)?;

        tracing::debug!(
            tenant_id = %tenant_id,
            count = chunks.len(),
            "indexed batch in HNSW"
        );

        Ok(())
    }

    /// Search for similar chunks
    pub async fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<DenseSearchResult>> {
        let (results, _, _) = self.search_with_timing(tenant_id, query, k).await?;
        Ok(results)
    }

    /// Search for similar chunks with timing information
    ///
    /// Returns (results, embed_time, search_time)
    pub async fn search_with_timing(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<(Vec<DenseSearchResult>, Duration, Duration)> {
        // A model that cannot load cannot answer a query. Callers fall back
        // to the keyword index on `ModelUnavailable`.
        if let Some(cause) = self.unavailable.get() {
            return Err(MemdError::ModelUnavailable(cause.clone()));
        }
        let embed_start = Instant::now();
        let query_embedding = self.note_model_failure(self.embedder.embed_query(query).await)?;
        let embed_time = embed_start.elapsed();

        let search_start = Instant::now();
        let index = self.get_or_create_index(tenant_id)?;
        let results = index.search(&query_embedding, k)?;
        let search_time = search_start.elapsed();

        let dense_results: Vec<DenseSearchResult> = results
            .into_iter()
            .map(|r| DenseSearchResult {
                chunk_id: r.chunk_id,
                score: r.score,
            })
            .collect();

        tracing::debug!(
            tenant_id = %tenant_id,
            query_len = query.len(),
            results = dense_results.len(),
            embed_ms = embed_time.as_millis(),
            search_ms = search_time.as_millis(),
            "dense search completed"
        );

        Ok((dense_results, embed_time, search_time))
    }

    /// Save every index that changed since its last save.
    ///
    /// A failed save does not stop the other tenants. The first error is
    /// returned once all of them have been tried, and the failed index stays
    /// dirty so the next call tries it again.
    pub fn save_all(&self) -> Result<()> {
        if self.read_only {
            tracing::debug!("read-only dense searcher skips save_all");
            return Ok(());
        }
        let indices = self.indices.read();
        let mut first_error = None;
        for (tenant_id, index) in indices.iter() {
            if let Err(e) = index.save_if_dirty() {
                tracing::warn!(tenant_id, error = %e, "failed to save index");
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Get embedding dimension
    pub fn dimension(&self) -> usize {
        self.embedder.dimension()
    }

    /// Whether the embedding model is in memory. A command that never embeds
    /// leaves it unloaded.
    pub fn embedder_loaded(&self) -> bool {
        self.embedder.is_loaded()
    }

    /// Get the number of indexed chunks for a tenant
    pub fn index_len(&self, tenant_id: &TenantId) -> usize {
        let indices = self.indices.read();
        let tenant_str = tenant_id.to_string();
        indices.get(&tenant_str).map(|i| i.len()).unwrap_or(0)
    }

    /// Check whether a specific chunk is present in the tenant's HNSW
    /// mapping.
    ///
    /// Used by startup backfill: comparing counts (`index_len` vs metadata
    /// row count) is unreliable because HNSW's `next_id` counter never
    /// decrements on delete, so a tenant with deletes + re-adds + a crash
    /// can satisfy `index_len >= active_count` while still missing live
    /// chunks. Per-chunk membership is the authoritative signal.
    pub fn contains_chunk(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> bool {
        let indices = self.indices.read();
        let tenant_str = tenant_id.to_string();
        match indices.get(&tenant_str) {
            Some(index) => index
                .get_mapping()
                .read()
                .get_internal_id(chunk_id)
                .is_some(),
            None => false,
        }
    }

    /// Check whether any of the supplied chunks still have valid
    /// embeddings in the tenant cache.
    ///
    /// HNSW compaction keeps the chunk-to-internal-id mapping stable
    /// after rebuild, but invalidates excluded embeddings in the cache.
    /// This helper distinguishes "known mapping" from "still live in the
    /// dense rebuild source", which is the signal purge/compaction needs.
    pub(crate) fn has_valid_embeddings_for_chunks(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &HashSet<ChunkId>,
    ) -> bool {
        if chunk_ids.is_empty() {
            return false;
        }

        let indices = self.indices.read();
        let tenant_str = tenant_id.to_string();
        let Some(index) = indices.get(&tenant_str) else {
            return false;
        };

        let mapping = index.get_mapping().read();
        let cache = index.get_embedding_cache().read();
        chunk_ids.iter().any(|chunk_id| {
            mapping
                .get_internal_id(chunk_id)
                .and_then(|internal_id| cache.get(internal_id))
                .is_some()
        })
    }

    /// Ensure the tenant's persisted HNSW index (mapping + embedding cache)
    /// is loaded into memory. The backfill calls this before deciding what is
    /// "missing", so a clean persisted index is not needlessly re-embedded.
    pub(crate) fn ensure_index_loaded(&self, tenant_id: &TenantId) -> Result<()> {
        self.get_or_create_index(tenant_id)?;
        Ok(())
    }

    /// Of `chunk_ids`, return those WITHOUT a live cached embedding in the
    /// loaded index — no mapping entry, OR a mapping entry whose embedding
    /// cache slot is empty (e.g. after a missing/corrupt `embeddings.bin`,
    /// where the loader keeps the mapping but resets the cache). Takes the
    /// per-tenant locks once. If the index is not loaded, every chunk is
    /// reported missing — callers should `ensure_index_loaded` first.
    ///
    /// This is the cache-aware membership the backfill needs: `contains_chunk`
    /// (mapping-only) would wrongly treat cache-less chunks as present and
    /// skip re-embedding them, leaving the index without usable vectors.
    pub(crate) fn chunks_missing_embeddings(
        &self,
        tenant_id: &TenantId,
        chunk_ids: &[ChunkId],
    ) -> Vec<ChunkId> {
        let indices = self.indices.read();
        let Some(index) = indices.get(&tenant_id.to_string()) else {
            return chunk_ids.to_vec();
        };
        let mapping = index.get_mapping().read();
        let cache = index.get_embedding_cache().read();
        chunk_ids
            .iter()
            .filter(|chunk_id| {
                mapping
                    .get_internal_id(chunk_id)
                    .and_then(|internal_id| cache.get(internal_id))
                    .is_none()
            })
            .cloned()
            .collect()
    }

    /// Get rebuild stats (cache_size, index_size) for a tenant
    ///
    /// Returns (0, 0) if no index exists for the tenant.
    pub fn get_rebuild_stats(&self, tenant_id: &TenantId) -> (usize, usize) {
        let indices = self.indices.read();
        if let Some(index) = indices.get(&tenant_id.to_string()) {
            index.rebuild_stats()
        } else {
            (0, 0)
        }
    }

    /// Rebuild HNSW index for a tenant, excluding deleted chunk IDs
    ///
    /// Creates a new clean index from embedding cache, filtering out
    /// deleted entries. Returns rebuild statistics.
    ///
    /// Note: Full atomic swap requires HnswIndex to support construction
    /// from a pre-built Hnsw graph. Currently returns rebuild result
    /// only - the index is rebuilt in place conceptually.
    pub fn rebuild_hnsw_for_tenant(
        &self,
        tenant_id: &TenantId,
        deleted_chunk_ids: &HashSet<ChunkId>,
    ) -> Result<RebuildResult> {
        let tenant_str = tenant_id.to_string();

        // Get existing index (read lock)
        let indices = self.indices.read();
        let existing_index = indices.get(&tenant_str).ok_or_else(|| {
            MemdError::StorageError(format!("no index for tenant: {}", tenant_id))
        })?;

        // Convert chunk IDs to internal IDs
        let mapping = existing_index.get_mapping().read();
        let deleted_internal_ids: HashSet<usize> = deleted_chunk_ids
            .iter()
            .filter_map(|chunk_id| mapping.get_internal_id(chunk_id))
            .collect();
        drop(mapping);

        // Use HnswRebuilder to create a clean graph that excludes
        // the deleted internal IDs.
        let rebuilder = HnswRebuilder::new();
        let (new_hnsw, result) = rebuilder.rebuild_clean(
            existing_index,
            &deleted_internal_ids,
            existing_index.config(),
        )?;

        // Phase 3.2: swap the new graph into the live HnswIndex AND
        // mark the excluded IDs invalid in the embedding cache so
        // save/load stay consistent with the rebuilt graph. Without
        // the cache invalidation, a restart would resurrect the
        // compacted-away points (the Codex-flagged durability gap).
        existing_index.swap_graph(new_hnsw, &deleted_internal_ids);

        tracing::info!(
            tenant_id = %tenant_id,
            processed = result.embeddings_processed,
            included = result.embeddings_included,
            excluded = result.embeddings_excluded,
            duration_ms = result.duration.as_millis(),
            "HNSW rebuild completed and swapped for tenant"
        );

        Ok(result)
    }
}

impl Drop for DenseSearcher {
    fn drop(&mut self) {
        if self.config.persist {
            if let Err(e) = self.save_all() {
                tracing::warn!(error = %e, "failed to save indices on drop");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::MockEmbedder;
    use std::path::Path;

    fn searcher(base: &Path) -> DenseSearcher {
        DenseSearcher::with_embedder(Arc::new(MockEmbedder::new()), DenseSearchConfig::default())
            .with_base_path(base.to_path_buf())
    }

    fn rows(texts: &[&str]) -> Vec<(ChunkId, String)> {
        texts
            .iter()
            .map(|text| (ChunkId::new(), (*text).to_string()))
            .collect()
    }

    fn warm_index(base: &Path, tenant: &TenantId) -> PathBuf {
        base.join("tenants")
            .join(tenant.to_string())
            .join("warm_index")
    }

    /// The saved files tell whether a save ran: a file removed after a save
    /// comes back only if another save writes it.
    #[tokio::test]
    async fn save_all_writes_only_what_changed_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let tenant = TenantId::new("dirty_flag").unwrap();
        let warm = warm_index(dir.path(), &tenant);
        let cache = warm.join("embeddings.bin");
        let searcher = searcher(dir.path());

        // An index that holds nothing new is not written.
        searcher.ensure_index_loaded(&tenant).unwrap();
        searcher.save_all().unwrap();
        assert!(!warm.join("mapping.bin").exists(), "unchanged index saved");

        searcher
            .index_batch(&tenant, &rows(&["alpha", "beta"]))
            .await
            .unwrap();
        searcher.save_all().unwrap();
        for name in ["mapping.bin", "embeddings.bin", "config.json"] {
            assert!(warm.join(name).exists(), "{name} missing after save");
        }

        // The second save finds nothing new.
        std::fs::remove_file(&cache).unwrap();
        searcher.save_all().unwrap();
        assert!(!cache.exists(), "a saved index was saved again");

        // One more chunk makes the index dirty again, and it saves once. Drop
        // is not a second save.
        searcher
            .index_batch(&tenant, &rows(&["gamma"]))
            .await
            .unwrap();
        searcher.save_all().unwrap();
        assert!(cache.exists(), "a changed index was not saved");
        std::fs::remove_file(&cache).unwrap();
        drop(searcher);
        assert!(!cache.exists(), "drop saved an index that was clean");
    }

    #[tokio::test]
    async fn save_all_returns_the_save_error_and_keeps_the_index_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let tenant = TenantId::new("save_error").unwrap();
        let warm = warm_index(dir.path(), &tenant);
        let searcher = searcher(dir.path());
        searcher
            .index_batch(&tenant, &rows(&["alpha"]))
            .await
            .unwrap();

        // A file where the index directory must go makes the save fail.
        std::fs::create_dir_all(warm.parent().unwrap()).unwrap();
        std::fs::write(&warm, b"not a directory").unwrap();
        searcher
            .save_all()
            .expect_err("a failed save must not report success");

        // The failed save did not clear the change: the next save writes it.
        std::fs::remove_file(&warm).unwrap();
        searcher.save_all().unwrap();
        assert!(warm.join("mapping.bin").exists());
    }

    #[tokio::test]
    async fn save_all_reports_a_failure_after_saving_the_other_tenants() {
        let dir = tempfile::tempdir().unwrap();
        let broken = TenantId::new("save_broken").unwrap();
        let healthy = TenantId::new("save_healthy").unwrap();
        let searcher = searcher(dir.path());
        for tenant in [&broken, &healthy] {
            searcher
                .index_batch(tenant, &rows(&["alpha"]))
                .await
                .unwrap();
        }
        let warm = warm_index(dir.path(), &broken);
        std::fs::create_dir_all(warm.parent().unwrap()).unwrap();
        std::fs::write(&warm, b"not a directory").unwrap();

        assert!(searcher.save_all().is_err());
        assert!(warm_index(dir.path(), &healthy)
            .join("mapping.bin")
            .exists());
    }
}
