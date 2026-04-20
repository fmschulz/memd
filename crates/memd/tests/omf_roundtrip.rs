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
use memd::omf::{MEMD_EXT_VERSION, OMF_VERSION};
use memd::store::Store;
use memd::types::lifecycle::{LifecycleDelta, MemoryTier};
use memd::types::ProjectId;

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
