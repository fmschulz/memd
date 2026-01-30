//! WAL writer implementation
//!
//! Stub for compilation. Full implementation comes in 02-02.

use std::fs::File;
use std::path::Path;

use crate::error::Result;
use super::format::WalRecord;

/// Write-ahead log writer
pub struct WalWriter {
    _file: File,
}

impl WalWriter {
    /// Open or create a WAL file
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::options()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self { _file: file })
    }

    /// Write a record to the WAL
    pub fn write(&mut self, _record: &WalRecord) -> Result<()> {
        unimplemented!("WAL writer implementation in 02-02")
    }

    /// Sync the WAL to disk
    pub fn sync(&self) -> Result<()> {
        unimplemented!("WAL writer implementation in 02-02")
    }
}
