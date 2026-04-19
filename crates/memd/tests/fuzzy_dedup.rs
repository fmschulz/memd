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
