//! SQLite metadata store implementation
//!
//! Full implementation in 02-03 plan. This is a stub to enable compilation.

use crate::error::Result;
use crate::types::{ChunkId, TenantId};

use super::{ChunkMetadata, MetadataStore};

/// SQLite-backed metadata store
pub struct SqliteMetadataStore {
    _placeholder: (),
}

impl SqliteMetadataStore {
    /// Create a new SQLite metadata store (stub)
    pub fn open(_path: std::path::PathBuf) -> Result<Self> {
        Ok(Self { _placeholder: () })
    }
}

impl MetadataStore for SqliteMetadataStore {
    fn insert(&self, _metadata: &ChunkMetadata) -> Result<()> {
        unimplemented!("SQLite implementation in 02-03")
    }

    fn get(&self, _tenant_id: &TenantId, _chunk_id: &ChunkId) -> Result<Option<ChunkMetadata>> {
        unimplemented!("SQLite implementation in 02-03")
    }

    fn list(
        &self,
        _tenant_id: &TenantId,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<ChunkMetadata>> {
        unimplemented!("SQLite implementation in 02-03")
    }

    fn mark_deleted(&self, _tenant_id: &TenantId, _chunk_id: &ChunkId) -> Result<bool> {
        unimplemented!("SQLite implementation in 02-03")
    }

    fn get_by_segment(&self, _segment_id: u64) -> Result<Vec<ChunkMetadata>> {
        unimplemented!("SQLite implementation in 02-03")
    }

    fn count_by_status(&self, _tenant_id: &TenantId) -> Result<(usize, usize)> {
        unimplemented!("SQLite implementation in 02-03")
    }
}
