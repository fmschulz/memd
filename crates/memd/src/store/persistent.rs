//! Persistent store implementation
//!
//! Integrates segments, WAL, SQLite metadata, and tombstones.
//! Implements crash recovery via WAL replay on startup.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, warn};

use super::metadata::{ChunkMetadata, MetadataStore, SqliteMetadataStore};
use super::segment::{SegmentReader, SegmentWriter};
use super::wal::{WalReader, WalRecordType, WalWriter};
use super::{Store, StoreStats};
use crate::error::{MemdError, Result};
use crate::types::{ChunkId, ChunkStatus, MemoryChunk, TenantId};

/// Configuration for persistent store
#[derive(Debug, Clone)]
pub struct PersistentStoreConfig {
    /// Base data directory
    pub data_dir: PathBuf,
    /// Maximum chunks per segment before rotation
    pub segment_max_chunks: u32,
    /// WAL checkpoint interval (chunks)
    pub wal_checkpoint_interval: u32,
}

impl Default for PersistentStoreConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            segment_max_chunks: 10_000,
            wal_checkpoint_interval: 100,
        }
    }
}

/// Persistent store with crash recovery
pub struct PersistentStore {
    config: PersistentStoreConfig,
    /// Per-tenant state
    tenants: RwLock<HashMap<String, Arc<TenantStore>>>,
    /// Global metadata store
    metadata: Arc<SqliteMetadataStore>,
}

/// Per-tenant storage state
struct TenantStore {
    tenant_id: String,
    base_dir: PathBuf,
    /// Current active segment writer (None if read-only)
    active_segment: Mutex<Option<ActiveSegment>>,
    /// Loaded segment readers
    segments: RwLock<HashMap<u64, SegmentReader>>,
    /// WAL writer
    wal: Mutex<WalWriter>,
    /// Counter for WAL checkpoint
    writes_since_checkpoint: Mutex<u32>,
    /// Max chunks per segment (for rotation)
    segment_max_chunks: u32,
}

struct ActiveSegment {
    writer: SegmentWriter,
    chunk_count: u32,
}

impl PersistentStore {
    /// Open or create persistent store
    pub fn open(config: PersistentStoreConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;

        // Open global metadata database
        let metadata_path = config.data_dir.join("metadata.db");
        let metadata = Arc::new(SqliteMetadataStore::open(&metadata_path)?);

        let store = Self {
            config,
            tenants: RwLock::new(HashMap::new()),
            metadata,
        };

        // Recover existing tenants
        store.discover_and_recover_tenants()?;

        Ok(store)
    }

    fn discover_and_recover_tenants(&self) -> Result<()> {
        let tenants_dir = self.config.data_dir.join("tenants");
        if !tenants_dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(&tenants_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(tenant_id) = entry.file_name().to_str() {
                    info!(tenant_id, "recovering tenant");
                    let _ = self.get_or_create_tenant(tenant_id)?;
                }
            }
        }

        Ok(())
    }

    fn get_or_create_tenant(&self, tenant_id: &str) -> Result<Arc<TenantStore>> {
        // Fast path: read lock
        {
            let tenants = self.tenants.read();
            if let Some(tenant) = tenants.get(tenant_id) {
                return Ok(Arc::clone(tenant));
            }
        }

        // Slow path: write lock + create
        let mut tenants = self.tenants.write();

        // Double-check after acquiring write lock
        if let Some(tenant) = tenants.get(tenant_id) {
            return Ok(Arc::clone(tenant));
        }

        let tenant = TenantStore::open(
            tenant_id.to_string(),
            self.config.data_dir.join("tenants").join(tenant_id),
            &self.metadata,
            self.config.segment_max_chunks,
        )?;

        let tenant = Arc::new(tenant);
        tenants.insert(tenant_id.to_string(), Arc::clone(&tenant));

        Ok(tenant)
    }

    /// Graceful shutdown - finalizes all active segments
    pub fn shutdown(&self) -> Result<()> {
        info!("PersistentStore shutting down");
        let tenants = self.tenants.read();
        for (tenant_id, tenant) in tenants.iter() {
            if let Err(e) = tenant.finalize_active_segment() {
                warn!(tenant_id, error = %e, "failed to finalize segment on shutdown");
            }
        }
        Ok(())
    }
}

impl Drop for PersistentStore {
    fn drop(&mut self) {
        // Best-effort finalization on drop
        if let Err(e) = self.shutdown() {
            warn!(error = %e, "error during PersistentStore drop");
        }
    }
}

impl TenantStore {
    fn open(
        tenant_id: String,
        base_dir: PathBuf,
        metadata: &SqliteMetadataStore,
        segment_max_chunks: u32,
    ) -> Result<Self> {
        std::fs::create_dir_all(&base_dir)?;
        std::fs::create_dir_all(base_dir.join("segments"))?;

        // Open WAL (use open_or_create for seamless startup)
        let wal_path = base_dir.join("wal.log");
        let wal_reader = WalReader::open(&wal_path)?;
        let wal_writer = WalWriter::open_or_create(&wal_path)?;

        let store = Self {
            tenant_id: tenant_id.clone(),
            base_dir,
            active_segment: Mutex::new(None),
            segments: RwLock::new(HashMap::new()),
            wal: Mutex::new(wal_writer),
            writes_since_checkpoint: Mutex::new(0),
            segment_max_chunks,
        };

        // Load existing segments
        store.load_segments()?;

        // Recover from WAL - FULL IMPLEMENTATION
        store.recover_from_wal(&wal_reader, metadata)?;

        Ok(store)
    }

    fn load_segments(&self) -> Result<()> {
        let segments_dir = self.base_dir.join("segments");
        if !segments_dir.exists() {
            return Ok(());
        }

        let mut segments = self.segments.write();
        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let dir_name = entry.file_name();
                if let Some(name) = dir_name.to_str() {
                    if name.starts_with("seg_") && entry.path().join("meta").exists() {
                        // Only load finalized segments (have meta file)
                        match SegmentReader::open(entry.path()) {
                            Ok(reader) => {
                                info!(segment_id = reader.id, "loaded segment");
                                segments.insert(reader.id, reader);
                            }
                            Err(e) => {
                                warn!(path = ?entry.path(), error = %e, "failed to load segment");
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Full WAL recovery implementation
    ///
    /// Replays Add and Delete records from WAL to restore uncommitted state.
    /// Idempotent: skips records for chunks that already exist in metadata.
    fn recover_from_wal(
        &self,
        wal_reader: &WalReader,
        metadata: &SqliteMetadataStore,
    ) -> Result<()> {
        if wal_reader.is_empty() {
            return Ok(());
        }

        let records = wal_reader.records_for_recovery()?;
        if records.is_empty() {
            return Ok(());
        }

        info!(
            records = records.len(),
            tenant = %self.tenant_id,
            "WAL recovery starting"
        );

        let mut adds = 0;
        let mut deletes = 0;
        let mut skipped = 0;

        for record in &records {
            match record.record_type {
                WalRecordType::Add => {
                    // Check if chunk already exists and is readable
                    let tenant_id = TenantId::new(&record.tenant_id).map_err(|e| {
                        MemdError::StorageError(format!("invalid tenant_id in WAL: {}", e))
                    })?;
                    let chunk_id = ChunkId::parse(&record.chunk_id).map_err(|e| {
                        MemdError::StorageError(format!("invalid chunk_id in WAL: {}", e))
                    })?;

                    // If metadata exists, check if segment data is readable
                    if let Some(existing_meta) = metadata.get(&tenant_id, &chunk_id)? {
                        // Try to read from segment to verify data is intact
                        let segments = self.segments.read();
                        if let Some(reader) = segments.get(&existing_meta.segment_id) {
                            if reader.read_chunk(existing_meta.ordinal).ok().flatten().is_some() {
                                // Data exists and is readable, skip
                                skipped += 1;
                                continue;
                            }
                        }
                        // Metadata exists but segment data is missing or unreadable
                        // This is a crash recovery case - re-write the chunk
                        debug!(
                            chunk_id = %chunk_id,
                            "recovering orphan metadata - segment data missing"
                        );
                    }

                    // Deserialize chunk from payload
                    let chunk: MemoryChunk = serde_json::from_slice(&record.payload).map_err(
                        |e| MemdError::StorageError(format!("deserialize WAL chunk: {}", e)),
                    )?;

                    // Write to active segment
                    self.get_or_create_active_segment(self.segment_max_chunks)?;
                    let (segment_id, ordinal) = {
                        let mut active = self.active_segment.lock();
                        let seg = active.as_mut().ok_or_else(|| {
                            MemdError::StorageError("no active segment during recovery".into())
                        })?;
                        let ordinal = seg.writer.append_chunk(&record.payload)?;
                        seg.chunk_count += 1;
                        (seg.writer.id(), ordinal)
                    };

                    // Write to metadata
                    let chunk_meta = ChunkMetadata {
                        chunk_id: chunk.chunk_id.clone(),
                        tenant_id: chunk.tenant_id.clone(),
                        project_id: chunk.project_id.as_option().map(|s| s.to_string()),
                        segment_id,
                        ordinal,
                        chunk_type: chunk.chunk_type,
                        status: chunk.status,
                        timestamp_created: chunk.timestamp_created,
                        hash: chunk.hash.clone(),
                        source_uri: chunk.source.uri.clone(),
                    };
                    metadata.insert(&chunk_meta)?;

                    adds += 1;
                }
                WalRecordType::Delete => {
                    // Apply delete: mark in metadata and tombstone
                    let tenant_id = TenantId::new(&record.tenant_id).map_err(|e| {
                        MemdError::StorageError(format!("invalid tenant_id in WAL: {}", e))
                    })?;
                    let chunk_id = ChunkId::parse(&record.chunk_id).map_err(|e| {
                        MemdError::StorageError(format!("invalid chunk_id in WAL: {}", e))
                    })?;

                    // Get metadata to find segment/ordinal
                    if let Some(meta) = metadata.get(&tenant_id, &chunk_id)? {
                        if meta.status != ChunkStatus::Deleted {
                            // Mark in metadata
                            metadata.mark_deleted(&tenant_id, &chunk_id)?;

                            // Mark tombstone in segment
                            let mut segments = self.segments.write();
                            if let Some(reader) = segments.get_mut(&meta.segment_id) {
                                reader.mark_deleted(meta.ordinal)?;
                            }

                            deletes += 1;
                        } else {
                            skipped += 1;
                        }
                    } else {
                        skipped += 1;
                    }
                }
                WalRecordType::Checkpoint => {
                    // Checkpoint records are filtered out by records_for_recovery()
                    // but handle gracefully if encountered
                }
            }
        }

        info!(
            adds,
            deletes,
            skipped,
            tenant = %self.tenant_id,
            "WAL recovery complete"
        );

        // After successful recovery, truncate WAL to start fresh
        {
            let mut wal = self.wal.lock();
            wal.truncate()?;
        }

        Ok(())
    }

    fn next_segment_id(&self) -> u64 {
        let segments = self.segments.read();
        segments.keys().max().map(|id| id + 1).unwrap_or(1)
    }

    fn get_or_create_active_segment(&self, max_chunks: u32) -> Result<()> {
        let mut active = self.active_segment.lock();

        if active.is_some() {
            let seg = active.as_ref().unwrap();
            if seg.chunk_count < max_chunks {
                return Ok(());
            }
            // Need to rotate - finalize current segment
            let seg = active.take().unwrap();
            let meta = seg.writer.finalize()?;
            info!(segment_id = meta.id, chunks = meta.chunk_count, "segment finalized");

            // Load as reader
            let segments_dir = self.base_dir.join("segments");
            let seg_dir = segments_dir.join(format!("seg_{:06}", meta.id));
            let reader = SegmentReader::open(seg_dir)?;
            self.segments.write().insert(meta.id, reader);
        }

        // Create new segment
        let segment_id = self.next_segment_id();
        let segments_dir = self.base_dir.join("segments");
        let writer = SegmentWriter::create(&segments_dir, segment_id)?;

        *active = Some(ActiveSegment {
            writer,
            chunk_count: 0,
        });

        Ok(())
    }

    /// Finalize active segment for graceful shutdown
    fn finalize_active_segment(&self) -> Result<()> {
        let mut active = self.active_segment.lock();
        if let Some(seg) = active.take() {
            if seg.chunk_count > 0 {
                let meta = seg.writer.finalize()?;
                info!(
                    segment_id = meta.id,
                    chunks = meta.chunk_count,
                    tenant = %self.tenant_id,
                    "segment finalized on shutdown"
                );

                // Load as reader
                let segments_dir = self.base_dir.join("segments");
                let seg_dir = segments_dir.join(format!("seg_{:06}", meta.id));
                let reader = SegmentReader::open(seg_dir)?;
                self.segments.write().insert(meta.id, reader);
            }
        }
        Ok(())
    }

    /// Read chunk from active segment by ordinal
    fn read_from_active_segment(&self, segment_id: u64, ordinal: u32) -> Result<Option<Vec<u8>>> {
        let mut active = self.active_segment.lock();
        if let Some(seg) = active.as_mut() {
            if seg.writer.id() == segment_id {
                return seg.writer.read_chunk(ordinal);
            }
        }
        Ok(None)
    }
}

impl Drop for TenantStore {
    fn drop(&mut self) {
        // Best-effort finalization on drop
        if let Err(e) = self.finalize_active_segment() {
            warn!(
                tenant = %self.tenant_id,
                error = %e,
                "failed to finalize segment on TenantStore drop"
            );
        }
    }
}

#[async_trait::async_trait]
impl Store for PersistentStore {
    async fn add(&self, mut chunk: MemoryChunk) -> Result<ChunkId> {
        let tenant_id_str = chunk.tenant_id.to_string();
        let tenant = self.get_or_create_tenant(&tenant_id_str)?;

        // Generate chunk ID
        let chunk_id = ChunkId::new();
        chunk.chunk_id = chunk_id.clone();

        // Compute hash
        chunk.hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(chunk.text.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let timestamp = chunk.timestamp_created;

        // Serialize chunk for storage
        let payload = serde_json::to_vec(&chunk)
            .map_err(|e| MemdError::StorageError(format!("serialize chunk: {}", e)))?;

        // Write to WAL first (durability)
        {
            let mut wal = tenant.wal.lock();
            wal.append_add(&tenant_id_str, &chunk_id.to_string(), timestamp, payload.clone())?;
        }

        // Write to segment
        tenant.get_or_create_active_segment(self.config.segment_max_chunks)?;
        let (segment_id, ordinal) = {
            let mut active = tenant.active_segment.lock();
            let seg = active
                .as_mut()
                .ok_or_else(|| MemdError::StorageError("no active segment".into()))?;
            let ordinal = seg.writer.append_chunk(&payload)?;
            seg.chunk_count += 1;
            (seg.writer.id(), ordinal)
        };

        // Write to metadata
        let metadata = ChunkMetadata {
            chunk_id: chunk_id.clone(),
            tenant_id: chunk.tenant_id.clone(),
            project_id: chunk.project_id.as_option().map(|s| s.to_string()),
            segment_id,
            ordinal,
            chunk_type: chunk.chunk_type,
            status: chunk.status,
            timestamp_created: chunk.timestamp_created,
            hash: chunk.hash.clone(),
            source_uri: chunk.source.uri.clone(),
        };
        self.metadata.insert(&metadata)?;

        // Check if we need checkpoint
        {
            let mut count = tenant.writes_since_checkpoint.lock();
            *count += 1;
            if *count >= self.config.wal_checkpoint_interval {
                let mut wal = tenant.wal.lock();
                wal.append_checkpoint(&tenant_id_str, timestamp)?;
                *count = 0;
            }
        }

        debug!(tenant_id = %tenant_id_str, chunk_id = %chunk_id, segment_id, ordinal, "chunk added");
        Ok(chunk_id)
    }

    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>> {
        let mut ids = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            ids.push(self.add(chunk).await?);
        }
        Ok(ids)
    }

    async fn get(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<Option<MemoryChunk>> {
        // Query metadata first
        let meta = self.metadata.get(tenant_id, chunk_id)?;
        let meta = match meta {
            Some(m) if m.status != ChunkStatus::Deleted => m,
            _ => return Ok(None),
        };

        // Load from segment
        let tenant_str = tenant_id.to_string();
        let tenant = match self.tenants.read().get(&tenant_str) {
            Some(t) => Arc::clone(t),
            None => return Ok(None),
        };

        // First check active segment (for chunks not yet in finalized segments)
        if let Some(bytes) = tenant.read_from_active_segment(meta.segment_id, meta.ordinal)? {
            let chunk: MemoryChunk = serde_json::from_slice(&bytes)
                .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
            return Ok(Some(chunk));
        }

        // Check finalized segments
        let segments = tenant.segments.read();
        let reader = match segments.get(&meta.segment_id) {
            Some(r) => r,
            None => return Ok(None),
        };

        let payload = reader.read_chunk(meta.ordinal)?;
        match payload {
            Some(bytes) => {
                let chunk: MemoryChunk = serde_json::from_slice(&bytes)
                    .map_err(|e| MemdError::StorageError(format!("deserialize chunk: {}", e)))?;
                Ok(Some(chunk))
            }
            None => Ok(None), // Tombstoned
        }
    }

    async fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<MemoryChunk>> {
        // For now, simple implementation: list + filter
        // Real search comes in Phase 3 with embeddings
        let metadata_list = self.metadata.list(tenant_id, k * 2, 0)?;

        let tenant_str = tenant_id.to_string();
        let tenant = match self.tenants.read().get(&tenant_str) {
            Some(t) => Arc::clone(t),
            None => return Ok(Vec::new()),
        };

        let segments = tenant.segments.read();
        let mut results = Vec::new();

        for meta in metadata_list {
            if meta.status == ChunkStatus::Deleted {
                continue;
            }

            // Try active segment first
            if let Some(bytes) = tenant.read_from_active_segment(meta.segment_id, meta.ordinal)? {
                if let Ok(chunk) = serde_json::from_slice::<MemoryChunk>(&bytes) {
                    // Basic text match
                    if query.is_empty()
                        || chunk.text.to_lowercase().contains(&query.to_lowercase())
                    {
                        results.push(chunk);
                        if results.len() >= k {
                            break;
                        }
                    }
                }
                continue;
            }

            // Try finalized segments
            if let Some(reader) = segments.get(&meta.segment_id) {
                if let Some(bytes) = reader.read_chunk(meta.ordinal)? {
                    if let Ok(chunk) = serde_json::from_slice::<MemoryChunk>(&bytes) {
                        // Basic text match
                        if query.is_empty()
                            || chunk.text.to_lowercase().contains(&query.to_lowercase())
                        {
                            results.push(chunk);
                            if results.len() >= k {
                                break;
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
    }

    async fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        // Get metadata to find segment/ordinal
        let meta = self.metadata.get(tenant_id, chunk_id)?;
        let meta = match meta {
            Some(m) if m.status != ChunkStatus::Deleted => m,
            _ => return Ok(false),
        };

        let tenant_str = tenant_id.to_string();

        // Write to WAL
        let tenant = self.get_or_create_tenant(&tenant_str)?;
        {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);

            let mut wal = tenant.wal.lock();
            wal.append_delete(&tenant_str, &chunk_id.to_string(), timestamp)?;
        }

        // Update metadata status
        self.metadata.mark_deleted(tenant_id, chunk_id)?;

        // Update tombstone in segment
        {
            let mut segments = tenant.segments.write();
            if let Some(reader) = segments.get_mut(&meta.segment_id) {
                reader.mark_deleted(meta.ordinal)?;
            }
        }

        info!(tenant_id = %tenant_str, chunk_id = %chunk_id, "chunk deleted");
        Ok(true)
    }

    async fn stats(&self, tenant_id: &TenantId) -> Result<StoreStats> {
        let (active, deleted) = self.metadata.count_by_status(tenant_id)?;

        // Get chunk types from metadata
        let chunks = self.metadata.list(tenant_id, 10000, 0)?;
        let mut chunk_types = HashMap::new();
        for meta in &chunks {
            *chunk_types.entry(meta.chunk_type.to_string()).or_insert(0) += 1;
        }

        Ok(StoreStats {
            total_chunks: active + deleted,
            deleted_chunks: deleted,
            chunk_types,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkType;
    use tempfile::tempdir;

    fn make_test_store() -> (PersistentStore, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = PersistentStoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_chunks: 100,
            wal_checkpoint_interval: 10,
        };
        let store = PersistentStore::open(config).unwrap();
        (store, dir)
    }

    fn make_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    fn make_chunk(tenant: &TenantId, text: &str) -> MemoryChunk {
        MemoryChunk::new(tenant.clone(), text, ChunkType::Doc)
    }

    #[tokio::test]
    async fn add_and_get() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();
        let chunk = make_chunk(&tenant, "hello persistent");

        let chunk_id = store.add(chunk).await.unwrap();
        let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().text, "hello persistent");
    }

    #[tokio::test]
    async fn tenant_isolation() {
        let (store, _dir) = make_test_store();
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();

        let chunk = make_chunk(&tenant_a, "secret");
        let chunk_id = store.add(chunk).await.unwrap();

        // Tenant B cannot see tenant A's chunk
        let result = store.get(&tenant_b, &chunk_id).await.unwrap();
        assert!(result.is_none());

        // Search isolation
        let results = store.search(&tenant_b, "secret", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn soft_delete() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();
        let chunk = make_chunk(&tenant, "to delete");

        let chunk_id = store.add(chunk).await.unwrap();
        let deleted = store.delete(&tenant, &chunk_id).await.unwrap();
        assert!(deleted);

        // Chunk no longer retrievable
        let result = store.get(&tenant, &chunk_id).await.unwrap();
        assert!(result.is_none());

        // Not in search results
        let results = store.search(&tenant, "delete", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn persistence_across_restarts() {
        let dir = tempdir().unwrap();
        let tenant = make_tenant();
        let chunk_id;

        // First session: add chunk
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
            };
            let store = PersistentStore::open(config).unwrap();
            let chunk = make_chunk(&tenant, "persistent data");
            chunk_id = store.add(chunk).await.unwrap();

            // Drop triggers finalization
            drop(store);
        }

        // Second session: retrieve chunk
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
            };
            let store = PersistentStore::open(config).unwrap();
            let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

            // Chunk survives restart
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().text, "persistent data");
        }
    }

    #[tokio::test]
    async fn wal_recovery_after_crash() {
        let dir = tempdir().unwrap();
        let tenant = make_tenant();
        let chunk_id;

        // First session: add chunk but simulate crash (no finalization)
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
            };
            let store = PersistentStore::open(config).unwrap();
            let chunk = make_chunk(&tenant, "crash test data");
            chunk_id = store.add(chunk).await.unwrap();

            // Simulate crash: forget without drop (leak the store)
            std::mem::forget(store);
        }

        // Second session: should recover from WAL
        {
            let config = PersistentStoreConfig {
                data_dir: dir.path().to_path_buf(),
                segment_max_chunks: 100,
                wal_checkpoint_interval: 10,
            };
            let store = PersistentStore::open(config).unwrap();
            let retrieved = store.get(&tenant, &chunk_id).await.unwrap();

            // Chunk recovered from WAL
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().text, "crash test data");
        }
    }

    #[tokio::test]
    async fn stats() {
        let (store, _dir) = make_test_store();
        let tenant = make_tenant();

        store.add(make_chunk(&tenant, "doc 1")).await.unwrap();
        store.add(make_chunk(&tenant, "doc 2")).await.unwrap();
        let to_delete = store.add(make_chunk(&tenant, "doc 3")).await.unwrap();

        store.delete(&tenant, &to_delete).await.unwrap();

        let stats = store.stats(&tenant).await.unwrap();
        assert_eq!(stats.total_chunks, 3);
        assert_eq!(stats.deleted_chunks, 1);
    }
}
