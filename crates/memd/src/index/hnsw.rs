//! HNSW (Hierarchical Navigable Small World) index for warm tier
//!
//! Provides fast approximate nearest neighbor search using hnsw_rs.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::{Hnsw, Neighbour};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::{MemdError, Result};
use crate::index::embedding_cache::EmbeddingCache;
use crate::types::ChunkId;

/// Configuration for HNSW index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswConfig {
    /// Maximum number of connections per node (M parameter)
    pub max_connections: usize,
    /// Size of dynamic candidate list during construction (efConstruction)
    pub ef_construction: usize,
    /// Size of dynamic candidate list during search (efSearch)
    pub ef_search: usize,
    /// Maximum number of elements the index can hold
    pub max_elements: usize,
    /// Embedding dimension
    pub dimension: usize,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            max_connections: 16,   // M = 16 is common default
            ef_construction: 200,  // Higher = better quality, slower build
            ef_search: 50,         // Higher = better recall, slower search
            max_elements: 100_000, // 100K chunks per tenant
            dimension: 384,        // all-MiniLM-L6-v2 (TODO: 1024 for Qwen3 upgrade)
        }
    }
}

/// Result of a nearest neighbor search
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Chunk ID of the result
    pub chunk_id: ChunkId,
    /// Cosine similarity score (0.0 to 1.0)
    pub score: f32,
}

/// Internal ID to ChunkId mapping
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct IndexMapping {
    /// Internal ID -> ChunkId
    id_to_chunk: HashMap<usize, String>,
    /// ChunkId string -> Internal ID
    chunk_to_id: HashMap<String, usize>,
    /// Next available internal ID
    next_id: usize,
    /// Version for invalidation checking
    version: u64,
}

impl IndexMapping {
    fn new() -> Self {
        Self {
            id_to_chunk: HashMap::new(),
            chunk_to_id: HashMap::new(),
            next_id: 0,
            version: 0,
        }
    }

    fn insert(&mut self, chunk_id: &ChunkId) -> usize {
        let chunk_str = chunk_id.to_string();
        if let Some(&id) = self.chunk_to_id.get(&chunk_str) {
            return id;
        }

        let id = self.next_id;
        self.id_to_chunk.insert(id, chunk_str.clone());
        self.chunk_to_id.insert(chunk_str, id);
        self.next_id += 1;
        self.version += 1;
        id
    }

    fn get_chunk_id(&self, id: usize) -> Option<ChunkId> {
        self.id_to_chunk
            .get(&id)
            .and_then(|s| ChunkId::parse(s).ok())
    }
}

/// HNSW warm tier index
pub struct HnswIndex {
    /// The HNSW graph structure
    hnsw: RwLock<Hnsw<'static, f32, DistCosine>>,
    /// ID mapping
    mapping: RwLock<IndexMapping>,
    /// Embedding cache for persistence
    embedding_cache: RwLock<EmbeddingCache>,
    /// Configuration
    config: HnswConfig,
    /// Path for persistence (None = in-memory only)
    persist_path: Option<PathBuf>,
}

impl HnswIndex {
    /// Create a new empty HNSW index
    pub fn new(config: HnswConfig) -> Self {
        let hnsw = Hnsw::new(
            config.max_connections,
            config.max_elements,
            16, // max_layer
            config.ef_construction,
            DistCosine {},
        );

        Self {
            hnsw: RwLock::new(hnsw),
            mapping: RwLock::new(IndexMapping::new()),
            embedding_cache: RwLock::new(EmbeddingCache::new(config.dimension)),
            config,
            persist_path: None,
        }
    }

    /// Create a new index with persistence path
    ///
    /// Note: Loading from an existing index is not yet supported due to
    /// hnsw_rs lifetime constraints. If an index exists at the path, it
    /// will be ignored and a new empty index created.
    pub fn with_persistence(config: HnswConfig, path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let mut index = Self::new(config);
        index.persist_path = Some(path);
        Ok(index)
    }

    /// Insert a chunk embedding into the index
    pub fn insert(&self, chunk_id: &ChunkId, embedding: &[f32]) -> Result<()> {
        if embedding.len() != self.config.dimension {
            return Err(MemdError::ValidationError(format!(
                "Embedding dimension mismatch: expected {}, got {}. \
                 This usually means the embedding model changed. \
                 To fix: delete the data directory and restart, or use --rebuild-index flag.",
                self.config.dimension,
                embedding.len()
            )));
        }

        let internal_id = self.mapping.write().insert(chunk_id);

        // Store in cache for persistence
        self.embedding_cache.write().insert(internal_id, embedding)?;

        let hnsw = self.hnsw.write();
        hnsw.insert_slice((embedding, internal_id));

        Ok(())
    }

    /// Insert multiple embeddings in batch
    pub fn insert_batch(&self, items: &[(ChunkId, Vec<f32>)]) -> Result<()> {
        let mut mapping = self.mapping.write();
        let mut cache = self.embedding_cache.write();
        let hnsw = self.hnsw.write();

        for (chunk_id, embedding) in items {
            if embedding.len() != self.config.dimension {
                return Err(MemdError::ValidationError(format!(
                    "Embedding dimension mismatch for {}: expected {}, got {}. \
                     This usually means the embedding model changed. \
                     To fix: delete the data directory and restart, or use --rebuild-index flag.",
                    chunk_id,
                    self.config.dimension,
                    embedding.len()
                )));
            }

            let internal_id = mapping.insert(chunk_id);
            cache.insert(internal_id, embedding)?;
            hnsw.insert_slice((embedding, internal_id));
        }

        Ok(())
    }

    /// Search for nearest neighbors
    pub fn search(&self, query_embedding: &[f32], k: usize) -> Result<Vec<SearchResult>> {
        if query_embedding.len() != self.config.dimension {
            return Err(MemdError::ValidationError(format!(
                "Query embedding dimension mismatch: expected {}, got {}. \
                 This usually means the embedding model changed. \
                 To fix: delete the data directory and restart, or use --rebuild-index flag.",
                self.config.dimension,
                query_embedding.len()
            )));
        }

        // Lock in same order as insert: mapping first, then hnsw
        // This prevents deadlock when insert and search run concurrently
        let mapping = self.mapping.read();
        let hnsw = self.hnsw.read();

        let neighbors: Vec<Neighbour> = hnsw.search(query_embedding, k, self.config.ef_search);

        let results: Vec<SearchResult> = neighbors
            .into_iter()
            .filter_map(|n| {
                let chunk_id = mapping.get_chunk_id(n.d_id)?;
                // Convert distance to similarity (cosine distance = 1 - similarity)
                let score = 1.0 - n.distance;
                Some(SearchResult { chunk_id, score })
            })
            .collect();

        Ok(results)
    }

    /// Get the number of items in the index
    pub fn len(&self) -> usize {
        self.mapping.read().next_id
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the current version (for invalidation checking)
    pub fn version(&self) -> u64 {
        self.mapping.read().version
    }

    /// Save index to disk
    pub fn save(&self) -> Result<()> {
        let path = self
            .persist_path
            .as_ref()
            .ok_or_else(|| MemdError::StorageError("no persistence path configured".into()))?;

        self.save_to(path)
    }

    /// Save index to specific path
    pub fn save_to(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path)?;

        // Acquire read locks for consistent snapshot
        let mapping = self.mapping.read();
        let cache = self.embedding_cache.read();

        // Save embedding cache (atomic write with temp file)
        let cache_path = path.join("embeddings.bin");
        cache.save_to(&cache_path)?;

        // Save mapping
        let mapping_path = path.join("mapping.json.tmp");
        let mapping_json = serde_json::to_vec(&*mapping)
            .map_err(|e| MemdError::StorageError(format!("serialize mapping: {}", e)))?;

        let mut file = File::create(&mapping_path)?;
        file.write_all(&mapping_json)?;
        file.sync_all()?;
        drop(file);

        // Atomic rename
        std::fs::rename(&mapping_path, path.join("mapping.json"))?;

        // Save config
        let config_path_tmp = path.join("config.json.tmp");
        let config_json = serde_json::to_vec(&self.config)
            .map_err(|e| MemdError::StorageError(format!("serialize config: {}", e)))?;

        let mut file = File::create(&config_path_tmp)?;
        file.write_all(&config_json)?;
        file.sync_all()?;
        drop(file);

        // Atomic rename
        std::fs::rename(&config_path_tmp, path.join("config.json"))?;

        // Save HNSW graph using hnsw_rs file_dump
        let hnsw = self.hnsw.read();
        hnsw.file_dump(path, "graph")
            .map_err(|e| MemdError::StorageError(format!("dump hnsw: {:?}", e)))?;

        // Sync parent directory
        if let Ok(dir) = File::open(path) {
            let _ = dir.sync_all();
        }

        tracing::info!("Saved HNSW index to {:?}", path);
        Ok(())
    }

    /// Load index from disk and rebuild HNSW from cached embeddings
    ///
    /// Loads the embedding cache and mapping, validates consistency, then
    /// rebuilds the HNSW graph from the cached embeddings. This is much faster
    /// than re-embedding (50-100x speedup).
    pub fn load(path: &Path, config: HnswConfig) -> Result<Self> {
        use std::time::Instant;

        let start = Instant::now();

        // Load mapping to get chunk IDs
        let mapping_path = path.join("mapping.json");
        let mut file = File::open(&mapping_path)?;
        let mut mapping_json = Vec::new();
        file.read_to_end(&mut mapping_json)?;

        let mapping: IndexMapping = serde_json::from_slice(&mapping_json)
            .map_err(|e| MemdError::StorageError(format!("deserialize mapping: {}", e)))?;

        // Try to load embedding cache
        let cache_path = path.join("embeddings.bin");
        let embedding_cache = if cache_path.exists() {
            match EmbeddingCache::load_from(&cache_path) {
                Ok(cache) => {
                    // Validate consistency
                    if let Err(e) = cache.validate_consistency(config.dimension, mapping.next_id) {
                        tracing::warn!(
                            "Embedding cache validation failed: {}. Will need rebuild from segments.",
                            e
                        );
                        // Delete corrupted cache
                        let _ = std::fs::remove_file(&cache_path);
                        EmbeddingCache::new(config.dimension)
                    } else {
                        cache
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to load embedding cache: {}. Will need rebuild from segments.",
                        e
                    );
                    // Delete corrupted cache
                    let _ = std::fs::remove_file(&cache_path);
                    EmbeddingCache::new(config.dimension)
                }
            }
        } else {
            tracing::info!("No embedding cache found. Will need rebuild from segments.");
            EmbeddingCache::new(config.dimension)
        };

        // Create fresh HNSW
        let mut hnsw = Hnsw::new(
            config.max_connections,
            config.max_elements,
            16,
            config.ef_construction,
            DistCosine {},
        );

        // Rebuild HNSW from cache if available
        let rebuild_count = if !embedding_cache.is_empty() {
            let mut count = 0;
            for (internal_id, embedding) in embedding_cache.iter_valid() {
                hnsw.insert_slice((embedding, internal_id));
                count += 1;
            }
            count
        } else {
            0
        };

        let elapsed = start.elapsed();

        if rebuild_count > 0 {
            tracing::info!(
                "Rebuilt HNSW index from cache: {} embeddings in {:?}",
                rebuild_count,
                elapsed
            );
        } else {
            tracing::info!("Created empty HNSW index (no cache available)");
        }

        Ok(Self {
            hnsw: RwLock::new(hnsw),
            mapping: RwLock::new(mapping),
            embedding_cache: RwLock::new(embedding_cache),
            config,
            persist_path: Some(path.to_path_buf()),
        })
    }

    /// Check if index needs rebuild (segment version changed)
    pub fn needs_rebuild(&self, segment_version: u64) -> bool {
        self.version() != segment_version
    }

    /// Get rebuild statistics (cache size, HNSW size)
    pub fn rebuild_stats(&self) -> (usize, usize) {
        let cache_size = self.embedding_cache.read().len();
        let hnsw_size = self.len();
        (cache_size, hnsw_size)
    }

    /// Check if embedding cache is empty (requires rebuild from segments)
    pub fn cache_is_empty(&self) -> bool {
        self.embedding_cache.read().is_empty()
    }

    /// Get read access to the embedding cache (for compaction rebuild)
    pub(crate) fn get_embedding_cache(&self) -> &RwLock<EmbeddingCache> {
        &self.embedding_cache
    }

    /// Get read access to the index mapping (for compaction)
    pub(crate) fn get_mapping(&self) -> &RwLock<IndexMapping> {
        &self.mapping
    }

    /// Get the HNSW configuration
    pub fn config(&self) -> &HnswConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize(v: &mut [f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
    }

    #[test]
    fn test_insert_and_search() {
        let config = HnswConfig {
            max_elements: 100,
            dimension: 4,
            ..Default::default()
        };

        let index = HnswIndex::new(config);

        // Insert some vectors
        let chunk1 = ChunkId::new();
        let chunk2 = ChunkId::new();
        let chunk3 = ChunkId::new();

        let mut emb1 = vec![1.0, 0.0, 0.0, 0.0];
        let mut emb2 = vec![0.9, 0.1, 0.0, 0.0]; // Similar to emb1
        let mut emb3 = vec![0.0, 0.0, 1.0, 0.0]; // Different

        normalize(&mut emb1);
        normalize(&mut emb2);
        normalize(&mut emb3);

        index.insert(&chunk1, &emb1).unwrap();
        index.insert(&chunk2, &emb2).unwrap();
        index.insert(&chunk3, &emb3).unwrap();

        assert_eq!(index.len(), 3);

        // Search for something similar to emb1
        let results = index.search(&emb1, 2).unwrap();

        assert_eq!(results.len(), 2);
        // First result should be chunk1 itself (exact match)
        assert_eq!(results[0].chunk_id, chunk1);
        assert!(results[0].score > 0.99);

        // Second should be chunk2 (similar)
        assert_eq!(results[1].chunk_id, chunk2);
        assert!(results[1].score > 0.9);
    }

    #[test]
    fn test_batch_insert() {
        let config = HnswConfig {
            max_elements: 100,
            dimension: 4,
            ..Default::default()
        };

        let index = HnswIndex::new(config);

        let items: Vec<(ChunkId, Vec<f32>)> = (0..10)
            .map(|i| {
                let mut emb = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
                normalize(&mut emb);
                (ChunkId::new(), emb)
            })
            .collect();

        index.insert_batch(&items).unwrap();

        assert_eq!(index.len(), 10);
    }

    #[test]
    fn test_dimension_mismatch() {
        let config = HnswConfig {
            dimension: 4,
            ..Default::default()
        };

        let index = HnswIndex::new(config);

        let chunk_id = ChunkId::new();
        let wrong_dim = vec![1.0, 0.0]; // Only 2 dimensions

        let result = index.insert(&chunk_id, &wrong_dim);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("dimension mismatch"),
            "error should mention dimension mismatch"
        );
        assert!(
            err_msg.contains("rebuild-index") || err_msg.contains("delete the data"),
            "error should include rebuild instructions"
        );
    }

    #[test]
    fn test_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index");

        let config = HnswConfig {
            max_elements: 100,
            dimension: 4,
            ..Default::default()
        };

        let chunk_id = ChunkId::new();
        let chunk_id_str = chunk_id.to_string();

        // Create, populate, and save index
        {
            let index = HnswIndex::with_persistence(config.clone(), &path).unwrap();

            let mut emb = vec![1.0, 0.0, 0.0, 0.0];
            normalize(&mut emb);

            index.insert(&chunk_id, &emb).unwrap();
            index.save().unwrap();
        }

        // Verify files were created
        assert!(path.join("mapping.json").exists());
        assert!(path.join("config.json").exists());

        // Load mapping (note: HNSW graph load not fully implemented)
        {
            let index = HnswIndex::load(&path, config).unwrap();
            // Mapping should be loaded
            let mapping = index.mapping.read();
            assert!(mapping.chunk_to_id.contains_key(&chunk_id_str));
        }
    }

    #[test]
    fn test_config_defaults() {
        let config = HnswConfig::default();
        assert_eq!(config.max_connections, 16);
        assert_eq!(config.ef_construction, 200);
        assert_eq!(config.ef_search, 50);
        assert_eq!(config.dimension, 384);
    }

    #[test]
    fn test_concurrent_insert_search() {
        use std::sync::Arc;
        use std::thread;

        let config = HnswConfig {
            max_elements: 1000,
            dimension: 4,
            ..Default::default()
        };

        let index = Arc::new(HnswIndex::new(config));

        // Spawn 100 insert threads and 100 search threads concurrently
        let mut handles = vec![];

        // Insert threads
        for i in 0..100 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                let chunk_id = ChunkId::new();
                let mut emb = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
                normalize(&mut emb);
                idx.insert(&chunk_id, &emb).unwrap();
            }));
        }

        // Search threads
        for i in 0..100 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                let mut query = vec![i as f32, (i + 1) as f32, (i + 2) as f32, (i + 3) as f32];
                normalize(&mut query);
                // This should not deadlock
                let _ = idx.search(&query, 10);
            }));
        }

        // Wait for all threads to complete
        // If there's a deadlock, this will hang
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify index has data
        assert!(index.len() > 0);
    }
}
