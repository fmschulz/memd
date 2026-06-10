//! HNSW (Hierarchical Navigable Small World) index for warm tier
//!
//! Provides fast approximate nearest neighbor search using hnsw_rs.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::{Hnsw, Neighbour};
use hnsw_rs::hnswio::HnswIo;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::compaction::hnsw_rebuild::{HnswRebuilder, RebuildResult};
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
    /// If true, also persist the full HNSW graph to disk as
    /// `graph.hnsw.{graph,data}` for fast startup. If false, only the
    /// embedding cache + mapping are written and the graph is rebuilt
    /// from the cache on load. The cache is already the source of truth
    /// so this halves warm_index disk footprint on disk-constrained
    /// installs. Defaults to true; older on-disk configs that lack the
    /// field deserialize to true via `default_persist_graph_dump`.
    #[serde(default = "default_persist_graph_dump")]
    pub persist_graph_dump: bool,
}

fn default_persist_graph_dump() -> bool {
    true
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            max_connections: 16,   // M = 16 is common default
            ef_construction: 200,  // Higher = better quality, slower build
            ef_search: 50,         // Higher = better recall, slower search
            max_elements: 100_000, // 100K chunks per tenant
            dimension: 384,        // all-MiniLM-L6-v2 (TODO: 1024 for Qwen3 upgrade)
            persist_graph_dump: true,
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

/// Internal ID to ChunkId mapping.
///
/// Exposed as `pub` (not `pub(crate)`) so integration tests can
/// round-trip the legacy `mapping.json` → `mapping.bin` migration path.
/// Struct fields remain private.
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexMapping {
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

    /// Get internal ID for a chunk (for compaction)
    pub(crate) fn get_internal_id(&self, chunk_id: &ChunkId) -> Option<usize> {
        self.chunk_to_id.get(&chunk_id.to_string()).copied()
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
    /// Atomically replace the internal Hnsw graph with a rebuilt one
    /// and mark the excluded entries invalid in the embedding cache.
    ///
    /// Phase 3.2: `rebuild_hnsw_for_tenant` used to compute a fresh
    /// graph excluding deleted entries but then dropped it, leaving
    /// the live index unchanged. This method closes that gap.
    ///
    /// Codex follow-up on 3.2: if we only replace the in-memory graph
    /// and leave the embedding cache / mapping alone, a subsequent
    /// `save()` persists the full cache, and on restart `load()`
    /// rebuilds the graph from ALL cached embeddings — resurrecting
    /// the points we just compacted away. To keep compaction durable
    /// across restarts, `swap_graph` now also marks the excluded IDs
    /// invalid in the embedding cache so `save` / `load` see the same
    /// view as the live graph.
    ///
    /// The write-lock order is hnsw → embedding_cache; both are held
    /// briefly and released before returning. Internal IDs remain
    /// stable: we do NOT renumber, we just flip valid bits.
    pub fn swap_graph(
        &self,
        new_hnsw: Hnsw<'static, f32, DistCosine>,
        excluded_internal_ids: &std::collections::HashSet<usize>,
    ) {
        {
            let mut guard = self.hnsw.write();
            *guard = new_hnsw;
        }
        let mut cache = self.embedding_cache.write();
        for id in excluded_internal_ids {
            cache.mark_invalid(*id);
        }
    }

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
    /// If a persisted mapping/cache exists at the path, attempt to load and
    /// rebuild the graph from cached embeddings.
    pub fn with_persistence(config: HnswConfig, path: impl AsRef<Path>) -> Result<Self> {
        Self::with_persistence_mode(config, path, false)
    }

    /// Open a persisted index for read-only search without filesystem cleanup.
    pub fn with_persistence_read_only(config: HnswConfig, path: impl AsRef<Path>) -> Result<Self> {
        Self::with_persistence_mode(config, path, true)
    }

    fn with_persistence_mode(
        config: HnswConfig,
        path: impl AsRef<Path>,
        read_only: bool,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if !read_only {
            // One-shot cleanup of orphan dumps from older memd versions.
            // Safe to call every load: it only removes `graph-NNNN.hnsw.*`
            // files that the loader never reads anyway.
            let _ = Self::purge_orphan_dumps(&path);
        }

        // Accept either the new bincode mapping or the legacy JSON
        // mapping. Order matters only for layout discoverability; load()
        // itself prefers .bin.
        let has_mapping = path.join("mapping.bin").exists() || path.join("mapping.json").exists();
        if has_mapping {
            match Self::load_with_mode(&path, config.clone(), read_only) {
                Ok(index) => {
                    tracing::info!("Loaded persisted HNSW index from {:?}", path);
                    return Ok(index);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = ?path,
                        "failed to load persisted HNSW index, creating empty index"
                    );
                }
            }
        }

        let mut index = Self::new(config);
        if !read_only {
            index.persist_path = Some(path);
        }
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
        self.embedding_cache
            .write()
            .insert(internal_id, embedding)?;

        let hnsw = self.hnsw.write();
        hnsw.insert_slice((embedding, internal_id));

        Ok(())
    }

    /// Insert multiple embeddings in batch
    pub fn insert_batch(&self, items: &[(ChunkId, Vec<f32>)]) -> Result<()> {
        let mut mapping = self.mapping.write();
        let mut cache = self.embedding_cache.write();
        let mut to_insert = Vec::with_capacity(items.len());

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
            to_insert.push((embedding.as_slice(), internal_id));
        }
        drop(cache);
        drop(mapping);

        let hnsw = self.hnsw.write();
        let threshold = std::thread::available_parallelism()
            .map(|n| std::cmp::max(64, n.get().saturating_mul(8)))
            .unwrap_or(64);

        if to_insert.len() >= threshold {
            hnsw.parallel_insert_slice(&to_insert);
        } else {
            for item in to_insert {
                hnsw.insert_slice(item);
            }
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

    /// Save index to specific path.
    ///
    /// Cleans up any orphan `graph-NNNN.hnsw.*` snapshots produced by an
    /// earlier reload-then-save cycle. hnsw_rs 0.3.3's `HnswIo::load_hnsw`
    /// unconditionally sets `datamap_opt = true` on the returned `Hnsw`,
    /// which forces `file_dump` to pick a unique basename instead of
    /// overwriting `graph.hnsw.{graph,data}`. The loader only ever reads
    /// the canonical basename, so without this cleanup memd accumulates
    /// ~200 MB of dead bytes per save after the first reload.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path)?;

        // Acquire read locks for consistent snapshot
        let mapping = self.mapping.read();
        let cache = self.embedding_cache.read();

        // Save embedding cache (atomic write with temp file)
        let cache_path = path.join("embeddings.bin");
        cache.save_to(&cache_path)?;

        // Save mapping in bincode (~5x smaller than the old JSON
        // representation and ~5x faster to parse on cold start). The
        // legacy `mapping.json` is best-effort removed below so disk
        // doesn't keep two copies after the first save under the new
        // format. `load()` falls back to `mapping.json` when the
        // bincode file is absent so freshly-upgraded installs continue
        // to open cleanly.
        let mapping_tmp = path.join("mapping.bin.tmp");
        let mapping_bytes =
            bincode::serde::encode_to_vec(&*mapping, bincode::config::standard())
                .map_err(|e| MemdError::StorageError(format!("serialize mapping: {}", e)))?;

        let mut file = File::create(&mapping_tmp)?;
        file.write_all(&mapping_bytes)?;
        file.sync_all()?;
        drop(file);

        // Atomic rename
        std::fs::rename(&mapping_tmp, path.join("mapping.bin"))?;

        // Sync the parent directory so that, after a power loss, either
        // (a) the rename is durable and a subsequent load sees
        // mapping.bin, or (b) the rename is not yet visible and load
        // falls back to mapping.json — never both missing.
        if let Ok(dir) = File::open(path) {
            let _ = dir.sync_all();
        }

        // Remove any leftover legacy mapping.json. Errors are ignored
        // (best-effort cleanup); a stale mapping.json next to a fresh
        // mapping.bin is harmless because `load` prefers .bin and never
        // reads .json when .bin exists. A separate dir sync runs at the
        // bottom of save_to to make the removal itself durable.
        let _ = std::fs::remove_file(path.join("mapping.json"));

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

        // Remove the canonical dump plus any orphan snapshots from
        // previous reload-then-save cycles. hnsw_rs only falls back to a
        // unique basename when the canonical files still exist, so the
        // removal forces the default basename "graph" on the next dump.
        // We purge unconditionally — leaving a stale dump while
        // `persist_graph_dump = false` would break the
        // embedding-cache-is-source-of-truth invariant. The window where
        // neither file is present is brief and bounded by the
        // `file_dump` call below; a crash there leaves embeddings.bin +
        // mapping.json intact, so the next startup rebuilds the graph
        // from the embedding cache.
        Self::purge_graph_dumps(path)?;

        if self.config.persist_graph_dump {
            // Save HNSW graph using hnsw_rs file_dump
            let hnsw = self.hnsw.read();
            hnsw.file_dump(path, "graph")
                .map_err(|e| MemdError::StorageError(format!("dump hnsw: {:?}", e)))?;
        } else {
            tracing::debug!(
                path = ?path,
                "persist_graph_dump=false; skipping HNSW file_dump, graph will be rebuilt from embedding cache on next load"
            );
        }

        // Sync parent directory
        if let Ok(dir) = File::open(path) {
            let _ = dir.sync_all();
        }

        tracing::info!("Saved HNSW index to {:?}", path);
        Ok(())
    }

    /// Remove the canonical HNSW dump and any orphan `graph-NNNN.hnsw.*`
    /// snapshots left behind by hnsw_rs's unique-basename fallback. Used
    /// immediately before `file_dump` so the dumper sees an empty slate
    /// and writes to the canonical basename.
    ///
    /// Canonical deletions must succeed (or the file must already be
    /// absent): if `graph.hnsw.graph` or `graph.hnsw.data` survives this
    /// call, `file_dump` silently falls back to a unique basename and
    /// the orphan leak this method exists to prevent recurs without any
    /// log line. Orphan deletions stay best-effort — losing a stale
    /// snapshot is a disk-space issue, not a correctness one.
    fn purge_graph_dumps(path: &Path) -> Result<()> {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(MemdError::StorageError(format!("read warm_index: {}", e))),
        };
        for entry in entries.flatten() {
            // Only operate on regular files. Symlinks and directories that
            // happen to match the name pattern are left untouched.
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let is_canonical = name == "graph.hnsw.graph" || name == "graph.hnsw.data";
            let is_orphan = name.starts_with("graph-")
                && (name.ends_with(".hnsw.graph") || name.ends_with(".hnsw.data"));
            if is_canonical {
                match std::fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(MemdError::StorageError(format!(
                            "remove canonical dump {}: {}",
                            name, e
                        )));
                    }
                }
            } else if is_orphan {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    /// Remove only orphan `graph-NNNN.hnsw.*` snapshots, leaving the
    /// canonical `graph.hnsw.*` pair intact. Invoked once on load so a
    /// freshly-upgraded memd reclaims disk without waiting for the next
    /// save. Best-effort: any IO error short of "directory missing" is
    /// reported, but the caller treats this as advisory and continues.
    fn purge_orphan_dumps(path: &Path) -> Result<()> {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(MemdError::StorageError(format!("read warm_index: {}", e))),
        };
        let mut removed = 0u64;
        let mut bytes = 0u64;
        for entry in entries.flatten() {
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("graph-")
                && (name.ends_with(".hnsw.graph") || name.ends_with(".hnsw.data"))
            {
                if let Ok(meta) = entry.metadata() {
                    bytes += meta.len();
                }
                if std::fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        if removed > 0 {
            tracing::info!(
                removed,
                bytes_freed = bytes,
                path = ?path,
                "purged orphan HNSW snapshots"
            );
        }
        Ok(())
    }

    /// Load index from disk and rebuild HNSW from cached embeddings
    ///
    /// Loads the embedding cache and mapping, validates consistency, then
    /// rebuilds the HNSW graph from the cached embeddings. This is much faster
    /// than re-embedding (50-100x speedup).
    pub fn load(path: &Path, config: HnswConfig) -> Result<Self> {
        Self::load_with_mode(path, config, false)
    }

    fn load_with_mode(path: &Path, config: HnswConfig, read_only: bool) -> Result<Self> {
        use std::time::Instant;

        let start = Instant::now();

        // Load mapping. Prefer the new bincode format; fall back to
        // legacy JSON for installs that haven't saved under the new
        // format yet. The fallback path runs at most once per upgraded
        // install — the next save rewrites as mapping.bin and removes
        // mapping.json (see save_to).
        let bin_path = path.join("mapping.bin");
        let json_path = path.join("mapping.json");
        let mapping: IndexMapping = if bin_path.exists() {
            let mut file = File::open(&bin_path)?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            let (mapping, _len) =
                bincode::serde::decode_from_slice(&buf, bincode::config::standard()).map_err(
                    |e| MemdError::StorageError(format!("deserialize mapping.bin: {}", e)),
                )?;
            mapping
        } else if json_path.exists() {
            tracing::info!(
                path = ?path,
                "loading legacy mapping.json; will rewrite as mapping.bin on next save"
            );
            let mut file = File::open(&json_path)?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            serde_json::from_slice(&buf)
                .map_err(|e| MemdError::StorageError(format!("deserialize mapping.json: {}", e)))?
        } else {
            return Err(MemdError::StorageError(
                "no mapping file present (expected mapping.bin or mapping.json)".into(),
            ));
        };

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
                        if !read_only {
                            // Delete corrupted cache so the writer path can rebuild cleanly.
                            let _ = std::fs::remove_file(&cache_path);
                        }
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
                    if !read_only {
                        // Delete corrupted cache so the writer path can rebuild cleanly.
                        let _ = std::fs::remove_file(&cache_path);
                    }
                    EmbeddingCache::new(config.dimension)
                }
            }
        } else {
            tracing::info!("No embedding cache found. Will need rebuild from segments.");
            EmbeddingCache::new(config.dimension)
        };

        let expected_points = embedding_cache.len();
        // When persist_graph_dump=false, ignore any stale graph dump
        // left over from a previous run that had the flag set. Loading
        // it would silently surface old graph state until the next save
        // purges the files, breaking the embedding-cache-is-source-of-
        // truth invariant for read-only / read-mostly workloads.
        let dump_load_result = if config.persist_graph_dump {
            Self::load_dumped_graph(path, expected_points)
        } else {
            Ok(None)
        };
        let (hnsw, point_count, loaded_dump) = match dump_load_result {
            Ok(Some(hnsw)) => {
                let count = hnsw.get_nb_point();
                (hnsw, count, true)
            }
            Ok(None) => {
                let (hnsw, count) = Self::rebuild_graph_from_cache(&config, &embedding_cache);
                (hnsw, count, false)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = ?path,
                    "failed to load dumped HNSW graph, rebuilding from embedding cache"
                );
                let (hnsw, count) = Self::rebuild_graph_from_cache(&config, &embedding_cache);
                (hnsw, count, false)
            }
        };

        let elapsed = start.elapsed();

        if loaded_dump {
            tracing::info!(
                "Loaded HNSW graph dump: {} embeddings in {:?}",
                point_count,
                elapsed
            );
        } else if point_count > 0 {
            tracing::info!(
                "Rebuilt HNSW index from cache: {} embeddings in {:?}",
                point_count,
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
            persist_path: (!read_only).then(|| path.to_path_buf()),
        })
    }

    fn load_dumped_graph(
        path: &Path,
        expected_points: usize,
    ) -> Result<Option<Hnsw<'static, f32, DistCosine>>> {
        if expected_points == 0 {
            return Ok(None);
        }

        if !path.join("graph.hnsw.graph").exists() || !path.join("graph.hnsw.data").exists() {
            return Ok(None);
        }

        // hnsw_rs 0.3.3 can PANIC (not Err) on a partial/corrupt dump —
        // `load_description` and friends contain `.unwrap()` / assertions.
        // A crash that landed mid-`file_dump` leaves both canonical files
        // present but truncated, which would otherwise propagate as an
        // unwinding panic out of memd. Catch it here so we fall through
        // to `rebuild_graph_from_cache`. On the panic path the leaked
        // HnswIo is unreachable but not freed; the allocation is small
        // (one per failed load) and the daemon stays up.
        let load_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_> {
                // hnsw_rs ties the loaded graph lifetime to HnswIo because reload can
                // mmap vector data. memd uses the default non-mmap reload, but keeping
                // the loader alive for the process lifetime avoids unsound lifetime
                // narrowing and is bounded to one tiny object per loaded tenant index.
                let loader = Box::leak(Box::new(HnswIo::new(path, "graph")));
                loader
                    .load_hnsw::<f32, DistCosine>()
                    .map_err(|e| MemdError::StorageError(format!("load hnsw dump: {e}")))
            }));
        let hnsw = match load_result {
            Ok(Ok(hnsw)) => hnsw,
            Ok(Err(e)) => return Err(e),
            Err(_panic) => {
                tracing::warn!(
                    path = ?path,
                    "hnsw_rs panicked while loading graph dump (likely partial after crash); falling back to rebuild from embedding cache"
                );
                return Ok(None);
            }
        };

        let loaded_points = hnsw.get_nb_point();
        if loaded_points != expected_points {
            return Err(MemdError::StorageError(format!(
                "dumped HNSW point count mismatch: graph has {loaded_points}, cache has {expected_points}"
            )));
        }

        Ok(Some(hnsw))
    }

    fn rebuild_graph_from_cache(
        config: &HnswConfig,
        embedding_cache: &EmbeddingCache,
    ) -> (Hnsw<'static, f32, DistCosine>, usize) {
        let hnsw = Hnsw::new(
            config.max_connections,
            config.max_elements,
            16,
            config.ef_construction,
            DistCosine {},
        );

        if embedding_cache.is_empty() {
            return (hnsw, 0);
        }

        let to_insert: Vec<(&[f32], usize)> = embedding_cache
            .iter_valid()
            .map(|(internal_id, embedding)| (embedding, internal_id))
            .collect();
        let count = to_insert.len();
        let threshold = std::thread::available_parallelism()
            .map(|n| std::cmp::max(64, n.get().saturating_mul(8)))
            .unwrap_or(64);
        if count >= threshold {
            hnsw.parallel_insert_slice(&to_insert);
        } else {
            for (embedding, internal_id) in &to_insert {
                hnsw.insert_slice((*embedding, *internal_id));
            }
        }

        (hnsw, count)
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

    /// Rebuild the HNSW index in place, excluding deleted chunk IDs
    ///
    /// This creates a new HNSW from the embedding cache, filtering out
    /// deleted entries, then returns the result. The actual index swap
    /// happens at DenseSearcher level where tenant_indices map is managed.
    ///
    /// # Arguments
    /// * `deleted_chunk_ids` - Set of chunk IDs to exclude from rebuild
    ///
    /// # Returns
    /// RebuildResult with statistics about the rebuild operation
    pub fn rebuild_clean_in_place(
        &self,
        deleted_chunk_ids: &HashSet<ChunkId>,
    ) -> Result<RebuildResult> {
        // Convert chunk IDs to internal IDs
        let mapping = self.mapping.read();
        let deleted_internal_ids: HashSet<usize> = deleted_chunk_ids
            .iter()
            .filter_map(|chunk_id| mapping.get_internal_id(chunk_id))
            .collect();
        drop(mapping);

        // Use HnswRebuilder to create clean index
        let rebuilder = HnswRebuilder::new();
        let (_new_hnsw, result) =
            rebuilder.rebuild_clean(self, &deleted_internal_ids, &self.config)?;

        // Note: The actual atomic swap happens at DenseSearcher level
        // where the tenant_indices map is stored. This method does the
        // rebuild work and returns the result.
        Ok(result)
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
        // Approximate search can vary ordering for very similar vectors.
        let exact = results.iter().find(|r| r.chunk_id == chunk1);
        let similar = results.iter().find(|r| r.chunk_id == chunk2);
        let unrelated = results.iter().find(|r| r.chunk_id == chunk3);

        assert!(exact.is_some(), "results should include exact match");
        assert!(similar.is_some(), "results should include nearest neighbor");
        assert!(
            unrelated.is_none(),
            "results should exclude unrelated vector"
        );
        assert!(exact.unwrap().score > 0.99);
        assert!(similar.unwrap().score > 0.9);
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
        assert!(path.join("mapping.bin").exists());
        assert!(
            !path.join("mapping.json").exists(),
            "fresh save under the new format must not leave mapping.json behind"
        );
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
