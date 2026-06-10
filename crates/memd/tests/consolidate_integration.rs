//! Phase 2 end-to-end test: 12 near-duplicate chunks are consolidated
//! into 3 `kind:consolidated` lessons, the 12 sources are
//! soft-tombstoned, and retrieval surfaces the consolidated lessons
//! while excluding the superseded raw chunks (the same visibility
//! filter `memd memory-md` relies on).
//!
//! The consolidator is the hermetic `mock` backend
//! (`MEMD_CONSOLIDATOR=mock`), so no real LLM CLI is spawned. This
//! file holds a single primary test because it mutates process-global
//! environment variables.

use memd::cli::{run_cli, CliCommand};
use memd::mcp::{handle_memory_search, SearchParams};
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::Store;
use memd::types::ChunkStatus;
use memd::{ChunkId, ChunkType, MemoryChunk, ProjectId, TenantId};
use serde_json::Value;
use tempfile::tempdir;

fn open_store(dir: &std::path::Path) -> PersistentStore {
    let cfg = PersistentStoreConfig {
        data_dir: dir.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    };
    PersistentStore::open(cfg).expect("open persistent store")
}

/// Chunk ids in the visibility-filtered result set for `query`.
fn hit_ids(response: &Value) -> Vec<String> {
    let text = response
        .get("content")
        .and_then(|c| c.get(0))
        .and_then(|i| i.get("text"))
        .and_then(Value::as_str)
        .expect("search response content[0].text");
    let parsed: Value = serde_json::from_str(text).expect("result text is JSON");
    parsed
        .get("results")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("chunk_id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn consolidate_replaces_raw_chunks_and_excludes_superseded() {
    let dir = tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tenant = TenantId::new("t").unwrap();
    let project = "p";

    // 12 near-duplicate raw chunks. Every chunk shares the substring
    // "tenant scoped keys" so the hybrid-off text fallback retrieves
    // them all — that is what makes the post-consolidation exclusion a
    // real assertion rather than a vacuous one.
    let mut ids = Vec::new();
    for i in 0..12 {
        let id = store
            .add(
                MemoryChunk::new(
                    tenant.clone(),
                    format!("Run {i} hit the cache bug; the fix uses tenant scoped keys."),
                    ChunkType::Summary,
                )
                .with_project(ProjectId::from(project))
                .with_tags(vec![
                    "kind:finish".to_string(),
                    "ctx:subsystem:cache".to_string(),
                ]),
            )
            .await
            .unwrap();
        ids.push(id.to_string());
    }

    // Mock consolidator returns 3 lessons, each superseding 4 sources.
    let entries: Vec<Value> = (0..3)
        .map(|g| {
            let group: Vec<&String> = ids[g * 4..g * 4 + 4].iter().collect();
            serde_json::json!({
                "text": format!(
                    "Consolidated lesson {g}: the durable fix uses tenant scoped keys."
                ),
                "supersedes": group,
                "kind": "consolidated",
                "priority": 8,
            })
        })
        .collect();
    let response = serde_json::to_string(&entries).unwrap();

    std::env::set_var("MEMD_CONSOLIDATOR", "mock");
    std::env::set_var("MEMD_CONSOLIDATOR_MOCK_RESPONSE", &response);

    run_cli(
        &store,
        None,
        CliCommand::Consolidate {
            tenant_id: Some("t".to_string()),
            project_id: Some(project.to_string()),
            project_dir: dir.path().to_path_buf(),
            max_region: 50,
            dry_run: false,
            background: false,
            force: false,
            promote_to_shared: false,
            warm: memd::cli::WarmMode::Off,
        },
    )
    .await
    .expect("consolidate run");

    std::env::remove_var("MEMD_CONSOLIDATOR");
    std::env::remove_var("MEMD_CONSOLIDATOR_MOCK_RESPONSE");

    // 3 consolidated chunks were written, each with provenance.
    let all = store
        .list_chunks_for_project(&tenant, Some(project), 500, 0)
        .await
        .unwrap();
    let consolidated: Vec<&MemoryChunk> = all
        .iter()
        .filter(|c| c.tags.iter().any(|t| t == "kind:consolidated"))
        .collect();
    assert_eq!(consolidated.len(), 3, "expected 3 consolidated chunks");
    for chunk in &consolidated {
        assert!(
            chunk.tags.iter().any(|t| t.starts_with("supersedes:")),
            "consolidated chunk must carry provenance"
        );
        assert!(
            chunk.tags.iter().any(|t| t == "consolidator:mock"),
            "consolidated chunk must record the consolidator"
        );
        assert!(
            chunk.tags.iter().any(|t| t == "ctx:subsystem:cache"),
            "consolidated chunk must inherit the dominant ctx tag"
        );
    }

    // All 12 sources are soft-tombstoned (status Superseded) yet still
    // present on disk.
    for id in &ids {
        let resolved = store
            .get_with_lifecycle(&tenant, &ChunkId::parse(id).unwrap())
            .await
            .unwrap()
            .expect("superseded chunk still present on disk");
        assert_eq!(
            resolved.status,
            ChunkStatus::Superseded,
            "source chunk {id} should be superseded"
        );
        assert!(resolved.lifecycle.superseded_by.is_some());
    }

    // Retrieval surfaces the 3 consolidated lessons and hides every
    // superseded source — the same visibility filter `memd memory-md`
    // applies when rendering takeaways.
    let search = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "t".to_string(),
            query: "tenant scoped keys".to_string(),
            project_id: Some(project.to_string()),
            k: 50,
            ..Default::default()
        },
    )
    .await
    .expect("search");
    let hits = hit_ids(&search);
    assert_eq!(
        hits.len(),
        3,
        "search must return exactly the 3 consolidated chunks, got {hits:?}"
    );
    for id in &ids {
        assert!(
            !hits.contains(id),
            "superseded source {id} must be excluded from retrieval"
        );
    }
    let consolidated_ids: Vec<String> = consolidated
        .iter()
        .map(|c| c.chunk_id.to_string())
        .collect();
    for cid in &consolidated_ids {
        assert!(hits.contains(cid), "consolidated chunk {cid} must surface");
    }

    // Opting into superseded chunks brings the raw sources back —
    // proving they were hidden, not deleted.
    let with_superseded = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "t".to_string(),
            query: "tenant scoped keys".to_string(),
            project_id: Some(project.to_string()),
            k: 50,
            include_superseded: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("search with superseded");
    let all_hits = hit_ids(&with_superseded);
    assert!(
        all_hits.len() > 3,
        "include_superseded must resurface the raw sources, got {} hits",
        all_hits.len()
    );
}

/// Live smoke test: exercises the real consolidator CLI. Gated behind
/// `--features live-llm` so the default suite stays hermetic.
#[cfg(feature = "live-llm")]
#[tokio::test]
async fn live_consolidator_returns_valid_json() {
    use memd::consolidate::prompt::{build_consolidation_prompt, RegionChunk};
    use memd::consolidate::select::select_consolidator;

    let region = vec![
        RegionChunk {
            chunk_id: "c1".to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:finish".to_string()],
            timestamp_created: 1,
            text: "Fixed the cache bug by using tenant-scoped keys.".to_string(),
            project_id: None,
        },
        RegionChunk {
            chunk_id: "c2".to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:finish".to_string()],
            timestamp_created: 2,
            text: "Cache bug recurred; tenant-scoped keys are the fix.".to_string(),
            project_id: None,
        },
    ];
    let prompt = build_consolidation_prompt(&region);
    let consolidator = select_consolidator().expect("a consolidator backend");
    let raw = consolidator
        .consolidate(&prompt)
        .await
        .expect("live consolidation call");
    assert!(!raw.trim().is_empty(), "live response must be non-empty");
}
