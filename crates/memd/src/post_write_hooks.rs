//! Post-write hooks fired by operation handlers after store writes succeed.
//!
//! The CLI operation dispatcher runs structural indexing for newly-written
//! file-backed chunks. Handlers return events instead of calling structural
//! indexing directly.
//!
//! Today there are two distinct shapes for how that contract is
//! honoured:
//!
//! - `memory.supersede` and `memory.import_omf` build a
//!   [`PostWriteEvent`] (single or vector) and return it alongside the
//!   operation response.
//! - `memory.add` and `memory.add_batch` pass the same fields directly
//!   because their caller already holds every field before consuming the
//!   `AddParams` / `AddBatchParams`.
//!
//! Both shapes are valid and the hook side effects are identical; the
//! struct's job is to carry the fields across the handler/server
//! boundary when the handler has already moved them out of the
//! incoming params. Widening `memory.add` / `memory.add_batch` to use the
//! struct would be a consistency improvement only.
//!
//! The struct was originally inlined in the historical MCP handler module.
//! Once `memory.supersede` and `memory.import_omf` both returned the
//! type, the inline placement became the wrong default home. Moved
//! here with the [`ImportedChunk`] adapter so OMF-specific code stays in
//! [`crate::omf::import`] and operation-dispatch code stays under
//! [`crate::ops`].
//!
//! [`ImportedChunk`]: crate::omf::import::ImportedChunk

use crate::omf::import::ImportedChunk;
use crate::types::ChunkId;

/// Per-write event emitted by any handler that creates or updates a
/// chunk payload. The operation dispatcher consumes these to run structural
/// indexing for the new chunk.
///
/// Keep this type small and field-public; adding new consumers is
/// cheap and layering handlers through a single event shape is the
/// whole point of this module.
#[derive(Debug, Clone)]
pub struct PostWriteEvent {
    pub tenant_id: String,
    pub chunk_id: ChunkId,
    pub chunk_type: String,
    pub project_id: Option<String>,
    pub source_path: Option<String>,
    pub text: String,
}

impl PostWriteEvent {
    /// Build a `PostWriteEvent` from an OMF [`ImportedChunk`].
    ///
    /// OMF imports carry no filesystem `source_path` — neither nanomem
    /// nor memd's own export emits one — so the field is always
    /// `None`. The structural indexer short-circuits on
    /// `source_path = None`, which is the correct signal for
    /// "this write is not a file-backed chunk."
    ///
    /// [`ImportedChunk`]: crate::omf::import::ImportedChunk
    pub fn from_imported_chunk(ic: ImportedChunk, tenant_id: &str) -> Self {
        Self {
            tenant_id: tenant_id.to_string(),
            chunk_id: ic.chunk_id,
            chunk_type: ic.chunk_type.to_string(),
            project_id: ic.project_id,
            source_path: None,
            text: ic.text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChunkType;

    #[test]
    fn from_imported_chunk_preserves_fields_and_drops_source_path() {
        let chunk_id = ChunkId::new();
        let ic = ImportedChunk {
            chunk_id: chunk_id.clone(),
            chunk_type: ChunkType::Code,
            project_id: Some("proj_a".to_string()),
            text: "fn hello() {}".to_string(),
        };
        let event = PostWriteEvent::from_imported_chunk(ic, "tenant_x");
        assert_eq!(event.tenant_id, "tenant_x");
        assert_eq!(event.chunk_id, chunk_id);
        assert_eq!(event.chunk_type, "code");
        assert_eq!(event.project_id.as_deref(), Some("proj_a"));
        assert_eq!(event.source_path, None);
        assert_eq!(event.text, "fn hello() {}");
    }

    #[test]
    fn from_imported_chunk_preserves_null_project_id() {
        let chunk_id = ChunkId::new();
        let ic = ImportedChunk {
            chunk_id: chunk_id.clone(),
            chunk_type: ChunkType::Doc,
            project_id: None,
            text: "anything".to_string(),
        };
        let event = PostWriteEvent::from_imported_chunk(ic, "tenant_y");
        assert_eq!(event.tenant_id, "tenant_y");
        assert_eq!(event.chunk_id, chunk_id);
        assert_eq!(event.project_id, None);
        assert_eq!(event.chunk_type, "doc");
        assert_eq!(event.source_path, None);
        assert_eq!(event.text, "anything");
    }

    /// Regression — the Item 6 refactor must preserve the pre-refactor
    /// public path `memd::mcp::handlers::PostWriteEvent` that the struct
    /// used to live at. A downstream caller that wrote
    /// `use memd::mcp::handlers::PostWriteEvent;` must keep compiling.
    ///
    /// This coerces a value through both paths; if the re-export at
    /// `crate::mcp::handlers::PostWriteEvent` were dropped or pointed at
    /// a different type, the function signature would stop type-checking.
    #[allow(dead_code)]
    fn _legacy_handlers_path_is_same_type(
        e: crate::mcp::handlers::PostWriteEvent,
    ) -> crate::post_write_hooks::PostWriteEvent {
        e
    }

    /// Also pin the flattened re-export at `memd::mcp::PostWriteEvent`.
    #[allow(dead_code)]
    fn _flat_mcp_path_is_same_type(
        e: crate::mcp::PostWriteEvent,
    ) -> crate::post_write_hooks::PostWriteEvent {
        e
    }
}
