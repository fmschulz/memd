# Phase 2: Persistent Cold Store - Research

**Researched:** 2026-01-29
**Domain:** Storage engine (segments, WAL, metadata, tombstones)
**Confidence:** HIGH

## Summary

Phase 2 transforms memd from an in-memory store to a crash-recoverable persistent storage system. The core challenge is implementing:

1. **Append-only segment files** with mmap reads for efficient cold tier access
2. **Write-ahead logging (WAL)** for crash recovery without corruption
3. **SQLite metadata store** with tenant isolation via indexes
4. **Tombstone tracking** using roaring bitmaps for soft deletes

The Rust ecosystem has mature crates for all components. The recommended approach uses `memmap2` for memory-mapped I/O, a custom simple WAL format (avoiding early-stage crates), `rusqlite` with WAL mode for metadata, and `roaring` for tombstone bitsets.

**Primary recommendation:** Build a custom segment format with explicit control over layout, use SQLite exclusively for metadata queries (not chunk payload), and implement a straightforward WAL with CRC-32 checksums for crash recovery.

## Standard Stack

The established libraries for this domain:

### Core

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| memmap2 | 0.9.x | Memory-mapped file I/O | De-facto Rust mmap library, actively maintained fork of memmap-rs |
| rusqlite | 0.38.x | SQLite bindings for metadata | Mature, well-documented, supports WAL mode natively |
| roaring | 0.11.x | Compressed bitmap for tombstones | Rust port of Roaring bitmap, efficient for sparse bitsets |
| crc32fast | 1.4.x | CRC-32 checksums for WAL | SIMD-accelerated, fast integrity checks |
| bincode | 2.x | Binary serialization | Fast, compact encoding for segment records |
| byteorder | 1.5.x | Endian-aware byte I/O | Low-level control for fixed-size record layouts |

### Supporting

| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| parking_lot | 0.12.x | RwLock for concurrent access | Better performance than std RwLock |
| tempfile | 3.x | Atomic file operations | Safe temp-file + rename pattern |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Custom WAL | okaywal crate | okaywal is early-stage ("do not use in production"), custom gives full control |
| Custom WAL | simple_wal crate | Less flexible, custom allows exact format needed |
| roaring | bitvec | bitvec is simpler but less space-efficient for sparse sets |
| bincode | rkyv | rkyv is faster but more complex; bincode sufficient for this use case |

**Installation:**
```bash
cargo add memmap2 rusqlite roaring crc32fast bincode byteorder parking_lot tempfile
cargo add rusqlite --features bundled  # bundles SQLite 3.51.1
```

## Architecture Patterns

### Recommended Project Structure
```
crates/memd/src/
├── store/
│   ├── mod.rs           # Store trait (existing)
│   ├── memory.rs        # In-memory store (existing, keep for tests)
│   ├── persistent.rs    # PersistentStore implementation
│   ├── segment/
│   │   ├── mod.rs       # Segment management
│   │   ├── writer.rs    # Segment writer (append-only)
│   │   ├── reader.rs    # Segment reader (mmap)
│   │   └── format.rs    # Segment file format definitions
│   ├── wal/
│   │   ├── mod.rs       # WAL interface
│   │   ├── writer.rs    # WAL writer with fsync
│   │   ├── reader.rs    # WAL reader for recovery
│   │   └── format.rs    # WAL record format
│   ├── metadata/
│   │   ├── mod.rs       # Metadata store interface
│   │   └── sqlite.rs    # SQLite implementation
│   └── tombstone.rs     # Tombstone bitset management
└── tenant.rs            # TenantManager (existing)
```

### Pattern 1: Segment File Format

**What:** Append-only segment files with separate payload, index, and embedding files.

**When to use:** Cold tier storage where data is written once and read many times via mmap.

**Layout:**
```
tenants/<tenant_id>/
  segments/
    seg_000001/
      payload.bin         # Concatenated chunk payloads (variable length)
      payload.idx         # Fixed-size index: (offset: u64, length: u32, crc: u32) per chunk
      emb_int8.bin        # Fixed-size embedding blocks [num_chunks x dim], 64-byte aligned
      meta                # Segment metadata (chunk count, created timestamp, status)
      tombstone.bin       # Roaring bitmap serialized
```

**Example:**
```rust
// payload.idx record format (16 bytes per chunk)
#[repr(C)]
pub struct PayloadIndexRecord {
    pub offset: u64,      // Offset in payload.bin
    pub length: u32,      // Length of payload in bytes
    pub crc32: u32,       // CRC-32 of payload for integrity
}

// Read chunk by ordinal with mmap
pub fn read_chunk(mmap: &Mmap, idx: &PayloadIndexRecord) -> Result<&[u8]> {
    let start = idx.offset as usize;
    let end = start + idx.length as usize;
    // Validate CRC before returning
    let data = &mmap[start..end];
    let computed_crc = crc32fast::hash(data);
    if computed_crc != idx.crc32 {
        return Err(CorruptionError::CrcMismatch);
    }
    Ok(data)
}
```

### Pattern 2: WAL Record Format

**What:** Simple append-only log with CRC-32 protected records for crash recovery.

**When to use:** Durability of write operations before segment commit.

**Format:**
```
+--------+--------+--------+--------+--------+--------+
| Magic  | Type   | Length |  CRC32 |   Payload       |
| (4B)   | (1B)   | (4B)   |  (4B)  |   (variable)    |
+--------+--------+--------+--------+--------+--------+
```

**Example:**
```rust
// WAL record types
#[repr(u8)]
pub enum WalRecordType {
    Add = 1,
    Delete = 2,
    Checkpoint = 3,  // Marks segment flush complete
}

#[derive(Serialize, Deserialize)]
pub struct WalRecord {
    pub record_type: WalRecordType,
    pub tenant_id: String,
    pub chunk_id: String,
    pub timestamp: i64,
    pub payload: Vec<u8>,  // Serialized chunk data for Add, empty for Delete
}

// Write with durability
pub fn write_wal_record(file: &mut File, record: &WalRecord) -> Result<()> {
    let payload = bincode::encode_to_vec(record, bincode::config::standard())?;
    let crc = crc32fast::hash(&payload);

    // Write header
    file.write_all(b"MWAL")?;  // Magic
    file.write_all(&[record.record_type as u8])?;
    file.write_all(&(payload.len() as u32).to_le_bytes())?;
    file.write_all(&crc.to_le_bytes())?;
    file.write_all(&payload)?;

    // Ensure durability
    file.sync_all()?;

    Ok(())
}
```

### Pattern 3: SQLite Metadata with Tenant Isolation

**What:** SQLite with WAL mode storing chunk metadata, indexed by tenant_id.

**When to use:** All metadata queries that need filtering, ordering, or joins.

**Example:**
```rust
use rusqlite::{Connection, params};

pub fn init_metadata_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;

    // Enable WAL mode for crash safety + concurrent readers
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;

    // Create chunks table with tenant isolation index
    conn.execute(
        "CREATE TABLE IF NOT EXISTS chunks (
            chunk_id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL,
            project_id TEXT,
            segment_id INTEGER NOT NULL,
            ordinal INTEGER NOT NULL,
            chunk_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'final',
            timestamp_created INTEGER NOT NULL,
            hash TEXT NOT NULL,
            source_uri TEXT,
            UNIQUE(segment_id, ordinal)
        )",
        [],
    )?;

    // Critical: tenant_id index for isolation queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_chunks_tenant
         ON chunks(tenant_id, status)",
        [],
    )?;

    // Secondary indexes
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_chunks_tenant_type
         ON chunks(tenant_id, chunk_type, timestamp_created DESC)",
        [],
    )?;

    Ok(conn)
}

// ALWAYS filter by tenant_id - never skip this
pub fn get_chunks_for_tenant(
    conn: &Connection,
    tenant_id: &str,
    limit: usize,
) -> Result<Vec<ChunkMetadata>> {
    let mut stmt = conn.prepare(
        "SELECT chunk_id, segment_id, ordinal, chunk_type, status, timestamp_created
         FROM chunks
         WHERE tenant_id = ?1 AND status != 'deleted'
         ORDER BY timestamp_created DESC
         LIMIT ?2"
    )?;

    // tenant_id is ALWAYS first parameter
    let rows = stmt.query_map(params![tenant_id, limit as i64], |row| {
        Ok(ChunkMetadata {
            chunk_id: row.get(0)?,
            segment_id: row.get(1)?,
            ordinal: row.get(2)?,
            chunk_type: row.get(3)?,
            status: row.get(4)?,
            timestamp_created: row.get(5)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>()
}
```

### Pattern 4: Tombstone Bitset with Roaring Bitmap

**What:** Space-efficient tracking of soft-deleted chunks per segment.

**When to use:** Tracking deletions without rewriting segment files.

**Example:**
```rust
use roaring::RoaringBitmap;
use std::fs::File;
use std::io::{Read, Write};

pub struct TombstoneSet {
    bitmap: RoaringBitmap,
    path: PathBuf,
}

impl TombstoneSet {
    pub fn load_or_create(path: PathBuf) -> Result<Self> {
        let bitmap = if path.exists() {
            let mut file = File::open(&path)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            RoaringBitmap::deserialize_from(&bytes[..])?
        } else {
            RoaringBitmap::new()
        };
        Ok(Self { bitmap, path })
    }

    pub fn mark_deleted(&mut self, ordinal: u32) -> Result<()> {
        self.bitmap.insert(ordinal);
        self.persist()
    }

    pub fn is_deleted(&self, ordinal: u32) -> bool {
        self.bitmap.contains(ordinal)
    }

    pub fn deleted_count(&self) -> u64 {
        self.bitmap.len()
    }

    fn persist(&self) -> Result<()> {
        let mut bytes = Vec::new();
        self.bitmap.serialize_into(&mut bytes)?;

        // Atomic write via temp file + rename
        let temp_path = self.path.with_extension("tmp");
        let mut file = File::create(&temp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp_path, &self.path)?;

        Ok(())
    }
}
```

### Anti-Patterns to Avoid

- **Storing chunk payloads in SQLite:** SQLite is for metadata queries, not blob storage. Keep payloads in segment files with mmap.
- **Single database file for all tenants:** Creates write contention. Use per-tenant SQLite databases for better isolation and concurrency.
- **Skipping fsync on WAL writes:** Data loss on crash. Always call `sync_all()` after WAL records.
- **Not filtering by tenant_id:** Every query must include tenant_id in WHERE clause. Add it as first parameter always.
- **Using MmapMut for segment files:** Segments are append-only; use regular file writes, then mmap for reads.

## Don't Hand-Roll

Problems that look simple but have existing solutions:

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| CRC checksums | Custom CRC impl | crc32fast | SIMD-accelerated, battle-tested |
| Compressed bitsets | Custom bitset | roaring | Space-efficient, fast set operations |
| Binary serialization | Manual byte packing | bincode | Handles versioning, endianness |
| Temp file + rename | Manual temp files | tempfile | Handles cleanup, security |
| Memory mapping | Raw mmap syscalls | memmap2 | Cross-platform, safe API |
| SQLite bindings | FFI to sqlite3 | rusqlite | Ergonomic Rust API |

**Key insight:** The Rust ecosystem has mature, production-tested crates for all low-level storage primitives. Focus engineering effort on the segment layout and recovery logic, not on reimplementing checksums or bitmaps.

## Common Pitfalls

### Pitfall 1: Incomplete fsync for Durability

**What goes wrong:** Data appears written but lost on crash because kernel buffers weren't flushed.

**Why it happens:** Rust's `std::fs::write` does NOT call fsync. File close also doesn't fsync.

**How to avoid:**
```rust
// CORRECT: Explicit sync_all after writes
file.write_all(&data)?;
file.sync_all()?;  // Forces write to disk

// For atomic writes, also sync directory
fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let temp = path.with_extension("tmp");
    let mut file = File::create(&temp)?;
    file.write_all(data)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)?;

    // Sync parent directory for rename durability
    let dir = File::open(path.parent().unwrap())?;
    dir.sync_all()?;
    Ok(())
}
```

**Warning signs:** Tests pass but production loses data; intermittent "file not found" after restart.

### Pitfall 2: SQLite SQLITE_BUSY Errors

**What goes wrong:** Write operations fail with "database is locked" under concurrent load.

**Why it happens:** SQLite allows only one writer at a time; default busy timeout is 0.

**How to avoid:**
```rust
// Set busy timeout (milliseconds)
conn.pragma_update(None, "busy_timeout", 5000)?;

// Use WAL mode for better concurrency
conn.pragma_update(None, "journal_mode", "WAL")?;

// Single writer connection, multiple reader connections
pub struct MetadataStore {
    writer: Mutex<Connection>,      // Single writer
    readers: Pool<Connection>,      // Pool of readers
}
```

**Warning signs:** Sporadic write failures; errors correlate with concurrent activity.

### Pitfall 3: mmap Safety with File Modifications

**What goes wrong:** SIGBUS crash when reading mmap'd file that was truncated or modified.

**Why it happens:** mmap creates a view into the file; if file shrinks, reads beyond new end crash.

**How to avoid:**
```rust
// Segments are append-only - NEVER truncate or modify in place
// Open mmap only on completed, immutable segment files
unsafe {
    // Safe because: segment is complete and will never be modified
    let mmap = MmapOptions::new()
        .len(file_len as usize)  // Explicit length
        .map(&file)?;
}

// For active segments, use regular file I/O until complete
```

**Warning signs:** Random SIGBUS crashes; crashes correlate with segment operations.

### Pitfall 4: WAL Recovery Ordering

**What goes wrong:** Duplicate chunks or missing chunks after crash recovery.

**Why it happens:** WAL replay applied to already-committed data, or checkpoint marker lost.

**How to avoid:**
```rust
// WAL recovery sequence:
// 1. Find last valid checkpoint record
// 2. Replay only records AFTER that checkpoint
// 3. Apply records in order, skip already-present chunk_ids

pub fn recover_from_wal(wal_path: &Path, store: &mut Store) -> Result<()> {
    let records = read_valid_wal_records(wal_path)?;

    // Find last checkpoint
    let last_checkpoint_idx = records.iter()
        .rposition(|r| r.record_type == WalRecordType::Checkpoint)
        .unwrap_or(0);

    // Replay only records after checkpoint
    for record in &records[last_checkpoint_idx..] {
        match record.record_type {
            WalRecordType::Add => {
                // Skip if chunk already exists (idempotent)
                if !store.has_chunk(&record.chunk_id)? {
                    store.add_from_wal(record)?;
                }
            }
            WalRecordType::Delete => {
                store.mark_deleted(&record.chunk_id)?;
            }
            WalRecordType::Checkpoint => {
                // Already handled, skip
            }
        }
    }

    Ok(())
}
```

**Warning signs:** Duplicate data after restart; missing recent writes.

### Pitfall 5: Tenant Isolation Leakage

**What goes wrong:** Tenant A sees Tenant B's data.

**Why it happens:** Query missing tenant_id filter; index scan bypasses WHERE clause.

**How to avoid:**
```rust
// EVERY query function takes tenant_id as FIRST parameter
// and includes it in WHERE clause

// BAD: Easy to forget tenant_id
fn get_chunk(chunk_id: &str) -> Result<Chunk>  // NO!

// GOOD: tenant_id is required, first parameter
fn get_chunk(tenant_id: &str, chunk_id: &str) -> Result<Option<Chunk>>

// Query template - tenant_id always first in WHERE
const QUERY: &str = "SELECT ... FROM chunks WHERE tenant_id = ?1 AND ...";

// Unit test: verify isolation
#[test]
fn test_tenant_isolation() {
    let store = setup();
    store.add("tenant_a", make_chunk("secret")).unwrap();

    // Tenant B must NOT see Tenant A's data
    let result = store.get("tenant_b", &chunk_id);
    assert!(result.unwrap().is_none());

    let search = store.search("tenant_b", "secret", 100);
    assert!(search.unwrap().is_empty());
}
```

**Warning signs:** Integration tests pass individually but fail when run together; data appearing in wrong tenant.

## Code Examples

Verified patterns from official sources and best practices:

### Complete Segment Write + Read Cycle
```rust
// Source: Based on commitlog and memmap2 patterns

use memmap2::{Mmap, MmapOptions};
use std::fs::{File, OpenOptions};
use std::io::{Write, Seek, SeekFrom};
use std::path::Path;

pub struct Segment {
    pub id: u64,
    pub dir: PathBuf,
    payload_file: File,
    payload_len: u64,
    index_records: Vec<PayloadIndexRecord>,
}

impl Segment {
    pub fn create(dir: PathBuf, id: u64) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;

        let payload_path = dir.join("payload.bin");
        let payload_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&payload_path)?;

        Ok(Self {
            id,
            dir,
            payload_file,
            payload_len: 0,
            index_records: Vec::new(),
        })
    }

    pub fn append_chunk(&mut self, data: &[u8]) -> Result<u32> {
        let ordinal = self.index_records.len() as u32;
        let offset = self.payload_len;
        let length = data.len() as u32;
        let crc = crc32fast::hash(data);

        // Write payload
        self.payload_file.write_all(data)?;
        self.payload_len += length as u64;

        // Record index entry (write to file later)
        self.index_records.push(PayloadIndexRecord {
            offset,
            length,
            crc32: crc,
        });

        Ok(ordinal)
    }

    pub fn finalize(mut self) -> Result<FinalizedSegment> {
        // Sync payload file
        self.payload_file.sync_all()?;

        // Write index file
        let index_path = self.dir.join("payload.idx");
        let mut index_file = File::create(&index_path)?;
        for record in &self.index_records {
            index_file.write_all(&record.offset.to_le_bytes())?;
            index_file.write_all(&record.length.to_le_bytes())?;
            index_file.write_all(&record.crc32.to_le_bytes())?;
        }
        index_file.sync_all()?;

        // Write metadata
        let meta_path = self.dir.join("meta");
        let meta = SegmentMeta {
            chunk_count: self.index_records.len() as u32,
            finalized: true,
        };
        std::fs::write(&meta_path, bincode::encode_to_vec(&meta, bincode::config::standard())?)?;

        // Open for reading
        FinalizedSegment::open(self.dir)
    }
}

pub struct FinalizedSegment {
    pub id: u64,
    payload_mmap: Mmap,
    index: Vec<PayloadIndexRecord>,
    tombstones: TombstoneSet,
}

impl FinalizedSegment {
    pub fn open(dir: PathBuf) -> Result<Self> {
        let payload_file = File::open(dir.join("payload.bin"))?;
        let payload_mmap = unsafe { MmapOptions::new().map(&payload_file)? };

        // Load index
        let index_bytes = std::fs::read(dir.join("payload.idx"))?;
        let index = parse_index(&index_bytes)?;

        // Load tombstones
        let tombstones = TombstoneSet::load_or_create(dir.join("tombstone.bin"))?;

        Ok(Self {
            id: extract_segment_id(&dir)?,
            payload_mmap,
            index,
            tombstones,
        })
    }

    pub fn read_chunk(&self, ordinal: u32) -> Result<Option<&[u8]>> {
        if self.tombstones.is_deleted(ordinal) {
            return Ok(None);
        }

        let record = &self.index[ordinal as usize];
        let start = record.offset as usize;
        let end = start + record.length as usize;
        let data = &self.payload_mmap[start..end];

        // Verify integrity
        if crc32fast::hash(data) != record.crc32 {
            return Err(Error::Corruption("CRC mismatch"));
        }

        Ok(Some(data))
    }
}
```

### SQLite Connection Configuration
```rust
// Source: rusqlite docs + SQLite PRAGMA reference

use rusqlite::{Connection, OpenFlags};

pub fn open_metadata_db(path: &Path, read_only: bool) -> Result<Connection> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };

    let conn = Connection::open_with_flags(path, flags)?;

    // Performance + durability configuration
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "cache_size", -64000)?;  // 64MB cache
    conn.pragma_update(None, "temp_store", "memory")?;

    // Foreign keys for referential integrity
    conn.pragma_update(None, "foreign_keys", "ON")?;

    Ok(conn)
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| memmap (unmaintained) | memmap2 | 2020 | memmap2 is actively maintained fork |
| SQLite rollback journal | SQLite WAL mode | 2010 (SQLite 3.7) | Better concurrent read/write performance |
| Custom bitmap | Roaring bitmap | 2014+ | 10-100x space savings for sparse sets |
| Manual byte packing | bincode 2.x | 2023 | Better config API, no_std support |

**Deprecated/outdated:**
- `memmap` crate: Unmaintained since 2018, use `memmap2` instead
- SQLite DELETE journal mode: WAL mode is now standard for concurrent access
- Custom bitmap implementations: Roaring is the industry standard

## Open Questions

Things that couldn't be fully resolved:

1. **Segment size threshold**
   - What we know: Segments should be large enough to amortize overhead, small enough for efficient compaction
   - What's unclear: Optimal segment size depends on workload (64MB? 256MB?)
   - Recommendation: Start with 64MB, make configurable, tune based on benchmarks

2. **Per-tenant vs shared SQLite database**
   - What we know: Per-tenant avoids write contention; shared is simpler to manage
   - What's unclear: At what tenant count does per-tenant become necessary?
   - Recommendation: Start with per-tenant (aligns with existing tenant directory structure)

3. **WAL checkpoint frequency**
   - What we know: More frequent = smaller WAL, less to replay on crash
   - What's unclear: Performance impact of frequent checkpoints
   - Recommendation: Checkpoint after every N chunks added (default 100) or time threshold (1 minute)

## Sources

### Primary (HIGH confidence)
- [memmap2 docs.rs](https://docs.rs/memmap2) - Memory-mapped I/O API
- [rusqlite docs.rs](https://docs.rs/rusqlite) - SQLite bindings API
- [roaring-rs GitHub](https://github.com/RoaringBitmap/roaring-rs) - Roaring bitmap implementation
- [SQLite Atomic Commit](https://sqlite.org/atomiccommit.html) - Crash recovery guarantees
- [SQLite WAL Mode](https://sqlite.org/wal.html) - Write-ahead logging in SQLite

### Secondary (MEDIUM confidence)
- [Building an Append-only Log](https://eileen-code4fun.medium.com/building-an-append-only-log-from-scratch-e8712b49c924) - Segment design patterns
- [WAL Tutorial by Adam Comer](https://adambcomer.com/blog/simple-database/wal/) - WAL format design
- [SQLite Performance Tuning](https://phiresky.github.io/blog/2020/sqlite-performance-tuning/) - PRAGMA recommendations
- [Segmented Log in Rust](https://arindas.github.io/blog/segmented-log-rust/) - Implementation patterns

### Tertiary (LOW confidence)
- okaywal crate - Interesting but "early in development, do not use in production"
- commitlog crate - Good reference but designed for different use case (distributed systems)

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - All recommended crates are mature, widely used, well-documented
- Architecture: HIGH - Segment + WAL + SQLite pattern is well-established in storage engines
- Pitfalls: HIGH - Based on documented issues in SQLite and Rust I/O, verified with official sources

**Research date:** 2026-01-29
**Valid until:** 60 days (stable domain, libraries change slowly)
