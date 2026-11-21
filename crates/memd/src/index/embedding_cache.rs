use crate::error::{MemdError, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use crc32fast::Hasher;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 4] = b"MEMB";
const VERSION: u32 = 1;
/// Floats per read or write block (64 KiB).
const IO_BLOCK_FLOATS: usize = 16 * 1024;

/// The validated fixed header of an embeddings file.
struct Header {
    dimension: usize,
    count: usize,
    next_id: usize,
}

/// Read and validate the fixed header: exactly 24 bytes (magic, version,
/// dimension, count, next id and the header CRC, 4 bytes each).
fn read_header(reader: &mut impl Read) -> Result<Header> {
    let mut magic_buf = [0u8; 4];
    reader
        .read_exact(&mut magic_buf)
        .map_err(|e| MemdError::StorageError(format!("Failed to read magic: {}", e)))?;

    if &magic_buf != MAGIC {
        return Err(MemdError::ValidationError(
            "Embedding cache corrupted: invalid magic bytes. \
            Delete warm_index/embeddings.bin to rebuild."
                .to_string(),
        ));
    }

    let version = reader
        .read_u32::<LittleEndian>()
        .map_err(|e| MemdError::StorageError(format!("Failed to read version: {}", e)))?;

    if version != VERSION {
        return Err(MemdError::ValidationError(format!(
            "Embedding cache version {} not supported (expected {}). \
            Delete warm_index/embeddings.bin to rebuild.",
            version, VERSION
        )));
    }

    let dimension = reader
        .read_u32::<LittleEndian>()
        .map_err(|e| MemdError::StorageError(format!("Failed to read dimension: {}", e)))?
        as usize;

    let count = reader
        .read_u32::<LittleEndian>()
        .map_err(|e| MemdError::StorageError(format!("Failed to read count: {}", e)))?
        as usize;

    let next_id = reader
        .read_u32::<LittleEndian>()
        .map_err(|e| MemdError::StorageError(format!("Failed to read next_id: {}", e)))?
        as usize;

    let stored_header_crc = reader
        .read_u32::<LittleEndian>()
        .map_err(|e| MemdError::StorageError(format!("Failed to read header CRC: {}", e)))?;

    // Verify header CRC
    let mut header_hasher = Hasher::new();
    header_hasher.update(&magic_buf);
    header_hasher.update(&version.to_le_bytes());
    header_hasher.update(&(dimension as u32).to_le_bytes());
    header_hasher.update(&(count as u32).to_le_bytes());
    header_hasher.update(&(next_id as u32).to_le_bytes());
    let calculated_header_crc = header_hasher.finalize();

    if stored_header_crc != calculated_header_crc {
        return Err(MemdError::ValidationError(
            "Embedding cache corrupted: header CRC mismatch. \
            Delete warm_index/embeddings.bin to rebuild."
                .to_string(),
        ));
    }

    Ok(Header {
        dimension,
        count,
        next_id,
    })
}

/// Cache of embeddings for HNSW index rebuild
#[derive(Debug)]
pub struct EmbeddingCache {
    /// Flat embedding storage (internal_id * dimension -> embedding)
    embeddings: Vec<f32>,
    /// Embedding dimension
    dimension: usize,
    /// Number of valid embeddings
    count: usize,
    /// Validity bitmap (1 byte per ID)
    valid_ids: Vec<bool>,
}

impl EmbeddingCache {
    pub fn new(dimension: usize) -> Self {
        Self {
            embeddings: Vec::new(),
            dimension,
            count: 0,
            valid_ids: Vec::new(),
        }
    }

    pub fn insert(&mut self, internal_id: usize, embedding: &[f32]) -> Result<()> {
        if embedding.len() != self.dimension {
            return Err(MemdError::ValidationError(format!(
                "Embedding dimension mismatch: expected {}, got {}",
                self.dimension,
                embedding.len()
            )));
        }

        // Expand storage if needed
        let required_size = (internal_id + 1) * self.dimension;
        if self.embeddings.len() < required_size {
            self.embeddings.resize(required_size, 0.0);
        }

        // Expand validity bitmap if needed
        if self.valid_ids.len() <= internal_id {
            self.valid_ids.resize(internal_id + 1, false);
        }

        // Store embedding
        let start_idx = internal_id * self.dimension;
        self.embeddings[start_idx..start_idx + self.dimension].copy_from_slice(embedding);

        // Mark as valid and update count if new
        if !self.valid_ids[internal_id] {
            self.valid_ids[internal_id] = true;
            self.count += 1;
        }

        Ok(())
    }

    pub fn get(&self, internal_id: usize) -> Option<&[f32]> {
        if internal_id < self.valid_ids.len() && self.valid_ids[internal_id] {
            let start_idx = internal_id * self.dimension;
            Some(&self.embeddings[start_idx..start_idx + self.dimension])
        } else {
            None
        }
    }

    /// Mark an internal_id as invalid. Subsequent `iter_valid` / `get`
    /// calls ignore the slot.
    ///
    /// Phase 3.2 durability: the HNSW graph swap on compaction must
    /// be persisted across restarts. Without this helper, `save()`
    /// persisted the full embedding cache (including rows the new
    /// graph excluded) and `load()` rebuilt the graph from the whole
    /// cache — resurrecting compacted-away points. Callers flip the
    /// valid bit here during swap so save/load reflects reality.
    ///
    /// Idempotent: marking an already-invalid slot is a no-op.
    pub fn mark_invalid(&mut self, internal_id: usize) {
        if internal_id < self.valid_ids.len() && self.valid_ids[internal_id] {
            self.valid_ids[internal_id] = false;
            self.count = self.count.saturating_sub(1);
        }
    }

    pub fn iter_valid(&self) -> impl Iterator<Item = (usize, &[f32])> + '_ {
        self.valid_ids
            .iter()
            .enumerate()
            .filter_map(|(id, &valid)| {
                if valid {
                    let start_idx = id * self.dimension;
                    Some((id, &self.embeddings[start_idx..start_idx + self.dimension]))
                } else {
                    None
                }
            })
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub fn next_id(&self) -> usize {
        self.valid_ids.len()
    }

    /// Validate cache consistency with index metadata
    pub fn validate_consistency(
        &self,
        expected_dimension: usize,
        expected_next_id: usize,
    ) -> Result<()> {
        if self.dimension != expected_dimension {
            return Err(MemdError::ValidationError(format!(
                "Embedding cache dimension mismatch: cache has {}, config expects {}. \
                Delete warm_index/embeddings.bin to rebuild.",
                self.dimension, expected_dimension
            )));
        }

        if self.next_id() != expected_next_id {
            return Err(MemdError::ValidationError(format!(
                "Embedding cache count mismatch: cache has {} IDs, mapping expects {}. \
                Delete warm_index/embeddings.bin to rebuild.",
                self.next_id(),
                expected_next_id
            )));
        }

        Ok(())
    }

    /// Save embeddings to binary file with atomic write
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let temp_path = path.with_extension("tmp");

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                MemdError::StorageError(format!("Failed to create directory: {}", e))
            })?;
        }

        // Write to temp file
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .map_err(|e| MemdError::StorageError(format!("Failed to create temp file: {}", e)))?;

        let mut writer = BufWriter::new(file);

        // Write header
        writer
            .write_all(MAGIC)
            .map_err(|e| MemdError::StorageError(format!("Failed to write magic: {}", e)))?;
        writer
            .write_u32::<LittleEndian>(VERSION)
            .map_err(|e| MemdError::StorageError(format!("Failed to write version: {}", e)))?;
        writer
            .write_u32::<LittleEndian>(self.dimension as u32)
            .map_err(|e| MemdError::StorageError(format!("Failed to write dimension: {}", e)))?;
        writer
            .write_u32::<LittleEndian>(self.count as u32)
            .map_err(|e| MemdError::StorageError(format!("Failed to write count: {}", e)))?;
        writer
            .write_u32::<LittleEndian>(self.next_id() as u32)
            .map_err(|e| MemdError::StorageError(format!("Failed to write next_id: {}", e)))?;

        // Calculate header CRC
        let mut header_hasher = Hasher::new();
        header_hasher.update(MAGIC);
        header_hasher.update(&VERSION.to_le_bytes());
        header_hasher.update(&(self.dimension as u32).to_le_bytes());
        header_hasher.update(&(self.count as u32).to_le_bytes());
        header_hasher.update(&(self.next_id() as u32).to_le_bytes());
        let header_crc = header_hasher.finalize();

        writer
            .write_u32::<LittleEndian>(header_crc)
            .map_err(|e| MemdError::StorageError(format!("Failed to write header CRC: {}", e)))?;

        // The data CRC covers the valid flags, then the embeddings, as little
        // endian bytes in file order. Both are written in blocks, and the CRC
        // is computed on the same blocks.
        let mut data_hasher = Hasher::new();

        let flags: Vec<u8> = self
            .valid_ids
            .iter()
            .map(|&valid| u8::from(valid))
            .collect();
        data_hasher.update(&flags);
        writer
            .write_all(&flags)
            .map_err(|e| MemdError::StorageError(format!("Failed to write valid flags: {}", e)))?;

        let mut block = Vec::with_capacity(IO_BLOCK_FLOATS * 4);
        for values in self.embeddings.chunks(IO_BLOCK_FLOATS) {
            block.clear();
            for value in values {
                block.extend_from_slice(&value.to_le_bytes());
            }
            data_hasher.update(&block);
            writer.write_all(&block).map_err(|e| {
                MemdError::StorageError(format!("Failed to write embeddings: {}", e))
            })?;
        }

        // Write data CRC
        writer
            .write_u32::<LittleEndian>(data_hasher.finalize())
            .map_err(|e| MemdError::StorageError(format!("Failed to write data CRC: {}", e)))?;

        // Flush and sync
        writer
            .flush()
            .map_err(|e| MemdError::StorageError(format!("Failed to flush: {}", e)))?;
        let file = writer
            .into_inner()
            .map_err(|e| MemdError::StorageError(format!("Failed to get file handle: {}", e)))?;
        file.sync_all()
            .map_err(|e| MemdError::StorageError(format!("Failed to sync file: {}", e)))?;

        // Atomic rename
        std::fs::rename(&temp_path, path)
            .map_err(|e| MemdError::StorageError(format!("Failed to rename file: {}", e)))?;

        // Sync parent directory
        if let Some(parent) = path.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        Ok(())
    }

    /// The vector width recorded in the header of an embeddings file.
    ///
    /// Reads and validates the fixed header only (magic, version, header CRC):
    /// no valid flags, no vector data, no data CRC. The cost does not grow
    /// with the cache. `load_from` validates the body where the index loads.
    pub fn read_dimension(path: &Path) -> Result<usize> {
        let mut file = File::open(path)
            .map_err(|e| MemdError::StorageError(format!("Failed to open cache file: {}", e)))?;
        Ok(read_header(&mut file)?.dimension)
    }

    /// Load embeddings from binary file
    pub fn load_from(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .map_err(|e| MemdError::StorageError(format!("Failed to open cache file: {}", e)))?;

        let mut reader = BufReader::new(file);

        let Header {
            dimension,
            count,
            next_id,
        } = read_header(&mut reader)?;

        // Read valid flags. Any nonzero byte counts as valid, and the CRC is
        // computed over the normalized 0/1 flags.
        let mut flags = vec![0u8; next_id];
        reader
            .read_exact(&mut flags)
            .map_err(|e| MemdError::StorageError(format!("Failed to read valid flags: {}", e)))?;
        for flag in &mut flags {
            *flag = u8::from(*flag != 0);
        }
        let valid_ids: Vec<bool> = flags.iter().map(|&flag| flag != 0).collect();

        // Verify count matches actual valid flags
        let actual_count = valid_ids.iter().filter(|&&v| v).count();
        if actual_count != count {
            return Err(MemdError::ValidationError(format!(
                "Cache count mismatch: header says {}, actual is {}. \
                Delete warm_index/embeddings.bin to rebuild.",
                count, actual_count
            )));
        }

        // Read embeddings data in blocks, hashing each block after the flags.
        let mut data_hasher = Hasher::new();
        data_hasher.update(&flags);
        let total_floats = next_id * dimension;
        let mut embeddings = Vec::with_capacity(total_floats);
        let mut block = vec![0u8; IO_BLOCK_FLOATS.min(total_floats) * 4];
        while embeddings.len() < total_floats {
            let floats = IO_BLOCK_FLOATS.min(total_floats - embeddings.len());
            let bytes = &mut block[..floats * 4];
            reader.read_exact(bytes).map_err(|e| {
                MemdError::StorageError(format!("Failed to read embedding data: {}", e))
            })?;
            data_hasher.update(bytes);
            embeddings.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
        }

        // Verify data CRC
        let stored_data_crc = reader
            .read_u32::<LittleEndian>()
            .map_err(|e| MemdError::StorageError(format!("Failed to read data CRC: {}", e)))?;

        let calculated_data_crc = data_hasher.finalize();

        if stored_data_crc != calculated_data_crc {
            return Err(MemdError::ValidationError(
                "Embedding cache corrupted: data CRC mismatch. \
                Delete warm_index/embeddings.bin to rebuild."
                    .to_string(),
            ));
        }

        Ok(Self {
            embeddings,
            dimension,
            count,
            valid_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes of the fixed header, as `read_header` documents them.
    const HEADER_LEN: usize = 24;

    #[test]
    fn test_new_cache() {
        let cache = EmbeddingCache::new(384);
        assert_eq!(cache.dimension(), 384);
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn test_insert_and_get() {
        let mut cache = EmbeddingCache::new(4);
        let embedding = vec![1.0, 2.0, 3.0, 4.0];

        cache.insert(0, &embedding).unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(0).unwrap(), &embedding[..]);
    }

    #[test]
    fn test_insert_sparse_ids() {
        let mut cache = EmbeddingCache::new(4);
        let emb1 = vec![1.0, 2.0, 3.0, 4.0];
        let emb2 = vec![5.0, 6.0, 7.0, 8.0];

        cache.insert(0, &emb1).unwrap();
        cache.insert(5, &emb2).unwrap();

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(0).unwrap(), &emb1[..]);
        assert_eq!(cache.get(5).unwrap(), &emb2[..]);
        assert!(cache.get(1).is_none());
        assert!(cache.get(3).is_none());
    }

    #[test]
    fn test_insert_dimension_mismatch() {
        let mut cache = EmbeddingCache::new(4);
        let wrong_embedding = vec![1.0, 2.0, 3.0]; // Only 3 dimensions

        let result = cache.insert(0, &wrong_embedding);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("dimension mismatch"));
    }

    #[test]
    fn test_iter_valid() {
        let mut cache = EmbeddingCache::new(4);
        cache.insert(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        cache.insert(2, &[5.0, 6.0, 7.0, 8.0]).unwrap();
        cache.insert(5, &[9.0, 10.0, 11.0, 12.0]).unwrap();

        let valid: Vec<_> = cache.iter_valid().collect();
        assert_eq!(valid.len(), 3);
        assert_eq!(valid[0].0, 0);
        assert_eq!(valid[1].0, 2);
        assert_eq!(valid[2].0, 5);
    }

    #[test]
    fn test_validate_consistency() {
        let mut cache = EmbeddingCache::new(384);
        cache.insert(0, &vec![0.0; 384]).unwrap();
        cache.insert(1, &vec![1.0; 384]).unwrap();

        // Should pass with correct values
        assert!(cache.validate_consistency(384, 2).is_ok());

        // Should fail with wrong dimension
        let result = cache.validate_consistency(256, 2);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("dimension mismatch"));

        // Should fail with wrong count
        let result = cache.validate_consistency(384, 5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("count mismatch"));
    }

    #[test]
    fn test_overwrite_existing() {
        let mut cache = EmbeddingCache::new(4);
        cache.insert(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(cache.len(), 1);

        // Overwrite same ID
        cache.insert(0, &[5.0, 6.0, 7.0, 8.0]).unwrap();
        assert_eq!(cache.len(), 1); // Count should not increase
        assert_eq!(cache.get(0).unwrap(), &[5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn test_save_and_load() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let cache_path = temp_dir.path().join("test_cache.bin");

        // Create and save cache
        let mut cache = EmbeddingCache::new(4);
        cache.insert(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        cache.insert(2, &[5.0, 6.0, 7.0, 8.0]).unwrap();
        cache.save_to(&cache_path).unwrap();

        // Load and verify
        let loaded = EmbeddingCache::load_from(&cache_path).unwrap();
        assert_eq!(loaded.dimension(), 4);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get(0).unwrap(), &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(loaded.get(2).unwrap(), &[5.0, 6.0, 7.0, 8.0]);
        assert!(loaded.get(1).is_none());
    }

    #[test]
    fn test_save_load_large_cache() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let cache_path = temp_dir.path().join("large_cache.bin");

        // Create cache with 100 embeddings
        let mut cache = EmbeddingCache::new(384);
        for i in 0..100 {
            let embedding: Vec<f32> = (0..384).map(|j| (i * 384 + j) as f32).collect();
            cache.insert(i, &embedding).unwrap();
        }

        cache.save_to(&cache_path).unwrap();

        // Load and verify
        let loaded = EmbeddingCache::load_from(&cache_path).unwrap();
        assert_eq!(loaded.dimension(), 384);
        assert_eq!(loaded.len(), 100);

        for i in 0..100 {
            let expected: Vec<f32> = (0..384).map(|j| (i * 384 + j) as f32).collect();
            assert_eq!(loaded.get(i).unwrap(), &expected[..]);
        }
    }

    #[test]
    fn test_load_corrupted_magic() {
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let cache_path = temp_dir.path().join("corrupted.bin");

        // Write invalid magic bytes
        let mut file = File::create(&cache_path).unwrap();
        file.write_all(b"WRONG").unwrap();
        drop(file);

        let result = EmbeddingCache::load_from(&cache_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("magic"));
    }

    #[test]
    fn test_load_wrong_version() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let cache_path = temp_dir.path().join("wrong_version.bin");

        // Create cache with fake version
        let mut cache = EmbeddingCache::new(4);
        cache.insert(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        cache.save_to(&cache_path).unwrap();

        // Modify version byte in file
        let mut data = std::fs::read(&cache_path).unwrap();
        data[4] = 99; // Change version from 1 to 99
        std::fs::write(&cache_path, data).unwrap();

        let result = EmbeddingCache::load_from(&cache_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("version"));
    }

    #[test]
    fn test_load_corrupted_header_crc() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let cache_path = temp_dir.path().join("corrupted_header.bin");

        let mut cache = EmbeddingCache::new(4);
        cache.insert(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        cache.save_to(&cache_path).unwrap();

        // Corrupt header CRC
        let mut data = std::fs::read(&cache_path).unwrap();
        data[20] ^= 0xFF; // Flip bits in header CRC
        std::fs::write(&cache_path, data).unwrap();

        let result = EmbeddingCache::load_from(&cache_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("header CRC"));
    }

    #[test]
    fn test_load_corrupted_data_crc() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let cache_path = temp_dir.path().join("corrupted_data.bin");

        let mut cache = EmbeddingCache::new(4);
        cache.insert(0, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        cache.save_to(&cache_path).unwrap();

        // Corrupt embedding data
        let mut data = std::fs::read(&cache_path).unwrap();
        let data_start = 24 + 1; // Header + valid flags
        data[data_start] ^= 0xFF; // Flip bits in first embedding value
        std::fs::write(&cache_path, data).unwrap();

        let result = EmbeddingCache::load_from(&cache_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("data CRC"));
    }

    /// The bytes of a cache file as the format defines them: header and its
    /// CRC, one valid byte per id, little-endian f32 values, and a CRC over
    /// the flags and values.
    fn reference_bytes(cache: &EmbeddingCache) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&VERSION.to_le_bytes());
        header.extend_from_slice(&(cache.dimension as u32).to_le_bytes());
        header.extend_from_slice(&(cache.count as u32).to_le_bytes());
        header.extend_from_slice(&(cache.next_id() as u32).to_le_bytes());
        let mut data: Vec<u8> = cache
            .valid_ids
            .iter()
            .map(|&valid| u8::from(valid))
            .collect();
        for value in &cache.embeddings {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut bytes = header.clone();
        bytes.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());
        bytes.extend_from_slice(&data);
        bytes.extend_from_slice(&crc32fast::hash(&data).to_le_bytes());
        bytes
    }

    /// 300 ids of 384 floats is more than any IO buffer holds, with gaps and
    /// invalidated slots among the valid ones.
    fn large_sparse_cache() -> EmbeddingCache {
        let mut cache = EmbeddingCache::new(384);
        for id in (0..300).filter(|id| id % 7 != 3) {
            let vector: Vec<f32> = (0..384)
                .map(|j| ((id * 31 + j * 7) % 1000) as f32 / 7.0 - 50.0)
                .collect();
            cache.insert(id, &vector).unwrap();
        }
        for id in [0, 11, 299] {
            cache.mark_invalid(id);
        }
        cache
    }

    #[test]
    fn saved_bytes_match_the_format_and_load_back_exactly() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("embeddings.bin");
        let cache = large_sparse_cache();
        cache.save_to(&path).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), reference_bytes(&cache));

        let loaded = EmbeddingCache::load_from(&path).unwrap();
        assert_eq!(loaded.len(), cache.len());
        assert_eq!(loaded.next_id(), cache.next_id());
        assert_eq!(loaded.valid_ids, cache.valid_ids);
        assert_eq!(loaded.embeddings, cache.embeddings);
    }

    #[test]
    fn empty_cache_round_trips() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("embeddings.bin");
        let cache = EmbeddingCache::new(384);
        cache.save_to(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), reference_bytes(&cache));
        let loaded = EmbeddingCache::load_from(&path).unwrap();
        assert!(loaded.is_empty());
        assert_eq!(loaded.dimension(), 384);
    }

    #[test]
    fn load_rejects_a_truncated_or_corrupted_large_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("embeddings.bin");
        large_sparse_cache().save_to(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(EmbeddingCache::load_from(&path).is_err());

        let mut flipped = bytes.clone();
        let late = flipped.len() - 1000;
        flipped[late] ^= 0xFF;
        std::fs::write(&path, flipped).unwrap();
        let error = EmbeddingCache::load_from(&path).unwrap_err().to_string();
        assert!(error.contains("data CRC"), "got {error}");
    }

    /// Counts the bytes taken from the inner reader.
    struct CountingReader<R> {
        inner: R,
        bytes_read: usize,
    }

    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.bytes_read += n;
            Ok(n)
        }
    }

    #[test]
    fn header_reader_takes_the_header_and_no_vector_data() {
        let bytes = reference_bytes(&large_sparse_cache());
        assert!(
            bytes.len() > 100 * HEADER_LEN,
            "the body must dwarf the header"
        );
        let mut reader = CountingReader {
            inner: std::io::Cursor::new(bytes),
            bytes_read: 0,
        };
        let header = read_header(&mut reader).unwrap();
        assert_eq!(header.dimension, 384);
        assert_eq!(reader.bytes_read, HEADER_LEN);
    }

    #[test]
    fn read_dimension_needs_only_a_valid_header() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("embeddings.bin");
        large_sparse_cache().save_to(&path).unwrap();
        assert_eq!(EmbeddingCache::read_dimension(&path).unwrap(), 384);
        let bytes = std::fs::read(&path).unwrap();

        // A body that fails the data CRC or ends early still has a valid header.
        let mut corrupt = bytes.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xFF;
        std::fs::write(&path, &corrupt).unwrap();
        assert!(EmbeddingCache::load_from(&path).is_err());
        assert_eq!(EmbeddingCache::read_dimension(&path).unwrap(), 384);

        std::fs::write(&path, &bytes[..HEADER_LEN + 5]).unwrap();
        assert!(EmbeddingCache::load_from(&path).is_err());
        assert_eq!(EmbeddingCache::read_dimension(&path).unwrap(), 384);
    }

    #[test]
    fn read_dimension_rejects_a_bad_header() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("embeddings.bin");
        large_sparse_cache().save_to(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        for (offset, expected) in [(0, "magic"), (4, "version"), (8, "header CRC")] {
            let mut bad = bytes.clone();
            bad[offset] ^= 0xFF;
            std::fs::write(&path, &bad).unwrap();
            let error = EmbeddingCache::read_dimension(&path)
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{expected}: got {error}");
        }

        std::fs::write(&path, &bytes[..10]).unwrap();
        assert!(EmbeddingCache::read_dimension(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(EmbeddingCache::read_dimension(&path).is_err());
    }
}
