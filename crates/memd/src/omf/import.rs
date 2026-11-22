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
//! - **Trust gate.** Lifecycle overlay fields (tier, review_after_ms,
//!   lifecycle_updated_at_ms, top-level status) and supersession
//!   graph edges (Item 5: `supersedes` / `superseded_by`) are
//!   honoured on import **only** when the document's `source.app`
//!   equals `memd` **and** `extensions.memd.v` matches
//!   `MEMD_EXT_VERSION`. Untrusted sources import their content with
//!   default lifecycle overlay and no reconstructed edges.
//! - **Supersession round-trip.** A trusted memd export carries a
//!   per-item `extensions.memd.chunk_id` (SOURCE-side id) and the
//!   lifecycle `supersedes` / `superseded_by` edges. `import_omf`
//!   assigns destination IDs in a read-only plan, including duplicate
//!   targets, then publishes chunks and both edge directions in one
//!   metadata transaction after finalizing the payloads. Edges whose other side is not
//!   in the document (partial export, e.g. `include_superseded=false`)
//!   are silently dropped — memd never writes a pointer to a chunk
//!   it didn't import.
//! - **Fail-closed parsing.** A trusted document whose
//!   `extensions.memd.lifecycle` block contains malformed values
//!   (non-string status, unknown tier, non-integer ms, non-UUID
//!   chunk_id / supersedes / superseded_by) returns
//!   `MemdError::ValidationError` rather than silently degrading.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use serde_json::Value;
use tracing::debug;

use crate::dedup::FUZZY_RECENT_POOL_SIZE;
use crate::error::{MemdError, Result};
use crate::store::metadata::MetadataStore;
use crate::store::persistent::PersistentStore;
use crate::store::supersession::{canonicalize_for_type, is_near_duplicate};
use crate::types::lifecycle::{LifecycleDelta, MemoryTier};
use crate::types::{
    ChunkId, ChunkStatus, ChunkType, IngestionMode, MemoryChunk, ProjectId, TenantId,
};

use super::time::now_utc_ms;

use super::{validate_omf, OmfDocument, MEMD_EXT_VERSION, MEMD_SOURCE_APP};

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

/// Import an OMF document. Planning is read-only; the store publishes all
/// eligible payloads, lifecycle overlays and translated edges together.
pub async fn import_omf(
    store: &PersistentStore,
    tenant_id: &TenantId,
    doc: &OmfDocument,
    opts: ImportOptions,
) -> Result<ImportResult> {
    let plan = plan_import(store, tenant_id, doc, &opts)?;
    store
        .import_prepared_chunks(tenant_id, plan.chunks, &plan.edges)
        .await?;
    Ok(plan.result)
}

struct ImportPlan {
    result: ImportResult,
    preview: PreviewResult,
    chunks: Vec<(MemoryChunk, LifecycleDelta)>,
    edges: Vec<(ChunkId, ChunkId)>,
}

fn plan_import(
    store: &PersistentStore,
    tenant_id: &TenantId,
    doc: &OmfDocument,
    opts: &ImportOptions,
) -> Result<ImportPlan> {
    validate_omf(doc)?;
    let TrustedGraph {
        source_ids,
        edges: source_edges,
    } = trusted_graph(doc)?;
    let graph_ids = source_edges
        .iter()
        .flat_map(|(old, new)| [old.clone(), new.clone()])
        .collect::<std::collections::HashSet<_>>();
    let mut assigned_graph_targets = std::collections::HashSet::new();
    let mut source_to_target = HashMap::new();
    let mut staged: Vec<crate::store::metadata::ChunkMetadata> = Vec::new();
    let mut plan = ImportPlan {
        result: ImportResult {
            total: doc.memories.len(),
            imported: 0,
            duplicates: 0,
            skipped: 0,
        },
        preview: PreviewResult {
            total: doc.memories.len(),
            ..Default::default()
        },
        chunks: Vec::new(),
        edges: Vec::new(),
    };
    for (item, source_id) in doc.memories.iter().zip(&source_ids) {
        if !opts.include_archived
            && matches!(item.status.as_deref(), Some("archived") | Some("expired"))
        {
            plan.result.skipped += 1;
            plan.preview.filtered += 1;
            continue;
        }
        // Parse even duplicate items before any writes, so malformed trusted
        // metadata cannot turn a retried partial document into silent success.
        let mut initial = if source_id.is_some() {
            extract_lifecycle_strict(&item.extensions)?
        } else {
            LifecycleDelta::default()
        };
        let mut project_id = extract_project_id(&item.extensions).or_else(|| item.category.clone());
        if ProjectId::validate_opt(project_id.as_deref()).is_err() {
            project_id = None;
        }
        let chunk_type = extract_chunk_type(&item.extensions).unwrap_or(ChunkType::Doc);
        let canonical = canonicalize_for_type(&item.content, chunk_type);
        let status = initial.status.unwrap_or(ChunkStatus::Final);
        let incoming_visible = crate::types::VisibilityPolicy::default().is_visible_at(
            status,
            &crate::types::LifecycleMetadata::default().apply(&initial),
            now_utc_ms(),
        );
        let graph_item = source_id.as_ref().is_some_and(|id| graph_ids.contains(id));
        let mut candidates = store.metadata().list_by_canonical_text(
            tenant_id,
            project_id.as_deref(),
            &canonical,
        )?;
        if opts.fuzzy_threshold.is_some() {
            candidates.extend(match project_id.as_deref() {
                Some(project) => store.metadata().list_recent_for_project(
                    tenant_id,
                    Some(project),
                    FUZZY_RECENT_POOL_SIZE,
                )?,
                None => store
                    .metadata()
                    .list_recent_with_null_project(tenant_id, FUZZY_RECENT_POOL_SIZE)?,
            });
        }
        candidates.extend(staged.iter().cloned());
        // Prefer the same lifecycle state, and preserve source order when a
        // chain contains multiple versions of the same canonical content.
        candidates.sort_by_key(|row| (row.status != status, row.timestamp_created));
        let duplicate = candidates.iter().find(|row| {
            row.project_id == project_id
                && row.status != ChunkStatus::Deleted
                && (!graph_item || !assigned_graph_targets.contains(&row.chunk_id))
                && (!incoming_visible
                    || (row.lifecycle.superseded_by.is_none()
                        && crate::types::VisibilityPolicy::default().is_visible_at(
                            row.status,
                            &row.lifecycle,
                            now_utc_ms(),
                        )))
                && row.canonical_text.as_deref().is_some_and(|existing| {
                    existing == canonical
                        || opts.fuzzy_threshold.is_some_and(|threshold| {
                            is_near_duplicate(&canonical, existing, threshold)
                        })
                })
        });
        if let Some(row) = duplicate {
            plan.result.duplicates += 1;
            plan.preview.duplicates += 1;
            if let Some(source_id) = source_id {
                source_to_target.insert(source_id.clone(), row.chunk_id.clone());
                if graph_item {
                    assigned_graph_targets.insert(row.chunk_id.clone());
                }
            }
            continue;
        }
        let ingestion_mode =
            extract_ingestion_mode(&item.extensions).unwrap_or(IngestionMode::Document);
        let prepared =
            crate::write_service::prepare_write(crate::write_service::PrepareWriteRequest {
                chunk_type,
                text: &item.content,
                tags: &item.tags,
                ingestion_mode,
                expires_at_ms: initial.expires_at_ms.flatten(),
                review_after_ms: initial.review_after_ms.flatten(),
            });
        if prepared.is_rejected() {
            plan.result.skipped += 1;
            plan.preview.filtered += 1;
            continue;
        }
        let defaults = prepared.lifecycle_delta();
        if initial.tier.is_none() {
            initial.tier = defaults.tier;
        }
        if initial.expires_at_ms.is_none() {
            initial.expires_at_ms = defaults.expires_at_ms;
        }
        if initial.review_after_ms.is_none() {
            initial.review_after_ms = defaults.review_after_ms;
        }
        let mut chunk = MemoryChunk::new(tenant_id.clone(), item.content.clone(), chunk_type)
            .with_ingestion_mode(ingestion_mode);
        if let Some(project) = &project_id {
            chunk = chunk.with_project(ProjectId::new(Some(project.clone())));
        }
        chunk = prepared.apply_to_chunk(chunk);
        if let Some(source_id) = source_id {
            source_to_target.insert(source_id.clone(), chunk.chunk_id.clone());
            if graph_item {
                assigned_graph_targets.insert(chunk.chunk_id.clone());
            }
        }
        staged.push(crate::store::metadata::ChunkMetadata {
            chunk_id: chunk.chunk_id.clone(),
            tenant_id: tenant_id.clone(),
            project_id: project_id.clone(),
            segment_id: 0,
            ordinal: 0,
            chunk_type,
            status,
            timestamp_created: chunk.timestamp_created,
            hash: chunk.hash.clone(),
            source_uri: chunk.source.uri.clone(),
            lifecycle: crate::types::LifecycleMetadata::default().apply(&initial),
            canonical_text: Some(canonical),
            ingestion_mode,
        });
        plan.chunks.push((chunk, initial));
        plan.result.imported += 1;
        plan.preview.to_import += 1;
        match project_id {
            Some(project) => *plan.preview.by_project.entry(project).or_default() += 1,
            None => plan.preview.unscoped += 1,
        }
    }
    for (old, new) in source_edges {
        if let (Some(old), Some(new)) = (source_to_target.get(&old), source_to_target.get(&new)) {
            plan.edges.push((old.clone(), new.clone()));
        } else {
            debug!("import_omf: dropping an edge whose other side was not imported");
        }
    }
    Ok(plan)
}

/// Normalize both edge directions and reject forks, joins and cycles before
/// writing. Missing source IDs get local keys so a one-sided edge still works.
struct TrustedGraph {
    source_ids: Vec<Option<ChunkId>>,
    edges: Vec<(ChunkId, ChunkId)>,
}

fn trusted_graph(doc: &OmfDocument) -> Result<TrustedGraph> {
    let mut ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut next = HashMap::new();
    let mut previous = HashMap::new();
    for item in &doc.memories {
        if !is_trusted(doc) || !ext_version_supported(&item.extensions) {
            ids.push(None);
            continue;
        }
        let id = extract_source_chunk_id(&item.extensions)?.unwrap_or_else(ChunkId::new);
        if !seen.insert(id.clone()) {
            return Err(MemdError::ValidationError(format!(
                "source chunk_id duplicates an earlier item: {id}"
            )));
        }
        let edges = [
            extract_supersession_ref(&item.extensions, "supersedes")?.map(|old| (old, id.clone())),
            extract_supersession_ref(&item.extensions, "superseded_by")?
                .map(|new| (id.clone(), new)),
        ];
        for (old, new) in edges.into_iter().flatten() {
            if old == new
                || next.get(&old).is_some_and(|existing| existing != &new)
                || previous.get(&new).is_some_and(|existing| existing != &old)
            {
                return Err(MemdError::ValidationError(
                    "supersession graph forks or has conflicting edges".into(),
                ));
            }
            next.insert(old.clone(), new.clone());
            previous.insert(new, old);
        }
        ids.push(Some(id));
    }
    let mut visited = std::collections::HashSet::new();
    for start in next.keys() {
        let mut path = std::collections::HashSet::new();
        let mut cursor = start;
        while !visited.contains(cursor) {
            if !path.insert(cursor.clone()) {
                return Err(MemdError::ValidationError(
                    "supersession graph contains a cycle".into(),
                ));
            }
            let Some(successor) = next.get(cursor) else {
                break;
            };
            cursor = successor;
        }
        visited.extend(path);
    }
    Ok(TrustedGraph {
        source_ids: ids,
        edges: next.into_iter().collect(),
    })
}

/// Preview uses the same read-only plan as the real import.
pub async fn preview_omf_import(
    store: &PersistentStore,
    tenant_id: &TenantId,
    doc: &OmfDocument,
    opts: ImportOptions,
) -> Result<PreviewResult> {
    Ok(plan_import(store, tenant_id, doc, &opts)?.preview)
}

fn is_trusted(doc: &OmfDocument) -> bool {
    doc.source
        .as_ref()
        .is_some_and(|source| source.app == MEMD_SOURCE_APP)
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
    ext.get("memd")
        .and_then(|m| m.get("v"))
        .and_then(|v| v.as_u64())
        == Some(MEMD_EXT_VERSION as u64)
}

/// Parse `extensions.memd.chunk_id` as a source-side `ChunkId`.
///
/// Missing field → `Ok(None)` (backward-compatible with older
/// memd-generated OMF documents that didn't emit `chunk_id` and with
/// importers that pre-date Item 5 when the field was exported but not
/// consumed). Present-but-not-string or string-but-not-UUID → fail-closed
/// `ValidationError`, matching the rest of the trusted-parse semantics.
fn extract_source_chunk_id(ext: &Value) -> Result<Option<ChunkId>> {
    let memd = match ext.get("memd") {
        Some(v) => v,
        None => return Ok(None),
    };
    match memd.get("chunk_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => ChunkId::parse(s).map(Some),
        Some(_) => Err(MemdError::ValidationError(
            "extensions.memd.chunk_id must be a string".into(),
        )),
    }
}

/// Parse `extensions.memd.lifecycle.<field>` as a source-side `ChunkId`.
///
/// Shared by the `supersedes` and `superseded_by` readers used during
/// Item 5 edge replay. Missing / null → `Ok(None)`; non-string →
/// fail-closed; string-but-not-UUID → fail-closed via `ChunkId::parse`.
fn extract_supersession_ref(ext: &Value, field: &str) -> Result<Option<ChunkId>> {
    let lc = match ext.get("memd").and_then(|m| m.get("lifecycle")) {
        Some(v) if v.is_object() => v,
        Some(_) => {
            return Err(MemdError::ValidationError(
                "extensions.memd.lifecycle must be an object".into(),
            ));
        }
        None => return Ok(None),
    };
    match lc.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => ChunkId::parse(s).map(Some),
        Some(_) => Err(MemdError::ValidationError(format!(
            "extensions.memd.lifecycle.{field} must be a string"
        ))),
    }
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
    if status == Some(ChunkStatus::Candidate) {
        return Err(MemdError::ValidationError(
            "extensions.memd.lifecycle.status=candidate requires a local consolidation journal"
                .to_string(),
        ));
    }
    let tier = parse_optional_string_field(lc, "tier", MemoryTier::from_str)?;
    let review_after_ms = parse_retention_field(lc, "review_after_ms")?;
    let lifecycle_updated_at_ms = parse_optional_i64_field(lc, "lifecycle_updated_at_ms")?;
    let expires_at_ms = parse_retention_field(lc, "expires_at_ms")?;

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

/// A trusted explicit null means retain no deadline, rather than admit the
/// write service's default deadline. Missing fields still use those defaults.
fn parse_retention_field(obj: &Value, key: &str) -> Result<Option<Option<i64>>> {
    if matches!(obj.get(key), Some(Value::Null)) {
        Ok(Some(None))
    } else {
        Ok(parse_optional_i64_field(obj, key)?.map(Some))
    }
}

#[cfg(test)]
mod tests {
    use super::super::OmfItem;
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

        // Candidate is a valid local status but cannot be imported without
        // its run journal and lineage transaction.
        let unjournaled_candidate = json!({"memd": {"lifecycle": {"status": "candidate"}}});
        assert!(matches!(
            extract_lifecycle_strict(&unjournaled_candidate),
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
        assert_eq!(
            extract_ingestion_mode(&ext),
            Some(IngestionMode::Conversation)
        );
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

    // Item 5 helpers — source-side chunk_id + supersession-ref parsing.

    #[test]
    fn extract_source_chunk_id_accepts_valid_uuid() {
        let id = ChunkId::new();
        let ext = json!({ "memd": { "chunk_id": id.to_string() } });
        assert_eq!(extract_source_chunk_id(&ext).unwrap(), Some(id));
    }

    #[test]
    fn extract_source_chunk_id_missing_returns_none() {
        assert_eq!(extract_source_chunk_id(&json!({})).unwrap(), None);
        assert_eq!(
            extract_source_chunk_id(&json!({"memd": {"v": 1}})).unwrap(),
            None
        );
        assert_eq!(
            extract_source_chunk_id(&json!({"memd": {"chunk_id": null}})).unwrap(),
            None
        );
    }

    #[test]
    fn extract_source_chunk_id_fails_closed_on_bad_input() {
        let bad_type = json!({"memd": {"chunk_id": 42}});
        assert!(matches!(
            extract_source_chunk_id(&bad_type),
            Err(MemdError::ValidationError(_))
        ));
        let bad_uuid = json!({"memd": {"chunk_id": "not-a-uuid"}});
        assert!(matches!(
            extract_source_chunk_id(&bad_uuid),
            Err(MemdError::ValidationError(_))
        ));
    }

    #[test]
    fn extract_supersession_ref_reads_both_fields() {
        let old = ChunkId::new();
        let new = ChunkId::new();
        let ext = json!({
            "memd": {
                "lifecycle": {
                    "supersedes": old.to_string(),
                    "superseded_by": new.to_string(),
                }
            }
        });
        assert_eq!(
            extract_supersession_ref(&ext, "supersedes").unwrap(),
            Some(old)
        );
        assert_eq!(
            extract_supersession_ref(&ext, "superseded_by").unwrap(),
            Some(new)
        );
    }

    #[test]
    fn extract_supersession_ref_missing_lifecycle_returns_none() {
        assert_eq!(
            extract_supersession_ref(&json!({"memd": {"v": 1}}), "supersedes").unwrap(),
            None
        );
        let null_field = json!({"memd": {"lifecycle": {"supersedes": null}}});
        assert_eq!(
            extract_supersession_ref(&null_field, "supersedes").unwrap(),
            None
        );
    }

    #[test]
    fn extract_supersession_ref_fails_closed_on_bad_shape() {
        // lifecycle present but not an object.
        let bad = json!({"memd": {"lifecycle": []}});
        assert!(matches!(
            extract_supersession_ref(&bad, "supersedes"),
            Err(MemdError::ValidationError(_))
        ));
        // field present but not a string.
        let bad = json!({"memd": {"lifecycle": {"supersedes": 7}}});
        assert!(matches!(
            extract_supersession_ref(&bad, "supersedes"),
            Err(MemdError::ValidationError(_))
        ));
        // field present and string but not a valid UUID.
        let bad = json!({"memd": {"lifecycle": {"supersedes": "not-a-uuid"}}});
        assert!(matches!(
            extract_supersession_ref(&bad, "supersedes"),
            Err(MemdError::ValidationError(_))
        ));
    }
}
