//! WAL record format definitions
//!
//! Defines the binary format for WAL records:
//! - Header: Magic(4B) | Type(1B) | Length(4B) | CRC32(4B) = 13 bytes
//! - Payload: Variable length serialized data

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};

/// Magic bytes identifying a WAL file: "MWAL"
pub const WAL_MAGIC: &[u8; 4] = b"MWAL";

/// Header size in bytes: magic(4) + type(1) + length(4) + crc(4) = 13
pub const WAL_HEADER_SIZE: usize = 13;

/// WAL record types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum WalRecordType {
    /// Add a new chunk
    Add = 1,
    /// Delete (soft-delete) a chunk
    Delete = 2,
    /// Checkpoint marker - segment flush complete
    Checkpoint = 3,
}

impl WalRecordType {
    /// Convert from u8, returns None for invalid values
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(WalRecordType::Add),
            2 => Some(WalRecordType::Delete),
            3 => Some(WalRecordType::Checkpoint),
            _ => None,
        }
    }
}

/// WAL record containing operation data
///
/// Serialized with bincode for the payload portion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalRecord {
    /// Type of operation
    pub record_type: WalRecordType,
    /// Tenant identifier
    pub tenant_id: String,
    /// Chunk identifier (UUIDv7 string)
    pub chunk_id: String,
    /// Timestamp of the operation (Unix milliseconds)
    pub timestamp: i64,
    /// Serialized chunk data for Add, empty for Delete/Checkpoint
    pub payload: Vec<u8>,
}

impl WalRecord {
    /// Create a new Add record
    pub fn add(tenant_id: String, chunk_id: String, timestamp: i64, payload: Vec<u8>) -> Self {
        Self {
            record_type: WalRecordType::Add,
            tenant_id,
            chunk_id,
            timestamp,
            payload,
        }
    }

    /// Create a new Delete record
    pub fn delete(tenant_id: String, chunk_id: String, timestamp: i64) -> Self {
        Self {
            record_type: WalRecordType::Delete,
            tenant_id,
            chunk_id,
            timestamp,
            payload: Vec::new(),
        }
    }

    /// Create a new Checkpoint record
    pub fn checkpoint(tenant_id: String, timestamp: i64) -> Self {
        Self {
            record_type: WalRecordType::Checkpoint,
            tenant_id,
            chunk_id: String::new(),
            timestamp,
            payload: Vec::new(),
        }
    }

    /// Encode the record to bytes
    ///
    /// Format: Magic(4B) | Type(1B) | Length(4B) | CRC32(4B) | Payload(variable)
    pub fn encode_to_bytes(&self) -> Vec<u8> {
        // Serialize the record payload with bincode
        let payload_bytes =
            bincode::serde::encode_to_vec(self, bincode::config::standard()).unwrap();

        // Compute CRC-32 of the serialized payload
        let crc = crc32fast::hash(&payload_bytes);

        // Build the full record
        let total_size = WAL_HEADER_SIZE + payload_bytes.len();
        let mut result = Vec::with_capacity(total_size);

        // Write header
        result.extend_from_slice(WAL_MAGIC);
        result.push(self.record_type as u8);
        result
            .write_u32::<LittleEndian>(payload_bytes.len() as u32)
            .unwrap();
        result.write_u32::<LittleEndian>(crc).unwrap();

        // Write payload
        result.extend_from_slice(&payload_bytes);

        result
    }

    /// Decode a record from bytes
    ///
    /// Returns the decoded record and the number of bytes consumed.
    /// Returns an error if the magic is invalid, CRC doesn't match, or data is truncated.
    pub fn decode_from_bytes(bytes: &[u8]) -> io::Result<(Self, usize)> {
        if bytes.len() < WAL_HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "not enough bytes for WAL header",
            ));
        }

        // Validate magic
        if &bytes[0..4] != WAL_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid WAL magic: expected {:?}, got {:?}",
                    WAL_MAGIC,
                    &bytes[0..4]
                ),
            ));
        }

        // Read record type
        let type_byte = bytes[4];
        let record_type = WalRecordType::from_u8(type_byte).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid WAL record type: {}", type_byte),
            )
        })?;

        // Read length and CRC
        let mut cursor = &bytes[5..WAL_HEADER_SIZE];
        let length = cursor.read_u32::<LittleEndian>()? as usize;
        let expected_crc = cursor.read_u32::<LittleEndian>()?;

        // Verify we have enough bytes for the payload
        let total_size = WAL_HEADER_SIZE + length;
        if bytes.len() < total_size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "truncated WAL record: expected {} bytes, have {}",
                    total_size,
                    bytes.len()
                ),
            ));
        }

        // Extract and verify payload
        let payload_bytes = &bytes[WAL_HEADER_SIZE..total_size];
        let computed_crc = crc32fast::hash(payload_bytes);
        if computed_crc != expected_crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "WAL CRC mismatch: expected {:08X}, computed {:08X}",
                    expected_crc, computed_crc
                ),
            ));
        }

        // Deserialize the record
        let (record, _): (WalRecord, _) =
            bincode::serde::decode_from_slice(payload_bytes, bincode::config::standard())
                .map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("failed to deserialize WAL record: {}", e),
                    )
                })?;

        // Verify the record type matches what we read from the header
        if record.record_type != record_type {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WAL record type mismatch between header and payload",
            ));
        }

        Ok((record, total_size))
    }

    /// Write the record to a writer
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let bytes = self.encode_to_bytes();
        writer.write_all(&bytes)
    }

    /// Read a record from a reader
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        // Read header first
        let mut header = [0u8; WAL_HEADER_SIZE];
        reader.read_exact(&mut header)?;

        // Validate magic
        if &header[0..4] != WAL_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid WAL magic",
            ));
        }

        // Read length
        let mut cursor = &header[5..9];
        let length = cursor.read_u32::<LittleEndian>()? as usize;

        // Read full record (header + payload)
        let mut full_bytes = Vec::with_capacity(WAL_HEADER_SIZE + length);
        full_bytes.extend_from_slice(&header);
        full_bytes.resize(WAL_HEADER_SIZE + length, 0);
        reader.read_exact(&mut full_bytes[WAL_HEADER_SIZE..])?;

        // Decode
        let (record, _) = Self::decode_from_bytes(&full_bytes)?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_record_type_from_u8() {
        assert_eq!(WalRecordType::from_u8(1), Some(WalRecordType::Add));
        assert_eq!(WalRecordType::from_u8(2), Some(WalRecordType::Delete));
        assert_eq!(WalRecordType::from_u8(3), Some(WalRecordType::Checkpoint));
        assert_eq!(WalRecordType::from_u8(0), None);
        assert_eq!(WalRecordType::from_u8(4), None);
        assert_eq!(WalRecordType::from_u8(255), None);
    }

    #[test]
    fn roundtrip_add_record() {
        let record = WalRecord::add(
            "tenant_1".to_string(),
            "chunk_abc".to_string(),
            1700000000000,
            b"test payload data".to_vec(),
        );

        let encoded = record.encode_to_bytes();
        assert!(encoded.len() >= WAL_HEADER_SIZE);

        // Check magic
        assert_eq!(&encoded[0..4], WAL_MAGIC);
        // Check type
        assert_eq!(encoded[4], WalRecordType::Add as u8);

        let (decoded, bytes_consumed) = WalRecord::decode_from_bytes(&encoded).unwrap();
        assert_eq!(bytes_consumed, encoded.len());
        assert_eq!(decoded, record);
    }

    #[test]
    fn roundtrip_delete_record() {
        let record = WalRecord::delete(
            "tenant_2".to_string(),
            "chunk_xyz".to_string(),
            1700000001000,
        );

        let encoded = record.encode_to_bytes();
        assert_eq!(encoded[4], WalRecordType::Delete as u8);

        let (decoded, _) = WalRecord::decode_from_bytes(&encoded).unwrap();
        assert_eq!(decoded, record);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn roundtrip_checkpoint_record() {
        let record = WalRecord::checkpoint("tenant_3".to_string(), 1700000002000);

        let encoded = record.encode_to_bytes();
        assert_eq!(encoded[4], WalRecordType::Checkpoint as u8);

        let (decoded, _) = WalRecord::decode_from_bytes(&encoded).unwrap();
        assert_eq!(decoded, record);
        assert!(decoded.chunk_id.is_empty());
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn detect_corrupt_crc() {
        let record = WalRecord::add(
            "tenant_1".to_string(),
            "chunk_abc".to_string(),
            1700000000000,
            b"important data".to_vec(),
        );

        let mut encoded = record.encode_to_bytes();

        // Corrupt the payload (after the header)
        let payload_start = WAL_HEADER_SIZE;
        if encoded.len() > payload_start {
            encoded[payload_start] ^= 0xFF;
        }

        let result = WalRecord::decode_from_bytes(&encoded);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("CRC mismatch"));
    }

    #[test]
    fn detect_invalid_magic() {
        let mut bytes = vec![0u8; 20];
        bytes[0..4].copy_from_slice(b"XXXX");

        let result = WalRecord::decode_from_bytes(&bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid WAL magic"));
    }

    #[test]
    fn detect_truncated_header() {
        let bytes = vec![0u8; 10]; // Less than WAL_HEADER_SIZE
        let result = WalRecord::decode_from_bytes(&bytes);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn detect_truncated_payload() {
        let record = WalRecord::add(
            "tenant_1".to_string(),
            "chunk_abc".to_string(),
            1700000000000,
            b"some payload".to_vec(),
        );

        let encoded = record.encode_to_bytes();
        // Truncate the payload
        let truncated = &encoded[..encoded.len() - 5];

        let result = WalRecord::decode_from_bytes(truncated);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("truncated"));
    }

    #[test]
    fn detect_invalid_record_type() {
        let record = WalRecord::add(
            "tenant_1".to_string(),
            "chunk_abc".to_string(),
            1700000000000,
            vec![],
        );

        let mut encoded = record.encode_to_bytes();
        // Set invalid record type
        encoded[4] = 99;

        let result = WalRecord::decode_from_bytes(&encoded);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid WAL record type"));
    }

    #[test]
    fn write_and_read_from_stream() {
        let record = WalRecord::add(
            "tenant_1".to_string(),
            "chunk_abc".to_string(),
            1700000000000,
            b"stream payload".to_vec(),
        );

        let mut buffer = Vec::new();
        record.write_to(&mut buffer).unwrap();

        let mut cursor = buffer.as_slice();
        let decoded = WalRecord::read_from(&mut cursor).unwrap();
        assert_eq!(decoded, record);
    }

    #[test]
    fn header_size_constant() {
        assert_eq!(WAL_HEADER_SIZE, 13);
    }

    #[test]
    fn large_payload_roundtrip() {
        let large_payload = vec![0xAB; 100_000]; // 100KB payload
        let record = WalRecord::add(
            "tenant_large".to_string(),
            "chunk_big".to_string(),
            1700000000000,
            large_payload.clone(),
        );

        let encoded = record.encode_to_bytes();
        let (decoded, _) = WalRecord::decode_from_bytes(&encoded).unwrap();

        assert_eq!(decoded.payload, large_payload);
    }

    #[test]
    fn multiple_records_in_sequence() {
        let records = vec![
            WalRecord::add(
                "t1".to_string(),
                "c1".to_string(),
                1000,
                b"data1".to_vec(),
            ),
            WalRecord::delete("t1".to_string(), "c2".to_string(), 2000),
            WalRecord::checkpoint("t1".to_string(), 3000),
        ];

        // Encode all records into a single buffer
        let mut buffer = Vec::new();
        for record in &records {
            record.write_to(&mut buffer).unwrap();
        }

        // Decode all records back
        let mut offset = 0;
        for expected in &records {
            let (decoded, consumed) = WalRecord::decode_from_bytes(&buffer[offset..]).unwrap();
            assert_eq!(&decoded, expected);
            offset += consumed;
        }
        assert_eq!(offset, buffer.len());
    }
}
