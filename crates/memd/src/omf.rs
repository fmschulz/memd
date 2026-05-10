//! Versioned memd OMF (Open Memory Format) envelope.

use serde::{Deserialize, Serialize};

use crate::types::{ChunkId, ChunkStatus, IngestionMode, LifecycleMetadata, MemoryChunk};

pub const OMF_FORMAT: &str = "memd.omf";
pub const OMF_VERSION: u32 = 1;
pub const INGESTION_MODE_TAG_PREFIX: &str = "ingestion_mode:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmfEnvelope {
    pub format: String,
    pub version: u32,
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    pub exported_at_ms: i64,
    pub chunks: Vec<OmfChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmfChunk {
    pub chunk_id: ChunkId,
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    pub text: String,
    #[serde(rename = "type")]
    pub chunk_type: String,
    pub timestamp_created: i64,
    pub status: ChunkStatus,
    pub lifecycle: LifecycleMetadata,
    pub ingestion_mode: IngestionMode,
    pub chunk: MemoryChunk,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub canonical_text: Option<String>,
}

pub fn ingestion_mode_from_tags(tags: &[String]) -> IngestionMode {
    tags.iter()
        .find_map(|tag| tag.strip_prefix(INGESTION_MODE_TAG_PREFIX))
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_default()
}

pub fn normalize_ingestion_mode_tag(tags: &mut Vec<String>, mode: IngestionMode) {
    tags.retain(|tag| !tag.starts_with(INGESTION_MODE_TAG_PREFIX));
    tags.push(format!("{INGESTION_MODE_TAG_PREFIX}{mode}"));
}
