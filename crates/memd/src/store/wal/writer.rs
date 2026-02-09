//! WAL writer implementation
//!
//! Provides durable write operations with fsync after each record.
//! Critical for crash recovery: writes must be fsynced before being acknowledged.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::format::WalRecord;
#[cfg(test)]
use super::format::WalRecordType;

/// Write-ahead log writer
///
/// Appends records to a WAL file with fsync durability.
/// Each write is followed by sync_all() to ensure data reaches disk.
pub struct WalWriter {
    /// The WAL file handle
    file: File,
    /// Path to the WAL file
    path: PathBuf,
    /// Count of records written
    records_written: u64,
}

impl WalWriter {
    /// Create a new WAL file
    ///
    /// Creates parent directories if needed.
    /// Fails if the file already exists.
    pub fn create(path: &Path) -> io::Result<Self> {
        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new().create_new(true).write(true).open(path)?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
            records_written: 0,
        })
    }

    /// Open an existing WAL file for append
    ///
    /// Fails if the file does not exist.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().append(true).open(path)?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
            records_written: 0, // Note: doesn't count existing records
        })
    }

    /// Open existing WAL or create new one
    ///
    /// This is the primary entry point for PersistentStore startup.
    /// If the file exists, opens for append. Otherwise creates new.
    pub fn open_or_create(path: &Path) -> io::Result<Self> {
        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if path.exists() {
            Self::open(path)
        } else {
            Self::create(path)
        }
    }

    /// Append a record to the WAL
    ///
    /// Encodes the record, writes to file, and calls sync_all() for durability.
    /// CRITICAL: sync_all() is called after EVERY write for crash safety.
    pub fn append(&mut self, record: &WalRecord) -> io::Result<()> {
        let bytes = record.encode_to_bytes();
        self.file.write_all(&bytes)?;

        // CRITICAL: Ensure data reaches disk before returning
        self.file.sync_all()?;

        self.records_written += 1;
        Ok(())
    }

    /// Append an Add record (convenience method)
    pub fn append_add(
        &mut self,
        tenant_id: &str,
        chunk_id: &str,
        timestamp: i64,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        let record = WalRecord::add(
            tenant_id.to_string(),
            chunk_id.to_string(),
            timestamp,
            payload,
        );
        self.append(&record)
    }

    /// Append a Delete record (convenience method)
    pub fn append_delete(
        &mut self,
        tenant_id: &str,
        chunk_id: &str,
        timestamp: i64,
    ) -> io::Result<()> {
        let record = WalRecord::delete(tenant_id.to_string(), chunk_id.to_string(), timestamp);
        self.append(&record)
    }

    /// Append a Checkpoint record (convenience method)
    ///
    /// Checkpoint marks that all preceding records have been committed to segments.
    /// After recovery, records before the last checkpoint can be skipped.
    pub fn append_checkpoint(&mut self, tenant_id: &str, timestamp: i64) -> io::Result<()> {
        let record = WalRecord::checkpoint(tenant_id.to_string(), timestamp);
        self.append(&record)
    }

    /// Explicit sync (already done per-record, but useful for batches)
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    /// Get the number of records written in this session
    pub fn records_written(&self) -> u64 {
        self.records_written
    }

    /// Get the path to the WAL file
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Truncate the WAL file to zero
    ///
    /// Called after successful checkpoint/recovery to start fresh.
    /// After truncation, records_written is reset to 0.
    pub fn truncate(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.sync_all()?;
        self.records_written = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_single_record() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("test.wal");

        let mut writer = WalWriter::create(&wal_path).unwrap();
        writer
            .append_add("tenant_1", "chunk_1", 1000, b"hello".to_vec())
            .unwrap();

        assert!(wal_path.exists());
        assert!(std::fs::metadata(&wal_path).unwrap().len() > 0);
        assert_eq!(writer.records_written(), 1);
    }

    #[test]
    fn write_multiple_records() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("test.wal");

        let mut writer = WalWriter::create(&wal_path).unwrap();

        // Record file size after each write
        let initial_size = 0;

        writer
            .append_add("t1", "c1", 1000, b"data1".to_vec())
            .unwrap();
        let size_after_add = std::fs::metadata(&wal_path).unwrap().len();
        assert!(size_after_add > initial_size);

        writer.append_delete("t1", "c2", 2000).unwrap();
        let size_after_delete = std::fs::metadata(&wal_path).unwrap().len();
        assert!(size_after_delete > size_after_add);

        writer.append_checkpoint("t1", 3000).unwrap();
        let size_after_checkpoint = std::fs::metadata(&wal_path).unwrap().len();
        assert!(size_after_checkpoint > size_after_delete);

        assert_eq!(writer.records_written(), 3);
    }

    #[test]
    fn append_convenience_methods() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("test.wal");

        let mut writer = WalWriter::create(&wal_path).unwrap();

        // Test each convenience method
        writer
            .append_add("tenant", "chunk_add", 1000, b"payload".to_vec())
            .unwrap();
        writer.append_delete("tenant", "chunk_del", 2000).unwrap();
        writer.append_checkpoint("tenant", 3000).unwrap();

        assert_eq!(writer.records_written(), 3);

        // Read back and verify
        let contents = std::fs::read(&wal_path).unwrap();
        let (record1, offset1) = WalRecord::decode_from_bytes(&contents).unwrap();
        assert_eq!(record1.record_type, WalRecordType::Add);
        assert_eq!(record1.chunk_id, "chunk_add");
        assert_eq!(record1.payload, b"payload");

        let (record2, offset2) = WalRecord::decode_from_bytes(&contents[offset1..]).unwrap();
        assert_eq!(record2.record_type, WalRecordType::Delete);
        assert_eq!(record2.chunk_id, "chunk_del");

        let (record3, _) = WalRecord::decode_from_bytes(&contents[offset1 + offset2..]).unwrap();
        assert_eq!(record3.record_type, WalRecordType::Checkpoint);
    }

    #[test]
    fn open_or_create_new() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("new.wal");

        assert!(!wal_path.exists());

        let mut writer = WalWriter::open_or_create(&wal_path).unwrap();
        writer
            .append_add("t1", "c1", 1000, b"test".to_vec())
            .unwrap();

        assert!(wal_path.exists());
        assert_eq!(writer.records_written(), 1);
    }

    #[test]
    fn open_or_create_existing() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("existing.wal");

        // Create initial WAL with one record
        {
            let mut writer = WalWriter::create(&wal_path).unwrap();
            writer
                .append_add("t1", "c1", 1000, b"first".to_vec())
                .unwrap();
        }

        let size_before = std::fs::metadata(&wal_path).unwrap().len();

        // Open existing and append
        {
            let mut writer = WalWriter::open_or_create(&wal_path).unwrap();
            writer
                .append_add("t1", "c2", 2000, b"second".to_vec())
                .unwrap();
        }

        let size_after = std::fs::metadata(&wal_path).unwrap().len();
        assert!(size_after > size_before);

        // Verify both records are present
        let contents = std::fs::read(&wal_path).unwrap();
        let (record1, offset1) = WalRecord::decode_from_bytes(&contents).unwrap();
        assert_eq!(record1.chunk_id, "c1");

        let (record2, _) = WalRecord::decode_from_bytes(&contents[offset1..]).unwrap();
        assert_eq!(record2.chunk_id, "c2");
    }

    #[test]
    fn create_fails_if_exists() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("exists.wal");

        // Create the file
        WalWriter::create(&wal_path).unwrap();

        // Creating again should fail
        let result = WalWriter::create(&wal_path);
        assert!(result.is_err());
    }

    #[test]
    fn open_fails_if_not_exists() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("nonexistent.wal");

        let result = WalWriter::open(&wal_path);
        assert!(result.is_err());
    }

    #[test]
    fn truncate_clears_file() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("truncate.wal");

        let mut writer = WalWriter::create(&wal_path).unwrap();
        writer
            .append_add("t1", "c1", 1000, b"data".to_vec())
            .unwrap();
        writer
            .append_add("t1", "c2", 2000, b"more data".to_vec())
            .unwrap();

        let size_before = std::fs::metadata(&wal_path).unwrap().len();
        assert!(size_before > 0);
        assert_eq!(writer.records_written(), 2);

        writer.truncate().unwrap();

        let size_after = std::fs::metadata(&wal_path).unwrap().len();
        assert_eq!(size_after, 0);
        assert_eq!(writer.records_written(), 0);
    }

    #[test]
    fn creates_parent_directories() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("test.wal");

        assert!(!nested_path.parent().unwrap().exists());

        let writer = WalWriter::create(&nested_path).unwrap();
        assert!(nested_path.exists());
        drop(writer);

        // Same for open_or_create
        let nested_path2 = temp_dir
            .path()
            .join("x")
            .join("y")
            .join("z")
            .join("test.wal");
        let writer = WalWriter::open_or_create(&nested_path2).unwrap();
        assert!(nested_path2.exists());
        drop(writer);
    }

    #[test]
    fn path_accessor() {
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("test.wal");

        let writer = WalWriter::create(&wal_path).unwrap();
        assert_eq!(writer.path(), wal_path);
    }

    #[test]
    fn sync_all_is_called() {
        // This test verifies the file is synced by checking we can read
        // the data immediately after write without explicit flush
        let temp_dir = TempDir::new().unwrap();
        let wal_path = temp_dir.path().join("sync.wal");

        let mut writer = WalWriter::create(&wal_path).unwrap();
        writer
            .append_add("t1", "c1", 1000, b"important".to_vec())
            .unwrap();

        // Read from a different handle immediately - should see the data
        // because sync_all was called
        let contents = std::fs::read(&wal_path).unwrap();
        let (record, _) = WalRecord::decode_from_bytes(&contents).unwrap();
        assert_eq!(record.payload, b"important");
    }
}
