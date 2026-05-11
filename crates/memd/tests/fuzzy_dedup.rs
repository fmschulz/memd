//! Track D integration tests for canonical-text persistence and
//! near-duplicate dedup. D2 covers the "every write populates
//! canonical_text" contract; later D3/D4/D5 tests will extend this file.

mod common;
use common::*;

use memd::store::metadata::MetadataStore;
use memd::store::Store;

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

// D2 round-1 MEDIUM regression — Codex flagged that pre-D2 production
// rows still carry canonical_text=NULL after upgrade and remain
// invisible to D3 exact dedup until backfilled. We expose
// `force_clear_canonical_text` as a test-support helper to simulate
// the legacy state; the backfill test is gated behind the feature so
// it only runs under `cargo test -p memd --features test-support`.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn backfill_canonical_text_repopulates_legacy_null_rows() {
    let (server, _tmp) = test_server().await;
    let id = add_chunk(&server, "t", "  Hello   World\n").await;

    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");

    // Simulate a pre-D2 row by clearing canonical_text directly.
    ps.metadata()
        .force_clear_canonical_text(&id)
        .expect("force_clear_canonical_text");
    let meta_before = ps
        .metadata()
        .get(&tenant("t"), &id)
        .expect("metadata.get")
        .expect("row");
    assert!(
        meta_before.canonical_text.is_none(),
        "precondition: row should look pre-D2 (canonical_text NULL)"
    );

    // Run backfill — the same call that startup schedules.
    let stats = ps.backfill_canonical_text_for_legacy_chunks();
    assert_eq!(
        stats.rows_backfilled, 1,
        "exactly one legacy row backfilled"
    );
    assert_eq!(stats.rows_skipped, 0);

    let meta_after = ps
        .metadata()
        .get(&tenant("t"), &id)
        .expect("metadata.get")
        .expect("row");
    assert_eq!(
        meta_after.canonical_text.as_deref(),
        Some("hello world"),
        "backfill must populate canonical_text from the row's own text"
    );
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

    // Pull every row for the tenant. The 1760-char input above MUST
    // trigger split_for_add under current chunking defaults (1000-char
    // threshold, ~1200-char target) so the regression is exercised
    // every time. If chunking defaults ever change such that this
    // input no longer splits, fail loudly so the test author updates
    // the input rather than silently degrading coverage.
    let metas = ps
        .metadata()
        .list(&tenant("t"), usize::MAX, 0)
        .expect("list rows");
    assert!(
        metas.len() > 1,
        "test input must trigger split_for_add — got {} rows. Update the \
         test input if chunking defaults changed.",
        metas.len()
    );

    let full_canonical = long_text
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    // The previous bug would have left the primary row with
    // full_canonical from the post-add overwrite. Assert no row carries
    // the WHOLE document's canonical.
    let _primary_meta = metas
        .iter()
        .find(|m| m.chunk_id == primary_id)
        .expect("primary row present");
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
}

// ---------- Track D3 ----------
//
// `memory.add` with `supersede_near_duplicates` should mark prior
// content-identical (or fuzzy-similar) rows for the same
// (tenant, project) as Superseded with a back-edge to the new chunk.

#[tokio::test]
async fn add_with_exact_dedup_supersedes_canonical_match() {
    use memd::types::{ChunkId, ChunkStatus};
    let (server, _tmp) = test_server().await;

    // Seed a chunk; second insert with exact-mode dedup should
    // supersede the first.
    let r1 = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "Release freeze begins Thursday.",
            "type": "doc",
        }),
    )
    .await;
    let body1 = parse_result_text(&r1);
    let id1 = ChunkId::parse(body1["chunk_id"].as_str().expect("chunk_id")).expect("valid id");

    let r2 = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "release freeze begins thursday.",
            "type": "doc",
            "supersede_near_duplicates": { "mode": "exact" },
        }),
    )
    .await;
    let body2 = parse_result_text(&r2);
    let id2 = ChunkId::parse(body2["chunk_id"].as_str().expect("chunk_id")).expect("valid id");
    let supersedes: Vec<String> = body2["superseded_ids"]
        .as_array()
        .expect("superseded_ids array")
        .iter()
        .map(|v| v.as_str().expect("string").to_string())
        .collect();

    assert_ne!(id1, id2);
    assert_eq!(supersedes, vec![id1.to_string()]);

    // Old chunk now Superseded with back-edge to id2.
    let ps = server.store().as_persistent().expect("persistent");
    let resolved = ps
        .get_with_lifecycle(&tenant("t"), &id1)
        .await
        .expect("get_with_lifecycle")
        .expect("old chunk still resolvable");
    assert_eq!(resolved.status, ChunkStatus::Superseded);
    assert_eq!(
        resolved
            .lifecycle
            .superseded_by
            .as_ref()
            .map(|c| c.to_string()),
        Some(id2.to_string())
    );
}

#[tokio::test]
async fn add_with_fuzzy_dedup_supersedes_paraphrase() {
    use memd::types::ChunkId;
    let (server, _tmp) = test_server().await;

    // Seed.
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "Release freeze begins Thursday.",
            "type": "doc",
        }),
    )
    .await;

    // Paraphrase: insert one extra word. Padded char-trigram Jaccard ~ 0.86.
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "Release freeze begins on Thursday.",
            "type": "doc",
            "supersede_near_duplicates": { "mode": "fuzzy", "threshold": 0.80 },
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let _id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");
    let supersedes: Vec<String> = body["superseded_ids"]
        .as_array()
        .expect("superseded_ids array")
        .iter()
        .map(|v| v.as_str().expect("string").to_string())
        .collect();
    assert_eq!(
        supersedes.len(),
        1,
        "fuzzy mode at threshold 0.80 must catch the paraphrase"
    );
}

#[tokio::test]
async fn add_without_dedup_flag_does_not_supersede_anything() {
    use memd::types::ChunkId;
    let (server, _tmp) = test_server().await;
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "Release freeze begins Thursday.",
            "type": "doc",
        }),
    )
    .await;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "release freeze begins thursday.",
            "type": "doc",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let _id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");
    // The default add path must NOT include superseded_ids in its
    // response (backwards-compatible shape).
    assert!(
        body.get("superseded_ids").is_none(),
        "absent dedup flag must not produce superseded_ids field"
    );
}

#[tokio::test]
async fn add_with_dedup_preserves_lifecycle_overlay_on_match() {
    // Codex round-1 D3 HIGH-1 regression: when a dedup match is found,
    // the requested expires_at_ms / review_after_ms must still apply
    // to the new chunk. Previously supersede_chunk was called with the
    // raw chunk and no delta, dropping the overlay.
    use memd::types::ChunkId;
    let (server, _tmp) = test_server().await;

    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "Release freeze begins Thursday.",
            "type": "doc",
        }),
    )
    .await;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let want_expires = now_ms + 60_000;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "release freeze begins thursday.",
            "type": "doc",
            "expires_at_ms": want_expires,
            "supersede_near_duplicates": { "mode": "exact" },
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let new_id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");

    let ps = server.store().as_persistent().expect("persistent");
    let resolved = ps
        .get_with_lifecycle(&tenant("t"), &new_id)
        .await
        .expect("get_with_lifecycle")
        .expect("new chunk");
    assert_eq!(
        resolved.lifecycle.expires_at_ms,
        Some(want_expires),
        "expires_at_ms must be preserved on the matched-dedup path"
    );
}

#[tokio::test]
async fn add_with_exact_dedup_skips_already_superseded_candidate() {
    // Codex round-1 D3 HIGH-2 regression: compute_dedup_candidates must
    // filter out non-head rows. Build a chain (A → B head), then run a
    // dedup add that should find ONLY B, not A. Without the head-only
    // filter, supersede_chunk on A would fail-closed (A is not head)
    // or update_lifecycle would overwrite A's existing back-edge.
    use memd::types::{ChunkId, ChunkStatus};
    let (server, _tmp) = test_server().await;

    let r1 = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "Release freeze begins Thursday.",
            "type": "doc",
        }),
    )
    .await;
    let id_a =
        ChunkId::parse(parse_result_text(&r1)["chunk_id"].as_str().expect("a")).expect("valid");

    // Build a 2-deep chain: B supersedes A.
    let r2 = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "release freeze begins thursday.",
            "type": "doc",
            "supersede_near_duplicates": { "mode": "exact" },
        }),
    )
    .await;
    let body2 = parse_result_text(&r2);
    let id_b = ChunkId::parse(body2["chunk_id"].as_str().expect("b")).expect("valid");

    // Third dedup add — must find ONLY B (head), not A (already
    // superseded). New chunk C supersedes B. A's superseded_by edge
    // (still pointing at B) must be preserved.
    let r3 = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "RELEASE FREEZE BEGINS Thursday.",
            "type": "doc",
            "supersede_near_duplicates": { "mode": "exact" },
        }),
    )
    .await;
    let body3 = parse_result_text(&r3);
    let id_c = ChunkId::parse(body3["chunk_id"].as_str().expect("c")).expect("valid");
    let supersedes: Vec<String> = body3["superseded_ids"]
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        supersedes,
        vec![id_b.to_string()],
        "third dedup add must supersede only the live head (B), not the historical A"
    );

    // A still points at B (preserved from r2).
    let ps = server.store().as_persistent().expect("persistent");
    let resolved_a = ps
        .get_with_lifecycle(&tenant("t"), &id_a)
        .await
        .expect("get_with_lifecycle")
        .expect("a");
    assert_eq!(resolved_a.status, ChunkStatus::Superseded);
    assert_eq!(
        resolved_a
            .lifecycle
            .superseded_by
            .as_ref()
            .map(|c| c.to_string()),
        Some(id_b.to_string()),
        "A's edge to B must be preserved — not overwritten by the C dedup"
    );

    // B now points at C.
    let resolved_b = ps
        .get_with_lifecycle(&tenant("t"), &id_b)
        .await
        .expect("get_with_lifecycle")
        .expect("b");
    assert_eq!(resolved_b.status, ChunkStatus::Superseded);
    assert_eq!(
        resolved_b
            .lifecycle
            .superseded_by
            .as_ref()
            .map(|c| c.to_string()),
        Some(id_c.to_string())
    );
}

#[tokio::test]
async fn add_with_dedup_scope_project_does_not_match_other_project() {
    // Codex round-1 D3 MEDIUM regression: scope=project must filter to
    // the SAME project_id even when the incoming chunk has no
    // project_id (project_id IS NULL bucket). Without the post-filter,
    // a `None` project_id wildcards the SQL filter and matches across
    // the whole tenant.
    let (server, _tmp) = test_server().await;
    // Prior chunk in project_a.
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "project_a",
            "text": "shared canonical text",
            "type": "doc",
        }),
    )
    .await;

    // Dedup add to project_b — exact canonical matches project_a's
    // chunk, but scope=project should reject the cross-project hit.
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "project_b",
            "text": "shared canonical text",
            "type": "doc",
            "supersede_near_duplicates": { "mode": "exact" },
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let supersedes = body["superseded_ids"].as_array().expect("array");
    assert!(
        supersedes.is_empty(),
        "scope=project must not cross project boundaries (got {supersedes:?})"
    );
}

#[tokio::test]
async fn add_with_dedup_scope_tenant_crosses_project_boundary() {
    // Symmetric to the previous test: scope=tenant should bridge
    // projects within the same tenant.
    let (server, _tmp) = test_server().await;
    let r1 = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "project_a",
            "text": "shared canonical text",
            "type": "doc",
        }),
    )
    .await;
    let id_a = parse_result_text(&r1)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();

    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "project_b",
            "text": "shared canonical text",
            "type": "doc",
            "supersede_near_duplicates": { "mode": "exact", "scope": "tenant" },
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let supersedes: Vec<String> = body["superseded_ids"]
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        supersedes,
        vec![id_a],
        "scope=tenant must bridge project boundaries"
    );
}

#[tokio::test]
async fn add_with_dedup_bool_true_uses_exact_mode_default() {
    use memd::types::ChunkId;
    let (server, _tmp) = test_server().await;
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "Release freeze begins Thursday.",
            "type": "doc",
        }),
    )
    .await;
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "p1",
            "text": "release freeze begins thursday.",
            "type": "doc",
            "supersede_near_duplicates": true,
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let _id = ChunkId::parse(body["chunk_id"].as_str().expect("chunk_id")).expect("valid id");
    let supersedes = body["superseded_ids"].as_array().expect("array");
    assert_eq!(
        supersedes.len(),
        1,
        "shorthand `true` must default to exact mode and catch the canonical match"
    );
}

// ---------- Track D5 ----------
//
// `memory.find_near_duplicates` is a read-only preview that returns
// exact and (optionally) fuzzy candidates without mutating store
// state. No supersession, no cache bumps.

#[tokio::test]
async fn find_near_duplicates_returns_exact_match_without_mutating() {
    use memd::types::ChunkStatus;
    let (server, _tmp) = test_server().await;
    let id_a = add_chunk(&server, "t", "Release freeze begins Thursday.").await;
    let _id_b = add_chunk(&server, "t", "Migration approved by legal.").await;

    let r = call_tool(
        &server,
        "memory.find_near_duplicates",
        serde_json::json!({
            "tenant_id": "t",
            "text": "release freeze begins thursday.",
            "type": "doc",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let exact: Vec<String> = body["exact_matches"]
        .as_array()
        .expect("exact_matches array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(exact, vec![id_a.to_string()], "exact canonical hit");

    // Fuzzy not requested → empty.
    let fuzzy = body["fuzzy_matches"]
        .as_array()
        .expect("fuzzy_matches array");
    assert!(
        fuzzy.is_empty(),
        "fuzzy_matches must be empty when no threshold"
    );

    // No mutation: original chunk still Final, no superseded_by.
    let ps = server.store().as_persistent().expect("persistent");
    let resolved = ps
        .get_with_lifecycle(&tenant("t"), &id_a)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.status, ChunkStatus::Final);
    assert!(resolved.lifecycle.superseded_by.is_none());
}

#[tokio::test]
async fn find_near_duplicates_returns_fuzzy_with_similarity_score() {
    let (server, _tmp) = test_server().await;
    let id = add_chunk(&server, "t", "Release freeze begins Thursday.").await;

    let r = call_tool(
        &server,
        "memory.find_near_duplicates",
        serde_json::json!({
            "tenant_id": "t",
            "text": "Release freeze begins on Thursday.",
            "type": "doc",
            "fuzzy_threshold": 0.80,
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let fuzzy = body["fuzzy_matches"]
        .as_array()
        .expect("fuzzy_matches array");
    assert_eq!(fuzzy.len(), 1, "single fuzzy match expected");
    let entry = &fuzzy[0];
    assert_eq!(entry["chunk_id"].as_str().unwrap(), id.to_string());
    let sim = entry["similarity"].as_f64().expect("similarity number");
    assert!(
        (0.80..=1.0).contains(&sim),
        "similarity must clear the requested threshold (got {sim})"
    );
}

#[tokio::test]
async fn find_near_duplicates_respects_project_scope() {
    let (server, _tmp) = test_server().await;
    add_chunk(&server, "t", "shared text").await;
    // Different project — must not be exact-matched when scope=project
    // is requested via project_id.
    call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "other",
            "text": "shared text",
            "type": "doc",
        }),
    )
    .await;

    let r = call_tool(
        &server,
        "memory.find_near_duplicates",
        serde_json::json!({
            "tenant_id": "t",
            "project_id": "scope_a",  // distinct project
            "text": "shared text",
            "type": "doc",
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let exact = body["exact_matches"].as_array().expect("exact_matches");
    assert!(
        exact.is_empty(),
        "scope=project (default) must not bridge across projects"
    );
}

// ---------- Track D4 ----------
//
// `memory.add_batch` accepts the same `supersede_near_duplicates`
// knob and applies it per chunk. Response gains a parallel
// `superseded_ids` array of arrays (per chunk).

#[tokio::test]
async fn add_batch_respects_supersede_near_duplicates_per_chunk() {
    use memd::types::{ChunkId, ChunkStatus};
    let (server, _tmp) = test_server().await;

    // Seed: two heads in p1 with distinct canonicals.
    let r1 = call_tool(
        &server,
        "memory.add_batch",
        serde_json::json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "Release freeze begins Thursday.", "type": "doc", "project_id": "p1" },
                { "text": "Migration approved by legal.",     "type": "doc", "project_id": "p1" },
            ],
        }),
    )
    .await;
    let body1 = parse_result_text(&r1);
    let ids1: Vec<String> = body1["chunk_ids"]
        .as_array()
        .expect("chunk_ids")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids1.len(), 2);
    let id_freeze_old = ChunkId::parse(&ids1[0]).unwrap();
    let _id_migration_old = ChunkId::parse(&ids1[1]).unwrap();

    // Now add a batch where:
    //   - chunk[0] paraphrases the freeze chunk (canonical match)
    //   - chunk[1] is brand new (no candidate)
    //   - chunk[2] paraphrases the migration chunk (canonical match)
    let r2 = call_tool(
        &server,
        "memory.add_batch",
        serde_json::json!({
            "tenant_id": "t",
            "supersede_near_duplicates": { "mode": "exact" },
            "chunks": [
                { "text": "release freeze begins thursday.", "type": "doc", "project_id": "p1" },
                { "text": "Brand new note.",                  "type": "doc", "project_id": "p1" },
                { "text": "migration approved by legal.",     "type": "doc", "project_id": "p1" },
            ],
        }),
    )
    .await;
    let body2 = parse_result_text(&r2);
    let new_ids: Vec<String> = body2["chunk_ids"]
        .as_array()
        .expect("chunk_ids")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let supersedes: Vec<Vec<String>> = body2["superseded_ids"]
        .as_array()
        .expect("superseded_ids parallel array")
        .iter()
        .map(|inner| {
            inner
                .as_array()
                .expect("inner array")
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        })
        .collect();
    assert_eq!(new_ids.len(), 3);
    assert_eq!(supersedes.len(), 3);
    // Chunk 0 paraphrase: must supersede the original freeze chunk.
    assert_eq!(supersedes[0], vec![id_freeze_old.to_string()]);
    // Chunk 1 brand new: no candidates.
    assert!(supersedes[1].is_empty());
    // Chunk 2 paraphrase: must supersede the original migration chunk.
    assert_eq!(supersedes[2], vec![ids1[1].clone()]);

    // Old freeze chunk now Superseded.
    let ps = server.store().as_persistent().expect("persistent");
    let resolved_old = ps
        .get_with_lifecycle(&tenant("t"), &id_freeze_old)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved_old.status, ChunkStatus::Superseded);
}

// Codex round-2 D3/D4 MEDIUM regression: fuzzy dedup with
// scope=project on an unscoped (project_id=None) chunk must not be
// evicted by recent project-scoped writes. Pre-fix the SQL helper
// pulled the most recent N rows tenant-wide and post-filtered to
// NULL project, so a valid older NULL-project candidate could be
// dropped entirely if N project-scoped rows arrived after it.
#[tokio::test]
async fn add_with_fuzzy_dedup_null_project_not_evicted_by_other_projects() {
    use memd::types::{ChunkId, ChunkStatus};
    let (server, _tmp) = test_server().await;

    // Seed 1: the NULL-project candidate. Add it FIRST so it is
    // older than the noise that follows.
    let r0 = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "Release freeze begins Thursday.",
            "type": "doc",
        }),
    )
    .await;
    let id_old =
        ChunkId::parse(parse_result_text(&r0)["chunk_id"].as_str().unwrap()).expect("valid id");

    // Seed 2: dump enough recent project-scoped rows to bury the
    // NULL-project candidate under FUZZY_RECENT_POOL_SIZE if the
    // SQL pre-filter is wrong.
    for i in 0..130 {
        call_tool(
            &server,
            "memory.add",
            serde_json::json!({
                "tenant_id": "t",
                "project_id": "noise",
                "text": format!("noise chunk {i}"),
                "type": "doc",
            }),
        )
        .await;
    }

    // Now insert a NULL-project chunk that should fuzzy-match the
    // original NULL-project seed. With scope=project, the match must
    // succeed — the SQL pre-filter has to honour project_id IS NULL.
    let r = call_tool(
        &server,
        "memory.add",
        serde_json::json!({
            "tenant_id": "t",
            "text": "Release freeze begins on Thursday.",
            "type": "doc",
            "supersede_near_duplicates": { "mode": "fuzzy", "threshold": 0.80 },
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let supersedes: Vec<String> = body["superseded_ids"]
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        supersedes,
        vec![id_old.to_string()],
        "NULL-project fuzzy candidate must not be evicted by recent project-scoped traffic"
    );

    // Confirm the original is now Superseded.
    let ps = server.store().as_persistent().expect("persistent");
    let resolved = ps
        .get_with_lifecycle(&tenant("t"), &id_old)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.status, ChunkStatus::Superseded);
}

// Codex round-2 D3/D4 LOW: D4 lacks coverage of lifecycle overlay
// preservation on the matched-dedup path. Mirror D3's regression.
#[tokio::test]
async fn add_batch_with_dedup_preserves_lifecycle_overlay_on_match() {
    use memd::types::ChunkId;
    let (server, _tmp) = test_server().await;
    call_tool(
        &server,
        "memory.add_batch",
        serde_json::json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "Release freeze begins Thursday.", "type": "doc", "project_id": "p1" },
            ],
        }),
    )
    .await;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let want_expires = now_ms + 60_000;
    let r = call_tool(
        &server,
        "memory.add_batch",
        serde_json::json!({
            "tenant_id": "t",
            "supersede_near_duplicates": { "mode": "exact" },
            "chunks": [
                {
                    "text": "release freeze begins thursday.",
                    "type": "doc",
                    "project_id": "p1",
                    "expires_at_ms": want_expires,
                },
            ],
        }),
    )
    .await;
    let body = parse_result_text(&r);
    let new_id = ChunkId::parse(
        body["chunk_ids"]
            .as_array()
            .unwrap()
            .first()
            .unwrap()
            .as_str()
            .unwrap(),
    )
    .expect("valid id");

    let ps = server.store().as_persistent().expect("persistent");
    let resolved = ps
        .get_with_lifecycle(&tenant("t"), &new_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        resolved.lifecycle.expires_at_ms,
        Some(want_expires),
        "batch dedup must preserve per-chunk expires_at_ms on the match path"
    );
}

#[tokio::test]
async fn add_batch_without_dedup_keeps_legacy_response_shape() {
    let (server, _tmp) = test_server().await;
    let r = call_tool(
        &server,
        "memory.add_batch",
        serde_json::json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "alpha", "type": "doc" },
                { "text": "beta",  "type": "doc" },
            ],
        }),
    )
    .await;
    let body = parse_result_text(&r);
    assert!(body["chunk_ids"].as_array().is_some(), "chunk_ids present");
    assert!(
        body.get("superseded_ids").is_none(),
        "no dedup → no superseded_ids field (legacy shape)"
    );
}

// D2 round-2 LOW: backfill must handle many rows (not just one) and
// must repopulate every NULL row in a single pass. This version stays
// within a single tenant; see
// `backfill_canonical_text_repopulates_many_rows_across_tenants` for
// the multi-tenant regression that pins Item 2 (cross-tenant UNIQUE
// fix).
#[cfg(feature = "test-support")]
#[tokio::test]
async fn backfill_canonical_text_repopulates_many_legacy_rows_in_one_pass() {
    let (server, _tmp) = test_server().await;

    let mut ids = Vec::new();
    let texts = ["Apple Pie", "Banana Bread", "Cherry Cobbler", "Date Loaf"];
    for text in &texts {
        ids.push(add_chunk(&server, "tenant_a", text).await);
    }

    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");

    for id in &ids {
        ps.metadata()
            .force_clear_canonical_text(id)
            .expect("force_clear_canonical_text");
    }

    let stats = ps.backfill_canonical_text_for_legacy_chunks();
    assert_eq!(
        stats.rows_backfilled,
        texts.len(),
        "every legacy row must be repopulated in a single pass"
    );
    assert_eq!(stats.rows_skipped, 0);

    let want = ["apple pie", "banana bread", "cherry cobbler", "date loaf"];
    for (id, expected) in ids.iter().zip(want.iter()) {
        let meta = ps
            .metadata()
            .get(&tenant("tenant_a"), id)
            .expect("metadata.get")
            .expect("row");
        assert_eq!(meta.canonical_text.as_deref(), Some(*expected));
    }
}

// Item 2 regression: after the chunks UNIQUE constraint is scoped to
// (tenant_id, segment_id, ordinal), multiple tenants writing the same
// (segment_id, ordinal) pair must coexist, and backfill must
// repopulate their canonical_text independently — the legacy global
// UNIQUE would have silently overwritten one tenant's row on INSERT
// OR REPLACE. This test writes multiple chunks per tenant across two
// tenants (so each tenant's `next_segment_id()` allocator produces
// its own starting segment), clears their canonical_text, and
// verifies that backfill repopulates every row across both tenants.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn backfill_canonical_text_repopulates_many_rows_across_tenants() {
    let (server, _tmp) = test_server().await;

    let mut ids_a = Vec::new();
    let mut ids_b = Vec::new();
    let texts_a = ["Apple Pie", "Banana Bread", "Cherry Cobbler"];
    let texts_b = ["Date Loaf", "Eggnog", "Fig Cake"];
    for text in &texts_a {
        ids_a.push(add_chunk(&server, "tenant_a", text).await);
    }
    for text in &texts_b {
        ids_b.push(add_chunk(&server, "tenant_b", text).await);
    }

    let ps = server
        .store()
        .as_persistent()
        .expect("test_server uses PersistentStore");

    for id in ids_a.iter().chain(ids_b.iter()) {
        ps.metadata()
            .force_clear_canonical_text(id)
            .expect("force_clear_canonical_text");
    }

    let stats = ps.backfill_canonical_text_for_legacy_chunks();
    assert_eq!(
        stats.rows_backfilled,
        texts_a.len() + texts_b.len(),
        "every legacy row across both tenants must be repopulated in one pass"
    );
    assert_eq!(stats.rows_skipped, 0);

    let want_a = ["apple pie", "banana bread", "cherry cobbler"];
    let want_b = ["date loaf", "eggnog", "fig cake"];
    for (id, expected) in ids_a.iter().zip(want_a.iter()) {
        let meta = ps
            .metadata()
            .get(&tenant("tenant_a"), id)
            .expect("metadata.get")
            .expect("tenant_a row survives both independent segment allocation AND backfill");
        assert_eq!(meta.canonical_text.as_deref(), Some(*expected));
    }
    for (id, expected) in ids_b.iter().zip(want_b.iter()) {
        let meta = ps
            .metadata()
            .get(&tenant("tenant_b"), id)
            .expect("metadata.get")
            .expect("tenant_b row survives both independent segment allocation AND backfill");
        assert_eq!(meta.canonical_text.as_deref(), Some(*expected));
    }
}
