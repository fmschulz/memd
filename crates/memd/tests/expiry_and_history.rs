//! Integration tests for Track C (temporal overlay + lazy hiding +
//! expiry sweep + history promotion + memory.set_expiry).

mod common;

use common::*;
use memd::store::Store;
use memd::types::{ChunkId, ChunkStatus, TenantId};
use serde_json::json;

fn current_time_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_millis() as i64
}

#[tokio::test]
async fn memory_add_persists_temporal_overlay_fields() {
    let (server, _tmp) = test_server().await;
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "sprint note",
            "type": "doc",
            "expires_at_ms": 1_900_000_000_000_i64,
            "review_after_ms": 1_800_000_000_000_i64,
        }),
    )
    .await;
    let id_str = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();
    let id = ChunkId::parse(&id_str).expect("valid chunk id");
    let tenant = TenantId::new("t").expect("valid tenant id");
    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("get_with_lifecycle ok")
        .expect("chunk present");
    assert_eq!(resolved.lifecycle.expires_at_ms, Some(1_900_000_000_000));
    assert_eq!(resolved.lifecycle.review_after_ms, Some(1_800_000_000_000));
}

#[tokio::test]
async fn memory_add_without_temporal_fields_leaves_overlay_empty() {
    let (server, _tmp) = test_server().await;
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "plain note",
            "type": "doc",
        }),
    )
    .await;
    let id_str = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .expect("chunk_id")
        .to_string();
    let id = ChunkId::parse(&id_str).expect("valid chunk id");
    let tenant = TenantId::new("t").expect("valid tenant id");
    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("get_with_lifecycle ok")
        .expect("chunk present");
    assert!(resolved.lifecycle.expires_at_ms.is_none());
    assert!(resolved.lifecycle.review_after_ms.is_none());
}

/// Track C2: lazy retrieval hiding of expired chunks. C1 writes the
/// overlay field; C2 hides the row via `VisibilityPolicy::is_visible_at`
/// before the compaction sweep materialises `status=Expired`.
///
/// Exercises the full MCP path: `memory.add` with `expires_at_ms` set
/// in the past → `memory.search` → only the non-expired row surfaces.
/// `include_expired=true` must surface the hidden row again.
#[tokio::test]
async fn expired_chunks_are_hidden_at_retrieval_before_sweep_runs() {
    let (server, _tmp) = test_server().await;
    let past = 1_i64; // unambiguously before `now` on any sane clock.
    let _expiring_id = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "expiring note",
            "type": "doc",
            "expires_at_ms": past,
        }),
    )
    .await;
    let fresh_resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "fresh note",
            "type": "doc",
        }),
    )
    .await;
    let fresh_id = parse_result_text(&fresh_resp)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Default search — expired note must be hidden, fresh note visible.
    let resp = call_tool(
        &server,
        "memory.search",
        json!({ "tenant_id": "t", "query": "note", "k": 10 }),
    )
    .await;
    let results = parse_result_text(&resp)["results"]
        .as_array()
        .expect("results array")
        .clone();
    let ids: Vec<String> = results
        .iter()
        .filter_map(|r| r.get("chunk_id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert!(
        ids.contains(&fresh_id),
        "fresh note must surface: {ids:?}"
    );
    for r in &results {
        let txt = r.get("text").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !txt.contains("expiring"),
            "wall-clock-expired note must be hidden by default, got: {txt:?}"
        );
    }

    // include_expired=true surfaces the expired row.
    let resp = call_tool(
        &server,
        "memory.search",
        json!({
            "tenant_id": "t",
            "query": "note",
            "k": 10,
            "include_expired": true
        }),
    )
    .await;
    let results = parse_result_text(&resp)["results"]
        .as_array()
        .expect("results array")
        .clone();
    let texts: Vec<String> = results
        .iter()
        .filter_map(|r| r.get("text").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("expiring")),
        "include_expired=true must surface the expired note: {texts:?}"
    );
}

/// Sanity check that future `expires_at_ms` values do NOT hide a chunk.
#[tokio::test]
async fn future_expiry_does_not_hide_chunk_before_the_deadline() {
    let (server, _tmp) = test_server().await;
    let future = current_time_ms() + 1_000 * 60 * 60 * 24; // 1 day from now
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "future deadline note",
            "type": "doc",
            "expires_at_ms": future,
        }),
    )
    .await;
    let id = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = call_tool(
        &server,
        "memory.search",
        json!({ "tenant_id": "t", "query": "deadline", "k": 10 }),
    )
    .await;
    let results = parse_result_text(&resp)["results"]
        .as_array()
        .expect("results array")
        .clone();
    let ids: Vec<String> = results
        .iter()
        .filter_map(|r| r.get("chunk_id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert!(
        ids.contains(&id),
        "chunk with future expiry must remain visible: {ids:?}"
    );
}

/// Track C3: `ExpirySweep` materialises `status=Expired` for rows whose
/// `expires_at_ms <= now_ms`.
///
/// Verifies both the count reported by the sweep and that the row's
/// authoritative status flips from `Final` to `Expired` on the overlay.
///
/// Note: `test_server()` constructs the `PersistentStore` with hybrid
/// search disabled, so `store.hybrid()` is `None` here and the sweep's
/// cache-bump branch is not directly exercised — that path is tested
/// separately in `expiry_sweep_bumps_hybrid_cache_when_enabled` below.
#[tokio::test]
async fn expiry_sweep_marks_rows_expired() {
    use memd::compaction::ExpirySweep;

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    let past = 1_i64;
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "stale note",
            "type": "doc",
            "expires_at_ms": past,
        }),
    )
    .await;
    let id_str = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = ChunkId::parse(&id_str).expect("valid chunk id");

    // Before: overlay is Final (C2 hides it from search but does not
    // flip the status).
    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(resolved.status, ChunkStatus::Final);

    let sweep = ExpirySweep::new();
    let result = sweep
        .run(
            server.store().metadata(),
            server.store().hybrid(),
            &tenant,
        )
        .expect("sweep ok");
    assert_eq!(result.expired_count, 1);

    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(
        resolved.status,
        ChunkStatus::Expired,
        "sweep must materialise status=Expired"
    );
    assert!(
        resolved.lifecycle.lifecycle_updated_at_ms > 0,
        "sweep must stamp lifecycle_updated_at_ms"
    );
}

/// Directly exercises the guarded UPDATE in `mark_expired_if_final`.
/// These cases bypass the pre-filter in `list_expired_before` so the
/// guard predicate (status='final' AND expires_at_ms <= now) is the
/// only thing stopping a wrong promotion.
#[tokio::test]
async fn mark_expired_if_final_rejects_non_final_rows() {
    use memd::store::metadata::MetadataStore;
    use memd::types::{ChunkStatus, LifecycleDelta};

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Seed a row that already has past expires_at_ms AND status=Superseded.
    // Simulates the state right after another writer superseded an
    // expired chunk. If the sweep called mark_expired_if_final anyway
    // (e.g. because the SELECT pre-filter was loosened), the guarded
    // UPDATE must still reject.
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "raced",
            "type": "doc",
            "expires_at_ms": 1_i64,
        }),
    )
    .await;
    let id = ChunkId::parse(parse_result_text(&resp)["chunk_id"].as_str().unwrap())
        .expect("valid chunk id");
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                ..Default::default()
            },
        )
        .expect("flip to superseded ok");

    let now = 10_000_i64;
    let promoted = server
        .store()
        .metadata()
        .mark_expired_if_final(&tenant, &id, now)
        .expect("mark ok");
    assert!(
        !promoted,
        "guarded UPDATE must reject rows whose status is not Final"
    );
    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(resolved.status, ChunkStatus::Superseded);
}

#[tokio::test]
async fn mark_expired_if_final_rejects_rows_whose_expiry_was_cleared() {
    use memd::store::metadata::MetadataStore;
    use memd::types::LifecycleDelta;

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Row was selected when it had past expires_at_ms, but before the
    // UPDATE lands another writer clears the expiry (pattern expected
    // from memory.set_expiry in Track C6 or from an in-flight retention
    // extension). The guard must refuse because the row is no longer
    // eligible at UPDATE time.
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "cleared",
            "type": "doc",
            "expires_at_ms": 1_i64,
        }),
    )
    .await;
    let id = ChunkId::parse(parse_result_text(&resp)["chunk_id"].as_str().unwrap())
        .expect("valid chunk id");

    // Concurrent writer clears expires_at_ms while leaving status=Final.
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                expires_at_ms: Some(None),
                ..Default::default()
            },
        )
        .expect("clear expiry ok");

    let now = 10_000_i64;
    let promoted = server
        .store()
        .metadata()
        .mark_expired_if_final(&tenant, &id, now)
        .expect("mark ok");
    assert!(
        !promoted,
        "guarded UPDATE must refuse when expires_at_ms was cleared before the UPDATE"
    );
}

#[tokio::test]
async fn mark_expired_if_final_rejects_rows_whose_expiry_moved_to_future() {
    use memd::store::metadata::MetadataStore;
    use memd::types::LifecycleDelta;

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Row started with past expiry but was just extended to a future
    // expiry. The guard must refuse at UPDATE time.
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "extended",
            "type": "doc",
            "expires_at_ms": 1_i64,
        }),
    )
    .await;
    let id = ChunkId::parse(parse_result_text(&resp)["chunk_id"].as_str().unwrap())
        .expect("valid chunk id");

    let future = current_time_ms() + 1_000 * 60 * 60 * 24;
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                expires_at_ms: Some(Some(future)),
                ..Default::default()
            },
        )
        .expect("extend expiry ok");

    let now = 10_000_i64;
    let promoted = server
        .store()
        .metadata()
        .mark_expired_if_final(&tenant, &id, now)
        .expect("mark ok");
    assert!(
        !promoted,
        "guarded UPDATE must refuse when expires_at_ms was pushed past the UPDATE's clock"
    );
}

/// Idempotency: a second sweep immediately after the first must return
/// 0 because the Expired rows were filtered out of `list_expired_before`.
#[tokio::test]
async fn expiry_sweep_is_idempotent_across_consecutive_runs() {
    use memd::compaction::ExpirySweep;

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "stale 1",
            "type": "doc",
            "expires_at_ms": 1_i64,
        }),
    )
    .await;
    call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "stale 2",
            "type": "doc",
            "expires_at_ms": 1_i64,
        }),
    )
    .await;

    let sweep = ExpirySweep::new();
    let r1 = sweep
        .run(
            server.store().metadata(),
            server.store().hybrid(),
            &tenant,
        )
        .expect("first sweep ok");
    assert_eq!(r1.expired_count, 2);

    let r2 = sweep
        .run(
            server.store().metadata(),
            server.store().hybrid(),
            &tenant,
        )
        .expect("second sweep ok");
    assert_eq!(
        r2.expired_count, 0,
        "already-expired rows must not be re-swept"
    );
}

/// Track C4: `HistoryPromotion` uses `lifecycle_updated_at_ms` as the
/// clock, not `timestamp_created`. A row that was written a long time
/// ago but whose overlay was touched recently (e.g. superseded today)
/// should NOT be promoted until its overlay has been idle for the
/// threshold window.
#[tokio::test]
async fn history_promotion_uses_lifecycle_updated_clock_not_created() {
    use memd::compaction::HistoryPromotion;
    use memd::store::metadata::MetadataStore;
    use memd::types::{ChunkStatus, LifecycleDelta, MemoryTier};

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Seed a superseded row whose overlay was JUST touched — this
    // simulates "superseded today". The promotion must NOT fire.
    let resp = call_tool(
        &server,
        "memory.add",
        json!({ "tenant_id": "t", "text": "active-history", "type": "doc" }),
    )
    .await;
    let id = ChunkId::parse(parse_result_text(&resp)["chunk_id"].as_str().unwrap())
        .expect("valid chunk id");
    let now = current_time_ms();
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                lifecycle_updated_at_ms: Some(now),
                ..Default::default()
            },
        )
        .expect("supersede ok");

    let promo = HistoryPromotion::new(30 * 86_400_000);
    let r1 = promo
        .run(
            server.store().metadata(),
            server.store().hybrid(),
            &tenant,
        )
        .expect("run ok");
    assert_eq!(
        r1.promoted_count, 0,
        "fresh overlay clock must not be promoted"
    );

    // Fast-forward the overlay clock by backdating
    // `lifecycle_updated_at_ms` past the threshold. The sweep must now
    // demote the row.
    let old = now - 31 * 86_400_000;
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                lifecycle_updated_at_ms: Some(old),
                ..Default::default()
            },
        )
        .expect("backdate ok");

    let r2 = promo
        .run(
            server.store().metadata(),
            server.store().hybrid(),
            &tenant,
        )
        .expect("run ok");
    assert_eq!(
        r2.promoted_count, 1,
        "stale overlay must be demoted to History"
    );

    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(resolved.lifecycle.tier, MemoryTier::History);
}

/// A second run immediately after a successful promotion must be a
/// no-op because `list_stale_superseded` excludes rows already on the
/// `history` tier.
#[tokio::test]
async fn history_promotion_is_idempotent_across_consecutive_runs() {
    use memd::compaction::HistoryPromotion;
    use memd::store::metadata::MetadataStore;
    use memd::types::{ChunkStatus, LifecycleDelta};

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    let resp = call_tool(
        &server,
        "memory.add",
        json!({ "tenant_id": "t", "text": "old", "type": "doc" }),
    )
    .await;
    let id = ChunkId::parse(parse_result_text(&resp)["chunk_id"].as_str().unwrap())
        .expect("valid chunk id");

    // Flip to superseded AND backdate the overlay clock past the
    // threshold in a single update.
    let old = current_time_ms() - 365 * 86_400_000;
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                lifecycle_updated_at_ms: Some(old),
                ..Default::default()
            },
        )
        .expect("flip ok");

    let promo = HistoryPromotion::new(30 * 86_400_000);
    let r1 = promo
        .run(
            server.store().metadata(),
            server.store().hybrid(),
            &tenant,
        )
        .expect("run ok");
    assert_eq!(r1.promoted_count, 1);

    let r2 = promo
        .run(
            server.store().metadata(),
            server.store().hybrid(),
            &tenant,
        )
        .expect("run ok");
    assert_eq!(
        r2.promoted_count, 0,
        "already-history rows must not be re-promoted"
    );
}

/// Directly exercises the guarded UPDATE in
/// `promote_to_history_if_stale` with each failure mode (non-stale
/// status, already-on-history tier, freshly refreshed overlay). These
/// cases bypass the `list_stale_superseded` pre-filter so the guard
/// predicate is the only thing preventing a wrong promotion.
#[tokio::test]
async fn promote_to_history_if_stale_rejects_rows_whose_overlay_was_refreshed() {
    use memd::store::metadata::MetadataStore;
    use memd::types::{ChunkStatus, LifecycleDelta, MemoryTier};

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Seed a Superseded row with old overlay clock; then simulate a
    // concurrent writer refreshing lifecycle_updated_at_ms just
    // before the promote UPDATE would fire.
    let resp = call_tool(
        &server,
        "memory.add",
        json!({ "tenant_id": "t", "text": "raced-history", "type": "doc" }),
    )
    .await;
    let id = ChunkId::parse(parse_result_text(&resp)["chunk_id"].as_str().unwrap())
        .expect("valid chunk id");

    let now = current_time_ms();
    let old = now - 365 * 86_400_000;
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                lifecycle_updated_at_ms: Some(old),
                ..Default::default()
            },
        )
        .expect("seed ok");

    // Concurrent refresh: bump lifecycle_updated_at_ms to now.
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                lifecycle_updated_at_ms: Some(now),
                ..Default::default()
            },
        )
        .expect("refresh ok");

    // Call promote directly with the cutoff the sweep would have
    // computed. The guard must refuse because the row is no longer
    // older than the cutoff.
    let cutoff = now - 30 * 86_400_000;
    let promoted = server
        .store()
        .metadata()
        .promote_to_history_if_stale(&tenant, &id, cutoff, now)
        .expect("promote ok");
    assert!(
        !promoted,
        "guarded UPDATE must refuse rows whose overlay was refreshed past the cutoff"
    );

    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert_ne!(
        resolved.lifecycle.tier,
        MemoryTier::History,
        "sweep must not demote a row that was just refreshed"
    );
}

#[tokio::test]
async fn promote_to_history_if_stale_rejects_non_superseded_rows() {
    use memd::store::metadata::MetadataStore;
    use memd::types::{ChunkStatus, LifecycleDelta};

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Row is Final (not Superseded or Expired) — must not be promoted.
    let resp = call_tool(
        &server,
        "memory.add",
        json!({ "tenant_id": "t", "text": "still-active", "type": "doc" }),
    )
    .await;
    let id = ChunkId::parse(parse_result_text(&resp)["chunk_id"].as_str().unwrap())
        .expect("valid chunk id");
    // Stamp an old lifecycle clock without changing status.
    let old = current_time_ms() - 365 * 86_400_000;
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                lifecycle_updated_at_ms: Some(old),
                ..Default::default()
            },
        )
        .expect("stamp ok");

    let now = current_time_ms();
    let cutoff = now - 30 * 86_400_000;
    let promoted = server
        .store()
        .metadata()
        .promote_to_history_if_stale(&tenant, &id, cutoff, now)
        .expect("promote ok");
    assert!(
        !promoted,
        "guarded UPDATE must refuse Final rows even when their overlay clock is old"
    );

    // Sanity: status stays Final.
    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(resolved.status, ChunkStatus::Final);
}

/// Expired rows (not just Superseded) should also be candidates for
/// history promotion once their overlay has been idle long enough.
#[tokio::test]
async fn history_promotion_also_demotes_long_stale_expired_rows() {
    use memd::compaction::HistoryPromotion;
    use memd::store::metadata::MetadataStore;
    use memd::types::{ChunkStatus, LifecycleDelta, MemoryTier};

    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    let resp = call_tool(
        &server,
        "memory.add",
        json!({ "tenant_id": "t", "text": "long-expired", "type": "doc" }),
    )
    .await;
    let id = ChunkId::parse(parse_result_text(&resp)["chunk_id"].as_str().unwrap())
        .expect("valid chunk id");

    // Simulate ExpirySweep (C3) having run a long time ago.
    let old = current_time_ms() - 365 * 86_400_000;
    server
        .store()
        .metadata()
        .update_lifecycle(
            &tenant,
            &id,
            &LifecycleDelta {
                status: Some(ChunkStatus::Expired),
                lifecycle_updated_at_ms: Some(old),
                ..Default::default()
            },
        )
        .expect("flip ok");

    let promo = HistoryPromotion::new(30 * 86_400_000);
    let result = promo
        .run(
            server.store().metadata(),
            server.store().hybrid(),
            &tenant,
        )
        .expect("run ok");
    assert_eq!(result.promoted_count, 1);

    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(resolved.lifecycle.tier, MemoryTier::History);
}

/// Track C5: `CompactionRunner::run_compaction` runs `ExpirySweep` and
/// `HistoryPromotion` before the existing HNSW/segment/cache phases and
/// surfaces their outputs on `CompactionResult.expired_count` and
/// `CompactionResult.promoted_count`.
///
/// This test builds a `CompactionRunner` directly and drives it with a
/// `SqliteMetadataStore` and a `MockEmbedder`-backed `DenseSearcher` so
/// the dense/HNSW pipeline can initialise without a real model.
#[tokio::test]
async fn compaction_runs_expiry_sweep_and_history_promotion() {
    use memd::compaction::{CompactionConfig, CompactionRunner};
    use memd::embeddings::MockEmbedder;
    use memd::store::dense::{DenseSearchConfig, DenseSearcher};
    use memd::store::metadata::MetadataStore;
    use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
    use memd::types::{ChunkStatus, LifecycleDelta, MemoryChunk, MemoryTier, ChunkType};
    use std::sync::Arc;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(cfg).expect("persistent store");
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Seed one row whose retention window elapsed (ExpirySweep should
    // flip it to Expired) and one row that was superseded long ago
    // (HistoryPromotion should move it to History tier).
    let expiring = store
        .add(MemoryChunk::new(tenant.clone(), "expiring", ChunkType::Doc))
        .await
        .expect("add ok");
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &expiring,
            &LifecycleDelta {
                expires_at_ms: Some(Some(1_i64)),
                ..Default::default()
            },
        )
        .expect("overlay ok");

    let old_superseded = store
        .add(MemoryChunk::new(tenant.clone(), "old superseded", ChunkType::Doc))
        .await
        .expect("add ok");
    let long_ago = current_time_ms() - 365 * 86_400_000;
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &old_superseded,
            &LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                lifecycle_updated_at_ms: Some(long_ago),
                ..Default::default()
            },
        )
        .expect("flip ok");

    // Construct a CompactionRunner with default Track C5 config (both
    // sweeps enabled, 90-day history threshold). DenseSearcher uses a
    // MockEmbedder so no external model is required.
    let runner = CompactionRunner::new(CompactionConfig::default());
    let embedder = Arc::new(MockEmbedder::new());
    let dense = DenseSearcher::with_embedder(
        embedder,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    );

    let result = runner
        .run_compaction(&tenant, store.metadata(), &dense, None, None, None)
        .expect("run_compaction ok");

    assert_eq!(result.expired_count, 1, "ExpirySweep must have fired");
    assert_eq!(
        result.promoted_count, 1,
        "HistoryPromotion must have fired"
    );

    // Verify the two rows now reflect the expected overlay state.
    let r_expired = store
        .get_with_lifecycle(&tenant, &expiring)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r_expired.status, ChunkStatus::Expired);

    let r_promoted = store
        .get_with_lifecycle(&tenant, &old_superseded)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r_promoted.lifecycle.tier, MemoryTier::History);
}

/// Disabling either sweep via config must make that phase a no-op.
#[tokio::test]
async fn compaction_skips_sweeps_when_disabled_via_config() {
    use memd::compaction::{CompactionConfig, CompactionRunner};
    use memd::embeddings::MockEmbedder;
    use memd::store::dense::{DenseSearchConfig, DenseSearcher};
    use memd::store::metadata::MetadataStore;
    use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
    use memd::types::{ChunkStatus, LifecycleDelta, MemoryChunk, ChunkType};
    use std::sync::Arc;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = PersistentStoreConfig {
        data_dir: tmp.path().to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    };
    let store = PersistentStore::open(cfg).expect("persistent store");
    let tenant = TenantId::new("t").expect("valid tenant id");

    let expiring = store
        .add(MemoryChunk::new(tenant.clone(), "expiring", ChunkType::Doc))
        .await
        .expect("add ok");
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &expiring,
            &LifecycleDelta {
                expires_at_ms: Some(Some(1_i64)),
                ..Default::default()
            },
        )
        .expect("overlay ok");

    let old_superseded = store
        .add(MemoryChunk::new(tenant.clone(), "old", ChunkType::Doc))
        .await
        .expect("add ok");
    let long_ago = current_time_ms() - 365 * 86_400_000;
    store
        .metadata()
        .update_lifecycle(
            &tenant,
            &old_superseded,
            &LifecycleDelta {
                status: Some(ChunkStatus::Superseded),
                lifecycle_updated_at_ms: Some(long_ago),
                ..Default::default()
            },
        )
        .expect("flip ok");

    let disabled_cfg = CompactionConfig {
        expiry_sweep_enabled: false,
        history_promotion_enabled: false,
        ..Default::default()
    };
    let runner = CompactionRunner::new(disabled_cfg);
    let embedder = Arc::new(MockEmbedder::new());
    let dense = DenseSearcher::with_embedder(
        embedder,
        DenseSearchConfig {
            persist: false,
            ..Default::default()
        },
    );

    let result = runner
        .run_compaction(&tenant, store.metadata(), &dense, None, None, None)
        .expect("run_compaction ok");

    assert_eq!(result.expired_count, 0, "sweep must not run when disabled");
    assert_eq!(
        result.promoted_count, 0,
        "promotion must not run when disabled"
    );
    // The rows must still be in their original (non-terminal) state.
    let r1 = store.get_with_lifecycle(&tenant, &expiring).await.unwrap().unwrap();
    assert_eq!(r1.status, ChunkStatus::Final);
}

/// Track C6: `memory.set_expiry` updates the overlay and bumps the
/// tenant cache version.
#[tokio::test]
async fn memory_set_expiry_updates_overlay() {
    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Seed a chunk without any temporal overlay.
    let resp = call_tool(
        &server,
        "memory.add",
        json!({ "tenant_id": "t", "text": "needs expiry", "type": "doc" }),
    )
    .await;
    let id_str = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = ChunkId::parse(&id_str).expect("valid chunk id");

    // Set expires_at_ms and review_after_ms via memory.set_expiry.
    let resp = call_tool(
        &server,
        "memory.set_expiry",
        json!({
            "tenant_id": "t",
            "chunk_id": id_str,
            "expires_at_ms": 1_900_000_000_000_i64,
            "review_after_ms": 1_800_000_000_000_i64,
        }),
    )
    .await;
    let body = parse_result_text(&resp);
    assert_eq!(body["updated"], true);

    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert_eq!(resolved.lifecycle.expires_at_ms, Some(1_900_000_000_000));
    assert_eq!(resolved.lifecycle.review_after_ms, Some(1_800_000_000_000));
}

/// `null` on an overlay field must clear it; absent must leave it alone.
#[tokio::test]
async fn memory_set_expiry_triple_state_clear_vs_leave() {
    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Start with both fields set via memory.add.
    let resp = call_tool(
        &server,
        "memory.add",
        json!({
            "tenant_id": "t",
            "text": "both set",
            "type": "doc",
            "expires_at_ms": 1_900_000_000_000_i64,
            "review_after_ms": 1_800_000_000_000_i64,
        }),
    )
    .await;
    let id_str = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = ChunkId::parse(&id_str).expect("valid chunk id");

    // Clear expires_at_ms (null) and leave review_after_ms alone (absent).
    call_tool(
        &server,
        "memory.set_expiry",
        json!({
            "tenant_id": "t",
            "chunk_id": id_str,
            "expires_at_ms": null,
        }),
    )
    .await;

    let resolved = server
        .store()
        .get_with_lifecycle(&tenant, &id)
        .await
        .expect("ok")
        .expect("present");
    assert!(
        resolved.lifecycle.expires_at_ms.is_none(),
        "expires_at_ms must be cleared by explicit null"
    );
    assert_eq!(
        resolved.lifecycle.review_after_ms,
        Some(1_800_000_000_000),
        "review_after_ms must be untouched when absent from payload"
    );
}

/// Payload with neither field must be rejected as a no-op.
#[tokio::test]
async fn memory_set_expiry_rejects_empty_payload() {
    let (server, _tmp) = test_server().await;
    let resp = call_tool(
        &server,
        "memory.add",
        json!({ "tenant_id": "t", "text": "x", "type": "doc" }),
    )
    .await;
    let id = parse_result_text(&resp)["chunk_id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = call_tool(
        &server,
        "memory.set_expiry",
        json!({ "tenant_id": "t", "chunk_id": id }),
    )
    .await;
    assert!(
        parse_error(&resp).is_some(),
        "memory.set_expiry with neither field set must return an error, got: {resp}"
    );
}

#[tokio::test]
async fn memory_add_batch_validation_failure_leaves_no_partial_writes() {
    // When any chunk in a lifecycle-enabled batch fails validation,
    // no rows should be written — validation must run before the first
    // store write. Regression test for Codex C1 round-1 HIGH.
    let (server, _tmp) = test_server().await;
    let tenant = TenantId::new("t").expect("valid tenant id");

    // Count rows already present in this tenant (new tempdir = 0).
    let before = server
        .store()
        .list_chunks(&tenant, 1024, 0)
        .await
        .expect("list ok")
        .len();

    // First chunk is valid + carries temporal fields (forces the
    // lifecycle path). Second chunk uses an invalid episode_id that
    // validate_episode_id rejects, so the whole batch must fail before
    // any chunk is persisted.
    let resp = call_tool(
        &server,
        "memory.add_batch",
        json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "first",  "type": "doc", "expires_at_ms": 1_900_000_000_000_i64 },
                { "text": "second", "type": "doc", "episode_id": "bad id with spaces" }
            ]
        }),
    )
    .await;
    assert!(
        parse_error(&resp).is_some(),
        "batch should return an error when any chunk fails validation, got: {resp}"
    );

    let after = server
        .store()
        .list_chunks(&tenant, 1024, 0)
        .await
        .expect("list ok")
        .len();
    assert_eq!(
        before, after,
        "no chunks must be persisted when batch validation fails"
    );
}

#[tokio::test]
async fn memory_add_batch_persists_temporal_overlay_fields_per_chunk() {
    let (server, _tmp) = test_server().await;
    let resp = call_tool(
        &server,
        "memory.add_batch",
        json!({
            "tenant_id": "t",
            "chunks": [
                { "text": "with expiry", "type": "doc", "expires_at_ms": 1_900_000_000_000_i64 },
                { "text": "plain",        "type": "doc" }
            ]
        }),
    )
    .await;
    let ids = parse_result_text(&resp)["chunk_ids"]
        .as_array()
        .expect("chunk_ids")
        .clone();
    assert_eq!(ids.len(), 2);
    let tenant = TenantId::new("t").expect("valid tenant id");
    let id_a = ChunkId::parse(ids[0].as_str().unwrap()).expect("valid chunk id");
    let id_b = ChunkId::parse(ids[1].as_str().unwrap()).expect("valid chunk id");
    let ra = server
        .store()
        .get_with_lifecycle(&tenant, &id_a)
        .await
        .unwrap()
        .unwrap();
    let rb = server
        .store()
        .get_with_lifecycle(&tenant, &id_b)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ra.lifecycle.expires_at_ms, Some(1_900_000_000_000));
    assert!(ra.lifecycle.review_after_ms.is_none());
    assert!(rb.lifecycle.expires_at_ms.is_none());
    assert!(rb.lifecycle.review_after_ms.is_none());
}
