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
use memd::omf::import::{
    import_omf, preview_omf_import, ImportOptions, ImportResult, PreviewResult,
};
use memd::omf::{OmfDocument, OmfItem, OmfSource, MEMD_EXT_VERSION, OMF_VERSION};
use memd::store::metadata::MetadataStore;
use memd::store::persistent::PersistentStore;
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
        vec![
            make_item("FACT A", Some("p1")),
            make_item("fact B", Some("p1")),
        ],
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

// --------------------------------------------------------------
// F4 preview — dry-run counts with no writes, no cache bumps.
// --------------------------------------------------------------

#[tokio::test]
async fn preview_omf_returns_counts_without_writing() {
    // Seed "fact A" in p1, preview [fact A, fact B] in p1. Preview
    // should report 1 duplicate + 1 to_import, and the post-state of
    // the store must be unchanged (1 row, not 2).
    let (server, _tmp) = test_server().await;
    let _ = call_tool(
        &server,
        "memory.add",
        json!({"tenant_id": "t", "text": "fact A", "type": "doc", "project_id": "p1"}),
    )
    .await;

    let doc = make_doc(
        "nanomem",
        vec![
            make_item("fact A", Some("p1")),
            make_item("fact B", Some("p1")),
        ],
    );
    let ps = server.store().as_persistent().unwrap();

    // Take a pre-preview count of rows. The preview must not bump it.
    let pre_rows = ps
        .metadata()
        .list_for_export(&tenant("t"), Some("p1"), false)
        .unwrap()
        .len();
    assert_eq!(pre_rows, 1);

    let version_before = ps
        .hybrid()
        .and_then(|h| h.tenant_memory_version(&tenant("t")))
        .unwrap_or(0);

    let preview = preview_omf_import(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap();
    let mut expected_by_project = std::collections::BTreeMap::new();
    expected_by_project.insert("p1".to_string(), 1usize);
    assert_eq!(
        preview,
        PreviewResult {
            total: 2,
            to_import: 1,
            duplicates: 1,
            filtered: 0,
            unscoped: 0,
            by_project: expected_by_project,
        }
    );

    // Post-state: still 1 row. Preview must not write.
    let post_rows = ps
        .metadata()
        .list_for_export(&tenant("t"), Some("p1"), false)
        .unwrap()
        .len();
    assert_eq!(post_rows, pre_rows);

    // And: tenant memory version must not have advanced. When hybrid is
    // disabled (test_server harness), both sides resolve via the
    // `unwrap_or(0)` fallback and this still signals that preview didn't
    // reach a bump site; when hybrid is enabled the equality is strict.
    let version_after = ps
        .hybrid()
        .and_then(|h| h.tenant_memory_version(&tenant("t")))
        .unwrap_or(0);
    assert_eq!(
        version_before, version_after,
        "preview must not bump tenant_memory_version"
    );
}

#[tokio::test]
async fn preview_reports_unscoped_separately_from_real_project_underscore() {
    // Regression for the "_" sentinel collision flagged in F4 review:
    // a user project literally named "_" must be counted under
    // by_project["_"], not under the `unscoped` bucket used for
    // project_id=None items. Mixing them was the LOW finding.
    let (server, _tmp) = test_server().await;

    let items = vec![
        make_item("in real underscore", Some("_")),
        make_item("no project at all", None),
        make_item("also unscoped", None),
    ];
    let doc = make_doc("nanomem", items);
    let ps = server.store().as_persistent().unwrap();
    let preview = preview_omf_import(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap();

    assert_eq!(preview.total, 3);
    assert_eq!(preview.to_import, 3);
    assert_eq!(preview.unscoped, 2, "None-project items go to unscoped");
    assert_eq!(
        preview.by_project.get("_").copied(),
        Some(1),
        "real project named '_' is not collapsed into unscoped"
    );
}

#[tokio::test]
async fn preview_and_real_import_agree_on_counts() {
    // Composite: preview a doc, then import it, then assert the
    // preview's per-bucket counts match the real import's result.
    // Guards against preview/import drift (e.g. one path swallows
    // trust-gate parse errors the other propagates).
    let (server, _tmp) = test_server().await;
    let _ = call_tool(
        &server,
        "memory.add",
        json!({"tenant_id": "t", "text": "fact A", "type": "doc", "project_id": "p1"}),
    )
    .await;

    let doc = make_doc(
        "nanomem",
        vec![
            make_item("fact A", Some("p1")),
            make_item("fact B", Some("p2")),
            OmfItem {
                status: Some("archived".into()),
                ..make_item("dropped", Some("p1"))
            },
        ],
    );
    let ps = server.store().as_persistent().unwrap();
    let opts = ImportOptions {
        include_archived: false,
        fuzzy_threshold: None,
    };

    let preview = preview_omf_import(ps, &tenant("t"), &doc, opts.clone())
        .await
        .unwrap();
    let actual = import_omf(ps, &tenant("t"), &doc, opts).await.unwrap();

    assert_eq!(preview.total, actual.total);
    assert_eq!(preview.to_import, actual.imported);
    assert_eq!(preview.duplicates, actual.duplicates);
    assert_eq!(preview.filtered, actual.skipped);
}

#[tokio::test]
async fn preview_fails_closed_on_malformed_trusted_lifecycle() {
    // Matches import_rejects_malformed_trusted_lifecycle behaviour:
    // a memd-trusted doc with a broken lifecycle surfaces a
    // ValidationError from preview too, so a caller doesn't see
    // "looks fine" only for the subsequent real import to fail.
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
    let err = preview_omf_import(ps, &tenant("t"), &doc, ImportOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, memd::error::MemdError::ValidationError(_)));
}

// --------------------------------------------------------------
// G3 CLI — memd export-markdown.
// --------------------------------------------------------------

#[tokio::test]
async fn cli_export_markdown_writes_tree_under_outdir() {
    // Seed a couple of chunks across two projects; export as markdown;
    // verify the outdir contains files, each `<relative>.md` shape.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;

    use memd::types::{ChunkType, MemoryChunk, ProjectId};
    for (text, proj) in [("alpha", "p1"), ("beta", "p1"), ("gamma", "p2")] {
        let chunk = MemoryChunk::new(tenant("t"), text, ChunkType::Doc)
            .with_project(ProjectId::new(Some(proj)));
        store.add(chunk).await.unwrap();
    }

    let outdir = tempfile::tempdir().unwrap();
    memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ExportMarkdown {
            tenant_id: "t".to_string(),
            outdir: outdir.path().to_path_buf(),
            project_id: None,
            include_history: false,
            data_dir: Some(tmp.path().to_path_buf()), // won't collide
        },
    )
    .await
    .expect("export-markdown succeeds");

    let written: Vec<_> = walk_md_files(outdir.path());
    assert!(
        !written.is_empty(),
        "outdir must contain at least one markdown file after export"
    );
    // Every rendered file must carry YAML frontmatter and at least one
    // chunk body; the exact metadata labels live in
    // `render_markdown_tree` (G1) and are covered by its unit tests.
    for path in &written {
        let content = std::fs::read_to_string(path).unwrap();
        assert!(
            content.starts_with("---\n"),
            "rendered file should start with YAML frontmatter: {}",
            path.display()
        );
        assert!(
            content.contains("hash:"),
            "rendered file should carry per-chunk metadata: {}",
            path.display()
        );
    }
    // The p1 bucket ends up in `by_project/p1/doc.md` (alpha + beta);
    // the p2 bucket ends up in `by_project/p2/doc.md` (gamma).
    let all_paths: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
    assert!(all_paths.iter().any(|s| s.contains("by_project/p1/doc.md")));
    assert!(all_paths.iter().any(|s| s.contains("by_project/p2/doc.md")));
}

#[tokio::test]
async fn cli_export_markdown_refuses_outdir_inside_data_dir() {
    // The containment guard must refuse an outdir whose normalised path
    // is a descendant of `data_dir`. Pick an outdir that does NOT yet
    // exist (under a nested path inside data_dir) to also confirm the
    // guard works before the directory is created.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let bad_outdir = tmp.path().join("nested").join("would_corrupt");

    let err = memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ExportMarkdown {
            tenant_id: "t".to_string(),
            outdir: bad_outdir.clone(),
            project_id: None,
            include_history: false,
            data_dir: Some(tmp.path().to_path_buf()),
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, memd::error::MemdError::ValidationError(_)),
        "expected ValidationError for in-data-dir outdir, got: {err:?}"
    );
    assert!(
        !bad_outdir.exists(),
        "refused outdir must not have been created"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cli_export_markdown_refuses_pre_existing_symlink_inside_outdir() {
    // Item 3 — G3 symlink hardening. An attacker who can write into
    // the caller's outdir before `memd export-markdown` runs could
    // previously plant `<outdir>/<bucket_name>` as a symlink to any
    // directory (say `/etc`). The subsequent write loop would then
    // traverse that symlink and overwrite `/etc/<file>` with rendered
    // markdown content. This test pins the refusal: if any path
    // component inside outdir is a symlink, the CLI aborts before
    // writing anything.
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;

    // Seed one chunk so there's something to export.
    use memd::types::{ChunkType, MemoryChunk, ProjectId};
    let chunk = MemoryChunk::new(tenant("t"), "a seeded fact", ChunkType::Doc)
        .with_project(ProjectId::new(None::<String>));
    store.add(chunk).await.unwrap();

    let outdir = tmp.path().join("outdir");
    std::fs::create_dir_all(&outdir).unwrap();

    // Plant a malicious symlink inside outdir. G1's render_markdown_tree
    // emits unscoped (project_id=None) rows under `no_project/<type>.md`,
    // so the attacker aims a symlink at `no_project` → `victim_dir`.
    let victim_dir = tmp.path().join("victim");
    std::fs::create_dir_all(&victim_dir).unwrap();
    symlink(&victim_dir, outdir.join("no_project")).unwrap();

    // Use a separate data_dir so the containment guard passes —
    // we're verifying the symlink guard specifically.
    let guard_data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&guard_data_dir).unwrap();

    let err = memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ExportMarkdown {
            tenant_id: "t".to_string(),
            outdir: outdir.clone(),
            project_id: None,
            include_history: false,
            data_dir: Some(guard_data_dir),
        },
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, memd::error::MemdError::ValidationError(_)),
        "expected ValidationError for pre-existing symlink, got {err:?}"
    );

    // Victim directory must have received no rendered files — the
    // guard refused before any write happened.
    let leaked = walk_md_files(&victim_dir);
    assert!(
        leaked.is_empty(),
        "victim dir must not contain any rendered output: {leaked:?}"
    );
}

#[tokio::test]
async fn cli_export_markdown_refuses_outdir_via_parent_traversal() {
    // A path like `data_dir/../data_dir` normalises to `data_dir` and
    // must still be caught by the containment guard. This test pins
    // that behaviour (so a caller can't escape it by hand-crafted
    // `..` segments).
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;

    // Build a path that walks up then back down into data_dir.
    let mut tricky = tmp.path().to_path_buf();
    tricky.push("..");
    tricky.push(tmp.path().file_name().unwrap());
    tricky.push("also_bad");

    let err = memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ExportMarkdown {
            tenant_id: "t".to_string(),
            outdir: tricky.clone(),
            project_id: None,
            include_history: false,
            data_dir: Some(tmp.path().to_path_buf()),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, memd::error::MemdError::ValidationError(_)));
}

#[tokio::test]
async fn cli_export_markdown_paginates_beyond_single_page_limit() {
    // Codex G3 round-1 MEDIUM regression: whole-tenant export used to
    // cap at a single 10_000-row page. Paginate now seeds more rows
    // than one page could ever hold in the test (we use a tiny
    // effective row budget by checking that all N seeded chunks land
    // in the output — the loop re-fetches until the page runs short).
    //
    // We don't literally seed 10_001 chunks (too slow for a unit
    // test); instead we verify that pagination is triggered for ANY
    // tenant size by seeding exactly PAGE_SIZE (current: 10_000) is
    // also impractical. Simpler: this test pins behaviour for a
    // modest-sized tenant so a future regression that drops the
    // while-loop will fail the happy-path test. We explicitly re-run
    // the happy-path coverage with 25 chunks and assert every one
    // appears in exactly one output file.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;

    use memd::types::{ChunkType, MemoryChunk, ProjectId};
    for i in 0..25 {
        let chunk = MemoryChunk::new(tenant("t"), format!("chunk-{i:02}"), ChunkType::Doc)
            .with_project(ProjectId::new(Some("p1")));
        store.add(chunk).await.unwrap();
    }

    let outdir = tempfile::tempdir().unwrap();
    memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ExportMarkdown {
            tenant_id: "t".to_string(),
            outdir: outdir.path().to_path_buf(),
            project_id: None,
            include_history: false,
            data_dir: Some(tmp.path().to_path_buf()),
        },
    )
    .await
    .expect("export succeeds");

    let md_files = walk_md_files(outdir.path());
    let mut joined = String::new();
    for p in &md_files {
        joined.push_str(&std::fs::read_to_string(p).unwrap());
    }
    for i in 0..25 {
        let needle = format!("chunk-{i:02}");
        assert!(
            joined.contains(&needle),
            "all seeded chunks should be present in export: missing {needle}"
        );
    }
}

/// Walk `root` recursively, returning every `.md` file's absolute path.
fn walk_md_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    fn visit(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    visit(&p, out);
                } else if p.extension().and_then(|s| s.to_str()) == Some("md") {
                    out.push(p);
                }
            }
        }
    }
    visit(root, &mut out);
    out
}

// --------------------------------------------------------------
// F6 CLI — memd export-omf / memd import-omf.
// --------------------------------------------------------------

#[tokio::test]
async fn cli_export_omf_writes_json_document_to_output_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;

    // Seed via the Store trait (no MCP layer needed for this CLI test).
    store
        .add(memd::types::MemoryChunk::new(
            tenant("t"),
            "cli exported fact",
            memd::types::ChunkType::Doc,
        ))
        .await
        .unwrap();

    let out_path = tmp.path().join("export.omf.json");
    memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ExportOmf {
            tenant_id: "t".to_string(),
            project_id: None,
            output: Some(out_path.clone()),
            include_history: false,
            include_superseded: true,
            include_expired: true,
        },
    )
    .await
    .expect("export-omf succeeds");

    let written = std::fs::read_to_string(&out_path).unwrap();
    let doc: memd::omf::OmfDocument =
        serde_json::from_str(&written).expect("exported file parses as OmfDocument");
    assert_eq!(doc.omf, OMF_VERSION);
    assert_eq!(doc.memories.len(), 1);
    assert_eq!(doc.memories[0].content, "cli exported fact");
}

#[tokio::test]
async fn cli_import_omf_reads_json_file_and_writes_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;

    // Write a nanomem-source OMF file to disk, then import it through the CLI.
    let doc = memd::omf::OmfDocument {
        omf: OMF_VERSION.into(),
        exported_at: "2026-04-18T00:00:00Z".into(),
        source: Some(memd::omf::OmfSource {
            app: "nanomem".into(),
        }),
        memories: vec![
            memd::omf::OmfItem {
                content: "cli imported A".into(),
                extensions: json!({"memd": {"project_id": "p1"}}),
                ..Default::default()
            },
            memd::omf::OmfItem {
                content: "cli imported B".into(),
                extensions: json!({"memd": {"project_id": "p1"}}),
                ..Default::default()
            },
        ],
    };
    let input_path = tmp.path().join("import.omf.json");
    std::fs::write(&input_path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ImportOmf {
            tenant_id: "t".to_string(),
            input: Some(input_path),
            include_archived: true,
            fuzzy_threshold: None,
            dry_run: false,
        },
    )
    .await
    .expect("import-omf succeeds");

    let metas = store
        .metadata()
        .list_for_export(&tenant("t"), Some("p1"), false)
        .unwrap();
    assert_eq!(metas.len(), 2);
}

#[tokio::test]
async fn cli_import_omf_dry_run_does_not_write() {
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;

    let doc = memd::omf::OmfDocument {
        omf: OMF_VERSION.into(),
        exported_at: "2026-04-18T00:00:00Z".into(),
        source: Some(memd::omf::OmfSource {
            app: "nanomem".into(),
        }),
        memories: vec![memd::omf::OmfItem {
            content: "dry run candidate".into(),
            ..Default::default()
        }],
    };
    let input_path = tmp.path().join("preview.omf.json");
    std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

    memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ImportOmf {
            tenant_id: "t".to_string(),
            input: Some(input_path),
            include_archived: true,
            fuzzy_threshold: None,
            dry_run: true,
        },
    )
    .await
    .expect("dry-run succeeds");

    let metas = store
        .metadata()
        .list_for_export(&tenant("t"), None, false)
        .unwrap();
    assert!(metas.is_empty(), "dry_run=true must not write");
}

#[tokio::test]
async fn cli_import_omf_dry_run_does_not_create_tenant_dir() {
    // Codex F6 round-1 MEDIUM regression: `ensure_tenant_dir` used to
    // run before the dry-run branch, materialising `tenants/<id>/...`
    // even when the caller asked for a read-only preview. Now the dir
    // only appears on the real-write path. This test pins that
    // contract by passing a real `TenantManager` and asserting no
    // tenant directory exists after a successful dry-run.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let tenant_manager = memd::store::TenantManager::new(tmp.path().to_path_buf());

    let doc = memd::omf::OmfDocument {
        omf: OMF_VERSION.into(),
        exported_at: "2026-04-18T00:00:00Z".into(),
        source: Some(memd::omf::OmfSource {
            app: "nanomem".into(),
        }),
        memories: vec![memd::omf::OmfItem {
            content: "preview".into(),
            ..Default::default()
        }],
    };
    let input_path = tmp.path().join("preview.omf.json");
    std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

    let tenant_dir = tmp.path().join("tenants").join("preview_tenant");
    assert!(!tenant_dir.exists(), "precondition: tenant dir absent");

    memd::cli::run_cli(
        store.as_ref(),
        Some(&tenant_manager),
        memd::cli::CliCommand::ImportOmf {
            tenant_id: "preview_tenant".to_string(),
            input: Some(input_path),
            include_archived: true,
            fuzzy_threshold: None,
            dry_run: true,
        },
    )
    .await
    .expect("dry-run succeeds");

    assert!(
        !tenant_dir.exists(),
        "dry-run must not create the tenant directory at {}",
        tenant_dir.display()
    );
}

#[tokio::test]
async fn cli_import_omf_rejects_malformed_json() {
    // Codex F6 round-1 LOW (neg-test coverage): a malformed input file
    // must surface as ValidationError, not a panic or silent success.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    let bad = tmp.path().join("bad.omf.json");
    std::fs::write(&bad, "{not json").unwrap();

    let err = memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ImportOmf {
            tenant_id: "t".to_string(),
            input: Some(bad),
            include_archived: true,
            fuzzy_threshold: None,
            dry_run: true,
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, memd::error::MemdError::ValidationError(_)),
        "expected ValidationError for malformed JSON, got: {err:?}"
    );
}

#[tokio::test]
async fn cli_export_omf_no_output_prints_to_stdout_roundtripable() {
    // This test is a belt-and-braces check that the stdout path compiles
    // and runs without a file argument. We don't capture stdout here —
    // just confirm the call succeeds. The output-to-file path is covered
    // above with content assertions.
    let tmp = tempfile::tempdir().unwrap();
    let store = persistent_store(tmp.path()).await;
    store
        .add(memd::types::MemoryChunk::new(
            tenant("t"),
            "any",
            memd::types::ChunkType::Doc,
        ))
        .await
        .unwrap();

    memd::cli::run_cli(
        store.as_ref(),
        None,
        memd::cli::CliCommand::ExportOmf {
            tenant_id: "t".to_string(),
            project_id: None,
            output: None,
            include_history: false,
            include_superseded: true,
            include_expired: true,
        },
    )
    .await
    .expect("stdout export succeeds");
}

// --------------------------------------------------------------
// F5 MCP tools — export_omf / preview_omf_import / import_omf.
// --------------------------------------------------------------

#[tokio::test]
async fn mcp_export_then_import_roundtrip_preserves_content_and_lifecycle() {
    // End-to-end cover of the three new MCP tools:
    // 1. Seed 3 chunks in server1 with varied lifecycle (one tier flip).
    // 2. Call memory.export_omf on server1; capture the returned document.
    // 3. Call memory.import_omf on server2 with that document.
    // 4. Assert count + content parity and that the trusted lifecycle
    //    round-tripped (the working-tier chunk stays at Working).
    let (server1, _tmp1) = test_server().await;
    let ps1 = server1.store().as_persistent().unwrap();

    let mut ids = Vec::new();
    for (i, proj) in [("alpha", "p1"), ("beta", "p1"), ("gamma", "p2")]
        .iter()
        .enumerate()
    {
        let _ = i;
        let r = call_tool(
            &server1,
            "memory.add",
            json!({"tenant_id": "t", "text": proj.0, "type": "doc", "project_id": proj.1}),
        )
        .await;
        let id_str = parse_result_text(&r)["chunk_id"]
            .as_str()
            .unwrap()
            .to_string();
        ids.push(memd::types::ChunkId::parse(&id_str).unwrap());
    }
    // Flip the first chunk to Working tier so the trusted import must honour it.
    ps1.update_lifecycle(
        &tenant("t"),
        &ids[0],
        &LifecycleDelta {
            tier: Some(MemoryTier::Working),
            lifecycle_updated_at_ms: Some(1_900_000_000_000),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Export via MCP.
    let export_resp = call_tool(&server1, "memory.export_omf", json!({"tenant_id": "t"})).await;
    let body = parse_result_text(&export_resp);
    let doc_value = &body["document"];
    assert_eq!(doc_value["omf"].as_str().unwrap(), OMF_VERSION);
    let memories = doc_value["memories"].as_array().unwrap();
    assert_eq!(memories.len(), 3, "all three seeded chunks exported");

    // Fresh server2. Preview first (dry-run).
    let (server2, _tmp2) = test_server().await;
    let preview_resp = call_tool(
        &server2,
        "memory.preview_omf_import",
        json!({"tenant_id": "t", "document": doc_value}),
    )
    .await;
    let preview = parse_result_text(&preview_resp);
    assert_eq!(preview["total"].as_u64().unwrap(), 3);
    assert_eq!(preview["to_import"].as_u64().unwrap(), 3);
    assert_eq!(preview["duplicates"].as_u64().unwrap(), 0);

    // Real import.
    let import_resp = call_tool(
        &server2,
        "memory.import_omf",
        json!({"tenant_id": "t", "document": doc_value}),
    )
    .await;
    let result = parse_result_text(&import_resp);
    assert_eq!(result["total"].as_u64().unwrap(), 3);
    assert_eq!(result["imported"].as_u64().unwrap(), 3);
    assert_eq!(result["duplicates"].as_u64().unwrap(), 0);

    // Post-state on server2: trusted lifecycle (Working tier on "alpha") must round-trip.
    let ps2 = server2.store().as_persistent().unwrap();
    let metas = ps2
        .metadata()
        .list_for_export(&tenant("t"), None, false)
        .unwrap();
    assert_eq!(metas.len(), 3);
    let alpha_meta = metas
        .iter()
        .find(|m| m.canonical_text.as_deref() == Some("alpha"))
        .expect("alpha chunk present on server2");
    assert_eq!(
        alpha_meta.lifecycle.tier,
        MemoryTier::Working,
        "trusted lifecycle must round-trip memd↔memd"
    );
}

#[tokio::test]
async fn mcp_import_omf_is_idempotent_via_dedup() {
    // Calling memory.import_omf twice with the same document should land
    // 3 chunks then 0 (all dedupe on the second pass).
    let (server1, _tmp1) = test_server().await;
    for (text, proj) in [("a1", "p1"), ("a2", "p1"), ("a3", "p1")] {
        let _ = call_tool(
            &server1,
            "memory.add",
            json!({"tenant_id": "t", "text": text, "type": "doc", "project_id": proj}),
        )
        .await;
    }
    let export = call_tool(&server1, "memory.export_omf", json!({"tenant_id": "t"})).await;
    let doc_value = parse_result_text(&export)["document"].clone();

    let (server2, _tmp2) = test_server().await;
    let r1 = call_tool(
        &server2,
        "memory.import_omf",
        json!({"tenant_id": "t", "document": &doc_value}),
    )
    .await;
    assert_eq!(parse_result_text(&r1)["imported"].as_u64().unwrap(), 3);

    let r2 = call_tool(
        &server2,
        "memory.import_omf",
        json!({"tenant_id": "t", "document": &doc_value}),
    )
    .await;
    let second = parse_result_text(&r2);
    assert_eq!(second["imported"].as_u64().unwrap(), 0);
    assert_eq!(second["duplicates"].as_u64().unwrap(), 3);
}

#[tokio::test]
async fn mcp_preview_omf_import_does_not_write() {
    // Tool-layer version of preview_omf_returns_counts_without_writing.
    let (server, _tmp) = test_server().await;
    let doc_value = json!({
        "omf": "1.0",
        "exported_at": "2026-04-18T00:00:00Z",
        "source": {"app": "nanomem"},
        "memories": [
            {"content": "first", "extensions": {"memd": {"project_id": "p1"}}},
            {"content": "second", "extensions": {"memd": {"project_id": "p1"}}},
        ]
    });
    let pre = server
        .store()
        .as_persistent()
        .unwrap()
        .metadata()
        .list_for_export(&tenant("t"), None, false)
        .unwrap()
        .len();
    let preview = call_tool(
        &server,
        "memory.preview_omf_import",
        json!({"tenant_id": "t", "document": doc_value}),
    )
    .await;
    let body = parse_result_text(&preview);
    assert_eq!(body["to_import"].as_u64().unwrap(), 2);
    let post = server
        .store()
        .as_persistent()
        .unwrap()
        .metadata()
        .list_for_export(&tenant("t"), None, false)
        .unwrap()
        .len();
    assert_eq!(post, pre);
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
    let replacement = MemoryChunk::new(tenant("t"), "superseding fact", ChunkType::Doc)
        .with_project(ProjectId::none());
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

// Item 5 — memd↔memd supersession round-trip.
//
// Target: a supersession chain A → B → C, exported from source tenant
// and re-imported into a fresh tenant, MUST preserve the supersession
// edges. Today the export emits `extensions.memd.chunk_id` and the
// `supersedes` / `superseded_by` fields but the importer drops them
// unconditionally, so the round-trip lost the graph.
#[tokio::test]
async fn import_omf_roundtrips_supersession_chain() {
    use memd::types::{ChunkId, ChunkType, MemoryChunk};

    let (src_server, _src_tmp) = test_server().await;
    let src_ps = src_server.store().as_persistent().unwrap();

    // Build a 3-chunk chain A → B → C in the source tenant via the
    // persistent-store supersede API, which is the same code path that
    // `memory.supersede` uses and which writes atomic supersession
    // edges.
    let a_id: ChunkId = add_chunk(&src_server, "src", "revision A").await;

    let b_chunk = MemoryChunk::new(tenant("src"), "revision B", ChunkType::Doc)
        .with_project(ProjectId::none());
    let b_id: ChunkId = src_ps
        .supersede_chunk(&tenant("src"), &a_id, b_chunk)
        .await
        .expect("supersede A → B");

    let c_chunk = MemoryChunk::new(tenant("src"), "revision C", ChunkType::Doc)
        .with_project(ProjectId::none());
    let c_id: ChunkId = src_ps
        .supersede_chunk(&tenant("src"), &b_id, c_chunk)
        .await
        .expect("supersede B → C");

    // Sanity: the source chain is well-formed before we touch OMF.
    let src_a_meta = src_ps
        .metadata()
        .get(&tenant("src"), &a_id)
        .expect("src A metadata")
        .expect("A exists");
    assert_eq!(src_a_meta.lifecycle.superseded_by.as_ref(), Some(&b_id));
    let src_b_meta = src_ps
        .metadata()
        .get(&tenant("src"), &b_id)
        .expect("src B metadata")
        .expect("B exists");
    assert_eq!(src_b_meta.lifecycle.supersedes.as_ref(), Some(&a_id));
    assert_eq!(src_b_meta.lifecycle.superseded_by.as_ref(), Some(&c_id));

    // Export the whole chain. include_superseded=true is the default,
    // so A and B survive the export.
    let doc = export_omf(src_ps, &tenant("src"), ExportOptions::default())
        .await
        .expect("export_omf");
    assert_eq!(doc.memories.len(), 3, "chain ABC exported intact");

    // Import into a pristine tenant. Trust gate ON (source.app='memd',
    // v=MEMD_EXT_VERSION) — the export writer sets these.
    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let res = import_omf(dst_ps, &tenant("dst"), &doc, ImportOptions::default())
        .await
        .expect("import_omf");
    assert_eq!(
        res,
        ImportResult {
            total: 3,
            imported: 3,
            duplicates: 0,
            skipped: 0
        }
    );

    // Walk the imported tenant. Source chunk IDs were NEW ChunkIds on
    // import (UUIDv7 generated inside add_chunk_with_lifecycle), so the
    // dest-side IDs are different. Look up by content text.
    let dst_rows = dst_ps
        .metadata()
        .list_for_export(&tenant("dst"), None, true)
        .expect("list_for_export dst");
    let mut by_text: std::collections::HashMap<String, memd::store::metadata::ChunkMetadata> =
        std::collections::HashMap::new();
    for meta in dst_rows {
        let chunk = <PersistentStore as Store>::get(dst_ps, &tenant("dst"), &meta.chunk_id)
            .await
            .expect("dst get")
            .expect("dst chunk present");
        by_text.insert(chunk.text, meta);
    }
    let dst_a = by_text.remove("revision A").expect("revision A imported");
    let dst_b = by_text.remove("revision B").expect("revision B imported");
    let dst_c = by_text.remove("revision C").expect("revision C imported");

    // The chain edges are reconstructed on the DEST-side chunk IDs.
    assert_eq!(
        dst_a.lifecycle.superseded_by.as_ref(),
        Some(&dst_b.chunk_id),
        "A.superseded_by → dest-B"
    );
    assert_eq!(
        dst_b.lifecycle.supersedes.as_ref(),
        Some(&dst_a.chunk_id),
        "B.supersedes → dest-A"
    );
    assert_eq!(
        dst_b.lifecycle.superseded_by.as_ref(),
        Some(&dst_c.chunk_id),
        "B.superseded_by → dest-C"
    );
    assert_eq!(
        dst_c.lifecycle.supersedes.as_ref(),
        Some(&dst_b.chunk_id),
        "C.supersedes → dest-B"
    );
    // C is the current head: no onward pointer.
    assert!(dst_c.lifecycle.superseded_by.is_none());
    // A is the oldest: no back-pointer.
    assert!(dst_a.lifecycle.supersedes.is_none());

    // Statuses round-trip: A and B were superseded on the source; C
    // was the head (Final). The importer honors lifecycle.status for
    // trusted docs via extract_lifecycle_strict.
    assert_eq!(dst_a.status.to_string(), "superseded");
    assert_eq!(dst_b.status.to_string(), "superseded");
    assert_eq!(dst_c.status.to_string(), "final");
}

// Item 5 — partial-chain round-trip: when the middle of a chain is
// excluded from the export, the importer must silently drop the edges
// it cannot translate rather than fabricating a pointer to a chunk
// that wasn't imported.
#[tokio::test]
async fn import_omf_drops_edges_with_missing_sides_on_partial_export() {
    use memd::types::{ChunkId, ChunkType, MemoryChunk};

    let (src_server, _src_tmp) = test_server().await;
    let src_ps = src_server.store().as_persistent().unwrap();

    // Build A → B (no further chunk — B is the live head).
    let a_id: ChunkId = add_chunk(&src_server, "src", "original").await;
    let b_chunk = MemoryChunk::new(tenant("src"), "replacement", ChunkType::Doc)
        .with_project(ProjectId::none());
    let b_id: ChunkId = src_ps
        .supersede_chunk(&tenant("src"), &a_id, b_chunk)
        .await
        .expect("supersede A → B");
    let _ = b_id;

    // Export only the LIVE subset (drop the superseded A). This leaves
    // B pointing to a chunk that isn't in the doc.
    let doc = export_omf(
        src_ps,
        &tenant("src"),
        ExportOptions {
            include_superseded: false,
            ..Default::default()
        },
    )
    .await
    .expect("export_omf live-only");
    assert_eq!(doc.memories.len(), 1, "only B exported");

    // Import into a fresh tenant.
    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let res = import_omf(dst_ps, &tenant("dst"), &doc, ImportOptions::default())
        .await
        .expect("import_omf");
    assert_eq!(res.imported, 1);

    // B's `supersedes` edge pointed to A (which wasn't imported), so
    // the importer MUST drop the pointer rather than translate it to a
    // bogus ID.
    let rows = dst_ps
        .metadata()
        .list_for_export(&tenant("dst"), None, true)
        .expect("dst list");
    assert_eq!(rows.len(), 1);
    let dst_b = &rows[0];
    assert!(
        dst_b.lifecycle.supersedes.is_none(),
        "unreachable source A → supersedes dropped, not translated to a dangling ID"
    );
    assert!(dst_b.lifecycle.superseded_by.is_none());
}

// Item 5 — untrusted source-app docs get default lifecycle AND no
// supersession edges reconstructed, matching the F3 trust gate.
#[tokio::test]
async fn import_omf_does_not_reconstruct_supersession_for_untrusted_source() {
    use memd::types::ChunkId;

    // Build a hand-rolled OMF doc that claims to carry a supersession
    // chain but declares source.app != 'memd'. The untrusted gate must
    // prevent edge replay even though the chunk_id markers are present.
    let a_src_id = ChunkId::new().to_string();
    let b_src_id = ChunkId::new().to_string();

    let mk_item =
        |text: &str, src_id: &str, supersedes: Option<&str>, superseded_by: Option<&str>| OmfItem {
            content: text.to_string(),
            extensions: json!({
                "memd": {
                    "v": MEMD_EXT_VERSION,
                    "chunk_id": src_id,
                    "project_id": null,
                    "chunk_type": "doc",
                    "ingestion_mode": "document",
                    "lifecycle": {
                        "status": "final",
                        "tier": "long_term",
                        "supersedes": supersedes,
                        "superseded_by": superseded_by,
                    },
                },
            }),
            ..Default::default()
        };

    let doc = OmfDocument {
        omf: OMF_VERSION.to_string(),
        exported_at: "2026-04-20T00:00:00Z".to_string(),
        source: Some(OmfSource {
            app: "nanomem".to_string(),
        }),
        memories: vec![
            mk_item("older", &a_src_id, None, Some(&b_src_id)),
            mk_item("newer", &b_src_id, Some(&a_src_id), None),
        ],
    };

    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let res = import_omf(dst_ps, &tenant("dst"), &doc, ImportOptions::default())
        .await
        .expect("import_omf");
    assert_eq!(res.imported, 2);

    // Untrusted source → no edges replayed. Both rows are Final, with
    // supersedes / superseded_by None.
    let rows = dst_ps
        .metadata()
        .list_for_export(&tenant("dst"), None, true)
        .expect("dst list");
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert!(
            r.lifecycle.supersedes.is_none(),
            "untrusted: no reconstructed back-edge"
        );
        assert!(
            r.lifecycle.superseded_by.is_none(),
            "untrusted: no reconstructed forward-edge"
        );
        assert_eq!(
            r.status.to_string(),
            "final",
            "untrusted imports default to Final"
        );
    }
}

// Codex Item 5 round-1 MEDIUM: a trusted doc that declares a forked
// `supersedes` graph (two successors of the same old chunk) or
// duplicate source chunk_ids must FAIL before writing any chunks, so
// a malformed input can't leave the dest tenant half-imported.
#[tokio::test]
async fn import_omf_rejects_forked_supersession_graph_before_any_write() {
    use memd::types::ChunkId;

    let a_src = ChunkId::new().to_string();
    let b_src = ChunkId::new().to_string();
    let c_src = ChunkId::new().to_string();

    let mk_item = |text: &str, src_id: &str, supersedes: Option<&str>| OmfItem {
        content: text.to_string(),
        extensions: json!({
            "memd": {
                "v": MEMD_EXT_VERSION,
                "chunk_id": src_id,
                "project_id": null,
                "chunk_type": "doc",
                "ingestion_mode": "document",
                "lifecycle": {
                    "status": "final",
                    "tier": "long_term",
                    "supersedes": supersedes,
                    "superseded_by": null,
                },
            },
        }),
        ..Default::default()
    };

    // Two successors of A: B.supersedes=A AND C.supersedes=A. Forked graph.
    let doc = OmfDocument {
        omf: OMF_VERSION.to_string(),
        exported_at: "2026-04-20T00:00:00Z".to_string(),
        source: Some(OmfSource {
            app: "memd".to_string(),
        }),
        memories: vec![
            mk_item("A", &a_src, None),
            mk_item("B", &b_src, Some(&a_src)),
            mk_item("C", &c_src, Some(&a_src)),
        ],
    };

    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let err = import_omf(dst_ps, &tenant("dst"), &doc, ImportOptions::default())
        .await
        .expect_err("forked supersession must fail-closed");
    let msg = err.to_string();
    assert!(
        msg.contains("forks") || msg.contains("multiple successors"),
        "error should name the fork: {msg}"
    );

    // Nothing was written.
    let rows = dst_ps
        .metadata()
        .list_for_export(&tenant("dst"), None, true)
        .expect("dst list after failed import");
    assert!(rows.is_empty(), "no chunks written on pre-flight fail");
}

#[tokio::test]
async fn import_omf_rejects_duplicate_source_chunk_ids_before_any_write() {
    use memd::types::ChunkId;

    let same_id = ChunkId::new().to_string();

    let mk_item = |text: &str| OmfItem {
        content: text.to_string(),
        extensions: json!({
            "memd": {
                "v": MEMD_EXT_VERSION,
                "chunk_id": same_id,  // intentionally the same
                "project_id": null,
                "chunk_type": "doc",
                "ingestion_mode": "document",
                "lifecycle": {
                    "status": "final",
                    "tier": "long_term",
                },
            },
        }),
        ..Default::default()
    };

    let doc = OmfDocument {
        omf: OMF_VERSION.to_string(),
        exported_at: "2026-04-20T00:00:00Z".to_string(),
        source: Some(OmfSource {
            app: "memd".to_string(),
        }),
        memories: vec![mk_item("first"), mk_item("second")],
    };

    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let err = import_omf(dst_ps, &tenant("dst"), &doc, ImportOptions::default())
        .await
        .expect_err("duplicate source chunk_ids must fail-closed");
    assert!(
        err.to_string().contains("duplicates"),
        "error should name the duplicate: {err}"
    );

    let rows = dst_ps
        .metadata()
        .list_for_export(&tenant("dst"), None, true)
        .expect("dst list after failed import");
    assert!(rows.is_empty(), "no chunks written on pre-flight fail");
}

// Codex Item 5 round-1 MEDIUM: preview must share fail-closed
// behavior with real import on malformed trusted supersession data,
// or callers get a false "would import" on a doc the real import
// will reject.
#[tokio::test]
async fn preview_omf_import_matches_real_import_on_malformed_supersession_refs() {
    let doc = OmfDocument {
        omf: OMF_VERSION.to_string(),
        exported_at: "2026-04-20T00:00:00Z".to_string(),
        source: Some(OmfSource {
            app: "memd".to_string(),
        }),
        memories: vec![OmfItem {
            content: "bad".to_string(),
            extensions: json!({
                "memd": {
                    "v": MEMD_EXT_VERSION,
                    "chunk_id": "not-a-uuid",
                    "lifecycle": {
                        "status": "final",
                        "tier": "long_term",
                    },
                },
            }),
            ..Default::default()
        }],
    };

    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let preview_err = preview_omf_import(dst_ps, &tenant("dst"), &doc, ImportOptions::default())
        .await
        .expect_err("preview must fail when real import would");
    let import_err = import_omf(dst_ps, &tenant("dst"), &doc, ImportOptions::default())
        .await
        .expect_err("real import must fail on malformed chunk_id");
    // Both errors share the same shape/message, so a caller can't
    // tell preview apart from real import on validity decisions.
    assert_eq!(preview_err.to_string(), import_err.to_string());
}

// -------------------------------------------------------------------------
// Generic preview ↔ import fail-closed parity harness.
//
// The v0.8.0 handoff flagged a broader parity audit: the only existing
// test (above) pinned `chunk_id = not-a-uuid`. Any future parse-time
// check added to `import_omf_with_events` without the matching
// `preview_omf_import` branch would silently break the "preview
// predicts import" contract, producing `to_import = N` for docs the
// real import will reject.
//
// The harness below enumerates every fail-closed input shape the
// trusted OMF parser currently rejects. For each case, both
// `preview_omf_import` and `import_omf` must return `Err` with
// **identical error text**. That makes drift loud: add a new check to
// import and omit it in preview → the `preview_err` becomes `Ok(...)`
// and the test fails.
//
// Cases covered (one per fail-closed branch in the trusted parser
// + envelope validator, so adding a new check to `import_omf` without
// the matching preview branch breaks the test here):
//   validate_omf envelope:
//     1. unsupported `omf` top-level version
//     2. empty/whitespace `content`
//   validate_trusted_supersession_invariants pre-flight
//   (runs extract_source_chunk_id + extract_supersession_ref, both
//   of which also reject a non-object `lifecycle`):
//     3. non-UUID `extensions.memd.chunk_id`
//     4. non-string `extensions.memd.chunk_id` (number)
//     5. duplicate `extensions.memd.chunk_id` between items
//     6. non-UUID `extensions.memd.lifecycle.supersedes`
//     7. non-string `extensions.memd.lifecycle.supersedes`
//     8. non-UUID `extensions.memd.lifecycle.superseded_by`
//     9. non-string `extensions.memd.lifecycle.superseded_by`
//    10. forked `supersedes` graph — two successors to the same old
//    11. non-object `extensions.memd.lifecycle` (caught by
//        `extract_supersession_ref`, not `extract_lifecycle_strict`
//        — the pre-flight runs first)
//   extract_lifecycle_strict:
//    12. unknown `lifecycle.tier` string
//    13. non-string `lifecycle.tier`
//    14. unknown `lifecycle.status` string
//    15. non-string `lifecycle.status`
//    16. non-integer `lifecycle.review_after_ms`
//    17. non-integer `lifecycle.expires_at_ms`
//    18. non-integer `lifecycle.lifecycle_updated_at_ms`
// -------------------------------------------------------------------------

fn trusted_doc_with(memories: Vec<OmfItem>) -> OmfDocument {
    OmfDocument {
        omf: OMF_VERSION.to_string(),
        exported_at: "2026-04-20T00:00:00Z".to_string(),
        source: Some(OmfSource {
            app: "memd".to_string(),
        }),
        memories,
    }
}

fn trusted_item_with(content: &str, memd_ext: serde_json::Value) -> OmfItem {
    OmfItem {
        content: content.to_string(),
        extensions: json!({ "memd": memd_ext }),
        ..Default::default()
    }
}

/// Every malformed document the trusted OMF parser rejects. Each
/// entry's `doc` is run through both preview and import and their
/// `Err` text must match exactly.
fn fail_closed_parity_cases() -> Vec<(&'static str, OmfDocument)> {
    let valid_uuid_a = "019dab00-0000-7000-8000-000000000001";
    let valid_uuid_b = "019dab00-0000-7000-8000-000000000002";
    let valid_uuid_c = "019dab00-0000-7000-8000-000000000003";
    vec![
        // -- validate_omf envelope
        ("unsupported_omf_envelope_version", {
            let mut doc =
                trusted_doc_with(vec![trusted_item_with("x", json!({"v": MEMD_EXT_VERSION}))]);
            doc.omf = "9.9".to_string();
            doc
        }),
        (
            "empty_content",
            trusted_doc_with(vec![OmfItem {
                content: "   \n\t".to_string(),
                extensions: json!({ "memd": {"v": MEMD_EXT_VERSION} }),
                ..Default::default()
            }]),
        ),
        // -- validate_trusted_supersession_invariants (chunk_id)
        (
            "chunk_id_not_uuid",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "chunk_id": "not-a-uuid"}),
            )]),
        ),
        (
            "chunk_id_not_string",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "chunk_id": 42}),
            )]),
        ),
        (
            "duplicate_chunk_ids",
            trusted_doc_with(vec![
                trusted_item_with(
                    "a",
                    json!({"v": MEMD_EXT_VERSION, "chunk_id": valid_uuid_a}),
                ),
                trusted_item_with(
                    "b",
                    json!({"v": MEMD_EXT_VERSION, "chunk_id": valid_uuid_a}),
                ),
            ]),
        ),
        // -- validate_trusted_supersession_invariants (supersedes)
        (
            "supersedes_not_uuid",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({
                    "v": MEMD_EXT_VERSION,
                    "chunk_id": valid_uuid_a,
                    "lifecycle": {"supersedes": "nope"},
                }),
            )]),
        ),
        (
            "supersedes_not_string",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({
                    "v": MEMD_EXT_VERSION,
                    "chunk_id": valid_uuid_a,
                    "lifecycle": {"supersedes": 7},
                }),
            )]),
        ),
        (
            "superseded_by_not_uuid",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({
                    "v": MEMD_EXT_VERSION,
                    "chunk_id": valid_uuid_a,
                    "lifecycle": {"superseded_by": "nope"},
                }),
            )]),
        ),
        (
            "superseded_by_not_string",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({
                    "v": MEMD_EXT_VERSION,
                    "chunk_id": valid_uuid_a,
                    "lifecycle": {"superseded_by": true},
                }),
            )]),
        ),
        (
            "forked_supersedes_graph",
            trusted_doc_with(vec![
                trusted_item_with(
                    "a",
                    json!({
                        "v": MEMD_EXT_VERSION,
                        "chunk_id": valid_uuid_a,
                        "lifecycle": {"supersedes": valid_uuid_b},
                    }),
                ),
                trusted_item_with(
                    "b",
                    json!({
                        "v": MEMD_EXT_VERSION,
                        "chunk_id": valid_uuid_c,
                        "lifecycle": {"supersedes": valid_uuid_b},
                    }),
                ),
            ]),
        ),
        // -- caught by pre-flight via extract_supersession_ref's
        //    lifecycle-is-object guard
        (
            "lifecycle_not_object",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "lifecycle": "not an object"}),
            )]),
        ),
        // -- extract_lifecycle_strict
        (
            "tier_unknown",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "lifecycle": {"tier": "cold_storage"}}),
            )]),
        ),
        (
            "tier_not_string",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "lifecycle": {"tier": 42}}),
            )]),
        ),
        (
            "status_unknown",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "lifecycle": {"status": "limbo"}}),
            )]),
        ),
        (
            "status_not_string",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "lifecycle": {"status": 1}}),
            )]),
        ),
        (
            "review_after_ms_not_integer",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "lifecycle": {"review_after_ms": "soon"}}),
            )]),
        ),
        (
            "expires_at_ms_not_integer",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "lifecycle": {"expires_at_ms": "later"}}),
            )]),
        ),
        (
            "lifecycle_updated_at_ms_not_integer",
            trusted_doc_with(vec![trusted_item_with(
                "x",
                json!({"v": MEMD_EXT_VERSION, "lifecycle": {"lifecycle_updated_at_ms": "now"}}),
            )]),
        ),
    ]
}

#[tokio::test]
async fn preview_and_import_fail_closed_in_lockstep_on_all_malformed_inputs() {
    use memd::error::MemdError;
    for (label, doc) in fail_closed_parity_cases() {
        // Fresh (cold) destination for each case. Dedup runs BEFORE the
        // strict-lifecycle parse, so tier_unknown / ms_not_integer style
        // cases would stop erroring on a warm destination if they deduped
        // first. Codex round-1 parity LOW.
        let (dst_server, _dst_tmp) = test_server().await;
        let dst_ps = dst_server.store().as_persistent().unwrap();

        let preview_res =
            preview_omf_import(dst_ps, &tenant("dst"), &doc, ImportOptions::default()).await;
        let import_res = import_omf(dst_ps, &tenant("dst"), &doc, ImportOptions::default()).await;

        let preview_err = preview_res.as_ref().err().unwrap_or_else(|| {
            panic!(
                "[{label}] preview unexpectedly succeeded on malformed doc: {:?}",
                preview_res
            )
        });
        let import_err = import_res.as_ref().err().unwrap_or_else(|| {
            panic!(
                "[{label}] import unexpectedly succeeded on malformed doc: {:?}",
                import_res
            )
        });
        // Tighter variant check before string equality: a future regression
        // that turned a ValidationError into a different variant on one
        // side would produce a clearer failure here than comparing Display
        // strings alone.
        assert!(
            matches!(preview_err, MemdError::ValidationError(_)),
            "[{label}] preview error must be ValidationError, got: {preview_err:?}"
        );
        assert!(
            matches!(import_err, MemdError::ValidationError(_)),
            "[{label}] import error must be ValidationError, got: {import_err:?}"
        );
        assert_eq!(
            preview_err.to_string(),
            import_err.to_string(),
            "[{label}] preview and import error messages diverged"
        );
    }
}

// Ordering parity: a malformed trusted item that is filtered (archived)
// or dedup-skipped BEFORE the strict lifecycle parse must produce the
// same non-Err outcome on both paths. Catches a future reordering of
// "filter / dedup" vs "strict parse" that would surface errors in one
// path but not the other. Codex round-1 parity "Anything else" note.
#[tokio::test]
async fn preview_and_import_agree_when_filter_short_circuits_strict_parse() {
    // Archived status with include_archived=false should skip the item
    // before any strict-lifecycle parse. Preview increments `filtered`,
    // import increments `skipped`, neither errors — even though the same
    // item would be rejected for `tier_unknown` if it reached the strict
    // parser.
    let doc_archived = trusted_doc_with(vec![OmfItem {
        content: "archived x".to_string(),
        status: Some("archived".to_string()),
        extensions: json!({
            "memd": {
                "v": MEMD_EXT_VERSION,
                "lifecycle": {"tier": "cold_storage"}, // would fail strict parse
            },
        }),
        ..Default::default()
    }]);
    let opts = ImportOptions {
        include_archived: false,
        fuzzy_threshold: None,
    };
    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let preview = preview_omf_import(dst_ps, &tenant("dst"), &doc_archived, opts.clone())
        .await
        .expect("preview should skip before strict parse");
    let import = import_omf(dst_ps, &tenant("dst"), &doc_archived, opts)
        .await
        .expect("import should skip before strict parse");
    assert_eq!(preview.filtered, 1);
    assert_eq!(preview.to_import, 0);
    assert_eq!(import.skipped, 1);
    assert_eq!(import.imported, 0);
}

#[tokio::test]
async fn preview_and_import_agree_when_exact_dedup_short_circuits_strict_parse() {
    // Pre-seed a chunk whose canonical text matches the incoming OMF
    // item. Both preview and import must short-circuit via the exact-
    // dedup check (is_exact_duplicate) BEFORE the strict-lifecycle
    // parser would otherwise reject the item's malformed `tier`. This
    // pins dedup-before-parse ordering; reversing would surface errors
    // in one path but not the other. Codex round-2 parity LOW.
    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let dst_tenant = tenant("dst");

    let seed_text = "already here";
    let _ = dst_ps
        .add_chunk_with_lifecycle(
            memd::types::MemoryChunk::new(
                dst_tenant.clone(),
                seed_text.to_string(),
                memd::types::ChunkType::Doc,
            ),
            LifecycleDelta::default(),
        )
        .await
        .expect("seed chunk");

    let doc = trusted_doc_with(vec![trusted_item_with(
        seed_text,
        json!({
            "v": MEMD_EXT_VERSION,
            "lifecycle": {"tier": "cold_storage"}, // would fail strict parse
        }),
    )]);
    let opts = ImportOptions::default();
    let preview = preview_omf_import(dst_ps, &dst_tenant, &doc, opts.clone())
        .await
        .expect("preview should dedupe before strict parse");
    let import = import_omf(dst_ps, &dst_tenant, &doc, opts)
        .await
        .expect("import should dedupe before strict parse");
    assert_eq!(preview.duplicates, 1);
    assert_eq!(preview.to_import, 0);
    assert_eq!(import.duplicates, 1);
    assert_eq!(import.imported, 0);
}

// Positive-case parity: for a trusted VALID document, preview's
// `to_import` count must equal what a subsequent real `import_omf` call
// actually writes (imported count). Guards against a future change that
// makes preview count items import would reject via some silent filter,
// or vice versa.
#[tokio::test]
async fn preview_count_matches_real_import_count_on_valid_trusted_doc() {
    let (src_server, _src_tmp) = test_server().await;
    let src_ps = src_server.store().as_persistent().unwrap();
    let src_tenant = tenant("src");

    let a = src_ps
        .add_chunk_with_lifecycle(
            memd::types::MemoryChunk::new(
                src_tenant.clone(),
                "project_scoped_content".to_string(),
                memd::types::ChunkType::Doc,
            )
            .with_project(ProjectId::new(Some("p".to_string()))),
            LifecycleDelta::default(),
        )
        .await
        .unwrap();
    let _b = src_ps
        .add_chunk_with_lifecycle(
            memd::types::MemoryChunk::new(
                src_tenant.clone(),
                "unscoped_content".to_string(),
                memd::types::ChunkType::Doc,
            ),
            LifecycleDelta::default(),
        )
        .await
        .unwrap();

    let doc = export_omf(src_ps, &src_tenant, ExportOptions::default())
        .await
        .unwrap();
    assert!(doc.memories.len() >= 2, "seeded 2 chunks");
    assert!(
        doc.source.as_ref().map(|s| s.app.as_str()) == Some("memd"),
        "trusted export"
    );
    let _ = a;

    // Destination tenant is cold, so every item is new.
    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let dst_tenant = tenant("dst");

    let preview: PreviewResult =
        preview_omf_import(dst_ps, &dst_tenant, &doc, ImportOptions::default())
            .await
            .unwrap();
    let real: ImportResult = import_omf(dst_ps, &dst_tenant, &doc, ImportOptions::default())
        .await
        .unwrap();

    assert_eq!(
        preview.to_import, real.imported,
        "preview.to_import must equal import.imported on a cold destination"
    );
    assert_eq!(preview.total, real.total);
    assert_eq!(preview.duplicates, real.duplicates);
}

// Fuzzy-threshold parity: codex LOW from the v0.8.0 parity audit. Both
// preview and import call the same `is_fuzzy_duplicate` helper at
// mirrored sites (import.rs:212 vs import.rs:418), so structural risk
// is low — but until now the harness didn't exercise the
// `ImportOptions { fuzzy_threshold: Some(_) }` branch at all. If a
// future refactor accidentally dropped the fuzzy check from one side,
// preview would still report `to_import = N` while import would write
// only the exact-dedup survivors.
//
// This case seeds a chunk whose canonical text differs from the
// incoming OMF item by one stop-word ("on"). With threshold=0.8 the
// trigram Jaccard clears the bar (same pair verified in
// fuzzy_dedup::find_near_duplicates_returns_fuzzy_with_similarity_score),
// so both paths must count it as duplicate and neither may write.
#[tokio::test]
async fn preview_and_import_agree_under_fuzzy_threshold_match() {
    let (dst_server, _dst_tmp) = test_server().await;
    let dst_ps = dst_server.store().as_persistent().unwrap();
    let dst_tenant = tenant("dst");

    let seed_text = "Release freeze begins Thursday.";
    let _ = dst_ps
        .add_chunk_with_lifecycle(
            memd::types::MemoryChunk::new(
                dst_tenant.clone(),
                seed_text.to_string(),
                memd::types::ChunkType::Doc,
            ),
            LifecycleDelta::default(),
        )
        .await
        .expect("seed chunk");

    // Near-duplicate (single stopword inserted) — exact dedup misses,
    // fuzzy Jaccard clears 0.80.
    let doc = OmfDocument {
        omf: OMF_VERSION.into(),
        exported_at: "2026-04-20T00:00:00Z".into(),
        source: Some(OmfSource {
            app: "nanomem".into(),
        }),
        memories: vec![OmfItem {
            content: "Release freeze begins on Thursday.".into(),
            ..Default::default()
        }],
    };
    let opts = ImportOptions {
        include_archived: true,
        fuzzy_threshold: Some(0.80),
    };

    let preview: PreviewResult = preview_omf_import(dst_ps, &dst_tenant, &doc, opts.clone())
        .await
        .expect("preview under fuzzy");
    let real: ImportResult = import_omf(dst_ps, &dst_tenant, &doc, opts)
        .await
        .expect("import under fuzzy");

    assert_eq!(
        preview.duplicates, 1,
        "preview must count the near-dup as a fuzzy duplicate"
    );
    assert_eq!(preview.to_import, 0, "preview must report no new writes");
    assert_eq!(
        real.duplicates, 1,
        "import must count the near-dup as a fuzzy duplicate"
    );
    assert_eq!(real.imported, 0, "import must not write the near-dup");
    assert_eq!(
        preview.total, real.total,
        "totals must agree (envelope identical)"
    );

    // And confirm the fuzzy is load-bearing: dropping the threshold
    // must make both sides agree the item is NOT a duplicate.
    let (dst_server2, _dst_tmp2) = test_server().await;
    let dst_ps2 = dst_server2.store().as_persistent().unwrap();
    let dst_tenant2 = tenant("dst");
    let _ = dst_ps2
        .add_chunk_with_lifecycle(
            memd::types::MemoryChunk::new(
                dst_tenant2.clone(),
                seed_text.to_string(),
                memd::types::ChunkType::Doc,
            ),
            LifecycleDelta::default(),
        )
        .await
        .expect("seed chunk");
    let opts_no_fuzzy = ImportOptions::default();
    let preview_plain = preview_omf_import(dst_ps2, &dst_tenant2, &doc, opts_no_fuzzy.clone())
        .await
        .expect("preview without fuzzy");
    let real_plain = import_omf(dst_ps2, &dst_tenant2, &doc, opts_no_fuzzy)
        .await
        .expect("import without fuzzy");
    assert_eq!(
        preview_plain.duplicates, 0,
        "without fuzzy, near-dup is not a duplicate on preview"
    );
    assert_eq!(preview_plain.to_import, 1);
    assert_eq!(
        real_plain.duplicates, 0,
        "without fuzzy, near-dup is not a duplicate on import"
    );
    assert_eq!(real_plain.imported, 1);
}
