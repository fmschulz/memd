//! WAL reader for crash recovery
//!
//! Reads WAL records and supports replaying for recovery.
//! Handles partial/corrupt records gracefully (stops at first error).

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use super::format::{WalRecord, WalRecordType, WAL_HEADER_SIZE};

/// WAL reader
pub struct WalReader {
    data: Vec<u8>,
}

impl WalReader {
    /// Open and read entire WAL file
    ///
    /// Returns empty reader if file doesn't exist (no records to recover).
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self { data: Vec::new() });
        }

        let mut file = File::open(path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        Ok(Self { data })
    }

    /// Check if WAL is empty (no records)
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Get raw data length
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Read all valid records from WAL
    ///
    /// Stops at first corrupted record (partial write on crash).
    /// Returns records and count of bytes successfully read.
    pub fn read_all_records(&self) -> io::Result<(Vec<WalRecord>, usize)> {
        let mut records = Vec::new();
        let mut offset = 0;

        while offset + WAL_HEADER_SIZE <= self.data.len() {
            match WalRecord::decode_from_bytes(&self.data[offset..]) {
                Ok((record, consumed)) => {
                    records.push(record);
                    offset += consumed;
                }
                Err(_) => {
                    // Corrupted record - stop here (partial write on crash)
                    break;
                }
            }
        }

        Ok((records, offset))
    }

    /// Find the index of the last checkpoint record
    pub fn find_last_checkpoint(&self) -> io::Result<Option<usize>> {
        let (records, _) = self.read_all_records()?;

        Ok(records
            .iter()
            .rposition(|r| matches!(r.record_type, WalRecordType::Checkpoint)))
    }

    /// Get records for recovery (after last checkpoint)
    ///
    /// Returns records that need to be replayed.
    pub fn records_for_recovery(&self) -> io::Result<Vec<WalRecord>> {
        let (records, _) = self.read_all_records()?;

        let checkpoint_idx = records
            .iter()
            .rposition(|r| matches!(r.record_type, WalRecordType::Checkpoint))
            .map(|i| i + 1) // Start after checkpoint
            .unwrap_or(0); // No checkpoint = replay all

        Ok(records[checkpoint_idx..].to_vec())
    }
}

/// Recovery helper functions
pub mod recovery {
    use super::*;

    /// Callback trait for applying recovered records
    pub trait RecoveryHandler {
        /// Called for each Add record
        fn on_add(&mut self, record: &WalRecord) -> io::Result<()>;

        /// Called for each Delete record
        fn on_delete(&mut self, record: &WalRecord) -> io::Result<()>;

        /// Check if a chunk already exists (for idempotency)
        fn chunk_exists(&self, chunk_id: &str) -> io::Result<bool>;
    }

    /// Recovery statistics
    #[derive(Debug, Default, Clone, PartialEq, Eq)]
    pub struct RecoveryStats {
        /// Number of Add records applied
        pub adds: usize,
        /// Number of Delete records applied
        pub deletes: usize,
        /// Number of records skipped (already exist)
        pub skipped: usize,
        /// Number of checkpoint records encountered (should be 0 after last checkpoint)
        pub checkpoints: usize,
    }

    /// Replay WAL records through handler
    ///
    /// Skips Add records for chunks that already exist (idempotent).
    pub fn replay<H: RecoveryHandler>(
        reader: &WalReader,
        handler: &mut H,
    ) -> io::Result<RecoveryStats> {
        let records = reader.records_for_recovery()?;

        let mut stats = RecoveryStats::default();

        for record in &records {
            match record.record_type {
                WalRecordType::Add => {
                    // Idempotent: skip if chunk already exists
                    if handler.chunk_exists(&record.chunk_id)? {
                        stats.skipped += 1;
                        continue;
                    }
                    handler.on_add(record)?;
                    stats.adds += 1;
                }
                WalRecordType::Delete => {
                    handler.on_delete(record)?;
                    stats.deletes += 1;
                }
                WalRecordType::Checkpoint => {
                    // Should not appear after last checkpoint
                    stats.checkpoints += 1;
                }
            }
        }

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::wal::WalWriter;
    use std::collections::HashSet;
    use tempfile::tempdir;

    #[test]
    fn read_empty_wal() {
        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("nonexistent.wal");

        let reader = WalReader::open(&wal_path).unwrap();
        assert!(reader.is_empty());
        assert_eq!(reader.len(), 0);

        let (records, bytes_read) = reader.read_all_records().unwrap();
        assert!(records.is_empty());
        assert_eq!(bytes_read, 0);
    }

    #[test]
    fn read_written_records() {
        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("test.wal");

        // Write records
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            writer
                .append_add("tenant_1", "chunk_1", 1000, b"data1".to_vec())
                .unwrap();
            writer
                .append_add("tenant_1", "chunk_2", 2000, b"data2".to_vec())
                .unwrap();
            writer.append_delete("tenant_1", "chunk_1", 3000).unwrap();
        }

        // Read them back
        let reader = WalReader::open(&wal_path).unwrap();
        assert!(!reader.is_empty());

        let (records, _) = reader.read_all_records().unwrap();
        assert_eq!(records.len(), 3);

        assert_eq!(records[0].record_type, WalRecordType::Add);
        assert_eq!(records[0].chunk_id, "chunk_1");
        assert_eq!(records[0].payload, b"data1");

        assert_eq!(records[1].record_type, WalRecordType::Add);
        assert_eq!(records[1].chunk_id, "chunk_2");

        assert_eq!(records[2].record_type, WalRecordType::Delete);
        assert_eq!(records[2].chunk_id, "chunk_1");
    }

    #[test]
    fn find_checkpoint() {
        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("checkpoint.wal");

        // Write records with checkpoint
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            writer.append_add("t1", "c1", 1000, b"d1".to_vec()).unwrap();
            writer.append_checkpoint("t1", 2000).unwrap();
            writer.append_add("t1", "c2", 3000, b"d2".to_vec()).unwrap();
        }

        let reader = WalReader::open(&wal_path).unwrap();
        let checkpoint_idx = reader.find_last_checkpoint().unwrap();
        assert_eq!(checkpoint_idx, Some(1)); // Index of checkpoint record
    }

    #[test]
    fn records_for_recovery() {
        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("recovery.wal");

        // Write records with checkpoint in middle
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            writer.append_add("t1", "c1", 1000, b"d1".to_vec()).unwrap();
            writer.append_add("t1", "c2", 2000, b"d2".to_vec()).unwrap();
            writer.append_checkpoint("t1", 3000).unwrap();
            writer.append_add("t1", "c3", 4000, b"d3".to_vec()).unwrap();
            writer.append_add("t1", "c4", 5000, b"d4".to_vec()).unwrap();
        }

        let reader = WalReader::open(&wal_path).unwrap();
        let recovery_records = reader.records_for_recovery().unwrap();

        // Should only have records after checkpoint
        assert_eq!(recovery_records.len(), 2);
        assert_eq!(recovery_records[0].chunk_id, "c3");
        assert_eq!(recovery_records[1].chunk_id, "c4");
    }

    #[test]
    fn records_for_recovery_no_checkpoint() {
        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("no_checkpoint.wal");

        // Write records without checkpoint
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            writer.append_add("t1", "c1", 1000, b"d1".to_vec()).unwrap();
            writer.append_add("t1", "c2", 2000, b"d2".to_vec()).unwrap();
        }

        let reader = WalReader::open(&wal_path).unwrap();
        let recovery_records = reader.records_for_recovery().unwrap();

        // All records should be returned
        assert_eq!(recovery_records.len(), 2);
    }

    #[test]
    fn partial_record_tolerance() {
        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("partial.wal");

        // Write records
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            writer
                .append_add("t1", "c1", 1000, b"good1".to_vec())
                .unwrap();
            writer
                .append_add("t1", "c2", 2000, b"good2".to_vec())
                .unwrap();
        }

        // Simulate partial write by appending garbage
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&wal_path)
                .unwrap();
            // Write partial header (not a complete record)
            file.write_all(b"MWAL").unwrap(); // Magic only, incomplete header
            file.write_all(&[1, 0, 0]).unwrap(); // Partial length
        }

        // Reader should return only the valid records
        let reader = WalReader::open(&wal_path).unwrap();
        let (records, bytes_read) = reader.read_all_records().unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].chunk_id, "c1");
        assert_eq!(records[1].chunk_id, "c2");
        // bytes_read should be less than total file size
        assert!(bytes_read < reader.len());
    }

    #[test]
    fn recovery_replay_idempotent() {
        use recovery::{replay, RecoveryHandler, RecoveryStats};

        // Mock handler that tracks calls
        struct MockHandler {
            existing: HashSet<String>,
            added: Vec<String>,
            deleted: Vec<String>,
        }

        impl RecoveryHandler for MockHandler {
            fn on_add(&mut self, record: &WalRecord) -> io::Result<()> {
                self.added.push(record.chunk_id.clone());
                self.existing.insert(record.chunk_id.clone());
                Ok(())
            }

            fn on_delete(&mut self, record: &WalRecord) -> io::Result<()> {
                self.deleted.push(record.chunk_id.clone());
                Ok(())
            }

            fn chunk_exists(&self, chunk_id: &str) -> io::Result<bool> {
                Ok(self.existing.contains(chunk_id))
            }
        }

        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("replay.wal");

        // Write WAL
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            writer
                .append_add("t1", "new_chunk", 1000, b"data".to_vec())
                .unwrap();
            writer
                .append_add("t1", "existing_chunk", 2000, b"data".to_vec())
                .unwrap();
            writer.append_delete("t1", "to_delete", 3000).unwrap();
        }

        let reader = WalReader::open(&wal_path).unwrap();

        // First replay - existing_chunk already exists
        let mut handler = MockHandler {
            existing: HashSet::from(["existing_chunk".to_string()]),
            added: Vec::new(),
            deleted: Vec::new(),
        };

        let stats = replay(&reader, &mut handler).unwrap();

        assert_eq!(
            stats,
            RecoveryStats {
                adds: 1,    // Only new_chunk added
                skipped: 1, // existing_chunk skipped
                deletes: 1, // to_delete processed
                checkpoints: 0,
            }
        );

        assert_eq!(handler.added, vec!["new_chunk"]);
        assert_eq!(handler.deleted, vec!["to_delete"]);
    }

    #[test]
    fn recovery_after_checkpoint() {
        use recovery::{replay, RecoveryHandler};

        struct CountingHandler {
            add_count: usize,
        }

        impl RecoveryHandler for CountingHandler {
            fn on_add(&mut self, _record: &WalRecord) -> io::Result<()> {
                self.add_count += 1;
                Ok(())
            }

            fn on_delete(&mut self, _record: &WalRecord) -> io::Result<()> {
                Ok(())
            }

            fn chunk_exists(&self, _chunk_id: &str) -> io::Result<bool> {
                Ok(false) // Nothing exists
            }
        }

        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("checkpoint_recovery.wal");

        // Write WAL with checkpoint
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            // Before checkpoint - these should NOT be replayed
            writer.append_add("t1", "c1", 1000, b"d1".to_vec()).unwrap();
            writer.append_add("t1", "c2", 2000, b"d2".to_vec()).unwrap();
            writer.append_checkpoint("t1", 3000).unwrap();
            // After checkpoint - these SHOULD be replayed
            writer.append_add("t1", "c3", 4000, b"d3".to_vec()).unwrap();
        }

        let reader = WalReader::open(&wal_path).unwrap();
        let mut handler = CountingHandler { add_count: 0 };

        let stats = replay(&reader, &mut handler).unwrap();

        // Only 1 add after checkpoint
        assert_eq!(stats.adds, 1);
        assert_eq!(handler.add_count, 1);
    }
}
