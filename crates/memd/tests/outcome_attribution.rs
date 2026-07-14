use memd::cli::{run_cli, CliCommand};
use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::store::{
    stable_query_hash, OutcomeEvent, OutcomeKind, OutcomeVerifier, RankingPolicyMode,
    RetrievalEpisode, RetrievalEpisodeId, RetrievalEpisodeItem, Store,
};
use memd::types::{ChunkType, MemoryChunk, ProjectId, TenantId};

#[path = "common/mod.rs"]
mod common;

fn open_store(path: &std::path::Path) -> PersistentStore {
    PersistentStore::open(PersistentStoreConfig {
        data_dir: path.to_path_buf(),
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn query_identity_is_stable_sha256() {
    assert_eq!(
        stable_query_hash("private query with literal payload"),
        "b25b3e9321f798800bf3460ac0090c962c93d35433576fdef605ad14a7fbc46e"
    );
}

#[tokio::test]
async fn episode_and_attributed_outcome_round_trip_without_raw_query() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("outcome_tenant").unwrap();
    let project = "project_a";
    let store = open_store(temp.path());
    let chunk_id = store
        .add(
            MemoryChunk::new(tenant.clone(), "validated reusable lesson", ChunkType::Doc)
                .with_project(ProjectId::from(project)),
        )
        .await
        .unwrap();
    let query = "private query with literal payload";
    let episode = RetrievalEpisode {
        episode_id: RetrievalEpisodeId::new(),
        tenant_id: tenant.clone(),
        project_id: Some(project.to_string()),
        query_hash: stable_query_hash(query),
        query_mode: "generic".to_string(),
        requested_k: 1,
        fetched_k: 1,
        rendered_k: 1,
        policy_version: "outcome-v1".to_string(),
        policy_mode: RankingPolicyMode::Shadow,
        task_id: Some("task-1".to_string()),
        thread_id: Some("thread-1".to_string()),
        created_at_ms: 1_700_000_000_000,
        expires_at_ms: 1_707_776_000_000,
    };
    let item = RetrievalEpisodeItem {
        episode_id: episode.episode_id.clone(),
        chunk_id: chunk_id.clone(),
        origin_tenant_id: tenant.clone(),
        origin_project_id: Some(project.to_string()),
        original_rank: 0,
        original_score: 0.5,
        lane_scores_json: "{\"base\":0.5}".to_string(),
        outcome_adjustment: 0.0,
        served_rank: Some(0),
        shadow_rank: Some(0),
        rendered: true,
        source_dedup_group: None,
    };
    store
        .record_retrieval_episode(episode.clone(), vec![item])
        .await
        .unwrap();

    let outcome = OutcomeEvent::new(
        episode.episode_id.clone(),
        OutcomeKind::Passed,
        OutcomeVerifier::AutomatedTest,
        vec![chunk_id.clone()],
        Vec::new(),
        Some("artifact:test-report".to_string()),
        1_700_000_001_000,
    );
    store
        .record_outcome(&tenant, outcome.clone())
        .await
        .unwrap();

    let loaded = store
        .get_retrieval_episode(&tenant, &episode.episode_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.0, episode);
    assert_eq!(loaded.1.len(), 1);
    let outcomes = store
        .list_outcomes_for_episode(&tenant, &episode.episode_id)
        .await
        .unwrap();
    assert_eq!(outcomes, vec![outcome]);

    let db = std::fs::read(temp.path().join("metadata.db")).unwrap();
    assert!(!String::from_utf8_lossy(&db).contains(query));
}

#[tokio::test]
async fn outcome_rejects_unrendered_or_cross_scope_attribution() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("scope_tenant").unwrap();
    let store = open_store(temp.path());
    let rendered = store
        .add(
            MemoryChunk::new(tenant.clone(), "rendered", ChunkType::Doc)
                .with_project(ProjectId::from("project_a")),
        )
        .await
        .unwrap();
    let hidden = store
        .add(
            MemoryChunk::new(tenant.clone(), "not rendered", ChunkType::Doc)
                .with_project(ProjectId::from("project_a")),
        )
        .await
        .unwrap();
    let episode_id = RetrievalEpisodeId::new();
    let episode = RetrievalEpisode {
        episode_id: episode_id.clone(),
        tenant_id: tenant.clone(),
        project_id: Some("project_a".to_string()),
        query_hash: stable_query_hash("q"),
        query_mode: "generic".to_string(),
        requested_k: 1,
        fetched_k: 2,
        rendered_k: 1,
        policy_version: "outcome-v1".to_string(),
        policy_mode: RankingPolicyMode::Shadow,
        task_id: None,
        thread_id: None,
        created_at_ms: 1_700_000_000_000,
        expires_at_ms: 1_707_776_000_000,
    };
    let items = [
        (rendered.clone(), true, Some(0)),
        (hidden.clone(), false, None),
    ]
    .into_iter()
    .enumerate()
    .map(
        |(rank, (chunk_id, is_rendered, served_rank))| RetrievalEpisodeItem {
            episode_id: episode_id.clone(),
            chunk_id,
            origin_tenant_id: tenant.clone(),
            origin_project_id: Some("project_a".to_string()),
            original_rank: rank,
            original_score: 0.5 - rank as f32 * 0.1,
            lane_scores_json: "{}".to_string(),
            outcome_adjustment: 0.0,
            served_rank,
            shadow_rank: Some(rank),
            rendered: is_rendered,
            source_dedup_group: None,
        },
    )
    .collect();
    store
        .record_retrieval_episode(episode, items)
        .await
        .unwrap();

    let invalid = OutcomeEvent::new(
        episode_id.clone(),
        OutcomeKind::Passed,
        OutcomeVerifier::User,
        vec![hidden],
        Vec::new(),
        None,
        1_700_000_001_000,
    );
    assert!(store.record_outcome(&tenant, invalid).await.is_err());

    let other_tenant = TenantId::new("other_scope_tenant").unwrap();
    let cross_tenant = OutcomeEvent::new(
        episode_id.clone(),
        OutcomeKind::Passed,
        OutcomeVerifier::User,
        vec![rendered],
        Vec::new(),
        None,
        1_700_000_001_000,
    );
    assert!(store
        .record_outcome(&other_tenant, cross_tenant)
        .await
        .is_err());
    assert!(store
        .get_retrieval_episode(&other_tenant, &episode_id)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .list_outcomes_for_episode(&other_tenant, &episode_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn structured_record_outcome_returns_event_and_eligibility() {
    let (server, _temp) = common::test_server().await;
    let tenant = TenantId::new("api_outcome_tenant").unwrap();
    let chunk_id = server
        .store()
        .add(
            MemoryChunk::new(tenant.clone(), "API outcome target", ChunkType::Doc)
                .with_project(ProjectId::from("p")),
        )
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let episode_id = RetrievalEpisodeId::new();
    server
        .store()
        .record_retrieval_episode(
            RetrievalEpisode {
                episode_id: episode_id.clone(),
                tenant_id: tenant.clone(),
                project_id: Some("p".to_string()),
                query_hash: stable_query_hash("API outcome query"),
                query_mode: "generic".to_string(),
                requested_k: 1,
                fetched_k: 1,
                rendered_k: 1,
                policy_version: "outcome-v1".to_string(),
                policy_mode: RankingPolicyMode::Shadow,
                task_id: Some("task-api".to_string()),
                thread_id: None,
                created_at_ms: now,
                expires_at_ms: now + 86_400_000,
            },
            vec![RetrievalEpisodeItem {
                episode_id: episode_id.clone(),
                chunk_id: chunk_id.clone(),
                origin_tenant_id: tenant.clone(),
                origin_project_id: Some("p".to_string()),
                original_rank: 0,
                original_score: 1.0,
                lane_scores_json: "{\"base\":1.0}".to_string(),
                outcome_adjustment: 0.0,
                served_rank: Some(0),
                shadow_rank: Some(0),
                rendered: true,
                source_dedup_group: None,
            }],
        )
        .await
        .unwrap();

    let response = common::call_tool(
        &server,
        "memory.record_outcome",
        serde_json::json!({
            "tenant_id": tenant.as_str(),
            "episode_id": episode_id.to_string(),
            "outcome": "passed",
            "verifier_type": "automated_test",
            "used_chunk_ids": [chunk_id.to_string()],
            "evidence_reference": "artifact:test-report"
        }),
    )
    .await;
    let payload = common::parse_result_text(&response);
    assert_eq!(payload["stored"], true);
    assert_eq!(payload["ranking_eligible"], true);
    assert!(payload["event_id"].as_str().is_some());
}

#[tokio::test]
async fn search_records_a_privacy_safe_shadow_episode() {
    let (server, temp) = common::test_server().await;
    let tenant = TenantId::new("search_episode_tenant").unwrap();
    let project = "episode_project";
    for text in [
        "alpha retrieval lesson primary",
        "alpha retrieval lesson secondary",
        "alpha retrieval lesson tertiary",
    ] {
        server
            .store()
            .add(
                MemoryChunk::new(tenant.clone(), text, ChunkType::Doc)
                    .with_project(ProjectId::from(project)),
            )
            .await
            .unwrap();
    }

    let raw_query = "private alpha retrieval literal";
    let response = common::call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": tenant.as_str(),
            "project_id": project,
            "query": raw_query,
            "k": 1,
            "task_id": "task-search",
            "thread_id": "thread-search"
        }),
    )
    .await;
    let payload = common::parse_result_text(&response);
    let episode_id = RetrievalEpisodeId::parse(
        payload["retrieval_episode_id"]
            .as_str()
            .expect("search returns retrieval episode id"),
    )
    .unwrap();
    assert_eq!(payload["ranking_policy"]["mode"], "shadow");
    assert_eq!(payload["results"].as_array().unwrap().len(), 1);

    let (episode, items) = server
        .store()
        .get_retrieval_episode(&tenant, &episode_id)
        .await
        .unwrap()
        .expect("recorded retrieval episode");
    assert_eq!(episode.query_hash, stable_query_hash(raw_query));
    assert_eq!(episode.task_id.as_deref(), Some("task-search"));
    assert_eq!(episode.thread_id.as_deref(), Some("thread-search"));
    assert_eq!(episode.policy_mode, RankingPolicyMode::Shadow);
    assert_eq!(episode.rendered_k, 1);
    assert!(items.len() >= episode.rendered_k);
    assert_eq!(items.iter().filter(|item| item.rendered).count(), 1);

    let rendered_chunk_id = payload["results"][0]["chunk_id"].as_str().unwrap();
    let feedback_response = common::call_tool(
        &server,
        "memory.feedback",
        serde_json::json!({
            "tenant_id": tenant.as_str(),
            "query": raw_query,
            "chunk_id": rendered_chunk_id,
            "relevance": "relevant"
        }),
    )
    .await;
    assert_eq!(
        common::parse_result_text(&feedback_response)["stored"],
        true
    );

    for name in ["metadata.db", "metadata.db-wal"] {
        let path = temp.path().join(name);
        if path.exists() {
            let bytes = std::fs::read(path).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(raw_query));
        }
    }
}

#[tokio::test]
async fn search_rejects_serve_mode_and_off_mode_records_no_adjustments() {
    let (server, _temp) = common::test_server().await;
    let tenant = TenantId::new("policy_mode_tenant").unwrap();
    let project = "policy_mode_project";
    server
        .store()
        .add(
            MemoryChunk::new(tenant.clone(), "policy mode retrieval", ChunkType::Doc)
                .with_project(ProjectId::from(project)),
        )
        .await
        .unwrap();

    let rejected = common::call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": tenant.as_str(),
            "project_id": project,
            "query": "policy mode retrieval",
            "k": 1,
            "ranking_policy": "serve"
        }),
    )
    .await;
    let (_, message) = common::parse_error(&rejected).expect("serve mode must fail closed");
    assert!(message.contains("ranking_policy=serve is not activated"));

    let response = common::call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": tenant.as_str(),
            "project_id": project,
            "query": "policy mode retrieval",
            "k": 1,
            "ranking_policy": "off"
        }),
    )
    .await;
    let payload = common::parse_result_text(&response);
    assert_eq!(payload["ranking_policy"]["mode"], "off");
    assert_eq!(payload["ranking_policy"]["shadow_order_changed"], false);
    let episode_id =
        RetrievalEpisodeId::parse(payload["retrieval_episode_id"].as_str().unwrap()).unwrap();
    let (episode, items) = server
        .store()
        .get_retrieval_episode(&tenant, &episode_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(episode.policy_mode, RankingPolicyMode::Off);
    assert!(items.iter().all(|item| item.outcome_adjustment == 0.0));
    assert!(items.iter().all(|item| item.shadow_rank.is_none()));
}

#[tokio::test]
async fn episode_finalization_tracks_the_actual_rendered_order() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("finalize_episode_tenant").unwrap();
    let store = open_store(temp.path());
    let first = store
        .add(MemoryChunk::new(tenant.clone(), "first", ChunkType::Doc))
        .await
        .unwrap();
    let second = store
        .add(MemoryChunk::new(tenant.clone(), "second", ChunkType::Doc))
        .await
        .unwrap();
    let episode_id = RetrievalEpisodeId::new();
    let episode = RetrievalEpisode {
        episode_id: episode_id.clone(),
        tenant_id: tenant.clone(),
        project_id: None,
        query_hash: stable_query_hash("rerank query"),
        query_mode: "generic".to_string(),
        requested_k: 2,
        fetched_k: 2,
        rendered_k: 2,
        policy_version: "outcome-v1".to_string(),
        policy_mode: RankingPolicyMode::Shadow,
        task_id: None,
        thread_id: None,
        created_at_ms: 1_700_000_000_000,
        expires_at_ms: 1_707_776_000_000,
    };
    let items = [first.clone(), second.clone()]
        .into_iter()
        .enumerate()
        .map(|(rank, chunk_id)| RetrievalEpisodeItem {
            episode_id: episode_id.clone(),
            chunk_id,
            origin_tenant_id: tenant.clone(),
            origin_project_id: None,
            original_rank: rank,
            original_score: 1.0 - rank as f32,
            lane_scores_json: "{}".to_string(),
            outcome_adjustment: 0.0,
            served_rank: Some(rank),
            shadow_rank: Some(rank),
            rendered: true,
            source_dedup_group: None,
        })
        .collect();
    store
        .record_retrieval_episode(episode, items)
        .await
        .unwrap();

    store
        .finalize_retrieval_episode(&tenant, &episode_id, &[second.clone(), first.clone()])
        .await
        .unwrap();
    let (_, items) = store
        .get_retrieval_episode(&tenant, &episode_id)
        .await
        .unwrap()
        .unwrap();
    let rank = |chunk_id: &memd::types::ChunkId| {
        items
            .iter()
            .find(|item| &item.chunk_id == chunk_id)
            .unwrap()
            .served_rank
    };
    assert_eq!(rank(&second), Some(0));
    assert_eq!(rank(&first), Some(1));
}

#[tokio::test]
async fn eligible_outcomes_produce_project_scoped_decayed_priors() {
    let temp = tempfile::tempdir().unwrap();
    let tenant = TenantId::new("prior_tenant").unwrap();
    let store = open_store(temp.path());
    let positive = store
        .add(
            MemoryChunk::new(tenant.clone(), "positive", ChunkType::Doc)
                .with_project(ProjectId::from("project_a")),
        )
        .await
        .unwrap();
    let other_project = store
        .add(
            MemoryChunk::new(tenant.clone(), "other", ChunkType::Doc)
                .with_project(ProjectId::from("project_b")),
        )
        .await
        .unwrap();
    let unscoped = store
        .add(MemoryChunk::new(
            tenant.clone(),
            "tenant-wide",
            ChunkType::Doc,
        ))
        .await
        .unwrap();
    let now = 1_700_000_001_000;
    let episode_id = RetrievalEpisodeId::new();
    store
        .record_retrieval_episode(
            RetrievalEpisode {
                episode_id: episode_id.clone(),
                tenant_id: tenant.clone(),
                project_id: Some("project_a".to_string()),
                query_hash: stable_query_hash("prior query"),
                query_mode: "generic".to_string(),
                requested_k: 1,
                fetched_k: 1,
                rendered_k: 1,
                policy_version: "outcome-v1".to_string(),
                policy_mode: RankingPolicyMode::Shadow,
                task_id: None,
                thread_id: None,
                created_at_ms: now - 1_000,
                expires_at_ms: now + 86_400_000,
            },
            vec![RetrievalEpisodeItem {
                episode_id: episode_id.clone(),
                chunk_id: positive.clone(),
                origin_tenant_id: tenant.clone(),
                origin_project_id: Some("project_a".to_string()),
                original_rank: 0,
                original_score: 1.0,
                lane_scores_json: "{}".to_string(),
                outcome_adjustment: 0.0,
                served_rank: Some(0),
                shadow_rank: Some(0),
                rendered: true,
                source_dedup_group: None,
            }],
        )
        .await
        .unwrap();
    store
        .record_outcome(
            &tenant,
            OutcomeEvent::new(
                episode_id,
                OutcomeKind::Passed,
                OutcomeVerifier::User,
                vec![positive.clone()],
                Vec::new(),
                None,
                now,
            ),
        )
        .await
        .unwrap();

    assert!(store
        .outcome_priors(
            &tenant,
            Some("project_a"),
            std::slice::from_ref(&positive),
            now - 1,
        )
        .await
        .unwrap()
        .is_empty());

    let priors = store
        .outcome_priors(
            &tenant,
            Some("project_a"),
            &[positive.clone(), other_project],
            now,
        )
        .await
        .unwrap();
    assert_eq!(priors.len(), 1);
    assert_eq!(priors[0].chunk_id, positive);
    assert_eq!(priors[0].eligible_episode_count, 1);
    assert!(priors[0].positive_weight > 0.99);
    assert_eq!(priors[0].negative_weight, 0.0);
    assert!(priors[0].bounded_adjustment() > 0.0);

    assert!(store
        .outcome_priors(&tenant, None, std::slice::from_ref(&positive), now)
        .await
        .unwrap()
        .is_empty());

    let tenant_wide_episode_id = RetrievalEpisodeId::new();
    store
        .record_retrieval_episode(
            RetrievalEpisode {
                episode_id: tenant_wide_episode_id.clone(),
                tenant_id: tenant.clone(),
                project_id: None,
                query_hash: stable_query_hash("tenant-wide prior query"),
                query_mode: "generic".to_string(),
                requested_k: 1,
                fetched_k: 1,
                rendered_k: 1,
                policy_version: "outcome-v1".to_string(),
                policy_mode: RankingPolicyMode::Shadow,
                task_id: None,
                thread_id: None,
                created_at_ms: now - 1_000,
                expires_at_ms: now + 86_400_000,
            },
            vec![RetrievalEpisodeItem {
                episode_id: tenant_wide_episode_id.clone(),
                chunk_id: unscoped.clone(),
                origin_tenant_id: tenant.clone(),
                origin_project_id: None,
                original_rank: 0,
                original_score: 1.0,
                lane_scores_json: "{}".to_string(),
                outcome_adjustment: 0.0,
                served_rank: Some(0),
                shadow_rank: Some(0),
                rendered: true,
                source_dedup_group: None,
            }],
        )
        .await
        .unwrap();
    store
        .record_outcome(
            &tenant,
            OutcomeEvent::new(
                tenant_wide_episode_id,
                OutcomeKind::Passed,
                OutcomeVerifier::User,
                vec![unscoped.clone()],
                Vec::new(),
                None,
                now,
            ),
        )
        .await
        .unwrap();
    let tenant_wide_priors = store
        .outcome_priors(&tenant, None, std::slice::from_ref(&unscoped), now)
        .await
        .unwrap();
    assert_eq!(tenant_wide_priors.len(), 1);
    assert_eq!(tenant_wide_priors[0].chunk_id, unscoped);
    assert!(store
        .outcome_priors(
            &tenant,
            Some("project_a"),
            std::slice::from_ref(&tenant_wide_priors[0].chunk_id),
            now,
        )
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn aliased_outcomes_update_only_the_requester_scope() {
    let temp = tempfile::tempdir().unwrap();
    let requester = TenantId::new("alias_requester").unwrap();
    let origin = TenantId::new("alias_origin").unwrap();
    let store = open_store(temp.path());
    let chunk_id = store
        .add(
            MemoryChunk::new(origin.clone(), "shared aliased lesson", ChunkType::Doc)
                .with_project(ProjectId::from("origin_project")),
        )
        .await
        .unwrap();
    let now = 1_700_000_001_000;
    let episode_id = RetrievalEpisodeId::new();
    store
        .record_retrieval_episode(
            RetrievalEpisode {
                episode_id: episode_id.clone(),
                tenant_id: requester.clone(),
                project_id: Some("requester_project".to_string()),
                query_hash: stable_query_hash("aliased retrieval"),
                query_mode: "generic".to_string(),
                requested_k: 1,
                fetched_k: 1,
                rendered_k: 1,
                policy_version: "outcome-v1".to_string(),
                policy_mode: RankingPolicyMode::Shadow,
                task_id: None,
                thread_id: None,
                created_at_ms: now - 1_000,
                expires_at_ms: now + 86_400_000,
            },
            vec![RetrievalEpisodeItem {
                episode_id: episode_id.clone(),
                chunk_id: chunk_id.clone(),
                origin_tenant_id: origin.clone(),
                origin_project_id: Some("origin_project".to_string()),
                original_rank: 0,
                original_score: 1.0,
                lane_scores_json: "{}".to_string(),
                outcome_adjustment: 0.0,
                served_rank: Some(0),
                shadow_rank: Some(0),
                rendered: true,
                source_dedup_group: None,
            }],
        )
        .await
        .unwrap();
    store
        .record_outcome(
            &requester,
            OutcomeEvent::new(
                episode_id,
                OutcomeKind::Passed,
                OutcomeVerifier::User,
                vec![chunk_id.clone()],
                Vec::new(),
                None,
                now,
            ),
        )
        .await
        .unwrap();

    let requester_priors = store
        .outcome_priors(
            &requester,
            Some("requester_project"),
            std::slice::from_ref(&chunk_id),
            now,
        )
        .await
        .unwrap();
    assert_eq!(requester_priors.len(), 1);
    assert_eq!(requester_priors[0].chunk_id, chunk_id);

    let origin_priors = store
        .outcome_priors(
            &origin,
            Some("origin_project"),
            std::slice::from_ref(&chunk_id),
            now,
        )
        .await
        .unwrap();
    assert!(origin_priors.is_empty());
}

#[tokio::test]
async fn verified_outcomes_change_shadow_order_without_changing_served_order() {
    let (server, _temp) = common::test_server().await;
    let tenant = TenantId::new("shadow_policy_tenant").unwrap();
    let project = "shadow_project";
    let helped = server
        .store()
        .add(
            MemoryChunk::new(tenant.clone(), "shared shadow query", ChunkType::Doc)
                .with_project(ProjectId::from(project)),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let served_first = server
        .store()
        .add(
            MemoryChunk::new(tenant.clone(), "shared shadow query", ChunkType::Doc)
                .with_project(ProjectId::from(project)),
        )
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    for offset in 0..6 {
        let episode_id = RetrievalEpisodeId::new();
        server
            .store()
            .record_retrieval_episode(
                RetrievalEpisode {
                    episode_id: episode_id.clone(),
                    tenant_id: tenant.clone(),
                    project_id: Some(project.to_string()),
                    query_hash: stable_query_hash("historical shadow query"),
                    query_mode: "generic".to_string(),
                    requested_k: 1,
                    fetched_k: 1,
                    rendered_k: 1,
                    policy_version: "outcome-v1".to_string(),
                    policy_mode: RankingPolicyMode::Shadow,
                    task_id: None,
                    thread_id: None,
                    created_at_ms: now - 10_000 + offset,
                    expires_at_ms: now + 86_400_000,
                },
                vec![RetrievalEpisodeItem {
                    episode_id: episode_id.clone(),
                    chunk_id: helped.clone(),
                    origin_tenant_id: tenant.clone(),
                    origin_project_id: Some(project.to_string()),
                    original_rank: 0,
                    original_score: 1.0,
                    lane_scores_json: "{}".to_string(),
                    outcome_adjustment: 0.0,
                    served_rank: Some(0),
                    shadow_rank: Some(0),
                    rendered: true,
                    source_dedup_group: None,
                }],
            )
            .await
            .unwrap();
        server
            .store()
            .record_outcome(
                &tenant,
                OutcomeEvent::new(
                    episode_id,
                    OutcomeKind::Passed,
                    OutcomeVerifier::User,
                    vec![helped.clone()],
                    Vec::new(),
                    None,
                    now - 5_000 + offset,
                ),
            )
            .await
            .unwrap();
    }

    let response = common::call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": tenant.as_str(),
            "project_id": project,
            "query": "shared shadow query",
            "k": 1
        }),
    )
    .await;
    let payload = common::parse_result_text(&response);
    assert_eq!(payload["results"][0]["chunk_id"], served_first.to_string());
    assert_eq!(payload["ranking_policy"]["shadow_order_changed"], true);
    let episode_id =
        RetrievalEpisodeId::parse(payload["retrieval_episode_id"].as_str().unwrap()).unwrap();
    let (_, items) = server
        .store()
        .get_retrieval_episode(&tenant, &episode_id)
        .await
        .unwrap()
        .unwrap();
    let helped_item = items.iter().find(|item| item.chunk_id == helped).unwrap();
    assert!(helped_item.outcome_adjustment > 0.0);
    assert_eq!(helped_item.shadow_rank, Some(0));
    assert!(!helped_item.rendered);
}

#[tokio::test]
async fn verified_harmful_outcomes_demote_shadow_rank_without_changing_served_order() {
    let (server, temp) = common::test_server().await;
    let tenant = TenantId::new("harmful_shadow_tenant").unwrap();
    let project = "harmful_shadow_project";
    let safer = server
        .store()
        .add(
            MemoryChunk::new(tenant.clone(), "shared harmful query", ChunkType::Doc)
                .with_project(ProjectId::from(project)),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let harmful = server
        .store()
        .add(
            MemoryChunk::new(tenant.clone(), "shared harmful query", ChunkType::Doc)
                .with_project(ProjectId::from(project)),
        )
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    for offset in 0..6 {
        let episode_id = RetrievalEpisodeId::new();
        server
            .store()
            .record_retrieval_episode(
                RetrievalEpisode {
                    episode_id: episode_id.clone(),
                    tenant_id: tenant.clone(),
                    project_id: Some(project.to_string()),
                    query_hash: stable_query_hash("historical harmful query"),
                    query_mode: "generic".to_string(),
                    requested_k: 1,
                    fetched_k: 1,
                    rendered_k: 1,
                    policy_version: "outcome-v1".to_string(),
                    policy_mode: RankingPolicyMode::Shadow,
                    task_id: None,
                    thread_id: None,
                    created_at_ms: now - 10_000 + offset,
                    expires_at_ms: now + 86_400_000,
                },
                vec![RetrievalEpisodeItem {
                    episode_id: episode_id.clone(),
                    chunk_id: harmful.clone(),
                    origin_tenant_id: tenant.clone(),
                    origin_project_id: Some(project.to_string()),
                    original_rank: 0,
                    original_score: 1.0,
                    lane_scores_json: "{}".to_string(),
                    outcome_adjustment: 0.0,
                    served_rank: Some(0),
                    shadow_rank: Some(0),
                    rendered: true,
                    source_dedup_group: None,
                }],
            )
            .await
            .unwrap();
        server
            .store()
            .record_outcome(
                &tenant,
                OutcomeEvent::new(
                    episode_id,
                    OutcomeKind::Failed,
                    OutcomeVerifier::AutomatedTest,
                    Vec::new(),
                    vec![harmful.clone()],
                    Some(format!("artifact:failed-check-{offset}")),
                    now - 5_000 + offset,
                ),
            )
            .await
            .unwrap();
    }

    let response = common::call_tool(
        &server,
        "memory.search",
        serde_json::json!({
            "tenant_id": tenant.as_str(),
            "project_id": project,
            "query": "shared harmful query",
            "k": 1
        }),
    )
    .await;
    let payload = common::parse_result_text(&response);
    assert_eq!(payload["results"][0]["chunk_id"], harmful.to_string());
    assert_eq!(payload["ranking_policy"]["shadow_order_changed"], true);
    let episode_id =
        RetrievalEpisodeId::parse(payload["retrieval_episode_id"].as_str().unwrap()).unwrap();
    let (_, items) = server
        .store()
        .get_retrieval_episode(&tenant, &episode_id)
        .await
        .unwrap()
        .unwrap();
    let harmful_item = items.iter().find(|item| item.chunk_id == harmful).unwrap();
    let safer_item = items.iter().find(|item| item.chunk_id == safer).unwrap();
    assert!(harmful_item.outcome_adjustment < 0.0);
    assert_eq!(harmful_item.served_rank, Some(0));
    assert_eq!(harmful_item.shadow_rank, Some(1));
    assert_eq!(safer_item.shadow_rank, Some(0));

    let queries = temp.path().join("outcome-ranking-queries.jsonl");
    std::fs::write(
        &queries,
        serde_json::json!({
            "id": "harmful-demotion",
            "query": "shared harmful query",
            "relevant_chunk_ids": [safer.to_string()],
            "harmful_chunk_ids": [harmful.to_string()]
        })
        .to_string()
            + "\n",
    )
    .unwrap();
    let report_json = temp.path().join("outcome-counterfactual.json");
    run_cli(
        server.store(),
        None,
        CliCommand::EvalOutcomeRanking {
            tenant_id: tenant.to_string(),
            project_id: Some(project.to_string()),
            project_dir: temp.path().to_path_buf(),
            queries,
            k: 1,
            report_json: report_json.clone(),
        },
    )
    .await
    .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report_json).unwrap()).unwrap();
    assert_eq!(report["summary"]["changed_query_count"], 1);
    assert_eq!(report["summary"]["served"]["mean_harmful_at_k"], 1.0);
    assert_eq!(report["summary"]["shadow"]["mean_harmful_at_k"], 0.0);
    assert_eq!(report["summary"]["shadow"]["mean_recall_at_k"], 1.0);
    assert!(report_json.with_extension("md").exists());
}

#[test]
fn legacy_feedback_migration_scrubs_raw_queries() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("metadata.db");
    let raw_query = "legacy private feedback literal";
    {
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE feedback (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    tenant_id TEXT NOT NULL,
                    query TEXT NOT NULL,
                    chunk_id TEXT NOT NULL,
                    relevance INTEGER NOT NULL,
                    timestamp_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO feedback (
                    tenant_id, query, chunk_id, relevance, timestamp_ms
                 ) VALUES (?1, ?2, ?3, 1, 1700000000000)",
                rusqlite::params![
                    "legacy_tenant",
                    raw_query,
                    "019e6d12-c1a7-7330-8bd8-4c9cdb45bc3c"
                ],
            )
            .unwrap();
    }

    let store = open_store(temp.path());
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let columns = connection
        .prepare("PRAGMA table_info(feedback)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(columns.iter().any(|column| column == "query_hash"));
    assert!(!columns.iter().any(|column| column == "query"));
    drop(connection);

    for name in ["metadata.db", "metadata.db-wal"] {
        let path = temp.path().join(name);
        if path.exists() {
            let bytes = std::fs::read(path).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(raw_query));
        }
    }
}
