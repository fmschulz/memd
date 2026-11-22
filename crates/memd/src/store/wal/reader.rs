//! WAL reader for crash recovery
//!
//! Reads WAL records and supports replaying for recovery.
//! Skips corrupt bytes and continues from the next valid record.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use super::format::{WalRecord, WalRecordType, WAL_HEADER_SIZE, WAL_MAGIC};

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
    /// Reports corruption and scans for the next magic/header/CRC-valid record.
    /// Returns records and the total byte count belonging to valid records.
    pub fn read_all_records(&self) -> io::Result<(Vec<WalRecord>, usize)> {
        let mut records = Vec::new();
        let mut offset = 0;
        let mut valid_bytes = 0;

        while offset + WAL_HEADER_SIZE <= self.data.len() {
            match WalRecord::decode_from_bytes(&self.data[offset..]) {
                Ok((record, consumed)) => {
                    records.push(record);
                    offset += consumed;
                    valid_bytes += consumed;
                }
                Err(error) => {
                    tracing::warn!(offset, error = %error, "skipping corrupt WAL record");
                    let next = self.data[offset + 1..]
                        .windows(WAL_MAGIC.len())
                        .position(|bytes| bytes == WAL_MAGIC);
                    match next {
                        Some(next) => offset += next + 1,
                        None => break,
                    }
                }
            }
        }

        if valid_bytes != self.data.len() {
            tracing::warn!(
                corrupt_bytes = self.data.len() - valid_bytes,
                "WAL contains skipped corrupt bytes"
            );
        }
        Ok((records, valid_bytes))
    }

    /// Get records for recovery (after last checkpoint)
    ///
    /// Returns records that need to be replayed.
    pub fn records_for_recovery(&self) -> io::Result<Vec<WalRecord>> {
        let (mut records, _) = self.read_all_records()?;

        let checkpoint_idx = records
            .iter()
            .rposition(|r| matches!(r.record_type, WalRecordType::Checkpoint))
            .map(|i| i + 1) // Start after checkpoint
            .unwrap_or(0); // No checkpoint = replay all

        Ok(records.drain(checkpoint_idx..).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::wal::WalWriter;
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
                .append(&WalRecord::add(
                    "tenant_1".into(),
                    "chunk_1".into(),
                    1000,
                    b"data1".to_vec(),
                ))
                .unwrap();
            writer
                .append(&WalRecord::add(
                    "tenant_1".into(),
                    "chunk_2".into(),
                    2000,
                    b"data2".to_vec(),
                ))
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
    fn records_for_recovery() {
        let temp_dir = tempdir().unwrap();
        let wal_path = temp_dir.path().join("recovery.wal");

        // Write records with checkpoint in middle
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            writer
                .append(&WalRecord::add(
                    "t1".into(),
                    "c1".into(),
                    1000,
                    b"d1".to_vec(),
                ))
                .unwrap();
            writer
                .append(&WalRecord::add(
                    "t1".into(),
                    "c2".into(),
                    2000,
                    b"d2".to_vec(),
                ))
                .unwrap();
            writer.append_checkpoint("t1", 3000).unwrap();
            writer
                .append(&WalRecord::add(
                    "t1".into(),
                    "c3".into(),
                    4000,
                    b"d3".to_vec(),
                ))
                .unwrap();
            writer
                .append(&WalRecord::add(
                    "t1".into(),
                    "c4".into(),
                    5000,
                    b"d4".to_vec(),
                ))
                .unwrap();
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
            writer
                .append(&WalRecord::add(
                    "t1".into(),
                    "c1".into(),
                    1000,
                    b"d1".to_vec(),
                ))
                .unwrap();
            writer
                .append(&WalRecord::add(
                    "t1".into(),
                    "c2".into(),
                    2000,
                    b"d2".to_vec(),
                ))
                .unwrap();
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
                .append(&WalRecord::add(
                    "t1".into(),
                    "c1".into(),
                    1000,
                    b"good1".to_vec(),
                ))
                .unwrap();
            writer
                .append(&WalRecord::add(
                    "t1".into(),
                    "c2".into(),
                    2000,
                    b"good2".to_vec(),
                ))
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
    fn recovery_continues_after_corrupt_record() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("corrupt.wal");
        let first = WalRecord::add("t1".into(), "c1".into(), 1, b"first".to_vec());
        let middle = WalRecord::add("t1".into(), "c2".into(), 2, b"bad".to_vec());
        let last = WalRecord::add("t2".into(), "c3".into(), 3, b"last".to_vec());
        let mut bytes = first.encode_to_bytes();
        let mut corrupt = middle.encode_to_bytes();
        *corrupt.last_mut().unwrap() ^= 0xff;
        bytes.extend(corrupt);
        bytes.extend(last.encode_to_bytes());
        std::fs::write(&path, bytes).unwrap();
        let reader = WalReader::open(&path).unwrap();
        let records = reader.records_for_recovery().unwrap();
        assert_eq!(
            records,
            vec![first, last],
            "CRC failure must not hide later valid records"
        );
    }
}
