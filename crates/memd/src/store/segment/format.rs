//! Segment file format definitions
//!
//! Defines the binary structures used in segment files:
//! - PayloadIndexRecord: 16-byte index entry (offset, length, crc32)
//! - SegmentMeta: Segment-level metadata

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};

/// Magic bytes at start of segment files: "MSEG"
pub const SEGMENT_MAGIC: &[u8; 4] = b"MSEG";

/// Index record for a single payload in a segment
///
/// Fixed 16-byte structure (repr(C) for consistent memory layout):
/// - offset: u64 (8 bytes) - byte offset in payload.bin
/// - length: u32 (4 bytes) - payload length in bytes
/// - crc32: u32 (4 bytes) - CRC-32 checksum of payload
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct PayloadIndexRecord {
    /// Byte offset in the payload file
    pub offset: u64,
    /// Length of the payload in bytes
    pub length: u32,
    /// CRC-32 checksum for integrity verification
    pub crc32: u32,
}

impl PayloadIndexRecord {
    /// Size of a serialized record in bytes
    pub const SIZE: usize = 16;

    /// Create a new index record
    pub fn new(offset: u64, length: u32, crc32: u32) -> Self {
        Self {
            offset,
            length,
            crc32,
        }
    }

    /// Serialize the record to bytes (little-endian)
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        let mut cursor = &mut buf[..];
        cursor.write_u64::<LittleEndian>(self.offset).unwrap();
        cursor.write_u32::<LittleEndian>(self.length).unwrap();
        cursor.write_u32::<LittleEndian>(self.crc32).unwrap();
        buf
    }

    /// Deserialize a record from bytes (little-endian)
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < Self::SIZE {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "not enough bytes for PayloadIndexRecord",
            ));
        }
        let mut cursor = &bytes[..Self::SIZE];
        let offset = cursor.read_u64::<LittleEndian>()?;
        let length = cursor.read_u32::<LittleEndian>()?;
        let crc32 = cursor.read_u32::<LittleEndian>()?;
        Ok(Self {
            offset,
            length,
            crc32,
        })
    }

    /// Write the record to a writer
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.to_bytes())
    }

    /// Read a record from a reader
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; Self::SIZE];
        reader.read_exact(&mut buf)?;
        Self::from_bytes(&buf)
    }

    /// Parse all index records from bytes
    ///
    /// Assumes bytes start after any magic header.
    pub fn parse_all(bytes: &[u8]) -> io::Result<Vec<Self>> {
        if bytes.len() % Self::SIZE != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid index size: not a multiple of record size",
            ));
        }

        let count = bytes.len() / Self::SIZE;
        let mut records = Vec::with_capacity(count);

        for i in 0..count {
            let offset = i * Self::SIZE;
            records.push(Self::from_bytes(&bytes[offset..offset + Self::SIZE])?);
        }

        Ok(records)
    }
}

/// Metadata for a segment
///
/// Stored in the `meta` file within the segment directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentMeta {
    /// Unique segment identifier
    pub id: u64,
    /// Number of chunks in this segment
    pub chunk_count: u32,
    /// Unix timestamp (milliseconds) when segment was created
    pub created_ts: i64,
    /// Whether the segment has been finalized (read-only)
    pub finalized: bool,
}

impl SegmentMeta {
    /// Create metadata for a new segment
    pub fn new(id: u64) -> Self {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        Self {
            id,
            chunk_count: 0,
            created_ts: now_ms,
            finalized: false,
        }
    }

    /// Load segment metadata from a file
    pub fn load(path: &std::path::Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let (meta, _): (SegmentMeta, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).map_err(
                |e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("failed to decode segment meta: {}", e),
                    )
                },
            )?;
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_index_record_size() {
        assert_eq!(PayloadIndexRecord::SIZE, 16);
        assert_eq!(
            std::mem::size_of::<PayloadIndexRecord>(),
            PayloadIndexRecord::SIZE
        );
    }

    #[test]
    fn payload_index_record_roundtrip() {
        let record = PayloadIndexRecord::new(12345, 678, 0xDEADBEEF);
        let bytes = record.to_bytes();
        let restored = PayloadIndexRecord::from_bytes(&bytes).unwrap();
        assert_eq!(record, restored);
    }

    #[test]
    fn payload_index_record_byte_order() {
        let record = PayloadIndexRecord::new(0x0102030405060708, 0x090A0B0C, 0x0D0E0F10);
        let bytes = record.to_bytes();

        // Verify little-endian encoding
        // offset: 0x0102030405060708 -> 08 07 06 05 04 03 02 01
        assert_eq!(bytes[0], 0x08);
        assert_eq!(bytes[7], 0x01);

        // length: 0x090A0B0C -> 0C 0B 0A 09
        assert_eq!(bytes[8], 0x0C);
        assert_eq!(bytes[11], 0x09);

        // crc32: 0x0D0E0F10 -> 10 0F 0E 0D
        assert_eq!(bytes[12], 0x10);
        assert_eq!(bytes[15], 0x0D);
    }

    #[test]
    fn payload_index_record_read_write() {
        let record = PayloadIndexRecord::new(999, 100, 0xCAFEBABE);
        let mut buf = Vec::new();
        record.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), PayloadIndexRecord::SIZE);

        let restored = PayloadIndexRecord::read_from(&mut buf.as_slice()).unwrap();
        assert_eq!(record, restored);
    }

    #[test]
    fn payload_index_record_from_bytes_too_short() {
        let bytes = [0u8; 10]; // Less than 16 bytes
        let result = PayloadIndexRecord::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn segment_meta_new() {
        let meta = SegmentMeta::new(42);
        assert_eq!(meta.id, 42);
        assert_eq!(meta.chunk_count, 0);
        assert!(!meta.finalized);
        assert!(meta.created_ts > 0);
    }

    #[test]
    fn segment_meta_serde_roundtrip() {
        let meta = SegmentMeta {
            id: 123,
            chunk_count: 456,
            created_ts: 1700000000000,
            finalized: true,
        };

        let encoded = bincode::serde::encode_to_vec(&meta, bincode::config::standard()).unwrap();
        let (decoded, _): (SegmentMeta, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();

        assert_eq!(meta, decoded);
    }
}
