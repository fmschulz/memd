//! Track D integration tests for canonical-text persistence and
//! near-duplicate dedup. D2 covers the "every write populates
//! canonical_text" contract; later D3/D4/D5 tests will extend this file.

mod common;
use common::*;

use memd::store::Store;
use memd::store::metadata::MetadataStore;

#[tokio::test]
async fn add_writes_canonical_text_and_find_by_canonical_returns_match() {
    let (server, _tmp) = test_server().await;
    let _id1 = add_chunk(&server, "t", "Release freeze begins Thursday.").await;
    let _id2 = add_chunk(&server, "t", "release  freeze\tbegins THURSDAY.").await;

    // Pull canonical_text rows back through the metadata layer. Both
    // inserts canonicalise to the same lowercased + whitespace-collapsed
    // form, so `list_by_canonical_text` must surface both rows.
    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");
    let canonical = "release freeze begins thursday.";
    let matches = ps
        .metadata()
        .list_by_canonical_text(&tenant("t"), None, canonical)
        .expect("list_by_canonical_text");
    assert_eq!(
        matches.len(),
        2,
        "canonical form must collapse trivial variation across writes"
    );
}

#[tokio::test]
async fn add_persists_canonical_text_for_plain_inserts_without_lifecycle_fields() {
    // Regression for D2: prior to D2 the no-lifecycle `memory.add` path
    // skipped `add_chunk_with_lifecycle` and therefore never populated
    // canonical_text — leaving Track D's exact-dedup index empty for the
    // common case. Verify the row carries canonical_text now.
    let (server, _tmp) = test_server().await;
    let id = add_chunk(&server, "t", "  Hello   World\n").await;

    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");
    let meta = ps
        .metadata()
        .get(&tenant("t"), &id)
        .expect("metadata.get")
        .expect("inserted chunk metadata");
    assert_eq!(
        meta.canonical_text.as_deref(),
        Some("hello world"),
        "plain memory.add must persist canonical_text on every write"
    );
}

#[tokio::test]
async fn add_batch_persists_canonical_text_for_every_row() {
    // memory.add_batch routes through `add_chunks_internal` (single-add
    // re-uses the same path). All rows must carry canonical_text.
    let (server, _tmp) = test_server().await;
    let r = call_tool(
        &server,
        "memory.add_batch",
        serde_json::json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "Alpha BETA Gamma", "type": "doc" },
                { "text": "  spaces  collapsed  ", "type": "doc" },
                { "text": "Mixed CASE input", "type": "doc" },
            ],
        }),
    )
    .await;
    let ids: Vec<String> = parse_result_text(&r)["chunk_ids"]
        .as_array()
        .expect("chunk_ids array")
        .iter()
        .map(|v| v.as_str().expect("string").to_string())
        .collect();
    assert_eq!(ids.len(), 3);

    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");
    let expected = ["alpha beta gamma", "spaces collapsed", "mixed case input"];
    for (id_str, want) in ids.iter().zip(expected.iter()) {
        let id = memd::types::ChunkId::parse(id_str).expect("valid chunk id");
        let meta = ps
            .metadata()
            .get(&tenant("t"), &id)
            .expect("metadata.get")
            .expect("inserted row");
        assert_eq!(
            meta.canonical_text.as_deref(),
            Some(*want),
            "batch row must canonicalise from its own text"
        );
    }
}

#[tokio::test]
async fn add_code_chunk_preserves_case_in_canonical_text() {
    // ChunkType::Code must NOT lowercase identifiers (D1 contract). The
    // INSERT-side D2 fix uses canonicalize_for_type, so code chunks land
    // case-preserved.
    let (server, _tmp) = test_server().await;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "  fn   Foo()\n",
            "type": "code",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let id_str = body["chunk_id"].as_str().expect("chunk_id");
    let id = memd::types::ChunkId::parse(id_str).expect("valid chunk id");

    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");
    let meta = ps
        .metadata()
        .get(&tenant("t"), &id)
        .expect("metadata.get")
        .expect("inserted code row");
    assert_eq!(
        meta.canonical_text.as_deref(),
        Some("fn Foo()"),
        "code chunks must preserve case in canonical_text"
    );
}

#[tokio::test]
async fn add_with_lifecycle_persists_canonical_text_consistently() {
    // memory.add with temporal-overlay fields routes through
    // `add_chunk_with_lifecycle`. After the D2 round-1 HIGH fix, the
    // redundant follow-up `set_canonical_text` UPDATE is gone, so the
    // INSERT-side per-row value is the single source of truth. Short
    // inputs (no split) must therefore carry the same canonical_text
    // shape as the no-lifecycle path.
    let (server, _tmp) = test_server().await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "  Hello   Lifecycle\n",
            "type": "doc",
            "expires_at_ms": now_ms + 60_000,
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let id_str = body["chunk_id"].as_str().expect("chunk_id");
    let id = memd::types::ChunkId::parse(id_str).expect("valid chunk id");

    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");
    let meta = ps
        .metadata()
        .get(&tenant("t"), &id)
        .expect("metadata.get")
        .expect("inserted lifecycle row");
    assert_eq!(meta.canonical_text.as_deref(), Some("hello lifecycle"));
    assert_eq!(meta.lifecycle.expires_at_ms, Some(now_ms + 60_000));
}

#[tokio::test]
async fn add_with_lifecycle_long_split_doc_uses_per_row_canonical() {
    // Codex round-1 D2 HIGH regression: when a long input was split by
    // `split_for_add`, the previous follow-up `set_canonical_text`
    // UPDATE inside `add_chunk_with_lifecycle` rewrote ONLY the primary
    // metadata row's canonical_text with the WHOLE original document's
    // canonical form, while WAL replay would later rebuild it from the
    // per-row text. After the fix, `add_chunk_with_lifecycle` no longer
    // does a follow-up UPDATE; per-row canonical_text written at INSERT
    // time is the single source of truth.
    //
    // Strategy: write a long doc through the lifecycle path, list all
    // rows for the tenant, assert that no row's canonical_text equals
    // the canonicalised whole document. (The whole-doc canonical would
    // only match the full input text — split children always have
    // shorter canonical forms.)
    let (server, _tmp) = test_server().await;

    // Build a > ADD_CHUNK_THRESHOLD (1000 chars) input with sentence
    // boundaries so chunk_text actually splits it.
    let unit = "Release freeze begins Thursday at noon PST. ";
    let long_text = unit.repeat(40);
    assert!(long_text.len() > 1500, "test input must trigger split");

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": long_text,
            "type": "doc",
            "expires_at_ms": now_ms + 3600_000,
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let primary_id_str = body["chunk_id"].as_str().expect("primary chunk_id");
    let primary_id = memd::types::ChunkId::parse(primary_id_str).expect("valid chunk id");

    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");

    // Pull every row for the tenant. If split_for_add fired, there will
    // be multiple. If splitting did not actually fire (e.g. chunking
    // config changes), the test still asserts the primary row's
    // canonical equals the full canonicalised doc — both branches
    // exercise the contract that canonical_text matches its own row.
    let metas = ps
        .metadata()
        .list(&tenant("t"), usize::MAX, 0)
        .expect("list rows");
    let split_count = metas.len();

    let full_canonical = long_text
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let primary_meta = metas
        .iter()
        .find(|m| m.chunk_id == primary_id)
        .expect("primary row present");

    if split_count > 1 {
        // Split case: every row must canonicalise from its own text,
        // shorter than the full doc canonical. The previous bug would
        // have left the primary row with full_canonical.
        for m in &metas {
            let c = m.canonical_text.as_deref().unwrap_or("");
            assert!(
                !c.is_empty(),
                "every split row must carry canonical_text (chunk_id={})",
                m.chunk_id
            );
            assert_ne!(
                c, full_canonical,
                "no row's canonical may equal the WHOLE document's canonical \
                 — that was the round-1 bug (chunk_id={})",
                m.chunk_id
            );
        }
    } else {
        // No-split case: primary row's canonical should match the
        // full-doc canonical, since the row's text IS the full doc.
        assert_eq!(
            primary_meta.canonical_text.as_deref(),
            Some(full_canonical.as_str()),
            "single-row writes still canonicalise from the row's own text"
        );
    }
}
