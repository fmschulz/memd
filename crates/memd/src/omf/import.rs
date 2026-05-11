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
//!   writes the chunks first (pass 1) to assign fresh DEST-side
//!   chunk ids, then replays each `supersedes` edge through
//!   `MetadataStore::atomic_supersede` (pass 2) using a
//!   source-to-target translation map. Edges whose other side is not
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

use crate::error::{MemdError, Result};
use crate::mcp::dedup::FUZZY_RECENT_POOL_SIZE;
use crate::store::metadata::MetadataStore;
use crate::store::persistent::PersistentStore;
use crate::store::supersession::{canonicalize_for_type, is_near_duplicate};
use crate::types::lifecycle::{LifecycleDelta, MemoryTier};
use crate::types::{
    ChunkId, ChunkStatus, ChunkType, IngestionMode, MemoryChunk, ProjectId, TenantId,
};

use super::time::now_utc_ms;

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

    // Codex Item 5 round-1 MEDIUM: pre-validate trusted supersession
    // invariants BEFORE any chunk write, so a malformed doc with
    // duplicate source chunk_ids or a forked `supersedes` graph
    // fails-closed before we produce a half-applied import. Legal
    // memd exports cannot violate these invariants (atomic_supersede
    // enforces head-only on the write side), so this is purely a
    // hardening pass for hand-rolled / corrupted trusted documents.
    if trusted {
        validate_trusted_supersession_invariants(doc)?;
    }

    let mut result = ImportResult {
        total: doc.memories.len(),
        imported: 0,
        duplicates: 0,
        skipped: 0,
    };
    let mut events: Vec<ImportedChunk> = Vec::new();

    // Item 5 — supersession round-trip. Writers in `export_omf` emit
    // per-item `extensions.memd.chunk_id` plus the lifecycle
    // `supersedes` / `superseded_by` edges. We preserve the graph only
    // when the document passes the F3 trust gate (source.app == "memd"
    // AND extensions.memd.v == MEMD_EXT_VERSION); otherwise the
    // imported rows keep default lifecycle with no edges — an
    // untrusted doc cannot ask memd to fabricate supersession state.
    //
    // Strategy: two passes.
    //   1. Write each chunk (status carried over by
    //      `extract_lifecycle_strict`, but supersedes/superseded_by
    //      always None in the first write so we don't dangle).
    //      Record `source_chunk_id → target_chunk_id` in a map.
    //   2. For each trusted item that names a `supersedes` edge,
    //      translate both sides via the map and call
    //      `MetadataStore::atomic_supersede` on the dest store. Edges
    //      whose other side is missing from the doc (partial export)
    //      are silently dropped — we never write a pointer to a chunk
    //      that wasn't imported.
    let mut source_to_target: HashMap<ChunkId, ChunkId> = HashMap::new();
    let mut pending_edges: Vec<PendingSupersede> = Vec::new();

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
            if is_fuzzy_duplicate(store, tenant_id, project_id.as_deref(), &canonical, thr)? {
                result.duplicates += 1;
                continue;
            }
        }

        let trusted_item = trusted && ext_version_supported(&item.extensions);
        let initial = if trusted_item {
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

        if trusted_item {
            if let Some(src_id) = extract_source_chunk_id(&item.extensions)? {
                source_to_target.insert(src_id, chunk_id.clone());
            }
            if let Some(supersedes_src) = extract_supersession_ref(&item.extensions, "supersedes")?
            {
                pending_edges.push(PendingSupersede {
                    old_source_id: supersedes_src,
                    new_target_id: chunk_id.clone(),
                });
            }
        }

        events.push(ImportedChunk {
            chunk_id,
            chunk_type,
            project_id,
            text,
        });
        result.imported += 1;
    }

    replay_supersession_edges(store, tenant_id, &source_to_target, &pending_edges)?;

    Ok((result, events))
}

/// A pending `old → new` supersession edge discovered during pass 1.
///
/// `old_source_id` is the SOURCE-side chunk id read from the OMF item's
/// `extensions.memd.lifecycle.supersedes` field; it must still be
/// translated to a dest-side id via `source_to_target` before being
/// written. `new_target_id` is already the dest-side id returned by
/// `add_chunk_with_lifecycle` for the superseding chunk.
#[derive(Debug)]
struct PendingSupersede {
    old_source_id: ChunkId,
    new_target_id: ChunkId,
}

/// Pre-flight check on a trusted OMF document's supersession graph.
///
/// Enforces two invariants that a legal `export_omf` output satisfies
/// by construction, but a hand-rolled or corrupted trusted doc might
/// violate:
///
/// - **Unique source chunk_ids.** Two items cannot share the same
///   `extensions.memd.chunk_id`. Violation would make the source→target
///   map's `last-write-wins` behaviour silently pick one of two
///   identically-addressed chunks, dropping the other's edges.
/// - **Fork-free `supersedes` graph.** Two items cannot both declare
///   `supersedes = X` on the same source id. memd's own writer enforces
///   head-only semantics in `atomic_supersede` (rolls back if the old
///   row already has a successor), so an import that allowed two
///   successors would half-apply: the first edge would commit, the
///   second would hit the head-only guard and ValidationError, leaving
///   the dest tenant in a split state.
///
/// Also runs the fail-closed parsers on `extensions.memd.chunk_id`,
/// `lifecycle.supersedes`, and `lifecycle.superseded_by` so malformed
/// fields surface here rather than mid-pass-1 (after some writes).
///
/// Untrusted items are ignored — the caller has already decided not to
/// honour their extension metadata.
fn validate_trusted_supersession_invariants(doc: &OmfDocument) -> Result<()> {
    use std::collections::HashSet;
    let mut seen_source_ids: HashSet<ChunkId> = HashSet::new();
    let mut seen_supersedes: HashSet<ChunkId> = HashSet::new();
    for (i, item) in doc.memories.iter().enumerate() {
        if !ext_version_supported(&item.extensions) {
            continue;
        }
        if let Some(src_id) = extract_source_chunk_id(&item.extensions)? {
            if !seen_source_ids.insert(src_id.clone()) {
                return Err(MemdError::ValidationError(format!(
                    "memories[{i}].extensions.memd.chunk_id duplicates an earlier item: {src_id}"
                )));
            }
        }
        if let Some(old_src) = extract_supersession_ref(&item.extensions, "supersedes")? {
            if !seen_supersedes.insert(old_src.clone()) {
                return Err(MemdError::ValidationError(format!(
                    "memories[{i}].extensions.memd.lifecycle.supersedes forks: {old_src} is named by multiple successors"
                )));
            }
        }
        // Run parse for fail-closed behaviour — any malformed edge
        // reference surfaces here rather than mid-import.
        let _ = extract_supersession_ref(&item.extensions, "superseded_by")?;
    }
    Ok(())
}

/// Walk pending supersession edges and apply them via
/// `MetadataStore::atomic_supersede`.
///
/// Edges whose `old_source_id` isn't in `source_to_target` are silently
/// dropped (logged at debug) — the sibling chunk wasn't imported, so
/// the edge has no valid dest-side counterpart. This keeps partial
/// exports round-tripping safely.
fn replay_supersession_edges(
    store: &PersistentStore,
    tenant_id: &TenantId,
    source_to_target: &HashMap<ChunkId, ChunkId>,
    pending: &[PendingSupersede],
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let now_ms = now_utc_ms();
    for edge in pending {
        let Some(old_target) = source_to_target.get(&edge.old_source_id) else {
            debug!(
                tenant_id = %tenant_id,
                old_source_id = %edge.old_source_id,
                new_target_id = %edge.new_target_id,
                "import_omf: supersession edge dropped — sibling chunk not in document"
            );
            continue;
        };
        store
            .metadata()
            .atomic_supersede(tenant_id, old_target, &edge.new_target_id, now_ms)?;
    }
    Ok(())
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

    // Mirror import_omf_with_events: fail-closed on bad trusted
    // supersession invariants BEFORE counting, so the preview's
    // "to_import" can't claim success for a doc that the real import
    // will reject. Codex Item 5 round-1 MEDIUM (preview parity).
    if is_trusted(doc) {
        validate_trusted_supersession_invariants(doc)?;
    }

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
            if is_fuzzy_duplicate(store, tenant_id, project_id.as_deref(), &canonical, thr)? {
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
        Some(p) => {
            store
                .metadata()
                .list_recent_for_project(tenant_id, Some(p), FUZZY_RECENT_POOL_SIZE)?
        }
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
