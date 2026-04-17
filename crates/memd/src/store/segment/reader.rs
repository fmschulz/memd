//! Segment reader with memory-mapped I/O
//!
//! Reads finalized segments efficiently using mmap.
//! Tombstone filtering ensures deleted chunks return None.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::{Mmap, MmapOptions};
use parking_lot::RwLock;

use super::format::{PayloadIndexRecord, SegmentMeta, SEGMENT_MAGIC};
use crate::store::TombstoneSet;

/// Reader for a finalized segment
///
/// Phase 3.6: tombstones live behind `Arc<RwLock<TombstoneSet>>` so a
/// delete no longer needs to take an exclusive lock on the enclosing
/// `HashMap<u64, SegmentReader>`. Readers on other segments (and even
/// reads on the same segment's payload bytes, which are mmap'd and
/// immutable) can run concurrently with a tombstone update on this
/// segment.
pub struct SegmentReader {
    /// Segment identifier
    pub id: u64,
    /// Path to segment directory
    dir: PathBuf,
    /// Memory-mapped payload file
    payload_mmap: Mmap,
    /// Index records (offset, length, crc32)
    index: Vec<PayloadIndexRecord>,
    /// Tombstone set for deleted ordinals, shared behind an RwLock so
    /// concurrent readers stay consistent with the latest persisted
    /// delete without needing the enclosing segments map's write lock.
    tombstones: Arc<RwLock<TombstoneSet>>,
}

impl SegmentReader {
    /// Open a finalized segment for reading
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        // Extract segment ID from directory name (seg_NNNNNN)
        let id = Self::parse_segment_id(&dir)?;

        // Open payload file and mmap
        let payload_path = dir.join("payload.bin");
        let payload_file = File::open(&payload_path)?;

        // SAFETY: File is finalized and immutable (append-only, no truncation)
        let payload_mmap = unsafe { MmapOptions::new().map(&payload_file)? };

        // Load index (skip magic header)
        let index_path = dir.join("payload.idx");
        let index = Self::load_index(&index_path)?;

        // Load tombstones
        let tombstone_path = dir.join("tombstone.bin");
        let tombstones = TombstoneSet::load_or_create(tombstone_path)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        Ok(Self {
            id,
            dir,
            payload_mmap,
            index,
            tombstones: Arc::new(RwLock::new(tombstones)),
        })
    }

    fn parse_segment_id(dir: &Path) -> io::Result<u64> {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid segment dir"))?;

        // Format: seg_NNNNNN
        if !name.starts_with("seg_") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid segment name: {}", name),
            ));
        }

        name[4..].parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid segment id: {}", name),
            )
        })
    }

    fn load_index(path: &Path) -> io::Result<Vec<PayloadIndexRecord>> {
        let bytes = std::fs::read(path)?;

        // Verify and skip magic header
        if bytes.len() < SEGMENT_MAGIC.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "index file too small",
            ));
        }

        if &bytes[..SEGMENT_MAGIC.len()] != SEGMENT_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid index magic",
            ));
        }

        // Parse records after magic
        PayloadIndexRecord::parse_all(&bytes[SEGMENT_MAGIC.len()..])
    }

    /// Number of chunks in this segment (including tombstoned)
    pub fn chunk_count(&self) -> u32 {
        self.index.len() as u32
    }

    /// Number of active (non-tombstoned) chunks
    pub fn active_count(&self) -> u32 {
        self.chunk_count() - self.tombstones.read().deleted_count() as u32
    }

    /// Read chunk by ordinal, returns None if tombstoned or out of bounds
    ///
    /// Verifies CRC-32 on read to detect corruption.
    pub fn read_chunk(&self, ordinal: u32) -> io::Result<Option<Vec<u8>>> {
        // Check bounds
        if ordinal as usize >= self.index.len() {
            return Ok(None);
        }

        // Check tombstone under a read lock so concurrent deletes on
        // this segment stay consistent without blocking other readers.
        if self.tombstones.read().is_deleted(ordinal) {
            return Ok(None);
        }

        let record = &self.index[ordinal as usize];
        let start = record.offset as usize;
        let end = start + record.length as usize;

        // Bounds check on mmap
        if end > self.payload_mmap.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "segment {} ordinal {} offset out of bounds",
                    self.id, ordinal
                ),
            ));
        }

        let data = &self.payload_mmap[start..end];

        // Verify CRC-32
        let computed_crc = crc32fast::hash(data);
        if computed_crc != record.crc32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "segment {} ordinal {} CRC mismatch: expected {:08x}, got {:08x}",
                    self.id, ordinal, record.crc32, computed_crc
                ),
            ));
        }

        Ok(Some(data.to_vec()))
    }

    /// Mark ordinal as deleted (updates tombstone).
    ///
    /// Takes `&self` now (not `&mut self`) — the tombstone set has
    /// interior mutability via `RwLock`, so callers no longer need to
    /// take an exclusive lock on the enclosing segments map.
    pub fn mark_deleted(&self, ordinal: u32) -> io::Result<()> {
        let mut tombstones = self.tombstones.write();
        tombstones.mark_deleted(ordinal);
        tombstones
            .persist()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    /// Get segment directory path
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Load segment metadata
    pub fn metadata(&self) -> io::Result<SegmentMeta> {
        let meta_path = self.dir.join("meta");
        SegmentMeta::load(&meta_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::segment::SegmentWriter;
    use tempfile::tempdir;

    #[test]
    fn read_written_segment() {
        let temp_dir = tempdir().unwrap();
        let base_dir = temp_dir.path();

        // Write a segment with multiple chunks
        let mut writer = SegmentWriter::create(base_dir, 1).unwrap();
        writer.append_chunk(b"hello").unwrap();
        writer.append_chunk(b"world").unwrap();
        writer.append_chunk(b"test data").unwrap();
        writer.finalize().unwrap();

        // Read it back
        let seg_dir = base_dir.join("seg_000001");
        let reader = SegmentReader::open(seg_dir).unwrap();

        assert_eq!(reader.id, 1);
        assert_eq!(reader.chunk_count(), 3);
        assert_eq!(reader.active_count(), 3);

        // Verify chunks
        assert_eq!(reader.read_chunk(0).unwrap(), Some(b"hello".to_vec()));
        assert_eq!(reader.read_chunk(1).unwrap(), Some(b"world".to_vec()));
        assert_eq!(reader.read_chunk(2).unwrap(), Some(b"test data".to_vec()));

        // Out of bounds returns None
        assert_eq!(reader.read_chunk(3).unwrap(), None);
        assert_eq!(reader.read_chunk(100).unwrap(), None);
    }

    #[test]
    fn tombstone_filtering() {
        let temp_dir = tempdir().unwrap();
        let base_dir = temp_dir.path();

        // Write a segment
        let mut writer = SegmentWriter::create(base_dir, 2).unwrap();
        writer.append_chunk(b"keep me").unwrap();
        writer.append_chunk(b"delete me").unwrap();
        writer.append_chunk(b"keep me too").unwrap();
        writer.finalize().unwrap();

        // Open and mark middle chunk as deleted
        let seg_dir = base_dir.join("seg_000002");
        let mut reader = SegmentReader::open(seg_dir.clone()).unwrap();

        assert_eq!(reader.active_count(), 3);
        reader.mark_deleted(1).unwrap();
        assert_eq!(reader.active_count(), 2);

        // Verify tombstoned chunk returns None
        assert_eq!(reader.read_chunk(0).unwrap(), Some(b"keep me".to_vec()));
        assert_eq!(reader.read_chunk(1).unwrap(), None); // Tombstoned
        assert_eq!(reader.read_chunk(2).unwrap(), Some(b"keep me too".to_vec()));

        // Reopen and verify tombstone persisted
        let reader2 = SegmentReader::open(seg_dir).unwrap();
        assert_eq!(reader2.active_count(), 2);
        assert_eq!(reader2.read_chunk(1).unwrap(), None);
    }

    #[test]
    fn crc_verification() {
        let temp_dir = tempdir().unwrap();
        let base_dir = temp_dir.path();

        // Write a segment
        let mut writer = SegmentWriter::create(base_dir, 3).unwrap();
        writer.append_chunk(b"verified data").unwrap();
        writer.finalize().unwrap();

        // Corrupt the payload file
        let seg_dir = base_dir.join("seg_000003");
        let payload_path = seg_dir.join("payload.bin");
        let mut payload = std::fs::read(&payload_path).unwrap();
        payload[0] ^= 0xFF; // Flip bits
        std::fs::write(&payload_path, payload).unwrap();

        // Reader should detect CRC mismatch
        let reader = SegmentReader::open(seg_dir).unwrap();
        let result = reader.read_chunk(0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("CRC mismatch"));
    }

    #[test]
    fn parse_segment_id_variants() {
        // Valid IDs
        let dir = PathBuf::from("/tmp/seg_000001");
        assert_eq!(SegmentReader::parse_segment_id(&dir).unwrap(), 1);

        let dir = PathBuf::from("/tmp/seg_000042");
        assert_eq!(SegmentReader::parse_segment_id(&dir).unwrap(), 42);

        let dir = PathBuf::from("/tmp/seg_999999");
        assert_eq!(SegmentReader::parse_segment_id(&dir).unwrap(), 999999);

        // Invalid names
        let dir = PathBuf::from("/tmp/segment_001");
        assert!(SegmentReader::parse_segment_id(&dir).is_err());

        let dir = PathBuf::from("/tmp/seg_abc");
        assert!(SegmentReader::parse_segment_id(&dir).is_err());
    }

    #[test]
    fn metadata_access() {
        let temp_dir = tempdir().unwrap();
        let base_dir = temp_dir.path();

        // Write a segment
        let mut writer = SegmentWriter::create(base_dir, 5).unwrap();
        writer.append_chunk(b"data").unwrap();
        writer.finalize().unwrap();

        // Access metadata through reader
        let seg_dir = base_dir.join("seg_000005");
        let reader = SegmentReader::open(seg_dir).unwrap();
        let meta = reader.metadata().unwrap();

        assert_eq!(meta.id, 5);
        assert_eq!(meta.chunk_count, 1);
        assert!(meta.finalized);
    }

    /// Phase 3.6 regression: after `mark_deleted` returns Ok, any
    /// subsequent `read_chunk` for the same ordinal — from any thread,
    /// without re-acquiring the enclosing segments map's write lock —
    /// must observe the deletion. Prior to this phase the tombstone
    /// lived on the SegmentReader by value and only the `&mut self`
    /// path could mutate it, so a reader holding a stale snapshot
    /// could see `is_deleted == false` after a concurrent deleter
    /// returned. The `Arc<RwLock<TombstoneSet>>` fix closes that gap.
    #[test]
    fn mark_deleted_is_visible_to_concurrent_reader() {
        use std::sync::Arc;

        let temp_dir = tempdir().unwrap();
        let base_dir = temp_dir.path();

        let mut writer = SegmentWriter::create(base_dir, 7).unwrap();
        for i in 0..8u32 {
            writer
                .append_chunk(format!("chunk-{}", i).as_bytes())
                .unwrap();
        }
        writer.finalize().unwrap();

        let seg_dir = base_dir.join("seg_000007");
        let reader = Arc::new(SegmentReader::open(seg_dir).unwrap());

        // Share the SAME SegmentReader across two threads. Thread A
        // deletes ordinal 3, thread B reads ordinal 3 after A returns.
        let reader_a = Arc::clone(&reader);
        let t_a = std::thread::spawn(move || {
            reader_a.mark_deleted(3).unwrap();
        });
        t_a.join().unwrap();

        let read_result = reader.read_chunk(3).unwrap();
        assert!(
            read_result.is_none(),
            "concurrent reader must observe the tombstone after mark_deleted returned"
        );

        // Untouched ordinals still read normally.
        let ok = reader.read_chunk(5).unwrap();
        assert_eq!(ok.as_deref(), Some(b"chunk-5".as_slice()));
    }
}
