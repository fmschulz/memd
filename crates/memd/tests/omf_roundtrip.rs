//! OMF 1.0 export/import integration tests — Tracks F2/F3/F4/F5.
//!
//! F2 scope (this file at this point in the plan): verify that
//! `export_omf` serialises tenant memory into an OmfDocument whose
//! per-item `extensions.memd` namespace preserves lifecycle overlay,
//! project_id, chunk_type, and ingestion_mode, and that the top-level
//! envelope carries the version + source-app markers needed for the
//! F3 trust gate.

mod common;
use common::*;

use memd::omf::export::{export_omf, ExportOptions};
use memd::omf::import::{import_omf, ImportOptions, ImportResult};
use memd::omf::{OmfDocument, OmfItem, OmfSource, MEMD_EXT_VERSION, OMF_VERSION};
use memd::store::metadata::MetadataStore;
use memd::store::Store;
use memd::types::lifecycle::{LifecycleDelta, MemoryTier};
use memd::types::ProjectId;
use serde_json::json;

#[tokio::test]
async fn export_omf_emits_memd_namespace_versioned() {
    let (server, _tmp) = test_server().await;

    // Seed one chunk scoped to project "p1" and tag it.
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "fact A",
            "type": "doc",
            "project_id": "p1",
            "tags": ["topic:release"],
        }),
    )
    .await;
    let id_str = parse_result_text(&r)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();
    let id = memd::types::ChunkId::parse(&id_str).expect("valid id");

    // Flip the lifecycle tier so the exported payload carries a
    // non-default overlay value (tier=Working) — otherwise we can't
    // tell a round-tripping importer read the memd extension or the
    // untouched default.
    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");
    let delta = LifecycleDelta {
        tier: Some(MemoryTier::Working),
        lifecycle_updated_at_ms: Some(1_800_000_000_000),
        ..Default::default()
    };
    ps.update_lifecycle(&tenant("t"), &id, &delta)
        .await
        .expect("update_lifecycle");

    let doc = export_omf(ps, &tenant("t"), ExportOptions::default())
        .await
        .expect("export_omf");

    assert_eq!(doc.omf, OMF_VERSION, "wire version");
    let src = doc.source.as_ref().expect("source block present");
    assert_eq!(src.app, "memd", "source.app is the F3 trust marker");
    assert_eq!(doc.memories.len(), 1);

    let m = &doc.memories[0];
    assert_eq!(m.content, "fact A");
    assert!(m.tags.iter().any(|t| t == "topic:release"));
    // status=Final → top-level status omitted (OMF archival vocabulary).
    assert!(m.status.is_none(), "Final status should be implicit");

    let ext = m.extensions.get("memd").expect("extensions.memd present");
    assert_eq!(ext["v"].as_u64().unwrap(), MEMD_EXT_VERSION as u64);
    assert_eq!(ext["chunk_id"].as_str().unwrap(), id.to_string());
    assert_eq!(ext["project_id"].as_str().unwrap(), "p1");
    assert_eq!(ext["chunk_type"].as_str().unwrap(), "doc");
    assert_eq!(ext["ingestion_mode"].as_str().unwrap(), "document");

    let lc = ext["lifecycle"].as_object().expect("lifecycle object");
    assert_eq!(lc["status"].as_str().unwrap(), "final");
    assert_eq!(lc["tier"].as_str().unwrap(), "working");
    assert_eq!(
        lc["lifecycle_updated_at_ms"].as_i64().unwrap(),
        1_800_000_000_000
    );
}

#[tokio::test]
async fn export_omf_orders_memories_by_timestamp_created_ascending() {
    let (server, _tmp) = test_server().await;

    let _first = add_chunk(&server, "t", "first").await;
    // Sleep a fraction of a ms to ensure timestamp_created differs even
    // on fast machines; UUIDv7 + SystemTime share a monotonic source
    // but the ordering assertion needs a strict < not <=.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let _second = add_chunk(&server, "t", "second").await;

    let ps = server.store().as_persistent().unwrap();
    let doc = export_omf(ps, &tenant("t"), ExportOptions::default())
        .await
        .unwrap();
    let contents: Vec<_> = doc.memories.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["first", "second"]);
}

#[tokio::test]
async fn export_omf_scopes_by_project_id() {
    let (server, _tmp) = test_server().await;

    // One chunk in "p1", one in "p2".
    let _a = call_tool(
        &server,
        "memory.add",
        serde_json::json!({"tenant_id": "t", "text": "in p1", "type": "doc", "project_id": "p1"}),
    )
    .await;
    let _b = call_tool(
        &server,
        "memory.add",
        serde_json::json!({"tenant_id": "t", "text": "in p2", "type": "doc", "project_id": "p2"}),
    )
    .await;

    let ps = server.store().as_persistent().unwrap();

    let p1_only = export_omf(
        ps,
        &tenant("t"),
        ExportOptions {
            project_id: Some("p1".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p1_only.memories.len(), 1);
    assert_eq!(p1_only.memories[0].content, "in p1");

    let all = export_omf(ps, &tenant("t"), ExportOptions::default())
        .await
        .unwrap();
    assert_eq!(all.memories.len(), 2);
}

#[tokio::test]
async fn export_omf_exported_at_is_rfc3339_utc_zulu() {
    let (server, _tmp) = test_server().await;
    let _ = add_chunk(&server, "t", "any").await;
    let ps = server.store().as_persistent().unwrap();
    let doc = export_omf(ps, &tenant("t"), ExportOptions::default())
        .await
        .unwrap();
    // "YYYY-MM-DDTHH:MM:SSZ"
    let s = &doc.exported_at;
    assert_eq!(s.len(), 20, "expected 20-char RFC3339 UTC: {s}");
    assert!(s.ends_with('Z'));
    assert!(s.chars().nth(4) == Some('-'));
    assert!(s.chars().nth(10) == Some('T'));
}

#[tokio::test]
async fn export_omf_empty_tenant_returns_well_formed_envelope() {
    let (server, _tmp) = test_server().await;
    let ps = server.store().as_persistent().unwrap();
    let doc = export_omf(ps, &tenant("empty"), ExportOptions::default())
        .await
        .unwrap();
    assert_eq!(doc.omf, OMF_VERSION);
    assert!(doc.memories.is_empty());
    assert_eq!(doc.source.as_ref().unwrap().app, "memd");
}

#[tokio::test]
async fn export_omf_hides_lazy_expired_when_include_expired_false() {
    // Codex F2 round-1 MEDIUM: `include_expired=false` must hide rows
    // whose `expires_at_ms` has passed even if the sweep hasn't yet
    // flipped `status=Final → Expired`. Mirrors the
    // `VisibilityPolicy::is_visible_at` clock check used by search.
    let (server, _tmp) = test_server().await;

    // Seed two chunks: one live, one with an `expires_at_ms` far in the past.
    let _live = add_chunk(&server, "t", "live").await;
    let expired_id = add_with_expiry(&server, "t", "dead", 1_000).await;

    let ps = server.store().as_persistent().unwrap();

    let with_expired = export_omf(
        ps,
        &tenant("t"),
        ExportOptions {
            include_expired: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        with_expired.memories.len(),
        2,
        "include_expired=true keeps the lazily-expired row"
    );

    let live_only = export_omf(
        ps,
        &tenant("t"),
        ExportOptions {
            include_expired: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        live_only.memories.len(),
        1,
        "include_expired=false must hide lazy-expired rows"
    );
    assert_eq!(live_only.memories[0].content, "live");

    // And the ignored chunk really is the one we marked expired.
    let _ = expired_id;
}

// --------------------------------------------------------------
// F3 import — trust-gated lifecycle + exact-canonical dedup.
// --------------------------------------------------------------

fn make_item(content: &str, project: Option<&str>) -> OmfItem {
    OmfItem {
        content: content.into(),
        extensions: project
            .map(|p| json!({"memd": {"v": MEMD_EXT_VERSION, "project_id": p}}))
            .unwrap_or_else(|| json!({"memd": {"v": MEMD_EXT_VERSION}})),
        ..Default::default()
    }
}

fn make_doc(source_app: &str, memories: Vec<OmfItem>) -> OmfDocument {
    OmfDocument {
        omf: OMF_VERSION.into(),
        exported_at: "2026-04-18T00:00:00Z".into(),
        source: Some(OmfSource {
            app: source_app.into(),
        }),
        memories,
    }
}

#[tokio::test]
async fn import_omf_is_semantic_merge_not_append() {
    // Seed "fact A" in project p1. Import [fact A (case-variant), fact B].
    // The case-variant canonicalizes to the same string, so it must be
    // deduplicated; only fact B is new.
    let (server, _tmp) = test_server().await;
    let _ = call_tool(
        &server,
        "memory.add",
        json!({"tenant_id": "t", "text": "fact A", "type": "doc", "project_id": "p1"}),
    )
    .await;

    let doc = make_doc(
        "nanomem",
        vec![make_item("FACT A", Some("p1")), make_item("fact B", Some("p1"))],
    );
    let ps = server.store().as_persistent().unwrap();
    let res = import_omf(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(
        res,
        ImportResult {
            total: 2,
            imported: 1,
            duplicates: 1,
            skipped: 0
        }
    );

    // Post-state: 2 chunks total (seeded A + imported B), both under p1.
    let metas = ps
        .metadata()
        .list_for_export(&tenant("t"), Some("p1"), false)
        .unwrap();
    let texts: Vec<_> = metas
        .iter()
        .map(|m| m.canonical_text.as_deref().unwrap_or(""))
        .collect();
    assert!(texts.contains(&"fact a"));
    assert!(texts.contains(&"fact b"));
    assert_eq!(metas.len(), 2);
}

#[tokio::test]
async fn import_ignores_lifecycle_from_untrusted_source() {
    // A non-memd source claiming a hostile lifecycle (tier=history,
    // status=expired) must NOT be honoured. The imported row is live
    // (status=Final, tier=LongTerm).
    let (server, _tmp) = test_server().await;
    let hostile = OmfItem {
        content: "hostile payload".into(),
        extensions: json!({
            "memd": {
                "v": 1,
                "project_id": "p1",
                "lifecycle": {"tier": "history", "status": "expired"}
            }
        }),
        ..Default::default()
    };
    let doc = make_doc("nanomem", vec![hostile]);
    let ps = server.store().as_persistent().unwrap();
    let res = import_omf(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(res.imported, 1);

    let metas = ps
        .metadata()
        .list_for_export(&tenant("t"), Some("p1"), true) // include history just in case
        .unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(
        metas[0].status,
        memd::types::ChunkStatus::Final,
        "untrusted lifecycle must not force Expired"
    );
    assert_eq!(
        metas[0].lifecycle.tier,
        MemoryTier::LongTerm,
        "untrusted lifecycle must not force History"
    );
}

#[tokio::test]
async fn import_honors_lifecycle_when_source_is_memd_and_version_matches() {
    // A memd source with matching v stamps the overlay — tier=Working,
    // review_after_ms=5_000_000 — which the row must adopt on write.
    let (server, _tmp) = test_server().await;
    let trusted = OmfItem {
        content: "trusted payload".into(),
        extensions: json!({
            "memd": {
                "v": 1,
                "project_id": "p1",
                "lifecycle": {"tier": "working", "review_after_ms": 5_000_000i64}
            }
        }),
        ..Default::default()
    };
    let doc = make_doc("memd", vec![trusted]);
    let ps = server.store().as_persistent().unwrap();
    let res = import_omf(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(res.imported, 1);

    let metas = ps
        .metadata()
        .list_for_export(&tenant("t"), Some("p1"), false)
        .unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].lifecycle.tier, MemoryTier::Working);
    assert_eq!(metas[0].lifecycle.review_after_ms, Some(5_000_000));
}

#[tokio::test]
async fn import_rejects_malformed_trusted_lifecycle() {
    // Trusted source with a garbage tier string must fail closed, not
    // silently fall back to LongTerm.
    let (server, _tmp) = test_server().await;
    let bad = OmfItem {
        content: "payload".into(),
        extensions: json!({
            "memd": {
                "v": 1,
                "project_id": "p1",
                "lifecycle": {"tier": "galaxy_brain"}
            }
        }),
        ..Default::default()
    };
    let doc = make_doc("memd", vec![bad]);
    let ps = server.store().as_persistent().unwrap();
    let err = import_omf(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, memd::error::MemdError::ValidationError(_)),
        "malformed trusted lifecycle must fail closed, got: {err:?}"
    );

    // Pre-write failure: no rows should exist.
    let metas = ps
        .metadata()
        .list_for_export(&tenant("t"), Some("p1"), true)
        .unwrap();
    assert!(metas.is_empty());
}

#[tokio::test]
async fn import_skips_archived_items_when_include_archived_false() {
    // Two items: one active, one with top-level status=archived. With
    // include_archived=false, only the active one imports; the other
    // counts as `skipped`.
    let (server, _tmp) = test_server().await;
    let active = make_item("keep me", Some("p1"));
    let archived = OmfItem {
        status: Some("archived".into()),
        ..make_item("drop me", Some("p1"))
    };
    let doc = make_doc("nanomem", vec![active, archived]);
    let ps = server.store().as_persistent().unwrap();
    let res = import_omf(
        ps,
        &tenant("t"),
        &doc,
        ImportOptions {
            include_archived: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        res,
        ImportResult {
            total: 2,
            imported: 1,
            duplicates: 0,
            skipped: 1
        }
    );
}

#[tokio::test]
async fn import_fuzzy_dedup_catches_near_duplicates_over_threshold() {
    // With a moderate threshold the import should drop a heavily
    // overlapping variant of an existing row.
    let (server, _tmp) = test_server().await;
    let _ = call_tool(
        &server,
        "memory.add",
        json!({"tenant_id": "t", "text": "release freeze begins Thursday afternoon", "type": "doc", "project_id": "p1"}),
    )
    .await;

    let near = make_item("release freeze starts Thursday afternoon", Some("p1"));
    let far = make_item("pizza is on Friday", Some("p1"));
    let doc = make_doc("nanomem", vec![near, far]);
    let ps = server.store().as_persistent().unwrap();

    let res = import_omf(
        ps,
        &tenant("t"),
        &doc,
        ImportOptions {
            include_archived: true,
            fuzzy_threshold: Some(0.6),
        },
    )
    .await
    .unwrap();
    assert_eq!(res.imported, 1, "only the unrelated 'pizza' should land");
    assert_eq!(res.duplicates, 1, "the near-duplicate should dedupe");
}

#[tokio::test]
async fn import_rejects_unsupported_wire_version() {
    let (server, _tmp) = test_server().await;
    let mut doc = make_doc("memd", vec![make_item("x", Some("p1"))]);
    doc.omf = "9.9".into();
    let ps = server.store().as_persistent().unwrap();
    let err = import_omf(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, memd::error::MemdError::ValidationError(_)));
}

#[tokio::test]
async fn import_memd_source_with_mismatched_ext_version_uses_default_lifecycle() {
    // Trust gate has two factors: `source.app=="memd"` AND `ext.memd.v==MEMD_EXT_VERSION`.
    // With the app match but version mismatch, the import must NOT attempt to parse
    // `extensions.memd.lifecycle` — it should fall back to the default delta.
    // Regression guard: if a future version mismatch were accidentally treated as
    // trusted, a malformed lifecycle block would ValidationError out here.
    let (server, _tmp) = test_server().await;
    let future_claim = OmfItem {
        content: "future payload".into(),
        extensions: json!({
            "memd": {
                // Intentionally mismatched; this writer claims to be memd
                // but speaks an extension version we don't support.
                "v": 999,
                "project_id": "p1",
                "lifecycle": {"tier": "galaxy_brain"} // malformed, would fail fail-closed parse
            }
        }),
        ..Default::default()
    };
    let doc = make_doc("memd", vec![future_claim]);
    let ps = server.store().as_persistent().unwrap();
    let res = import_omf(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .expect("mismatched ext version should not reach strict lifecycle parse");
    assert_eq!(res.imported, 1);

    // Row was persisted with default overlay, not "galaxy_brain" or History.
    let metas = ps
        .metadata()
        .list_for_export(&tenant("t"), Some("p1"), false)
        .unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].lifecycle.tier, MemoryTier::LongTerm);
    assert_eq!(metas[0].status, memd::types::ChunkStatus::Final);
}

#[tokio::test]
async fn import_unscoped_item_does_not_dedupe_against_scoped_rows() {
    // Seed a scoped chunk in p1 with canonical "shared fact". Import an
    // unscoped OMF item with the SAME content. `list_by_canonical_text(None)`
    // widens to every project, so a naive exact-dedup check would falsely
    // mark the unscoped item as duplicate of the scoped row and refuse to
    // import it. Correct behaviour: unscoped dedup targets NULL-project
    // rows only — the import must land.
    let (server, _tmp) = test_server().await;
    let _seed = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "shared fact",
            "type": "doc",
            "project_id": "p1",
        }),
    )
    .await;

    // Import the same canonical text with no project_id — expect it to
    // land (1 new NULL-project row), not dedupe to 0.
    let unscoped = make_item("shared fact", None); // None project in extensions
    let doc = make_doc("nanomem", vec![unscoped]);
    let ps = server.store().as_persistent().unwrap();
    let res = import_omf(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(
        res,
        ImportResult {
            total: 1,
            imported: 1,
            duplicates: 0,
            skipped: 0
        },
        "unscoped dedup must not cross into scoped rows"
    );

    // Second import of the same unscoped item DOES dedupe (same NULL-project canonical).
    let doc2 = make_doc("nanomem", vec![make_item("shared fact", None)]);
    let res2 = import_omf(ps, &tenant("t"), &doc2, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(res2.imported, 0);
    assert_eq!(res2.duplicates, 1);
}

#[tokio::test]
async fn export_omf_respects_include_flags_for_superseded() {
    // Seed two chunks; supersede the first with the second via the
    // persistent-store supersede_chunk API, then verify the default
    // export includes both (include_superseded=true by default) and
    // an opt-out flag drops the superseded row.
    let (server, _tmp) = test_server().await;

    let first = add_chunk(&server, "t", "original fact").await;
    let ps = server.store().as_persistent().unwrap();

    use memd::types::{ChunkType, MemoryChunk};
    let replacement =
        MemoryChunk::new(tenant("t"), "superseding fact", ChunkType::Doc).with_project(ProjectId::none());
    let _new_id: memd::types::ChunkId = ps
        .supersede_chunk(&tenant("t"), &first, replacement)
        .await
        .expect("supersede_chunk");

    let default_export = export_omf(ps, &tenant("t"), ExportOptions::default())
        .await
        .unwrap();
    assert_eq!(
        default_export.memories.len(),
        2,
        "default export must include superseded rows"
    );

    let without_superseded = export_omf(
        ps,
        &tenant("t"),
        ExportOptions {
            include_superseded: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        without_superseded.memories.len(),
        1,
        "opt-out drops the superseded row"
    );
    assert_eq!(without_superseded.memories[0].content, "superseding fact");
}
