//! OMF 1.0 export — Task F2.
//!
//! `export_omf` walks `list_for_export` on the metadata side, reads each
//! payload via `PersistentStore::get`, and emits an `OmfDocument` whose
//! per-item `extensions.memd` namespace round-trips enough state for a
//! memd ↔ memd import to preserve lifecycle overlay (tier, supersession
//! edges, review/expiry windows, timestamps) when the importer chooses
//! to honour it (F3 trust gate).
//!
//! Non-memd importers ignore `extensions.memd` entirely per OMF 1.0:
//! extensions are defined as per-app scratch space.

use serde_json::json;
use tracing::debug;

use crate::error::Result;
use crate::store::metadata::{ChunkMetadata, MetadataStore};
use crate::store::persistent::PersistentStore;
use crate::types::lifecycle::MemoryTier;
use crate::types::{ChunkStatus, MemoryChunk, TenantId};

use super::time::{format_date_ms, now_utc_ms, now_utc_rfc3339};
use super::{OmfDocument, OmfItem, OmfSource, MEMD_EXT_VERSION, MEMD_SOURCE_APP, OMF_VERSION};

/// Export options.
///
/// Defaults mirror nanomem's semantic-merge assumption: include everything
/// that has content (superseded/expired rows still carry real text), but
/// exclude history-tier rows unless explicitly asked. Callers who want a
/// narrower export (live rows only, current project only) pass the
/// corresponding flags.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub project_id: Option<String>,
    pub include_history: bool,
    pub include_superseded: bool,
    pub include_expired: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            project_id: None,
            include_history: false,
            include_superseded: true,
            include_expired: true,
        }
    }
}

/// Export a tenant's memory as an OMF 1.0 document.
///
/// Order: stable ascending `timestamp_created` (matches `list_for_export`).
/// Metadata errors propagate. Unreadable segment payloads are skipped with
/// the same warning path used by retrieval, so one stale metadata row does not
/// poison full-tenant archive/export operations.
pub async fn export_omf(
    store: &PersistentStore,
    tenant_id: &TenantId,
    opts: ExportOptions,
) -> Result<OmfDocument> {
    let rows = store.metadata().list_for_export(
        tenant_id,
        opts.project_id.as_deref(),
        opts.include_history,
    )?;

    let now_ms = now_utc_ms();
    let mut memories = Vec::with_capacity(rows.len());
    for meta in rows {
        if matches!(
            meta.status,
            ChunkStatus::Candidate | ChunkStatus::Deleted | ChunkStatus::Error
        ) {
            continue;
        }
        if !opts.include_superseded && meta.status == ChunkStatus::Superseded {
            continue;
        }
        // Lazy-expiry clock check: a row whose sweep hasn't run yet may
        // still be `status=Final` with an `expires_at_ms` in the past.
        // Mirror `VisibilityPolicy::is_visible_at` here so a caller
        // asking for "live only" can't observe a lazily-expired row
        // leak into the export.
        if !opts.include_expired {
            if meta.status == ChunkStatus::Expired {
                continue;
            }
            if let Some(exp) = meta.lifecycle.expires_at_ms {
                if exp <= now_ms {
                    continue;
                }
            }
        }
        // History-tier rows are SQL-filtered by `list_for_export` unless
        // `include_history` is set, so this guard is defence-in-depth
        // against a future list helper that widens the projection.
        if !opts.include_history && meta.lifecycle.tier == MemoryTier::History {
            continue;
        }

        let chunk = match store
            .get_chunk_for_retrieval(tenant_id, &meta.chunk_id, "export_omf")
            .await?
        {
            Some(c) => c,
            None => {
                // list_for_export just surfaced this row; a None here
                // means the payload is missing for a metadata-present
                // chunk. That is narrow (compaction race, or real
                // metadata/payload drift). Log at debug so the row
                // isn't lost to silence, then move on — returning an
                // error here would poison full-tenant exports for a
                // single rogue chunk.
                debug!(
                    tenant_id = %tenant_id,
                    chunk_id = %meta.chunk_id,
                    "export_omf: metadata row has no payload, skipping"
                );
                continue;
            }
        };
        memories.push(to_omf_item(&chunk, &meta));
    }

    Ok(OmfDocument {
        omf: OMF_VERSION.to_string(),
        exported_at: now_utc_rfc3339(),
        source: Some(OmfSource {
            app: MEMD_SOURCE_APP.to_string(),
        }),
        memories,
    })
}

/// Convert one `(chunk, metadata)` pair to an `OmfItem`.
///
/// Public so tests in sibling modules can build items from fixtures
/// without round-tripping through a PersistentStore.
pub fn to_omf_item(chunk: &MemoryChunk, meta: &ChunkMetadata) -> OmfItem {
    // Top-level `status` carries OMF-defined archival hints (nanomem uses
    // it for soft-archive). We emit values only for the non-Final states
    // that map cleanly to OMF vocabulary; Final is the implicit default.
    let status = match meta.status {
        ChunkStatus::Superseded => Some("superseded".to_string()),
        ChunkStatus::Expired => Some("expired".to_string()),
        _ => None,
    };

    let created_at = format_date_ms(chunk.timestamp_created);
    let updated_at = if meta.lifecycle.lifecycle_updated_at_ms > 0 {
        format_date_ms(meta.lifecycle.lifecycle_updated_at_ms)
    } else {
        None
    };
    let expires_at = meta.lifecycle.expires_at_ms.and_then(format_date_ms);

    let lifecycle_ext = json!({
        "status": meta.status.to_string(),
        "tier": meta.lifecycle.tier.to_string(),
        "supersedes": meta.lifecycle.supersedes.as_ref().map(|c| c.to_string()),
        "superseded_by": meta.lifecycle.superseded_by.as_ref().map(|c| c.to_string()),
        "expires_at_ms": meta.lifecycle.expires_at_ms,
        "review_after_ms": meta.lifecycle.review_after_ms,
        "lifecycle_updated_at_ms": meta.lifecycle.lifecycle_updated_at_ms,
    });

    let extensions = json!({
        "memd": {
            "v": MEMD_EXT_VERSION,
            "chunk_id": chunk.chunk_id.to_string(),
            "project_id": meta.project_id,
            "chunk_type": chunk.chunk_type.to_string(),
            "ingestion_mode": chunk.ingestion_mode.to_string(),
            "lifecycle": lifecycle_ext,
        }
    });

    OmfItem {
        content: chunk.text.clone(),
        category: None,
        tags: chunk.tags.clone(),
        status,
        created_at,
        updated_at,
        expires_at,
        extensions,
    }
}
