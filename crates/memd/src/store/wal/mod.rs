//! Write-Ahead Log module
//!
//! Provides durability for write operations before segment commit.
//! Format: Magic(4B) | Type(1B) | Length(4B) | CRC32(4B) | Payload(variable)

pub mod format;
pub mod reader;
pub mod writer;

pub use format::{TaskArtifactWalPayload, WAL_MAGIC, WalRecord, WalRecordType};
pub use reader::{WalReader, recovery};
pub use writer::WalWriter;
