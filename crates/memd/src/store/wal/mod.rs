//! Write-Ahead Log module
//!
//! Provides durability for write operations before segment commit.
//! Format: Magic(4B) | Type(1B) | Length(4B) | CRC32(4B) | Payload(variable)

pub mod format;
pub mod writer;

pub use format::{WalRecord, WalRecordType, WAL_MAGIC};
pub use writer::WalWriter;
