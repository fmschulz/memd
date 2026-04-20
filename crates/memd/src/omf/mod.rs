//! OMF 1.0 (Open Memory Format) — JSON interchange types and top-level
//! validator.
//!
//! This module currently covers Task F1 of the nanomem-inspired features
//! plan: the wire envelope (`omf`, `exported_at`, `source`, `memories`)
//! plus a placeholder `extensions` field per item. Export (F2), import
//! with trust-gated lifecycle (F3), and preview (F4) land as separate
//! submodules in later tasks and are not yet present on the module tree.
//!
//! Wire shape is nanomem-compatible; memd-specific state will round-trip
//! through a versioned `extensions.memd` namespace once F2/F3 land.

use serde::{Deserialize, Serialize};

use crate::error::{MemdError, Result};

/// OMF wire version that this build produces and accepts.
pub const OMF_VERSION: &str = "1.0";

/// `extensions.memd.v` value that this build produces and trusts on import.
pub const MEMD_EXT_VERSION: u32 = 1;

/// `source.app` value that this build writes when exporting.
pub const MEMD_SOURCE_APP: &str = "memd";

/// Top-level OMF 1.0 envelope.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmfDocument {
    pub omf: String,
    pub exported_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<OmfSource>,
    pub memories: Vec<OmfItem>,
}

/// Producer identity. `app == "memd"` combined with a matching
/// `extensions.memd.v` is the trust predicate on import.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmfSource {
    pub app: String,
}

/// One memory item in an OMF document. `content` is the only required
/// field per OMF 1.0; everything else is optional and per-app.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmfItem {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extensions: serde_json::Value,
}

/// Validate the top-level document shape.
///
/// Rejects unsupported wire versions and memory items with empty/whitespace-only
/// content. Runs in O(memories) time and allocates only for the error path.
pub fn validate_omf(doc: &OmfDocument) -> Result<()> {
    if doc.omf != OMF_VERSION {
        return Err(MemdError::ValidationError(format!(
            "unsupported omf version: {} (expected {OMF_VERSION})",
            doc.omf
        )));
    }
    for (i, m) in doc.memories.iter().enumerate() {
        if m.content.trim().is_empty() {
            return Err(MemdError::ValidationError(format!(
                "memories[{i}].content is empty"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_doc() -> OmfDocument {
        OmfDocument {
            omf: OMF_VERSION.into(),
            exported_at: "2026-04-18T00:00:00Z".into(),
            source: Some(OmfSource {
                app: MEMD_SOURCE_APP.into(),
            }),
            memories: vec![OmfItem {
                content: "hi".into(),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn omf_document_validates_version_and_content() {
        validate_omf(&base_doc()).unwrap();

        let bad_version = OmfDocument {
            omf: "9.9".into(),
            ..base_doc()
        };
        assert!(matches!(
            validate_omf(&bad_version),
            Err(MemdError::ValidationError(_))
        ));

        let empty_content = OmfDocument {
            memories: vec![OmfItem {
                content: "".into(),
                ..Default::default()
            }],
            ..base_doc()
        };
        assert!(matches!(
            validate_omf(&empty_content),
            Err(MemdError::ValidationError(_))
        ));

        let whitespace_only = OmfDocument {
            memories: vec![OmfItem {
                content: "   \n\t".into(),
                ..Default::default()
            }],
            ..base_doc()
        };
        assert!(matches!(
            validate_omf(&whitespace_only),
            Err(MemdError::ValidationError(_))
        ));
    }

    #[test]
    fn omf_document_roundtrips_through_serde_json() {
        let doc = base_doc();
        let s = serde_json::to_string(&doc).unwrap();
        let back: OmfDocument = serde_json::from_str(&s).unwrap();
        assert_eq!(back.omf, OMF_VERSION);
        assert_eq!(back.memories.len(), 1);
        assert_eq!(back.memories[0].content, "hi");
        assert_eq!(back.source.as_ref().unwrap().app, MEMD_SOURCE_APP);
    }

    #[test]
    fn omf_item_skips_empty_collections_when_serialized() {
        let m = OmfItem {
            content: "x".into(),
            ..Default::default()
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains("tags"), "empty tags should be skipped: {s}");
        assert!(
            !s.contains("extensions"),
            "null extensions should be skipped: {s}"
        );
    }

    #[test]
    fn memd_ext_version_constant_is_stable() {
        assert_eq!(MEMD_EXT_VERSION, 1);
    }

    #[test]
    fn omf_document_rejects_missing_memories_key() {
        // `memories` is a required envelope field. Missing the key entirely
        // (as opposed to an explicit empty array) must fail deserialization
        // so downstream validators never see a false-empty document.
        let no_memories = r#"{"omf":"1.0","exported_at":"2026-04-18T00:00:00Z","source":{"app":"memd"}}"#;
        let err = serde_json::from_str::<OmfDocument>(no_memories).unwrap_err();
        assert!(
            err.to_string().contains("memories"),
            "error should mention missing memories field: {err}"
        );

        // Empty array is a valid envelope — a deliberate "nothing to export".
        let empty_array = r#"{"omf":"1.0","exported_at":"2026-04-18T00:00:00Z","source":{"app":"memd"},"memories":[]}"#;
        let doc: OmfDocument = serde_json::from_str(empty_array).unwrap();
        assert!(doc.memories.is_empty());
        validate_omf(&doc).unwrap();
    }
}

pub mod export;
pub mod import;
pub mod time;
