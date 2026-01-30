//! Tombstone bitset for tracking soft-deleted chunks per segment
//!
//! Uses Roaring bitmap for space-efficient storage of sparse deletion sets.
//! Persists atomically via temp file + rename pattern.

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

use roaring::RoaringBitmap;

use crate::error::{MemdError, Result};

/// Tombstone set for a single segment
pub struct TombstoneSet {
    bitmap: RoaringBitmap,
    path: PathBuf,
    dirty: bool,
}

impl TombstoneSet {
    /// Load existing tombstone file or create empty set
    pub fn load_or_create(path: PathBuf) -> Result<Self> {
        let bitmap = if path.exists() {
            let mut file = File::open(&path)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            RoaringBitmap::deserialize_from(&bytes[..])
                .map_err(|e| MemdError::StorageError(format!("tombstone deserialize: {}", e)))?
        } else {
            RoaringBitmap::new()
        };

        Ok(Self {
            bitmap,
            path,
            dirty: false,
        })
    }

    /// Mark an ordinal as deleted
    pub fn mark_deleted(&mut self, ordinal: u32) {
        self.bitmap.insert(ordinal);
        self.dirty = true;
    }

    /// Check if ordinal is deleted (O(1) lookup)
    pub fn is_deleted(&self, ordinal: u32) -> bool {
        self.bitmap.contains(ordinal)
    }

    /// Number of deleted items
    pub fn deleted_count(&self) -> u64 {
        self.bitmap.len()
    }

    /// Check if any deletions exist
    pub fn is_empty(&self) -> bool {
        self.bitmap.is_empty()
    }

    /// Persist to disk atomically (temp + rename)
    pub fn persist(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let mut bytes = Vec::new();
        self.bitmap
            .serialize_into(&mut bytes)
            .map_err(|e| MemdError::StorageError(format!("tombstone serialize: {}", e)))?;

        // Atomic write: temp file + rename
        let temp_path = self.path.with_extension("tmp");

        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = File::create(&temp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;

        std::fs::rename(&temp_path, &self.path)?;

        // Sync parent directory for rename durability
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        self.dirty = false;
        Ok(())
    }

    /// Get iterator over all deleted ordinals
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.bitmap.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn empty_tombstone() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tombstone.bin");
        let ts = TombstoneSet::load_or_create(path).unwrap();

        assert!(ts.is_empty());
        assert_eq!(ts.deleted_count(), 0);
        assert!(!ts.is_deleted(0));
        assert!(!ts.is_deleted(100));
    }

    #[test]
    fn mark_and_check() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tombstone.bin");
        let mut ts = TombstoneSet::load_or_create(path).unwrap();

        ts.mark_deleted(5);
        ts.mark_deleted(100);
        ts.mark_deleted(1000);

        assert!(!ts.is_empty());
        assert_eq!(ts.deleted_count(), 3);
        assert!(ts.is_deleted(5));
        assert!(ts.is_deleted(100));
        assert!(ts.is_deleted(1000));
        assert!(!ts.is_deleted(6));
        assert!(!ts.is_deleted(0));
    }

    #[test]
    fn persist_and_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tombstone.bin");

        // Create and persist
        {
            let mut ts = TombstoneSet::load_or_create(path.clone()).unwrap();
            ts.mark_deleted(42);
            ts.mark_deleted(999);
            ts.persist().unwrap();
        }

        // Reload and verify
        {
            let ts = TombstoneSet::load_or_create(path).unwrap();
            assert_eq!(ts.deleted_count(), 2);
            assert!(ts.is_deleted(42));
            assert!(ts.is_deleted(999));
            assert!(!ts.is_deleted(0));
        }
    }

    #[test]
    fn idempotent_delete() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tombstone.bin");
        let mut ts = TombstoneSet::load_or_create(path).unwrap();

        ts.mark_deleted(10);
        ts.mark_deleted(10); // Duplicate
        ts.mark_deleted(10); // Duplicate

        assert_eq!(ts.deleted_count(), 1);
    }

    #[test]
    fn iterate_deleted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tombstone.bin");
        let mut ts = TombstoneSet::load_or_create(path).unwrap();

        ts.mark_deleted(3);
        ts.mark_deleted(1);
        ts.mark_deleted(2);

        let deleted: Vec<u32> = ts.iter().collect();
        assert_eq!(deleted, vec![1, 2, 3]); // Roaring iterates in sorted order
    }

    #[test]
    fn no_persist_when_clean() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tombstone.bin");

        // Load empty (doesn't create file)
        let mut ts = TombstoneSet::load_or_create(path.clone()).unwrap();
        ts.persist().unwrap();

        // File shouldn't exist since nothing was marked
        assert!(!path.exists());

        // Mark something
        ts.mark_deleted(1);
        ts.persist().unwrap();

        // Now file should exist
        assert!(path.exists());
    }
}
