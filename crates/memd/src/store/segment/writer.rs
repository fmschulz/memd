//! Append-only segment writer
//!
//! SegmentWriter handles the creation of segment directories and writing
//! of chunk payloads with CRC-32 checksums.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::{MemdError, Result};

use super::format::{PayloadIndexRecord, SegmentMeta, SEGMENT_MAGIC};

/// Append-only writer for segment files
///
/// Creates a segment directory structure:
/// ```text
/// seg_000001/
///   payload.bin   # Concatenated chunk payloads
///   payload.idx   # Index records (16 bytes each)
///   meta          # Bincode-encoded SegmentMeta
/// ```
pub struct SegmentWriter {
    /// Segment identifier
    id: u64,
    /// Path to segment directory
    dir: PathBuf,
    /// Buffered writer for payload.bin
    payload_writer: BufWriter<File>,
    /// Current byte position in payload file
    payload_len: u64,
    /// Accumulated index records
    index_records: Vec<PayloadIndexRecord>,
    /// Segment metadata
    meta: SegmentMeta,
}

impl SegmentWriter {
    /// Create a new segment writer
    ///
    /// Creates the segment directory (seg_NNNNNN format) and opens
    /// payload.bin for writing.
    ///
    /// # Arguments
    /// * `base_dir` - Parent directory for segments
    /// * `segment_id` - Unique segment identifier
    ///
    /// # Returns
    /// A new SegmentWriter ready to accept chunks
    pub fn create(base_dir: &Path, segment_id: u64) -> Result<Self> {
        // Create segment directory: seg_000001 (6-digit zero-padded)
        let dir_name = format!("seg_{:06}", segment_id);
        let dir = base_dir.join(&dir_name);

        fs::create_dir_all(&dir).map_err(|e| {
            MemdError::StorageError(format!(
                "failed to create segment directory {}: {}",
                dir.display(),
                e
            ))
        })?;

        // Open payload.bin for writing
        let payload_path = dir.join("payload.bin");
        let payload_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&payload_path)
            .map_err(|e| {
                MemdError::StorageError(format!(
                    "failed to create payload file {}: {}",
                    payload_path.display(),
                    e
                ))
            })?;

        let meta = SegmentMeta::new(segment_id);

        Ok(Self {
            id: segment_id,
            dir,
            payload_writer: BufWriter::new(payload_file),
            payload_len: 0,
            index_records: Vec::new(),
            meta,
        })
    }

    /// Append a chunk payload to the segment
    ///
    /// Computes CRC-32, writes data to payload.bin, and records
    /// the index entry.
    ///
    /// # Arguments
    /// * `data` - Raw chunk payload bytes
    ///
    /// # Returns
    /// The ordinal (0-indexed position) of the written chunk
    pub fn append_chunk(&mut self, data: &[u8]) -> Result<u32> {
        let offset = self.payload_len;
        let length = data.len() as u32;

        // Compute CRC-32
        let crc32 = crc32fast::hash(data);

        // Write to payload file
        self.payload_writer
            .write_all(data)
            .map_err(|e| MemdError::StorageError(format!("failed to write payload: {}", e)))?;

        // Update position
        self.payload_len += length as u64;

        // Record index entry
        let record = PayloadIndexRecord::new(offset, length, crc32);
        self.index_records.push(record);

        // Return ordinal (0-indexed)
        Ok((self.index_records.len() - 1) as u32)
    }

    /// Finalize the segment
    ///
    /// Flushes all buffers, writes the index file, writes metadata,
    /// and syncs to disk.
    ///
    /// # Returns
    /// The finalized SegmentMeta
    pub fn finalize(mut self) -> Result<SegmentMeta> {
        // Write index file first (before moving payload_writer)
        self.write_index_file()?;

        // Update metadata
        self.meta.chunk_count = self.index_records.len() as u32;
        self.meta.finalized = true;
        self.write_meta_file()?;

        // Flush and sync payload file
        self.payload_writer
            .flush()
            .map_err(|e| MemdError::StorageError(format!("failed to flush payload: {}", e)))?;

        let payload_file = self.payload_writer.into_inner().map_err(|e| {
            MemdError::StorageError(format!("failed to get payload file: {}", e.error()))
        })?;

        payload_file
            .sync_all()
            .map_err(|e| MemdError::StorageError(format!("failed to sync payload: {}", e)))?;

        // Sync parent directory (ensures directory entry is persisted)
        Self::sync_directory_path(&self.dir)?;

        Ok(self.meta)
    }

    /// Write the index file (payload.idx)
    fn write_index_file(&self) -> Result<()> {
        let index_path = self.dir.join("payload.idx");
        let mut file = File::create(&index_path).map_err(|e| {
            MemdError::StorageError(format!(
                "failed to create index file {}: {}",
                index_path.display(),
                e
            ))
        })?;

        // Write magic header
        file.write_all(SEGMENT_MAGIC)
            .map_err(|e| MemdError::StorageError(format!("failed to write magic: {}", e)))?;

        // Write all index records
        for record in &self.index_records {
            record.write_to(&mut file).map_err(|e| {
                MemdError::StorageError(format!("failed to write index record: {}", e))
            })?;
        }

        file.sync_all()
            .map_err(|e| MemdError::StorageError(format!("failed to sync index: {}", e)))?;

        Ok(())
    }

    /// Write the metadata file (meta)
    fn write_meta_file(&self) -> Result<()> {
        let meta_path = self.dir.join("meta");
        let encoded = bincode::serde::encode_to_vec(&self.meta, bincode::config::standard())
            .map_err(|e| MemdError::StorageError(format!("failed to encode meta: {}", e)))?;

        let mut file = File::create(&meta_path).map_err(|e| {
            MemdError::StorageError(format!(
                "failed to create meta file {}: {}",
                meta_path.display(),
                e
            ))
        })?;

        file.write_all(&encoded)
            .map_err(|e| MemdError::StorageError(format!("failed to write meta: {}", e)))?;

        file.sync_all()
            .map_err(|e| MemdError::StorageError(format!("failed to sync meta: {}", e)))?;

        Ok(())
    }

    /// Sync the segment directory
    fn sync_directory_path(dir: &Path) -> Result<()> {
        // Open directory and sync - this ensures the directory entry itself is persisted
        let dir_file = File::open(dir).map_err(|e| {
            MemdError::StorageError(format!(
                "failed to open directory for sync {}: {}",
                dir.display(),
                e
            ))
        })?;

        dir_file
            .sync_all()
            .map_err(|e| MemdError::StorageError(format!("failed to sync directory: {}", e)))?;

        Ok(())
    }

    /// Get the segment ID
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Get the segment directory path
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Get the number of chunks written so far
    pub fn chunk_count(&self) -> usize {
        self.index_records.len()
    }

    /// Get the current payload size in bytes
    pub fn payload_size(&self) -> u64 {
        self.payload_len
    }

    /// Read a chunk by ordinal from the active segment
    ///
    /// This is used by PersistentStore to read chunks that haven't been
    /// finalized yet. Returns None if ordinal is out of bounds.
    ///
    /// Note: This flushes the buffer before reading to ensure data is on disk.
    /// CRC verification is performed.
    pub fn read_chunk(&mut self, ordinal: u32) -> Result<Option<Vec<u8>>> {
        let idx = ordinal as usize;
        if idx >= self.index_records.len() {
            return Ok(None);
        }

        // Flush the buffer to ensure data is written to disk
        self.payload_writer
            .flush()
            .map_err(|e| MemdError::StorageError(format!("flush for read: {}", e)))?;

        let record = &self.index_records[idx];
        let payload_path = self.dir.join("payload.bin");

        // Read from the file
        let payload_data = std::fs::read(&payload_path).map_err(|e| {
            MemdError::StorageError(format!("read payload for chunk {}: {}", ordinal, e))
        })?;

        let start = record.offset as usize;
        let end = start + record.length as usize;

        if end > payload_data.len() {
            return Err(MemdError::StorageError(format!(
                "chunk {} offset out of bounds: {} > {}",
                ordinal,
                end,
                payload_data.len()
            )));
        }

        let data = &payload_data[start..end];

        // Verify CRC
        let computed_crc = crc32fast::hash(data);
        if computed_crc != record.crc32 {
            return Err(MemdError::StorageError(format!(
                "chunk {} CRC mismatch: expected {:08x}, got {:08x}",
                ordinal, record.crc32, computed_crc
            )));
        }

        Ok(Some(data.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn write_single_chunk() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();

        let mut writer = SegmentWriter::create(base_dir, 1).unwrap();
        let ordinal = writer.append_chunk(b"hello world").unwrap();
        assert_eq!(ordinal, 0);

        let meta = writer.finalize().unwrap();

        // Verify metadata
        assert_eq!(meta.id, 1);
        assert_eq!(meta.chunk_count, 1);
        assert!(meta.finalized);

        // Verify files exist
        let seg_dir = base_dir.join("seg_000001");
        assert!(seg_dir.join("payload.bin").exists());
        assert!(seg_dir.join("payload.idx").exists());
        assert!(seg_dir.join("meta").exists());

        // Verify payload content
        let payload = fs::read(seg_dir.join("payload.bin")).unwrap();
        assert_eq!(payload, b"hello world");

        // Verify index file structure
        let index = fs::read(seg_dir.join("payload.idx")).unwrap();
        // Magic (4) + 1 record (16) = 20 bytes
        assert_eq!(index.len(), 20);
        assert_eq!(&index[0..4], SEGMENT_MAGIC);
    }

    #[test]
    fn write_multiple_chunks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();

        let mut writer = SegmentWriter::create(base_dir, 42).unwrap();

        // Write 3 chunks
        let ord1 = writer.append_chunk(b"first").unwrap();
        let ord2 = writer.append_chunk(b"second").unwrap();
        let ord3 = writer.append_chunk(b"third").unwrap();

        assert_eq!(ord1, 0);
        assert_eq!(ord2, 1);
        assert_eq!(ord3, 2);
        assert_eq!(writer.chunk_count(), 3);

        let meta = writer.finalize().unwrap();
        assert_eq!(meta.chunk_count, 3);

        // Verify index has 3 records
        let seg_dir = base_dir.join("seg_000042");
        let index = fs::read(seg_dir.join("payload.idx")).unwrap();
        // Magic (4) + 3 records (48) = 52 bytes
        assert_eq!(index.len(), 52);

        // Verify concatenated payload
        let payload = fs::read(seg_dir.join("payload.bin")).unwrap();
        assert_eq!(payload, b"firstsecondthird");
    }

    #[test]
    fn crc_integrity() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();

        let test_data = b"test data for CRC verification";
        let expected_crc = crc32fast::hash(test_data);

        let mut writer = SegmentWriter::create(base_dir, 1).unwrap();
        writer.append_chunk(test_data).unwrap();
        writer.finalize().unwrap();

        // Read back the index and verify CRC
        let seg_dir = base_dir.join("seg_000001");
        let index_bytes = fs::read(seg_dir.join("payload.idx")).unwrap();

        // Skip magic, read first record
        let record = PayloadIndexRecord::from_bytes(&index_bytes[4..]).unwrap();

        assert_eq!(record.offset, 0);
        assert_eq!(record.length, test_data.len() as u32);
        assert_eq!(record.crc32, expected_crc);

        // Verify CRC matches recomputed value from payload
        let payload = fs::read(seg_dir.join("payload.bin")).unwrap();
        let recomputed_crc = crc32fast::hash(&payload);
        assert_eq!(record.crc32, recomputed_crc);
    }

    #[test]
    fn segment_directory_naming() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();

        // Test various segment IDs
        let _ = SegmentWriter::create(base_dir, 1).unwrap();
        let _ = SegmentWriter::create(base_dir, 999999).unwrap();
        let _ = SegmentWriter::create(base_dir, 1000000).unwrap();

        assert!(base_dir.join("seg_000001").exists());
        assert!(base_dir.join("seg_999999").exists());
        assert!(base_dir.join("seg_1000000").exists());
    }

    #[test]
    fn meta_file_readable() {
        let temp_dir = tempfile::tempdir().unwrap();
        let base_dir = temp_dir.path();

        let mut writer = SegmentWriter::create(base_dir, 7).unwrap();
        writer.append_chunk(b"data").unwrap();
        let original_meta = writer.finalize().unwrap();

        // Read meta file and decode
        let seg_dir = base_dir.join("seg_000007");
        let meta_bytes = fs::read(seg_dir.join("meta")).unwrap();
        let (decoded_meta, _): (SegmentMeta, _) =
            bincode::serde::decode_from_slice(&meta_bytes, bincode::config::standard()).unwrap();

        assert_eq!(original_meta.id, decoded_meta.id);
        assert_eq!(original_meta.chunk_count, decoded_meta.chunk_count);
        assert_eq!(original_meta.finalized, decoded_meta.finalized);
    }
}
