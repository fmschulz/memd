//! OMF 1.0 import with trust-gated lifecycle + exact-canonical dedup.
//!
//! Task F3 of the nanomem-inspired features plan.
//!
//! Semantics:
//! - **Merge, not append.** Each `OmfItem` is compared against the
//!   tenant's existing canonical index (`list_by_canonical_text`) and
//!   is skipped if an exact match already exists for the resolved
//!   `project_id`. Optional `fuzzy_threshold` layers a trigram Jaccard
//!   check over a recent-chunk pool so sub-1.0 matches dedupe too.
//! - **Trust gate.** Lifecycle overlay fields (tier, supersedes,
//!   superseded_by, review_after_ms, lifecycle_updated_at_ms) are
//!   honoured on import **only** when the document's `source.app`
//!   equals `memd` **and** `extensions.memd.v` matches
//!   `MEMD_EXT_VERSION`. Untrusted sources import their content with
//!   default lifecycle overlay; imported rows always start at
//!   `status=Final, tier=LongTerm` regardless of what the item's
//!   extensions claim.
//! - **Fail-closed parsing.** A trusted document whose
//!   `extensions.memd.lifecycle` block contains malformed values
//!   (non-string status, unknown tier, non-integer ms) returns
//!   `MemdError::ValidationError` rather than silently degrading.

use std::collections::BTreeMap;
use std::str::FromStr;

use serde_json::Value;

use crate::error::{MemdError, Result};
use crate::mcp::dedup::FUZZY_RECENT_POOL_SIZE;
use crate::store::metadata::MetadataStore;
use crate::store::persistent::PersistentStore;
use crate::store::supersession::{canonicalize_for_type, is_near_duplicate};
use crate::types::lifecycle::{LifecycleDelta, MemoryTier};
use crate::types::{ChunkId, ChunkStatus, ChunkType, IngestionMode, MemoryChunk, ProjectId, TenantId};

#[allow(unused_imports)]
use super::{validate_omf, OmfDocument, OmfItem, MEMD_EXT_VERSION, MEMD_SOURCE_APP};

/// Tuning knobs for one `import_omf` / `preview_omf_import` call.
///
/// Defaults match the plan's "semantic merge" intent: include
/// archived-vocabulary rows (status=archived|expired) so nothing is
/// dropped silently, and use exact-canonical dedup only (no fuzzy).
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Include items whose top-level `status` is `"archived"` or
    /// `"expired"`. When false, those items increment the `skipped`
    /// counter and are not written.
    pub include_archived: bool,
    /// Optional Jaccard threshold for opt-in fuzzy dedup against the
    /// `FUZZY_RECENT_POOL_SIZE` most recent chunks in the tenant's
    /// resolved project scope. `None` (default) means exact-canonical
    /// only.
    pub fuzzy_threshold: Option<f32>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            include_archived: true,
            fuzzy_threshold: None,
        }
    }
}

/// Outcome of one `import_omf` call. Counters sum to `total` with no
/// double-counting: each item lands in exactly one of `imported`,
/// `duplicates`, or `skipped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    pub total: usize,
    pub imported: usize,
    pub duplicates: usize,
    pub skipped: usize,
}

/// Minimal per-write record emitted by `import_omf_with_events` so the
/// MCP handler can run `post_write_hooks` for each newly-written chunk
/// without cross-module awareness of the concrete `PostWriteEvent`
/// type. One instance per actually-imported item (skipped/duplicate
/// items emit none).
#[derive(Debug, Clone)]
pub struct ImportedChunk {
    pub chunk_id: ChunkId,
    pub chunk_type: ChunkType,
    pub project_id: Option<String>,
    pub text: String,
}

/// Outcome of one `preview_omf_import` call — Task F4 dry-run shape.
///
/// Counters mirror `ImportResult` semantically (`to_import` ↔ `imported`,
/// `filtered` ↔ `skipped`), with two additions for per-scope visibility:
/// `by_project` summarises prospective imports keyed by a real
/// project_id string, and `unscoped` carries the count of items that
/// resolved to `project_id = None` so it can't collide with a user
/// project literally named `"_"` (an earlier draft used `"_"` as a
/// sentinel and Codex flagged the collision).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreviewResult {
    pub total: usize,
    pub to_import: usize,
    pub duplicates: usize,
    pub filtered: usize,
    pub unscoped: usize,
    pub by_project: BTreeMap<String, usize>,
}

/// Import an OMF 1.0 document into a tenant. Discards per-write events.
///
/// Thin wrapper over `import_omf_with_events` for call sites that only
/// need the `ImportResult` (direct store callers, tests, CLI). The MCP
/// handler consumes `import_omf_with_events` so it can run
/// `post_write_hooks` per imported chunk.
pub async fn import_omf(
    store: &PersistentStore,
    tenant_id: &TenantId,
    doc: &OmfDocument,
    opts: ImportOptions,
) -> Result<ImportResult> {
    let (result, _events) = import_omf_with_events(store, tenant_id, doc, opts).await?;
    Ok(result)
}

/// Import an OMF 1.0 document and return both the result counters and
/// one `ImportedChunk` event per actually-imported item. Duplicate /
/// skipped items emit no event.
///
/// The MCP handler calls this variant so it can run the server-owned
/// post-write hooks (structural indexing) for each newly written chunk.
pub async fn import_omf_with_events(
    store: &PersistentStore,
    tenant_id: &TenantId,
    doc: &OmfDocument,
    opts: ImportOptions,
) -> Result<(ImportResult, Vec<ImportedChunk>)> {
    validate_omf(doc)?;
    let trusted = is_trusted(doc);

    let mut result = ImportResult {
        total: doc.memories.len(),
        imported: 0,
        duplicates: 0,
        skipped: 0,
    };
    let mut events: Vec<ImportedChunk> = Vec::new();

    for item in &doc.memories {
        if !opts.include_archived
            && matches!(item.status.as_deref(), Some("archived") | Some("expired"))
        {
            result.skipped += 1;
            continue;
        }

        let project_id = extract_project_id(&item.extensions).or_else(|| item.category.clone());
        let chunk_type = extract_chunk_type(&item.extensions).unwrap_or(ChunkType::Doc);
        let canonical = canonicalize_for_type(&item.content, chunk_type);

        if is_exact_duplicate(store, tenant_id, project_id.as_deref(), &canonical)? {
            result.duplicates += 1;
            continue;
        }

        if let Some(thr) = opts.fuzzy_threshold {
            if is_fuzzy_duplicate(
                store,
                tenant_id,
                project_id.as_deref(),
                &canonical,
                thr,
            )? {
                result.duplicates += 1;
                continue;
            }
        }

        let initial = if trusted && ext_version_supported(&item.extensions) {
            extract_lifecycle_strict(&item.extensions)?
        } else {
            LifecycleDelta::default()
        };

        let mut chunk = MemoryChunk::new(tenant_id.clone(), item.content.clone(), chunk_type);
        if let Some(ref p) = project_id {
            chunk = chunk.with_project(ProjectId::new(Some(p.clone())));
        }
        if !item.tags.is_empty() {
            chunk = chunk.with_tags(item.tags.clone());
        }
        if let Some(mode) = extract_ingestion_mode(&item.extensions) {
            chunk = chunk.with_ingestion_mode(mode);
        }

        let text = chunk.text.clone();
        let chunk_id = store.add_chunk_with_lifecycle(chunk, initial).await?;
        events.push(ImportedChunk {
            chunk_id,
            chunk_type,
            project_id,
            text,
        });
        result.imported += 1;
    }

    Ok((result, events))
}

/// Dry-run `import_omf`: walk the same dedup + filter path, count what
/// would happen, and return. Never writes, never bumps cache versions,
/// never calls `add_chunk_with_lifecycle`.
///
/// Shares the dedup helpers with `import_omf` so the preview cannot
/// diverge from what the real import would do.
pub async fn preview_omf_import(
    store: &PersistentStore,
    tenant_id: &TenantId,
    doc: &OmfDocument,
    opts: ImportOptions,
) -> Result<PreviewResult> {
    validate_omf(doc)?;

    let mut result = PreviewResult {
        total: doc.memories.len(),
        ..Default::default()
    };

    for item in &doc.memories {
        if !opts.include_archived
            && matches!(item.status.as_deref(), Some("archived") | Some("expired"))
        {
            result.filtered += 1;
            continue;
        }

        let project_id = extract_project_id(&item.extensions).or_else(|| item.category.clone());
        let chunk_type = extract_chunk_type(&item.extensions).unwrap_or(ChunkType::Doc);
        let canonical = canonicalize_for_type(&item.content, chunk_type);

        if is_exact_duplicate(store, tenant_id, project_id.as_deref(), &canonical)? {
            result.duplicates += 1;
            continue;
        }

        if let Some(thr) = opts.fuzzy_threshold {
            if is_fuzzy_duplicate(
                store,
                tenant_id,
                project_id.as_deref(),
                &canonical,
                thr,
            )? {
                result.duplicates += 1;
                continue;
            }
        }

        // Trust gate: if the preview would fail-closed on a malformed
        // trusted lifecycle, surface that here too. Callers expect the
        // preview to predict import success/failure, not paper over a
        // parse error that would block the subsequent real import.
        let trusted = is_trusted(doc) && ext_version_supported(&item.extensions);
        if trusted {
            let _ = extract_lifecycle_strict(&item.extensions)?;
        }

        result.to_import += 1;
        match project_id {
            Some(p) => *result.by_project.entry(p).or_default() += 1,
            None => result.unscoped += 1,
        }
    }

    Ok(result)
}

fn is_trusted(doc: &OmfDocument) -> bool {
    doc.source
        .as_ref()
        .is_some_and(|s| s.app == MEMD_SOURCE_APP)
}

fn is_exact_duplicate(
    store: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    canonical: &str,
) -> Result<bool> {
    // `list_by_canonical_text` widens to every project when project_id
    // is None (SQL `:project IS NULL OR project_id = :project`), so an
    // unscoped OMF item would otherwise be falsely deduped against any
    // scoped row carrying the same canonical text. D3 hit the same trap
    // and solved it with a NULL-only helper — for exact dedup we just
    // post-filter to rows whose metadata.project_id is also None.
    let matches = store
        .metadata()
        .list_by_canonical_text(tenant_id, project_id, canonical)?;
    let hit = match project_id {
        Some(_) => !matches.is_empty(),
        None => matches.iter().any(|m| m.project_id.is_none()),
    };
    Ok(hit)
}

fn is_fuzzy_duplicate(
    store: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    canonical: &str,
    threshold: f32,
) -> Result<bool> {
    // Compare canonical-to-canonical: existing rows have `canonical_text`
    // populated at INSERT time (Track D2), and `canonical` above is
    // canonicalized for the probe item. Comparing the raw `text` column
    // would re-introduce the case/whitespace sensitivity D1 fixed.
    //
    // NULL-project scope: `list_recent_for_project(tenant, None, limit)`
    // widens to "any project, then LIMIT" which can evict valid older
    // NULL-project candidates under recent scoped traffic. For
    // project_id=None we use `list_recent_with_null_project` which
    // filters BEFORE LIMIT — same reasoning as D3/D4.
    let recent = match project_id {
        Some(p) => store.metadata().list_recent_for_project(
            tenant_id,
            Some(p),
            FUZZY_RECENT_POOL_SIZE,
        )?,
        None => store
            .metadata()
            .list_recent_with_null_project(tenant_id, FUZZY_RECENT_POOL_SIZE)?,
    };
    Ok(recent.iter().any(|m| {
        m.canonical_text
            .as_deref()
            .is_some_and(|existing_canon| is_near_duplicate(canonical, existing_canon, threshold))
    }))
}

fn extract_project_id(ext: &Value) -> Option<String> {
    ext.get("memd")?
        .get("project_id")?
        .as_str()
        .map(|s| s.to_string())
}

fn extract_chunk_type(ext: &Value) -> Option<ChunkType> {
    ext.get("memd")?.get("chunk_type")?.as_str()?.parse().ok()
}

fn extract_ingestion_mode(ext: &Value) -> Option<IngestionMode> {
    ext.get("memd")?
        .get("ingestion_mode")?
        .as_str()?
        .parse()
        .ok()
}

fn ext_version_supported(ext: &Value) -> bool {
    ext.get("memd").and_then(|m| m.get("v")).and_then(|v| v.as_u64()) == Some(MEMD_EXT_VERSION as u64)
}

/// Extract a `LifecycleDelta` from a trusted `extensions.memd.lifecycle`.
///
/// Fail-closed: any malformed field (string expected but not a string,
/// integer expected but not an integer) returns `ValidationError`
/// rather than silently defaulting. Absent fields are fine — they
/// simply don't set the corresponding delta field.
///
/// Callers must confirm trust (via `is_trusted` + `ext_version_supported`)
/// before invoking this function; on an untrusted document the safe
/// answer is `LifecycleDelta::default()`, not a parse attempt.
fn extract_lifecycle_strict(ext: &Value) -> Result<LifecycleDelta> {
    let lc = match ext.get("memd").and_then(|m| m.get("lifecycle")) {
        Some(v) if v.is_object() => v,
        Some(_) => {
            return Err(MemdError::ValidationError(
                "extensions.memd.lifecycle must be an object".into(),
            ));
        }
        None => return Ok(LifecycleDelta::default()),
    };

    let status = parse_optional_string_field(lc, "status", ChunkStatus::from_str)?;
    let tier = parse_optional_string_field(lc, "tier", MemoryTier::from_str)?;
    let review_after_ms = parse_optional_i64_field(lc, "review_after_ms")?.map(Some);
    let lifecycle_updated_at_ms = parse_optional_i64_field(lc, "lifecycle_updated_at_ms")?;
    let expires_at_ms = parse_optional_i64_field(lc, "expires_at_ms")?.map(Some);

    Ok(LifecycleDelta {
        status,
        tier,
        supersedes: None,
        superseded_by: None,
        expires_at_ms,
        review_after_ms,
        lifecycle_updated_at_ms,
    })
}

/// Parse an optional string field under `obj[key]` via the caller's
/// `FromStr`-style function. Missing → `Ok(None)`; present-but-not-string
/// → `ValidationError`; present-and-string-but-parser-errors → that error.
fn parse_optional_string_field<T, F>(obj: &Value, key: &str, parse: F) -> Result<Option<T>>
where
    F: FnOnce(&str) -> Result<T>,
{
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => parse(s).map(Some),
        Some(_) => Err(MemdError::ValidationError(format!(
            "extensions.memd.lifecycle.{key} must be a string"
        ))),
    }
}

/// Parse an optional i64 field under `obj[key]`.
fn parse_optional_i64_field(obj: &Value, key: &str) -> Result<Option<i64>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_i64().map(Some).ok_or_else(|| {
            MemdError::ValidationError(format!(
                "extensions.memd.lifecycle.{key} must be an integer"
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_trusted_requires_source_app_memd() {
        let mut doc = OmfDocument {
            omf: "1.0".into(),
            exported_at: "x".into(),
            source: Some(super::super::OmfSource {
                app: "nanomem".into(),
            }),
            memories: vec![],
        };
        assert!(!is_trusted(&doc));
        doc.source = Some(super::super::OmfSource {
            app: MEMD_SOURCE_APP.into(),
        });
        assert!(is_trusted(&doc));
        doc.source = None;
        assert!(!is_trusted(&doc));
    }

    #[test]
    fn ext_version_supported_matches_exact_u64() {
        assert!(ext_version_supported(&json!({"memd": {"v": 1}})));
        assert!(!ext_version_supported(&json!({"memd": {"v": 2}})));
        assert!(!ext_version_supported(&json!({"memd": {}})));
        assert!(!ext_version_supported(&json!({})));
    }

    #[test]
    fn extract_lifecycle_strict_parses_known_fields() {
        let ext = json!({
            "memd": {
                "lifecycle": {
                    "status": "superseded",
                    "tier": "working",
                    "expires_at_ms": 123i64,
                    "review_after_ms": 456i64,
                    "lifecycle_updated_at_ms": 789i64,
                }
            }
        });
        let delta = extract_lifecycle_strict(&ext).unwrap();
        assert_eq!(delta.status, Some(ChunkStatus::Superseded));
        assert_eq!(delta.tier, Some(MemoryTier::Working));
        assert_eq!(delta.expires_at_ms, Some(Some(123)));
        assert_eq!(delta.review_after_ms, Some(Some(456)));
        assert_eq!(delta.lifecycle_updated_at_ms, Some(789));
    }

    #[test]
    fn extract_lifecycle_strict_fails_closed_on_malformed_fields() {
        // Non-string status → error, not silent fallback.
        let bad_status = json!({"memd": {"lifecycle": {"status": 42}}});
        assert!(matches!(
            extract_lifecycle_strict(&bad_status),
            Err(MemdError::ValidationError(_))
        ));

        // Unknown tier value → error from MemoryTier::FromStr.
        let bad_tier = json!({"memd": {"lifecycle": {"tier": "galaxy_brain"}}});
        assert!(matches!(
            extract_lifecycle_strict(&bad_tier),
            Err(MemdError::ValidationError(_))
        ));

        // Non-integer ms → error.
        let bad_ms = json!({"memd": {"lifecycle": {"expires_at_ms": "tomorrow"}}});
        assert!(matches!(
            extract_lifecycle_strict(&bad_ms),
            Err(MemdError::ValidationError(_))
        ));

        // Lifecycle as a non-object → error (e.g. array).
        let bad_shape = json!({"memd": {"lifecycle": []}});
        assert!(matches!(
            extract_lifecycle_strict(&bad_shape),
            Err(MemdError::ValidationError(_))
        ));
    }

    #[test]
    fn extract_lifecycle_strict_missing_block_returns_default() {
        let no_lifecycle = json!({"memd": {"v": 1}});
        let delta = extract_lifecycle_strict(&no_lifecycle).unwrap();
        assert!(delta.is_empty());
    }

    #[test]
    fn extract_project_id_and_chunk_type_and_mode_roundtrip() {
        let ext = json!({
            "memd": {
                "project_id": "p1",
                "chunk_type": "decision",
                "ingestion_mode": "conversation",
            }
        });
        assert_eq!(extract_project_id(&ext), Some("p1".to_string()));
        assert_eq!(extract_chunk_type(&ext), Some(ChunkType::Decision));
        assert_eq!(extract_ingestion_mode(&ext), Some(IngestionMode::Conversation));
    }

    /// Guard: `OmfItem` must use an untagged `OmfSource` so this
    /// import-side parser can look through `doc.source.app` without a
    /// fragile JSON-path hop. Belt-and-braces for the F5/MCP layer.
    #[test]
    fn omf_document_source_app_is_directly_accessible() {
        let doc = OmfDocument {
            omf: "1.0".into(),
            exported_at: "x".into(),
            source: Some(super::super::OmfSource {
                app: MEMD_SOURCE_APP.into(),
            }),
            memories: vec![OmfItem {
                content: "x".into(),
                ..Default::default()
            }],
        };
        assert_eq!(doc.source.as_ref().unwrap().app, MEMD_SOURCE_APP);
    }
}
