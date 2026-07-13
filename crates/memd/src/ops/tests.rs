use super::*;

#[test]
fn test_format_epoch_ms_date() {
    // 2023-05-08 00:00:00 UTC = 1_683_504_000_000 ms
    assert_eq!(format_epoch_ms_date(1_683_504_000_000), "2023-05-08");
    // Unix epoch and a leap-year boundary.
    assert_eq!(format_epoch_ms_date(0), "1970-01-01");
    assert_eq!(format_epoch_ms_date(1_583_020_800_000), "2020-03-01");
    // A mid-day timestamp resolves to the same calendar date.
    assert_eq!(
        format_epoch_ms_date(1_683_504_000_000 + 13 * 3_600_000),
        "2023-05-08"
    );
}

#[test]
fn test_render_observed_time_into_text() {
    let tenant = TenantId::new("render_event_time").unwrap();
    let mut dated = MemoryChunk::new(tenant.clone(), "had lunch with Alex", ChunkType::Message);
    dated.timestamp_observed = Some(1_683_504_000_000); // 2023-05-08
    let plain = MemoryChunk::new(tenant, "no event time here", ChunkType::Message);

    let mut chunks = vec![(dated, 1.0_f32), (plain, 0.5_f32)];
    render_observed_time_into_text(&mut chunks);

    assert_eq!(chunks[0].0.text, "[2023-05-08] had lunch with Alex");
    // A chunk without an observed time is returned unchanged.
    assert_eq!(chunks[1].0.text, "no event time here");
}
use crate::config::ProjectAliasScopeConfig;
use crate::metrics::{MetricsCollector, QueryMetrics};
use crate::store::persistent::{PersistentStore, PersistentStoreConfig};
use crate::store::{MemoryStore, Store};
use crate::types::lifecycle::MemoryTier;
use proptest::prelude::*;
use serde::de::DeserializeOwned;
use std::sync::Mutex;
use tempfile::TempDir;

/// Serialize tests that flip the process-global
/// `ALLOW_CROSS_TENANT_PROJECT_FALLBACK` atomic. Without this, parallel
/// tests would interleave writes to the flag and observe each other's
/// state.
static FALLBACK_FLAG_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn with_fallback_flag() -> tokio::sync::MutexGuard<'static, ()> {
    FALLBACK_FLAG_MUTEX.lock().await
}

fn make_store() -> MemoryStore {
    MemoryStore::new()
}

struct SearchMissStore {
    chunks: Mutex<Vec<MemoryChunk>>,
}

impl SearchMissStore {
    fn new(chunks: Vec<MemoryChunk>) -> Self {
        Self {
            chunks: Mutex::new(chunks),
        }
    }
}

#[tokio::test]
async fn store_defaults_report_unsupported_capabilities() {
    let store = SearchMissStore::new(Vec::new());
    let tenant = TenantId::new("unsupported").unwrap();
    let error = store
        .get_task_artifact(&tenant, "artifact")
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("store capability unsupported: task artifact lookup"));
}

#[async_trait::async_trait]
impl Store for SearchMissStore {
    async fn add(&self, chunk: MemoryChunk) -> crate::error::Result<ChunkId> {
        let chunk_id = chunk.chunk_id.clone();
        self.chunks.lock().unwrap().push(chunk);
        Ok(chunk_id)
    }

    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> crate::error::Result<Vec<ChunkId>> {
        let chunk_ids = chunks
            .iter()
            .map(|chunk| chunk.chunk_id.clone())
            .collect::<Vec<_>>();
        self.chunks.lock().unwrap().extend(chunks);
        Ok(chunk_ids)
    }

    async fn get(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
    ) -> crate::error::Result<Option<MemoryChunk>> {
        Ok(self
            .chunks
            .lock()
            .unwrap()
            .iter()
            .find(|chunk| &chunk.tenant_id == tenant_id && &chunk.chunk_id == chunk_id)
            .cloned())
    }

    async fn list_tenants(&self) -> crate::error::Result<Vec<TenantId>> {
        let mut tenants = self
            .chunks
            .lock()
            .unwrap()
            .iter()
            .map(|chunk| chunk.tenant_id.clone())
            .collect::<Vec<_>>();
        tenants.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        tenants.dedup_by(|left, right| left.as_str() == right.as_str());
        Ok(tenants)
    }

    async fn list_tasks(
        &self,
        _tenant_id: &TenantId,
        _project_id: Option<&str>,
        _limit: usize,
    ) -> crate::error::Result<Vec<crate::task_memory::TaskRecord>> {
        Ok(Vec::new())
    }

    async fn search_task_projection_chunk_ids(
        &self,
        _tenant_id: &TenantId,
        _filters: &crate::task_memory::TaskSearchFilters,
        _limit: usize,
    ) -> crate::error::Result<Vec<ChunkId>> {
        Ok(Vec::new())
    }

    async fn outcome_priors(
        &self,
        _scope_tenant_id: &TenantId,
        _scope_project_id: Option<&str>,
        _chunk_ids: &[ChunkId],
        _now_ms: i64,
    ) -> crate::error::Result<Vec<crate::store::OutcomePrior>> {
        Ok(Vec::new())
    }

    async fn record_retrieval_episode(
        &self,
        _episode: crate::store::RetrievalEpisode,
        _items: Vec<crate::store::RetrievalEpisodeItem>,
    ) -> crate::error::Result<()> {
        Ok(())
    }

    async fn resolve_artifacts_for_chunks(
        &self,
        _tenant_id: &TenantId,
        _chunk_ids: &[ChunkId],
    ) -> crate::error::Result<HashMap<String, TaskArtifact>> {
        Ok(HashMap::new())
    }

    async fn search(
        &self,
        _tenant_id: &TenantId,
        query: &str,
        _k: usize,
    ) -> crate::error::Result<Vec<MemoryChunk>> {
        if query.is_empty() {
            Ok(self.chunks.lock().unwrap().clone())
        } else {
            Ok(Vec::new())
        }
    }

    async fn search_with_scores(
        &self,
        _tenant_id: &TenantId,
        _query: &str,
        _k: usize,
    ) -> crate::error::Result<Vec<(MemoryChunk, f32)>> {
        Ok(Vec::new())
    }

    async fn list_chunks(
        &self,
        tenant_id: &TenantId,
        limit: usize,
        offset: usize,
    ) -> crate::error::Result<Vec<MemoryChunk>> {
        Ok(self
            .chunks
            .lock()
            .unwrap()
            .iter()
            .filter(|chunk| &chunk.tenant_id == tenant_id)
            .skip(offset)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn delete(
        &self,
        _tenant_id: &TenantId,
        _chunk_id: &ChunkId,
    ) -> crate::error::Result<bool> {
        Ok(false)
    }

    async fn stats(&self, _tenant_id: &TenantId) -> crate::error::Result<StoreStats> {
        Ok(StoreStats::default())
    }
}

fn make_persistent_store() -> (PersistentStore, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = PersistentStore::open(PersistentStoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_chunks: 100,
        wal_checkpoint_interval: 10,
        enable_dense_search: false,
        enable_hybrid_search: false,
        ..Default::default()
    })
    .unwrap();
    (store, dir)
}

fn project_brief_digest_fixture(
    tenant: &TenantId,
    project_id: &str,
    summary: &str,
    source_updated_at_ms: i64,
) -> TaskArtifact {
    let (artifact_id, task_id, digest_key) =
        crate::task_memory::stable_digest_identity(DIGEST_ROLE_PROJECT_BRIEF, project_id);
    let mut artifact = TaskArtifact::new_digest(
        tenant.clone(),
        task_id,
        digest_key,
        DIGEST_ROLE_PROJECT_BRIEF,
    );
    artifact.artifact_id = artifact_id;
    artifact.project_id = ProjectId::new(Some(project_id.to_string()));
    artifact.summary = Some(summary.to_string());
    artifact.source_updated_at_ms = Some(source_updated_at_ms);
    artifact
}

struct ProjectAliasResetGuard;

impl Drop for ProjectAliasResetGuard {
    fn drop(&mut self) {
        set_project_aliases(Vec::new());
        set_cross_tenant_project_fallback(false);
    }
}

fn parse_tool_payload<T: DeserializeOwned>(result: &Value) -> T {
    let text = result["content"][0]["text"]
        .as_str()
        .expect("tool response should include JSON text");
    serde_json::from_str(text).expect("tool response text should parse as JSON payload")
}

#[tokio::test]
async fn search_empty_store() {
    let store = make_store();
    let params = SearchParams {
        tenant_id: "test".to_string(),
        query: "hello".to_string(),
        project_id: None,
        k: 10,
        filters: None,
        debug_tiers: None,
        mode: None,
        include_superseded: None,
        include_expired: None,
        include_history: None,
        oversample_factor: None,
        expand_event_siblings: false,
        compact: false,
        token_budget: None,
        include_text: None,
        include_artifact: None,
        suppress_usage_event: false,
        ..Default::default()
    };

    let result = handle_memory_search(&store, params).await.unwrap();
    assert!(result["content"].is_array());

    let text = result["content"][0]["text"].as_str().unwrap();
    let search_result: SearchResult = serde_json::from_str(text).unwrap();
    assert!(search_result.results.is_empty());
}

#[tokio::test]
async fn search_rejects_k_zero() {
    let store = make_store();
    let params = SearchParams {
        tenant_id: "test".to_string(),
        query: "hello".to_string(),
        project_id: None,
        k: 0,
        filters: None,
        debug_tiers: None,
        mode: None,
        include_superseded: None,
        include_expired: None,
        include_history: None,
        oversample_factor: None,
        expand_event_siblings: false,
        compact: false,
        token_budget: None,
        include_text: None,
        include_artifact: None,
        suppress_usage_event: false,
        ..Default::default()
    };

    let result = handle_memory_search(&store, params).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
}

#[tokio::test]
async fn search_rejects_k_above_max() {
    let store = make_store();
    let params = SearchParams {
        tenant_id: "test".to_string(),
        query: "hello".to_string(),
        project_id: None,
        k: 101,
        filters: None,
        debug_tiers: None,
        mode: None,
        include_superseded: None,
        include_expired: None,
        include_history: None,
        oversample_factor: None,
        expand_event_siblings: false,
        compact: false,
        token_budget: None,
        include_text: None,
        include_artifact: None,
        suppress_usage_event: false,
        ..Default::default()
    };

    let result = handle_memory_search(&store, params).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
}

proptest! {
    #[test]
    fn validate_search_k_property(k in 0usize..=200usize) {
        let result = validate_search_k(k);
        if (1..=100).contains(&k) {
            prop_assert!(result.is_ok());
        } else {
            prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
        }
    }
}

#[test]
fn adaptive_fetch_k_expands_for_complex_queries() {
    let query = "this is a very long and complex search query with many tokens";
    assert_eq!(adaptive_fetch_k(10, query, false), 20);
    assert_eq!(adaptive_fetch_k(10, query, true), 100);
    assert_eq!(adaptive_fetch_k(10, "short query", false), 10);
}

#[test]
fn normalize_query_for_repair_rewrites_noise() {
    let repaired = normalize_query_for_repair("Alpha!unique?marker").unwrap();
    assert_eq!(repaired, "alpha unique marker");
    assert!(normalize_query_for_repair("clean query").is_none());
}

proptest! {
    #[test]
    fn validate_search_time_range_order_property(day_a in 1u8..=28, day_b in 1u8..=28) {
        let filters = SearchFilters {
            types: None,
            episode_id: None,
            time_range: Some(TimeRange {
                from: Some(format!("2026-01-{day_a:02}T00:00:00Z")),
                to: Some(format!("2026-01-{day_b:02}T23:59:59Z")),
            }),
        };

        let result = validate_search_time_range(Some(&filters));
        if day_a <= day_b {
            prop_assert!(result.is_ok());
        } else {
            prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
        }
    }
}

proptest! {
    #[test]
    fn validate_search_time_range_rejects_invalid_iso(invalid in "[A-Za-z]{1,16}") {
        let filters = SearchFilters {
            types: None,
            episode_id: None,
            time_range: Some(TimeRange {
                from: Some(invalid),
                to: Some("2026-01-01T00:00:00Z".to_string()),
            }),
        };

        let result = validate_search_time_range(Some(&filters));
        prop_assert!(matches!(result, Err(McpError::InvalidParams(_))));
    }
}

#[tokio::test]
async fn add_and_search() {
    let store = make_store();

    // Add a chunk
    let add_params = AddParams {
        tenant_id: "test".to_string(),
        text: "hello world".to_string(),
        chunk_type: "doc".to_string(),
        project_id: None,
        episode_id: None,
        source: None,
        tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,

        mode: None,
        supersede_near_duplicates: None,
        event_time_ms: None,
    };

    let add_result = handle_memory_add(&store, None, add_params).await.unwrap();
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();
    assert!(!add_response.chunk_id.is_empty());
    assert_eq!(add_response.admission_decision.as_deref(), Some("durable"));
    assert_eq!(add_response.admission_reason.as_deref(), Some("accepted"));

    // Search for it
    let search_params = SearchParams {
        tenant_id: "test".to_string(),
        query: "hello".to_string(),
        project_id: None,
        k: 10,
        filters: None,
        debug_tiers: None,
        mode: None,
        include_superseded: None,
        include_expired: None,
        include_history: None,
        oversample_factor: None,
        expand_event_siblings: false,
        compact: false,
        token_budget: None,
        include_text: None,
        include_artifact: None,
        suppress_usage_event: false,
        ..Default::default()
    };

    let search_result = handle_memory_search(&store, search_params).await.unwrap();
    let text = search_result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert_eq!(search_response.results.len(), 1);
    assert_eq!(search_response.results[0].text, "hello world");
}

#[tokio::test]
async fn memory_add_rejects_low_signal_progress_and_generated_wrappers() {
    let store = make_store();

    let low_signal = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "quality_gate".to_string(),
            text: "starting to inspect the files".to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:progress".to_string()],
            ..Default::default()
        },
    )
    .await;
    let err = low_signal.expect_err("low-signal progress should be rejected");
    assert!(err
        .message()
        .contains("low-signal progress chatter needs a concrete result"));

    let generated = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "quality_gate".to_string(),
            text: "Task digest status generated. Summary: Highlight library for p contains 0 ranked lessons.".to_string(),
            chunk_type: "summary".to_string(),
            tags: vec![
                "task:status:generated".to_string(),
                "task:role:highlight_library".to_string(),
            ],
            ..Default::default()
        },
    )
    .await;
    let err = generated.expect_err("generated digest wrapper should be rejected");
    assert!(err.message().contains("generated digest wrapper records"));

    let search_result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "quality_gate".to_string(),
            query: "starting generated Highlight library".to_string(),
            k: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = search_result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert!(
        search_response.results.is_empty(),
        "rejected writes must not be retrievable"
    );
}

#[tokio::test]
async fn memory_add_downgrades_explicit_high_priority_without_agent_action() {
    let store = make_store();

    let add_result = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "quality_gate_override".to_string(),
            text: "starting".to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:progress".to_string(), "priority:9".to_string()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // Admitted, not rejected — stored at priority 7 with an
    // in-band warning instead of losing the lesson outright.
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();
    assert_eq!(add_response.admission_decision.as_deref(), Some("durable"));
    let warning = add_response.admission_warning.expect("downgrade warning");
    assert!(warning.contains("Agent action"));
    let tenant = TenantId::new("quality_gate_override").unwrap();
    let chunk = store
        .get(&tenant, &ChunkId::parse(&add_response.chunk_id).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(chunk.tags.iter().any(|t| t == "priority:7"));
    assert!(!chunk.tags.iter().any(|t| t == "priority:9"));
}

#[tokio::test]
async fn memory_add_allows_explicit_high_priority_with_agent_action() {
    let store = make_store();

    let add_result = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "quality_gate_override_action".to_string(),
            text: "Validation: startup memory passed after action guidance. Agent action: Verify the action guidance gate before promoting high-priority memory.".to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:progress".to_string(), "priority:9".to_string()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();
    assert!(!add_response.chunk_id.is_empty());
    assert_eq!(add_response.admission_decision.as_deref(), Some("durable"));
}

#[tokio::test]
async fn memory_add_downgrades_low_signal_conversation_progress_to_ephemeral() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("quality_gate_ephemeral").unwrap();

    let add_result = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: tenant.to_string(),
            text: "starting to inspect the files".to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:progress".to_string()],
            mode: Some("conversation".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();
    assert_eq!(
        add_response.admission_decision.as_deref(),
        Some("ephemeral")
    );
    assert!(add_response
        .admission_reason
        .as_deref()
        .unwrap_or_default()
        .contains("short-lived hidden context"));
    assert_eq!(add_response.lifecycle_tier.as_deref(), Some("history"));
    assert!(add_response.expires_at_ms.is_some());
    assert!(add_response.review_after_ms.is_some());

    let chunk_id = ChunkId::parse(&add_response.chunk_id).unwrap();
    let resolved = store
        .get_with_lifecycle(&tenant, &chunk_id)
        .await
        .unwrap()
        .expect("ephemeral chunk should still be stored");
    assert_eq!(resolved.lifecycle.tier, MemoryTier::History);
    assert!(resolved
        .chunk
        .tags
        .iter()
        .any(|tag| tag == "admission:ephemeral"));

    let default_result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: tenant.to_string(),
            query: "starting to inspect the files".to_string(),
            k: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = default_result["content"][0]["text"].as_str().unwrap();
    let default_response: SearchResult = serde_json::from_str(text).unwrap();
    assert!(
        default_response.results.is_empty(),
        "history-tier ephemeral progress should be hidden from default search"
    );

    let history_result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: tenant.to_string(),
            query: "starting to inspect the files".to_string(),
            k: 10,
            include_history: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = history_result["content"][0]["text"].as_str().unwrap();
    let history_response: SearchResult = serde_json::from_str(text).unwrap();
    assert_eq!(history_response.results.len(), 1);
}

#[tokio::test]
async fn memory_add_applies_medium_ttl_to_ordinary_run_trace() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("run_trace_ttl").unwrap();
    let before_ms = current_time_ms();

    let add_result = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: tenant.to_string(),
            text: "Command: cargo test -p memd trace_retention -- --nocapture.".to_string(),
            chunk_type: "trace".to_string(),
            tags: vec!["kind:run".to_string()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();
    let expires_at = add_response
        .expires_at_ms
        .expect("ordinary run trace should get a medium TTL");
    assert_eq!(add_response.review_after_ms, Some(expires_at));
    assert_eq!(add_response.lifecycle_tier, None);
    assert!(expires_at >= before_ms + WRITE_ADMISSION_RUN_TRACE_TTL_MS);
    assert!(expires_at <= current_time_ms() + WRITE_ADMISSION_RUN_TRACE_TTL_MS);

    let chunk_id = ChunkId::parse(&add_response.chunk_id).unwrap();
    let resolved = store
        .get_with_lifecycle(&tenant, &chunk_id)
        .await
        .unwrap()
        .expect("trace chunk should be stored");
    assert_eq!(resolved.lifecycle.expires_at_ms, Some(expires_at));
    assert_eq!(resolved.lifecycle.review_after_ms, Some(expires_at));
    assert_eq!(resolved.lifecycle.tier, MemoryTier::LongTerm);
}

#[tokio::test]
async fn memory_add_keeps_evidence_trace_durable() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("run_trace_evidence").unwrap();

    let add_result = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: tenant.to_string(),
            text: "Command: cargo test -p memd passed and supports the release gate.".to_string(),
            chunk_type: "trace".to_string(),
            tags: vec!["kind:run".to_string(), "kind:evidence".to_string()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();
    assert_eq!(add_response.expires_at_ms, None);
    assert_eq!(add_response.review_after_ms, None);

    let chunk_id = ChunkId::parse(&add_response.chunk_id).unwrap();
    let resolved = store
        .get_with_lifecycle(&tenant, &chunk_id)
        .await
        .unwrap()
        .expect("evidence trace chunk should be stored");
    assert_eq!(resolved.lifecycle.expires_at_ms, None);
    assert_eq!(resolved.lifecycle.review_after_ms, None);
    assert_eq!(resolved.lifecycle.tier, MemoryTier::LongTerm);
}

#[tokio::test]
async fn memory_add_applies_short_ttl_to_ordinary_progress_summary() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("progress_summary_ttl").unwrap();
    let before_ms = current_time_ms();

    let add_result = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: tenant.to_string(),
            text: "Mapped auth middleware touchpoints; next step is validating RS256 issuance."
                .to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:progress".to_string()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();
    assert_eq!(add_response.admission_decision.as_deref(), Some("durable"));
    let expires_at = add_response
        .expires_at_ms
        .expect("ordinary progress summary should get a short TTL");
    assert_eq!(add_response.review_after_ms, Some(expires_at));
    assert_eq!(add_response.lifecycle_tier, None);
    assert!(expires_at >= before_ms + WRITE_ADMISSION_PROGRESS_TTL_MS);
    assert!(expires_at <= current_time_ms() + WRITE_ADMISSION_PROGRESS_TTL_MS);

    let chunk_id = ChunkId::parse(&add_response.chunk_id).unwrap();
    let resolved = store
        .get_with_lifecycle(&tenant, &chunk_id)
        .await
        .unwrap()
        .expect("progress chunk should be stored");
    assert_eq!(resolved.lifecycle.expires_at_ms, Some(expires_at));
    assert_eq!(resolved.lifecycle.review_after_ms, Some(expires_at));
    assert_eq!(resolved.lifecycle.tier, MemoryTier::LongTerm);
}

#[tokio::test]
async fn memory_add_keeps_priority_progress_summary_durable() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("progress_summary_priority").unwrap();

    let add_result = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: tenant.to_string(),
            text: "Decision: keep RS256 validation result for future auth work. Agent action: Reuse the validated RS256 result when debugging future auth validation work.".to_string(),
            chunk_type: "summary".to_string(),
            tags: vec!["kind:progress".to_string(), "priority:8".to_string()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();
    assert_eq!(add_response.expires_at_ms, None);
    assert_eq!(add_response.review_after_ms, None);

    let chunk_id = ChunkId::parse(&add_response.chunk_id).unwrap();
    let resolved = store
        .get_with_lifecycle(&tenant, &chunk_id)
        .await
        .unwrap()
        .expect("priority progress chunk should be stored");
    assert_eq!(resolved.lifecycle.expires_at_ms, None);
    assert_eq!(resolved.lifecycle.review_after_ms, None);
}

#[tokio::test]
async fn memory_add_batch_reports_admission_decisions() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("quality_gate_batch").unwrap();

    let result = handle_memory_add_batch(
        &store,
        None,
        AddBatchParams {
            tenant_id: tenant.to_string(),
            chunks: vec![
                BatchChunkParams {
                    text: "Validation: cargo test -p memd passed.".to_string(),
                    chunk_type: "summary".to_string(),
                    tags: vec!["kind:progress".to_string()],
                    ..Default::default()
                },
                BatchChunkParams {
                    text: "starting to inspect the files".to_string(),
                    chunk_type: "summary".to_string(),
                    tags: vec!["kind:progress".to_string()],
                    mode: Some("conversation".to_string()),
                    ..Default::default()
                },
            ],
            supersede_near_duplicates: None,
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let response: AddBatchResult = serde_json::from_str(text).unwrap();
    assert_eq!(response.chunk_ids.len(), 2);
    assert_eq!(
        response.admission_decisions.unwrap(),
        vec!["durable".to_string(), "ephemeral".to_string()]
    );

    let ephemeral_id = ChunkId::parse(&response.chunk_ids[1]).unwrap();
    let resolved = store
        .get_with_lifecycle(&tenant, &ephemeral_id)
        .await
        .unwrap()
        .expect("ephemeral batch chunk should be stored");
    assert_eq!(resolved.lifecycle.tier, MemoryTier::History);
}

#[tokio::test]
async fn memory_add_batch_applies_run_trace_retention_per_chunk() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("run_trace_batch_ttl").unwrap();

    let result = handle_memory_add_batch(
        &store,
        None,
        AddBatchParams {
            tenant_id: tenant.to_string(),
            chunks: vec![
                BatchChunkParams {
                    text: "Command: cargo test -p memd ordinary trace.".to_string(),
                    chunk_type: "trace".to_string(),
                    tags: vec!["kind:run".to_string()],
                    ..Default::default()
                },
                BatchChunkParams {
                    text: "Command: cargo test -p memd evidence trace passed.".to_string(),
                    chunk_type: "trace".to_string(),
                    tags: vec!["kind:run".to_string(), "kind:evidence".to_string()],
                    ..Default::default()
                },
            ],
            supersede_near_duplicates: None,
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let response: AddBatchResult = serde_json::from_str(text).unwrap();
    assert_eq!(response.chunk_ids.len(), 2);

    let ordinary_id = ChunkId::parse(&response.chunk_ids[0]).unwrap();
    let ordinary = store
        .get_with_lifecycle(&tenant, &ordinary_id)
        .await
        .unwrap()
        .expect("ordinary run trace should be stored");
    assert!(
        ordinary.lifecycle.expires_at_ms.is_some(),
        "ordinary batch run trace should get a medium TTL"
    );
    assert_eq!(
        ordinary.lifecycle.review_after_ms,
        ordinary.lifecycle.expires_at_ms
    );

    let evidence_id = ChunkId::parse(&response.chunk_ids[1]).unwrap();
    let evidence = store
        .get_with_lifecycle(&tenant, &evidence_id)
        .await
        .unwrap()
        .expect("evidence run trace should be stored");
    assert_eq!(evidence.lifecycle.expires_at_ms, None);
    assert_eq!(evidence.lifecycle.review_after_ms, None);
}

#[tokio::test]
async fn memory_add_batch_applies_progress_summary_retention_per_chunk() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("progress_summary_batch_ttl").unwrap();

    let result = handle_memory_add_batch(
        &store,
        None,
        AddBatchParams {
            tenant_id: tenant.to_string(),
            chunks: vec![
                BatchChunkParams {
                    text: "Mapped auth middleware touchpoints; next step is RS256 validation."
                        .to_string(),
                    chunk_type: "summary".to_string(),
                    tags: vec!["kind:progress".to_string()],
                    ..Default::default()
                },
                BatchChunkParams {
                    text: "Validation: RS256 auth tests passed after UTC claim normalization."
                        .to_string(),
                    chunk_type: "summary".to_string(),
                    tags: vec!["kind:progress".to_string(), "kind:evidence".to_string()],
                    ..Default::default()
                },
            ],
            supersede_near_duplicates: None,
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let response: AddBatchResult = serde_json::from_str(text).unwrap();
    assert_eq!(response.chunk_ids.len(), 2);

    let ordinary_id = ChunkId::parse(&response.chunk_ids[0]).unwrap();
    let ordinary = store
        .get_with_lifecycle(&tenant, &ordinary_id)
        .await
        .unwrap()
        .expect("ordinary progress summary should be stored");
    assert!(
        ordinary.lifecycle.expires_at_ms.is_some(),
        "ordinary batch progress summary should get a short TTL"
    );
    assert_eq!(
        ordinary.lifecycle.review_after_ms,
        ordinary.lifecycle.expires_at_ms
    );

    let evidence_id = ChunkId::parse(&response.chunk_ids[1]).unwrap();
    let evidence = store
        .get_with_lifecycle(&tenant, &evidence_id)
        .await
        .unwrap()
        .expect("evidence progress summary should be stored");
    assert_eq!(evidence.lifecycle.expires_at_ms, None);
    assert_eq!(evidence.lifecycle.review_after_ms, None);
}

#[tokio::test]
async fn search_filters_by_project_id() {
    let store = make_store();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "project a chunk".to_string(),
            chunk_type: "doc".to_string(),
            project_id: Some("project_a".to_string()),
            episode_id: None,
            source: None,
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "project b chunk".to_string(),
            chunk_type: "doc".to_string(),
            project_id: Some("project_b".to_string()),
            episode_id: None,
            source: None,
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "chunk".to_string(),
            project_id: Some("project_a".to_string()),
            k: 10,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert_eq!(search_response.results.len(), 1);
    assert_eq!(search_response.results[0].text, "project a chunk");
}

#[tokio::test]
async fn search_filters_by_types() {
    let store = make_store();

    for (text, chunk_type) in [
        ("doc chunk", "doc"),
        ("code chunk", "code"),
        ("trace chunk", "trace"),
    ] {
        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: text.to_string(),
                chunk_type: chunk_type.to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![],
                expires_at_ms: None,
                review_after_ms: None,

                mode: None,
                supersede_near_duplicates: None,
                event_time_ms: None,
            },
        )
        .await
        .unwrap();
    }

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "chunk".to_string(),
            project_id: None,
            k: 10,
            filters: Some(SearchFilters {
                types: Some(vec!["code".to_string(), "doc".to_string()]),
                episode_id: None,
                time_range: None,
            }),
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert_eq!(search_response.results.len(), 2);
    assert!(search_response
        .results
        .iter()
        .all(|r| matches!(r.chunk_type.as_str(), "doc" | "code")));
}

#[tokio::test]
async fn search_filters_by_time_range() {
    let store = make_store();
    let tenant_id = TenantId::new("test").unwrap();

    let mut old_chunk = MemoryChunk::new(tenant_id.clone(), "old chunk", ChunkType::Doc);
    old_chunk.timestamp_created =
        crate::structural::parse_iso_datetime("2026-01-01T00:00:00Z").unwrap();
    store.add(old_chunk).await.unwrap();

    let mut middle_chunk = MemoryChunk::new(tenant_id.clone(), "middle chunk", ChunkType::Doc);
    middle_chunk.timestamp_created =
        crate::structural::parse_iso_datetime("2026-01-15T12:00:00Z").unwrap();
    store.add(middle_chunk).await.unwrap();

    let mut new_chunk = MemoryChunk::new(tenant_id, "new chunk", ChunkType::Doc);
    new_chunk.timestamp_created =
        crate::structural::parse_iso_datetime("2026-02-01T00:00:00Z").unwrap();
    store.add(new_chunk).await.unwrap();

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "chunk".to_string(),
            project_id: None,
            k: 10,
            filters: Some(SearchFilters {
                types: None,
                episode_id: None,
                time_range: Some(TimeRange {
                    from: Some("2026-01-10T00:00:00Z".to_string()),
                    to: Some("2026-01-20T23:59:59Z".to_string()),
                }),
            }),
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert_eq!(search_response.results.len(), 1);
    assert_eq!(search_response.results[0].text, "middle chunk");
}

#[tokio::test]
async fn search_filters_by_episode_id() {
    let store = make_store();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "episode alpha".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: Some("ep1".to_string()),
            source: None,
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "episode beta".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: Some("ep2".to_string()),
            source: None,
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "episode".to_string(),
            project_id: None,
            k: 10,
            filters: Some(SearchFilters {
                types: None,
                episode_id: Some("ep1".to_string()),
                time_range: None,
            }),
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert_eq!(search_response.results.len(), 1);
    assert_eq!(
        search_response.results[0].episode_id.as_deref(),
        Some("ep1")
    );
}

#[tokio::test]
async fn search_returns_citation_with_provenance_and_offsets() {
    let store = make_store();

    let long_text = format!(
        "alpha_unique_marker {}",
        "lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(80)
    );

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: long_text,
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: Some(SourceParams {
                uri: Some("file:///tmp/test_doc.md".to_string()),
                repo: Some("acme/repo".to_string()),
                commit: Some("abc123".to_string()),
                path: Some("docs/test_doc.md".to_string()),
                tool_name: Some("ingest".to_string()),
                tool_call_id: Some("call-1".to_string()),
            }),
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "alpha_unique_marker".to_string(),
            project_id: None,
            k: 10,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert!(!search_response.results.is_empty());

    let citation = search_response.results[0]
        .citation
        .as_ref()
        .expect("citation should be present");

    assert!(!citation.citation_id.is_empty());
    assert!(!citation.content_hash.is_empty());
    assert_eq!(citation.source_path.as_deref(), Some("docs/test_doc.md"));
    assert_eq!(citation.source_tool_name.as_deref(), Some("ingest"));
    assert!(citation.chunk_index.is_some());
    assert!(citation.total_chunks.is_some());
    assert!(citation.char_start.is_some());
    assert!(citation.char_end.is_some());
}

#[tokio::test]
async fn search_repair_loop_recovers_result() {
    let store = make_store();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "alpha unique marker".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "alpha!unique?marker".to_string(),
            project_id: None,
            k: 5,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let text = result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert_eq!(search_response.results.len(), 1);
    assert_eq!(search_response.results[0].text, "alpha unique marker");

    let repair_info = search_response
        .repair_info
        .as_ref()
        .expect("repair_info should be present");
    assert!(repair_info.attempted);
    assert!(repair_info.repaired);
    assert_eq!(
        repair_info.repaired_query.as_deref(),
        Some("alpha unique marker")
    );
}

#[tokio::test]
async fn add_with_all_fields() {
    let store = make_store();

    let add_params = AddParams {
        tenant_id: "test".to_string(),
        text: "function hello() {}".to_string(),
        chunk_type: "code".to_string(),
        project_id: Some("my_project".to_string()),
        episode_id: None,
        source: Some(SourceParams {
            path: Some("src/main.rs".to_string()),
            repo: Some("my-repo".to_string()),
            ..Default::default()
        }),
        tags: vec!["rust".to_string(), "function".to_string()],
        expires_at_ms: None,
        review_after_ms: None,

        mode: None,
        supersede_near_duplicates: None,
        event_time_ms: None,
    };

    let result = handle_memory_add(&store, None, add_params).await.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: AddResult = serde_json::from_str(text).unwrap();

    // Verify the chunk was stored correctly
    let get_params = GetParams {
        tenant_id: "test".to_string(),
        chunk_id: response.chunk_id.clone(),
        include_superseded: None,
        include_expired: None,
        include_history: None,
    };

    let get_result = handle_memory_get(&store, get_params).await.unwrap();
    let text = get_result["content"][0]["text"].as_str().unwrap();
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["found"].as_bool(), Some(true));
    let chunk: MemoryChunk = serde_json::from_value(body["chunk"].clone()).unwrap();

    assert_eq!(chunk.text, "function hello() {}");
    assert_eq!(chunk.chunk_type, ChunkType::Code);
    assert_eq!(chunk.source.path, Some("src/main.rs".to_string()));
    assert_eq!(chunk.tags, vec!["rust", "function"]);
}

#[tokio::test]
async fn add_batch() {
    let store = make_store();

    let params = AddBatchParams {
        tenant_id: "test".to_string(),
        supersede_near_duplicates: None,
        chunks: vec![
            BatchChunkParams {
                text: "chunk 1".to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![],
                expires_at_ms: None,
                review_after_ms: None,

                mode: None,
                event_time_ms: None,
            },
            BatchChunkParams {
                text: "chunk 2".to_string(),
                chunk_type: "code".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![],
                expires_at_ms: None,
                review_after_ms: None,

                mode: None,
                event_time_ms: None,
            },
        ],
    };

    let result = handle_memory_add_batch(&store, None, params).await.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let response: AddBatchResult = serde_json::from_str(text).unwrap();
    assert_eq!(response.chunk_ids.len(), 2);
}

#[tokio::test]
async fn delete_chunk() {
    let store = make_store();

    // Add a chunk
    let add_params = AddParams {
        tenant_id: "test".to_string(),
        text: "to be deleted".to_string(),
        chunk_type: "doc".to_string(),
        project_id: None,
        episode_id: None,
        source: None,
        tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,

        mode: None,
        supersede_near_duplicates: None,
        event_time_ms: None,
    };

    let add_result = handle_memory_add(&store, None, add_params).await.unwrap();
    let text = add_result["content"][0]["text"].as_str().unwrap();
    let add_response: AddResult = serde_json::from_str(text).unwrap();

    // Delete it
    let delete_params = DeleteParams {
        tenant_id: "test".to_string(),
        chunk_id: add_response.chunk_id.clone(),
    };

    let delete_result = handle_memory_delete(&store, delete_params).await.unwrap();
    let text = delete_result["content"][0]["text"].as_str().unwrap();
    let delete_response: DeleteResult = serde_json::from_str(text).unwrap();
    assert!(delete_response.deleted);

    // Verify it's no longer retrievable
    let get_params = GetParams {
        tenant_id: "test".to_string(),
        chunk_id: add_response.chunk_id,
        include_superseded: None,
        include_expired: None,
        include_history: None,
    };

    let get_result = handle_memory_get(&store, get_params).await.unwrap();
    let text = get_result["content"][0]["text"].as_str().unwrap();
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        body["found"].as_bool(),
        Some(false),
        "deleted chunk must surface as found=false via memory.get"
    );
}

#[tokio::test]
async fn feedback_records_relevance_event() {
    let store = make_store();

    let add_result = handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "feedback target chunk".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();
    let add_text = add_result["content"][0]["text"].as_str().unwrap();
    let add_payload: AddResult = serde_json::from_str(add_text).unwrap();

    let feedback_result = handle_memory_feedback(
        &store,
        FeedbackParams {
            tenant_id: "test".to_string(),
            query: "feedback target".to_string(),
            chunk_id: add_payload.chunk_id,
            relevance: "relevant".to_string(),
        },
    )
    .await
    .unwrap();

    let text = feedback_result["content"][0]["text"].as_str().unwrap();
    let payload: FeedbackResult = serde_json::from_str(text).unwrap();
    assert!(payload.stored);
}

#[tokio::test]
async fn stats() {
    let store = make_store();

    // Add some chunks
    for i in 0..3 {
        let add_params = AddParams {
            tenant_id: "test".to_string(),
            text: format!("doc {}", i),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        };
        handle_memory_add(&store, None, add_params).await.unwrap();
    }

    let params = StatsParams {
        tenant_id: "test".to_string(),
    };

    let result = handle_memory_stats(&store, None, params).await.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let stats: StatsResult = serde_json::from_str(text).unwrap();

    assert_eq!(stats.total_chunks, 3);
    assert_eq!(stats.deleted_chunks, 0);
    assert_eq!(stats.chunk_types.get("doc"), Some(&3));
}

#[tokio::test]
async fn memory_search_compact_omits_large_fields_but_keeps_trust_metadata() {
    let store = make_store();

    for i in 0..5 {
        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: format!(
                    "alpha compact response fixture {i} {}",
                    "long repeated context ".repeat(80)
                ),
                chunk_type: "doc".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "alpha compact response".to_string(),
            k: 5,
            compact: true,
            token_budget: Some(1200),
            include_text: Some(false),
            include_artifact: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let payload: SearchResult = parse_tool_payload(&result);
    assert!(!payload.results.is_empty());
    let budget = payload
        .budget_info
        .expect("compact search should include budget info");
    assert_eq!(budget.requested_budget, Some(1200));
    assert!(budget.omitted_fields.contains(&"text".to_string()));
    assert!(budget.omitted_fields.contains(&"artifact".to_string()));

    for hit in payload.results {
        assert!(!hit.chunk_id.is_empty());
        assert!(hit.text.is_empty());
        assert!(hit.artifact.is_none());
        assert_eq!(hit.trust_tier, TrustTier::SemanticCandidate);
        assert!(hit.verification_hint.requires_verification);
    }
}

#[tokio::test]
async fn memory_health_reports_empty_and_duplicate_scopes() {
    let (store, _dir) = make_persistent_store();
    let metrics = MetricsCollector::new(10);

    let empty = handle_memory_health(
        &store,
        &metrics,
        HealthParams {
            tenant_id: "tenant_empty".to_string(),
            project_id: None,
            include_examples: false,
            duplicate_limit: 10,
            include_recent: false,
        },
    )
    .await
    .unwrap();
    let empty_payload: MemoryHealthResult = parse_tool_payload(&empty);
    assert_eq!(empty_payload.counts.total_chunks, 0);
    assert!(empty_payload
        .warnings
        .contains(&"no chunks found for requested scope".to_string()));

    let tenant = TenantId::new("tenant_health").unwrap();
    for text in [
        "duplicate health text",
        "duplicate health text",
        "unique health text",
    ] {
        store
            .add(MemoryChunk::new(tenant.clone(), text, ChunkType::Doc))
            .await
            .unwrap();
    }
    metrics.record_query(QueryMetrics {
        total_ms: 42,
        ..Default::default()
    });

    let result = handle_memory_health(
        &store,
        &metrics,
        HealthParams {
            tenant_id: "tenant_health".to_string(),
            project_id: None,
            include_examples: true,
            duplicate_limit: 5,
            include_recent: true,
        },
    )
    .await
    .unwrap();
    let payload: MemoryHealthResult = parse_tool_payload(&result);

    assert_eq!(payload.counts.total_chunks, 3);
    assert_eq!(payload.duplicates.unique_text_count, 2);
    assert_eq!(payload.duplicates.exact_duplicate_group_count, 1);
    assert_eq!(payload.duplicates.duplicate_row_count, 1);
    assert!((payload.duplicates.duplicate_row_ratio - (1.0 / 3.0)).abs() < 0.001);
    assert_eq!(payload.duplicates.examples.len(), 1);
    assert_eq!(payload.index_coverage.indexed_percentage, 100.0);
    assert_eq!(payload.latency.recent_search_count, 1);
    assert_eq!(payload.latency.p95_total_ms, 42);
}

#[tokio::test]
async fn invalid_tenant_id() {
    let store = make_store();

    let params = SearchParams {
        tenant_id: "invalid-tenant".to_string(), // hyphens not allowed
        query: "test".to_string(),
        project_id: None,
        k: 10,
        filters: None,
        debug_tiers: None,
        mode: None,
        include_superseded: None,
        include_expired: None,
        include_history: None,
        oversample_factor: None,
        expand_event_siblings: false,
        compact: false,
        token_budget: None,
        include_text: None,
        include_artifact: None,
        suppress_usage_event: false,
        ..Default::default()
    };

    let result = handle_memory_search(&store, params).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
}

#[tokio::test]
async fn invalid_chunk_type() {
    let store = make_store();

    let params = AddParams {
        tenant_id: "test".to_string(),
        text: "hello".to_string(),
        chunk_type: "invalid_type".to_string(),
        project_id: None,
        episode_id: None,
        source: None,
        tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,

        mode: None,
        supersede_near_duplicates: None,
        event_time_ms: None,
    };

    let result = handle_memory_add(&store, None, params).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
}

#[tokio::test]
async fn invalid_chunk_id() {
    let store = make_store();

    let params = GetParams {
        tenant_id: "test".to_string(),
        chunk_id: "not-a-uuid".to_string(),
        include_superseded: None,
        include_expired: None,
        include_history: None,
    };

    let result = handle_memory_get(&store, params).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), McpError::InvalidParams(_)));
}

#[tokio::test]
async fn tenant_isolation() {
    let store = make_store();

    // Add chunk as tenant A
    let add_params = AddParams {
        tenant_id: "tenant_a".to_string(),
        text: "secret data".to_string(),
        chunk_type: "doc".to_string(),
        project_id: None,
        episode_id: None,
        source: None,
        tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,

        mode: None,
        supersede_near_duplicates: None,
        event_time_ms: None,
    };

    handle_memory_add(&store, None, add_params).await.unwrap();

    // Search as tenant B - should return empty
    let search_params = SearchParams {
        tenant_id: "tenant_b".to_string(),
        query: "secret".to_string(),
        project_id: None,
        k: 10,
        filters: None,
        debug_tiers: None,
        mode: None,
        include_superseded: None,
        include_expired: None,
        include_history: None,
        oversample_factor: None,
        expand_event_siblings: false,
        compact: false,
        token_budget: None,
        include_text: None,
        include_artifact: None,
        suppress_usage_event: false,
        ..Default::default()
    };

    let result = handle_memory_search(&store, search_params).await.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();
    assert!(search_response.results.is_empty());
}

#[tokio::test]
async fn search_with_debug_tiers() {
    let store = make_store();

    // Add a chunk
    let add_params = AddParams {
        tenant_id: "test".to_string(),
        text: "debug tier test".to_string(),
        chunk_type: "doc".to_string(),
        project_id: None,
        episode_id: None,
        source: None,
        tags: vec![],
        expires_at_ms: None,
        review_after_ms: None,

        mode: None,
        supersede_near_duplicates: None,
        event_time_ms: None,
    };

    handle_memory_add(&store, None, add_params).await.unwrap();

    // Search with debug_tiers enabled
    let search_params = SearchParams {
        tenant_id: "test".to_string(),
        query: "debug".to_string(),
        project_id: None,
        k: 10,
        filters: None,
        debug_tiers: Some(true),
        mode: None,
        include_superseded: None,
        include_expired: None,
        include_history: None,
        oversample_factor: None,
        expand_event_siblings: false,
        compact: false,
        token_budget: None,
        include_text: None,
        include_artifact: None,
        suppress_usage_event: false,
        ..Default::default()
    };

    let result = handle_memory_search(&store, search_params).await.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap();
    let search_response: SearchResult = serde_json::from_str(text).unwrap();

    // MemoryStore doesn't have tiered support, so tier_info should be None
    // and source_tier on results should be None (since timing is None)
    assert_eq!(search_response.results.len(), 1);
    assert!(search_response.tier_info.is_none());
}

#[tokio::test]
async fn context_list_subsystems_groups_by_subsystem_tag() {
    let store = make_store();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "retrieval planning doc".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: Some(SourceParams {
                path: Some("src/retrieval/mod.rs".to_string()),
                ..Default::default()
            }),
            tags: vec![
                "ctx:doc".to_string(),
                "ctx:subsystem:retrieval".to_string(),
                "ctx:file:src/retrieval/mod.rs".to_string(),
            ],

            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "retrieval indexing notes".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: Some(SourceParams {
                path: Some("src/retrieval/index.rs".to_string()),
                ..Default::default()
            }),
            tags: vec![
                "ctx:doc".to_string(),
                "ctx:subsystem:retrieval".to_string(),
                "ctx:file:src/retrieval/index.rs".to_string(),
            ],

            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "planner decision".to_string(),
            chunk_type: "decision".to_string(),
            project_id: None,
            episode_id: None,
            source: Some(SourceParams {
                path: Some("src/planner/mod.rs".to_string()),
                ..Default::default()
            }),
            tags: vec![
                "ctx:doc".to_string(),
                "ctx:subsystem:planner".to_string(),
                "ctx:file:src/planner/mod.rs".to_string(),
            ],

            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    let result = handle_context_list_subsystems(
        &store,
        ContextListSubsystemsParams {
            tenant_id: "test".to_string(),
            prefix: None,
            limit: 50,
        },
    )
    .await
    .unwrap();

    let payload: ContextListSubsystemsResult = parse_tool_payload(&result);
    assert_eq!(payload.subsystems.len(), 2);

    let retrieval = payload
        .subsystems
        .iter()
        .find(|entry| entry.key == "retrieval")
        .expect("retrieval subsystem should exist");
    assert_eq!(retrieval.chunk_count, 2);
    assert_eq!(retrieval.file_count, 2);
}

#[tokio::test]
async fn context_get_files_for_subsystem_returns_tag_and_source_paths() {
    let store = make_store();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "storage architecture".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: Some(SourceParams {
                path: Some("crates/memd/src/store/mod.rs".to_string()),
                ..Default::default()
            }),
            tags: vec![
                "ctx:subsystem:storage".to_string(),
                "ctx:file:crates/memd/src/store/hybrid.rs".to_string(),
            ],

            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    let result = handle_context_get_files_for_subsystem(
        &store,
        ContextGetFilesForSubsystemParams {
            tenant_id: "test".to_string(),
            subsystem_key: "storage".to_string(),
            limit: 10,
        },
    )
    .await
    .unwrap();

    let payload: ContextGetFilesForSubsystemResult = parse_tool_payload(&result);
    assert_eq!(payload.subsystem_key, "storage");
    assert_eq!(payload.files.len(), 2);
    assert!(payload
        .files
        .contains(&"crates/memd/src/store/mod.rs".to_string()));
    assert!(payload
        .files
        .contains(&"crates/memd/src/store/hybrid.rs".to_string()));
}

#[tokio::test]
async fn context_search_documents_filters_by_tier_and_subsystem() {
    let store = make_store();

    for (text, tier_tag) in [
        ("hot retrieval context", "ctx:tier:hot"),
        ("cold retrieval context", "ctx:tier:cold"),
    ] {
        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: "test".to_string(),
                text: text.to_string(),
                chunk_type: "doc".to_string(),
                project_id: None,
                episode_id: None,
                source: None,
                tags: vec![
                    "ctx:doc".to_string(),
                    "ctx:subsystem:retrieval".to_string(),
                    tier_tag.to_string(),
                ],

                expires_at_ms: None,
                review_after_ms: None,

                mode: None,
                supersede_near_duplicates: None,
                event_time_ms: None,
            },
        )
        .await
        .unwrap();
    }

    let result = handle_context_search_documents(
        &store,
        ContextSearchDocumentsParams {
            tenant_id: "test".to_string(),
            query: "retrieval".to_string(),
            k: 10,
            subsystem_key: Some("retrieval".to_string()),
            tier: Some("hot".to_string()),
        },
    )
    .await
    .unwrap();

    let payload: ContextSearchDocumentsResult = parse_tool_payload(&result);
    assert_eq!(payload.results.len(), 1);
    assert_eq!(payload.results[0].text, "hot retrieval context");
    assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
}

#[tokio::test]
async fn context_find_relevant_context_can_prepend_hot_chunks() {
    let store = make_store();
    let tenant = TenantId::new("test").unwrap();

    let mut hot = MemoryChunk::new(tenant.clone(), "incident runbook", ChunkType::Doc);
    hot.tags = vec!["ctx:tier:hot".to_string(), "ctx:subsystem:ops".to_string()];
    hot.timestamp_created = 10;
    store.add(hot).await.unwrap();

    let mut relevant = MemoryChunk::new(
        tenant,
        "database migration checklist for ops",
        ChunkType::Doc,
    );
    relevant.tags = vec![
        "ctx:doc".to_string(),
        "ctx:subsystem:ops".to_string(),
        "ctx:tier:cold".to_string(),
    ];
    relevant.timestamp_created = 5;
    store.add(relevant).await.unwrap();

    let result = handle_context_find_relevant_context(
        &store,
        ContextFindRelevantContextParams {
            tenant_id: "test".to_string(),
            task: "database migration".to_string(),
            k: 5,
            subsystem_keys: Some(vec!["ops".to_string()]),
            include_hot: true,
        },
    )
    .await
    .unwrap();

    let payload: ContextFindRelevantContextResult = parse_tool_payload(&result);
    assert!(payload.hot_included);
    assert!(!payload.results.is_empty());
    assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
}

#[tokio::test]
async fn context_suggest_agent_uses_trigger_and_file_matches() {
    let store = make_store();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "test".to_string(),
            text: "storage compaction and WAL tuning playbook".to_string(),
            chunk_type: "doc".to_string(),
            project_id: None,
            episode_id: None,
            source: None,
            tags: vec![
                "ctx:agent:storage-specialist".to_string(),
                "ctx:trigger:crates/memd/src/store/*".to_string(),
                "ctx:subsystem:storage".to_string(),
                "ctx:file:crates/memd/src/store/hybrid.rs".to_string(),
                "ctx:tier:hot".to_string(),
            ],

            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    let result = handle_context_suggest_agent(
        &store,
        ContextSuggestAgentParams {
            tenant_id: "test".to_string(),
            task: "Improve storage compaction behavior".to_string(),
            changed_files: Some(vec!["crates/memd/src/store/hybrid.rs".to_string()]),
            k: 3,
        },
    )
    .await
    .unwrap();

    let payload: ContextSuggestAgentResult = parse_tool_payload(&result);
    assert!(!payload.recommendations.is_empty());
    assert_eq!(
        payload.recommendations[0].agent_name,
        "storage-specialist".to_string()
    );
    assert!(!payload.recommendations[0].matched_triggers.is_empty());
}

#[tokio::test]
async fn context_get_hot_context_returns_most_recent_chunks() {
    let store = make_store();
    let tenant = TenantId::new("test").unwrap();

    let mut older = MemoryChunk::new(tenant.clone(), "older hot context", ChunkType::Doc);
    older.tags = vec!["ctx:tier:hot".to_string()];
    older.timestamp_created = 1;
    store.add(older).await.unwrap();

    let mut newest = MemoryChunk::new(tenant, "newest hot context", ChunkType::Doc);
    newest.tags = vec!["ctx:tier:hot".to_string()];
    newest.timestamp_created = 2;
    store.add(newest).await.unwrap();

    let result = handle_context_get_hot_context(
        &store,
        ContextGetHotContextParams {
            tenant_id: "test".to_string(),
            k: 1,
        },
    )
    .await
    .unwrap();

    let payload: ContextGetHotContextResult = parse_tool_payload(&result);
    assert_eq!(payload.results.len(), 1);
    assert_eq!(payload.results[0].text, "newest hot context");
    assert_eq!(payload.results[0].source_tier.as_deref(), Some("hot"));
}

#[tokio::test]
async fn task_get_returns_full_artifact_history() {
    let store = make_store();

    let start: TaskArtifactResult = parse_tool_payload(
        &handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "test".to_string(),
                project_id: Some("proj_alpha".to_string()),
                parent_task_id: None,
                agent_id: Some("agent-1".to_string()),
                session_id: Some("session-7".to_string()),
                goal: "Quantify the stress-response regulon".to_string(),
                motivation: "The regulator mechanism is unresolved".to_string(),
                hypothesis: "Sigma factor S drives the induced genes".to_string(),
                scientific_question: "Which genes increase after the perturbation?".to_string(),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "rna_seq".to_string(),
                    version: Some("v1".to_string()),
                    description: None,
                }],
                expected_outputs: vec!["differential expression table".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap(),
    );

    handle_task_progress(
        &store,
        None,
        TaskProgressParams {
            tenant_id: "test".to_string(),
            task_id: start.task_id.clone(),
            project_id: Some("proj_alpha".to_string()),
            agent_id: None,
            session_id: None,
            summary: "Initial QC exposed one low-depth replicate".to_string(),
            blockers: vec!["One replicate is borderline".to_string()],
            failed_attempts: vec!["Default trimming removed too much signal".to_string()],
            next_step: "Re-run with stricter QC but lighter trimming".to_string(),
            dataset_refs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    handle_task_run_start(
        &store,
        None,
        TaskRunStartParams {
            tenant_id: "test".to_string(),
            task_id: start.task_id.clone(),
            project_id: Some("proj_alpha".to_string()),
            agent_id: None,
            session_id: None,
            tool_name: "mmseqs".to_string(),
            tool_version: Some("15".to_string()),
            command: "mmseqs search db query out tmp".to_string(),
            why_chosen: "Fast enough for iterative parameter sweeps".to_string(),
            parameters: json!({"sensitivity": 7.5}),
            inputs: vec!["query.faa".to_string()],
            summary: Some("Homology search for candidate regulators".to_string()),
            dataset_refs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    handle_task_run_finish(
        &store,
        None,
        TaskRunFinishParams {
            tenant_id: "test".to_string(),
            task_id: start.task_id.clone(),
            project_id: Some("proj_alpha".to_string()),
            agent_id: None,
            session_id: None,
            status: "completed".to_string(),
            tool_name: Some("mmseqs".to_string()),
            tool_version: Some("15".to_string()),
            command: Some("mmseqs search db query out tmp".to_string()),
            outputs: vec!["hits.tsv".to_string()],
            metrics: Some(json!({"top_hit_bitscore": 310.5})),
            notes: "Recovered a strong candidate regulator".to_string(),
            validation: vec!["Top hit was stable across reruns".to_string()],
            dataset_refs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    handle_task_add_evidence(
        &store,
        None,
        TaskAddEvidenceParams {
            tenant_id: "test".to_string(),
            task_id: start.task_id.clone(),
            project_id: Some("proj_alpha".to_string()),
            agent_id: None,
            session_id: None,
            summary: "Top hit exceeded the curated threshold".to_string(),
            evidence_kind: "metric".to_string(),
            supports_claim: Some(true),
            metric_name: Some("top_hit_bitscore".to_string()),
            metric_value: Some(json!(310.5)),
            metrics: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    let result = handle_task_get(
        &store,
        TaskGetParams {
            tenant_id: "test".to_string(),
            task_id: start.task_id,
        },
    )
    .await
    .unwrap();

    let payload: TaskGetResult = parse_tool_payload(&result);
    assert_eq!(payload.artifacts.len(), 5);
    assert!(payload
        .artifacts
        .iter()
        .any(|artifact| artifact.artifact_kind == ArtifactKind::TaskStart));
    assert!(payload
        .artifacts
        .iter()
        .any(|artifact| artifact.artifact_kind == ArtifactKind::Evidence));
}

#[tokio::test]
async fn task_search_filters_exactly_by_tool_and_dataset() {
    let store = make_store();

    let task_a: TaskArtifactResult = parse_tool_payload(
        &handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "test".to_string(),
                project_id: Some("proj_alpha".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Task A goal".to_string(),
                motivation: "Task A motivation".to_string(),
                hypothesis: "Task A hypothesis".to_string(),
                scientific_question: "Task A question".to_string(),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "rna_seq".to_string(),
                    version: Some("v1".to_string()),
                    description: None,
                }],
                expected_outputs: vec!["table".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap(),
    );

    handle_task_run_start(
        &store,
        None,
        TaskRunStartParams {
            tenant_id: "test".to_string(),
            task_id: task_a.task_id.clone(),
            project_id: Some("proj_alpha".to_string()),
            agent_id: None,
            session_id: None,
            tool_name: "mmseqs".to_string(),
            tool_version: None,
            command: "mmseqs search db query out tmp".to_string(),
            why_chosen: "Fast iterative search".to_string(),
            parameters: json!({"sensitivity": 7.5}),
            inputs: vec!["query.faa".to_string()],
            summary: Some("Candidate search".to_string()),
            dataset_refs: vec![TaskDatasetRefParams {
                name: "rna_seq".to_string(),
                version: Some("v1".to_string()),
                description: None,
            }],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    let task_b: TaskArtifactResult = parse_tool_payload(
        &handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "test".to_string(),
                project_id: Some("proj_beta".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Task B goal".to_string(),
                motivation: "Task B motivation".to_string(),
                hypothesis: "Task B hypothesis".to_string(),
                scientific_question: "Task B question".to_string(),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "proteomics".to_string(),
                    version: Some("v2".to_string()),
                    description: None,
                }],
                expected_outputs: vec!["summary".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap(),
    );

    handle_task_run_start(
        &store,
        None,
        TaskRunStartParams {
            tenant_id: "test".to_string(),
            task_id: task_b.task_id,
            project_id: Some("proj_beta".to_string()),
            agent_id: None,
            session_id: None,
            tool_name: "blast".to_string(),
            tool_version: None,
            command: "blastp -query q -db db".to_string(),
            why_chosen: "Reference comparison".to_string(),
            parameters: json!({"evalue": 1e-5}),
            inputs: vec!["query.faa".to_string()],
            summary: Some("Candidate search".to_string()),
            dataset_refs: vec![TaskDatasetRefParams {
                name: "proteomics".to_string(),
                version: Some("v2".to_string()),
                description: None,
            }],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    let result = handle_task_search(
        &store,
        TaskSearchParams {
            tenant_id: "test".to_string(),
            query: "parameter sweeps".to_string(),
            k: 10,
            filters: Some(TaskSearchFiltersParams {
                task_id: Some(task_a.task_id),
                artifact_kind: Some("run_start".to_string()),
                status: Some("started".to_string()),
                challenge_id: None,
                thread_id: None,
                reply_to_artifact_id: None,
                artifact_role: None,
                dataset_name: Some("rna_seq".to_string()),
                dataset_version: Some("v1".to_string()),
                entity_name: None,
                entity_type: None,
                tool_name: Some("mmseqs".to_string()),
                project_id: Some("proj_alpha".to_string()),
                agent_id: None,
                session_id: None,
                requested_action: None,
                verification_status: None,
                relation_kind: None,
            }),
            mode: None,
            compact: false,
            token_budget: None,
            include_artifact: None,
            include_matched_text: None,
        },
    )
    .await
    .unwrap();

    let payload: SearchResult = parse_tool_payload(&result);
    assert_eq!(payload.results.len(), 1);
    assert!(payload.results[0]
        .tags
        .iter()
        .any(|tag| tag.starts_with("task:kind:run_start")));
}

#[tokio::test]
async fn task_search_project_scope_spans_tenants() {
    let _flag_guard = with_fallback_flag().await;
    let store = make_store();

    // This test exercises the LEGACY cross-tenant project fallback,
    // which became opt-in in v0.3.1 (see the tenant-isolation
    // regression test above). Flip the flag on for this scenario only
    // and restore the default at the end so sibling tests stay
    // isolated.
    set_cross_tenant_project_fallback(true);

    let start: TaskArtifactResult = parse_tool_payload(
        &handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "default".to_string(),
                project_id: Some("advanced_benchmark".to_string()),
                parent_task_id: None,
                agent_id: None,
                session_id: None,
                goal: "Record benchmark continuity".to_string(),
                motivation: "Later agents should recover this task across tenant aliases"
                    .to_string(),
                hypothesis: "Project-scoped retrieval should bridge tenant mismatch".to_string(),
                scientific_question: "Can task search recover cross-tenant project history?"
                    .to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["handoff".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap(),
    );

    handle_task_progress(
        &store,
        None,
        TaskProgressParams {
            tenant_id: "default".to_string(),
            task_id: start.task_id,
            project_id: Some("advanced_benchmark".to_string()),
            agent_id: None,
            session_id: None,
            summary: "Recovered prior benchmark context".to_string(),
            blockers: vec![],
            failed_attempts: vec![],
            next_step: "Continue strict reproduction".to_string(),
            dataset_refs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    let result = handle_task_search(
        &store,
        TaskSearchParams {
            tenant_id: "benchmark".to_string(),
            query: "benchmark continuity".to_string(),
            k: 5,
            filters: Some(TaskSearchFiltersParams {
                project_id: Some("advanced_benchmark".to_string()),
                ..Default::default()
            }),
            mode: None,
            compact: false,
            token_budget: None,
            include_artifact: None,
            include_matched_text: None,
        },
    )
    .await
    .unwrap();

    let payload: SearchResult = parse_tool_payload(&result);
    assert!(!payload.results.is_empty());
    let artifact = payload.results[0]
        .artifact
        .clone()
        .expect("artifact should be attached");
    assert_eq!(artifact.tenant_id.as_str(), "default");
    assert_eq!(artifact.project_id.as_option(), Some("advanced_benchmark"));

    set_cross_tenant_project_fallback(false);
}

#[tokio::test]
async fn memory_search_project_scope_spans_tenants_for_raw_chunks() {
    let _flag_guard = with_fallback_flag().await;
    // Same legacy-fallback scenario as task_search_project_scope_spans_tenants:
    // the widening is opt-in in v0.3.1+ and must be enabled here.
    set_cross_tenant_project_fallback(true);

    let store = make_store();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "default".to_string(),
            text: "strict reproduction blocker for advanced benchmark".to_string(),
            chunk_type: "summary".to_string(),
            project_id: Some("advanced_benchmark".to_string()),
            episode_id: None,
            source: None,
            tags: vec![],
            expires_at_ms: None,
            review_after_ms: None,

            mode: None,
            supersede_near_duplicates: None,
            event_time_ms: None,
        },
    )
    .await
    .unwrap();

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "benchmark".to_string(),
            query: "strict reproduction blocker".to_string(),
            project_id: Some("advanced_benchmark".to_string()),
            k: 5,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let payload: SearchResult = parse_tool_payload(&result);
    assert!(!payload.results.is_empty());
    assert!(payload.results[0]
        .text
        .contains("strict reproduction blocker"));

    set_cross_tenant_project_fallback(false);
}

#[tokio::test]
async fn memory_search_project_scope_respects_isolation_default_for_raw_chunks() {
    let _flag_guard = with_fallback_flag().await;
    set_cross_tenant_project_fallback(false);

    let store = make_store();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "tenant_a".to_string(),
            text: "tenant a private alpha".to_string(),
            chunk_type: "doc".to_string(),
            project_id: Some("shared".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "tenant_b".to_string(),
            text: "tenant b private beta".to_string(),
            chunk_type: "doc".to_string(),
            project_id: Some("shared".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "tenant_a".to_string(),
            query: "private".to_string(),
            project_id: Some("shared".to_string()),
            k: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let payload: SearchResult = parse_tool_payload(&result);
    assert_eq!(payload.results.len(), 1);
    assert_eq!(payload.results[0].text, "tenant a private alpha");
}

#[tokio::test]
async fn memory_search_rescues_exact_code_token_when_indexes_miss() {
    let tenant = TenantId::new("default").unwrap();
    let chunk = MemoryChunk::new(
        tenant.clone(),
        "Recovered prior repeat-spike benchmark run context after chat loss.",
        ChunkType::Summary,
    )
    .with_project(ProjectId::new(Some("virosync".to_string())))
    .with_tags(vec![
        "kind:finish".to_string(),
        "benchmark:repeat_spike".to_string(),
    ]);
    let store = SearchMissStore::new(vec![chunk]);

    let rescued = exact_lexical_candidates_for_tenants(
        &store,
        &[tenant],
        "repeat_spike",
        Some("virosync"),
        5,
    )
    .await
    .unwrap();

    assert_eq!(rescued.len(), 1);
    assert!(rescued[0].0.text.contains("repeat-spike benchmark"));
}

#[tokio::test]
async fn memory_search_rescues_project_scoped_signal_terms_when_indexes_miss() {
    let tenant = TenantId::new("memd").unwrap();
    let target = MemoryChunk::new(
        tenant.clone(),
        "Phase 4 purge-side physical segment rewrite copied live chunks into fresh segments, removed hidden payload bytes, kept durable payloads readable, and reported bytes reclaimed after metadata hard-delete.",
        ChunkType::Summary,
    )
    .with_project(ProjectId::new(Some("memd".to_string())))
    .with_tags(vec![
        "kind:progress".to_string(),
        "phase:4".to_string(),
        "priority:8".to_string(),
    ]);
    let decoy = MemoryChunk::new(
        tenant,
        "Token savings benchmark evidence for agent retrieval payload accounting.",
        ChunkType::Research,
    )
    .with_project(ProjectId::new(Some("memd".to_string())));
    let store = SearchMissStore::new(vec![decoy, target.clone()]);

    let result = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "memd".to_string(),
            query: "purge rewrite segments physical segment payload bytes reclaimed hard delete hidden durable remains".to_string(),
            project_id: Some("memd".to_string()),
            k: 5,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let payload: SearchResult = parse_tool_payload(&result);
    assert_eq!(payload.results.len(), 1);
    assert_eq!(payload.results[0].chunk_id, target.chunk_id.to_string());
}

#[test]
fn merge_preferred_and_raw_orders_by_boosted_score() {
    let tenant = TenantId::new("memd").unwrap();
    let preferred = MemoryChunk::new(
        tenant.clone(),
        "older preferred project summary",
        ChunkType::Summary,
    );
    let raw = MemoryChunk::new(tenant, "strong raw lexical match", ChunkType::Summary);

    let merged = merge_preferred_and_raw(vec![(preferred, 1.0)], vec![(raw.clone(), 20.0)], 2);

    assert_eq!(merged[0].0.chunk_id, raw.chunk_id);
    assert_eq!(merged[0].1, 20.0);
}

#[test]
fn dedupe_by_source_uri_keeps_best_ranked_per_source() {
    let tenant = TenantId::new("memd").unwrap();
    let mk = |uri: Option<&str>, text: &str| {
        let mut chunk = MemoryChunk::new(tenant.clone(), text, ChunkType::Doc);
        chunk.source.uri = uri.map(str::to_string);
        chunk
    };
    // Sorted best-first, as the call sites guarantee.
    let scored = vec![
        (mk(Some("doc:a"), "a fragment 1"), 0.9),
        (mk(Some("doc:a"), "a fragment 2"), 0.8),
        (mk(None, "no uri kept"), 0.7),
        (mk(Some("doc:b"), "b fragment"), 0.6),
        (mk(None, "no uri also kept"), 0.5),
    ];

    let out = dedupe_scored_chunks_by_source_uri(scored);

    let scores: Vec<f32> = out.iter().map(|(_, s)| *s).collect();
    assert_eq!(scores, vec![0.9, 0.7, 0.6, 0.5]);
    assert_eq!(out[0].0.source.uri.as_deref(), Some("doc:a"));
    assert_eq!(out[2].0.source.uri.as_deref(), Some("doc:b"));
}

#[tokio::test]
async fn project_alias_search_is_explicit_and_annotates_origin() {
    let _flag_guard = with_fallback_flag().await;
    let _reset = ProjectAliasResetGuard;
    set_cross_tenant_project_fallback(false);
    set_project_aliases(Vec::new());

    let store = make_store();
    for (tenant_id, text) in [
        ("default", "aliased memd history marker"),
        ("other", "otheronly memd history marker"),
    ] {
        handle_memory_add(
            &store,
            None,
            AddParams {
                tenant_id: tenant_id.to_string(),
                text: text.to_string(),
                chunk_type: "doc".to_string(),
                project_id: Some("memd".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let isolated = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "memd".to_string(),
            query: "aliased memd history".to_string(),
            project_id: Some("memd".to_string()),
            k: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let isolated_payload: SearchResult = parse_tool_payload(&isolated);
    assert!(isolated_payload
        .results
        .iter()
        .all(|result| result.tenant_id == "memd"));
    assert!(isolated_payload.scope_expansion.is_none());

    set_project_aliases(vec![ProjectAliasConfig {
        tenant_id: "memd".to_string(),
        project_id: "memd".to_string(),
        aliases: vec![ProjectAliasScopeConfig {
            tenant_id: "default".to_string(),
            project_id: None,
            reason: Some("fragmented_project_history".to_string()),
        }],
    }]);

    let aliased = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "memd".to_string(),
            query: "aliased memd history".to_string(),
            project_id: Some("memd".to_string()),
            k: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let aliased_payload: SearchResult = parse_tool_payload(&aliased);
    let alias_hit = aliased_payload
        .results
        .iter()
        .find(|result| result.tenant_id == "default")
        .expect("alias search should include the configured default tenant");
    let origin = alias_hit
        .origin
        .as_ref()
        .expect("alias hit should carry origin metadata");
    assert_eq!(origin.requested_tenant_id, "memd");
    assert_eq!(origin.origin_tenant_id, "default");
    assert_eq!(origin.origin_project_id.as_deref(), Some("memd"));
    assert_eq!(origin.alias_reason, "fragmented_project_history");
    assert_eq!(
        aliased_payload
            .scope_expansion
            .as_ref()
            .expect("alias search should report scope expansion")
            .aliases
            .len(),
        1
    );

    let unrelated = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "memd".to_string(),
            query: "otheronly".to_string(),
            project_id: Some("memd".to_string()),
            k: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let unrelated_payload: SearchResult = parse_tool_payload(&unrelated);
    assert!(
        unrelated_payload
            .results
            .iter()
            .all(|result| result.tenant_id != "other"),
        "explicit alias must not widen to every tenant with the same project_id"
    );
}

#[tokio::test]
async fn project_alias_search_can_target_different_project_id_in_same_tenant() {
    let _flag_guard = with_fallback_flag().await;
    let _reset = ProjectAliasResetGuard;
    set_cross_tenant_project_fallback(false);
    set_project_aliases(Vec::new());

    let store = make_store();
    handle_memory_add(
        &store,
        None,
        AddParams {
            tenant_id: "fschulz".to_string(),
            text: "Bester restore lesson: Tailscale gateway came back after route proxy readiness loop.".to_string(),
            chunk_type: "summary".to_string(),
            project_id: Some("bester-hosting".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let isolated = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "fschulz".to_string(),
            query: "Bester restore lesson".to_string(),
            project_id: Some("bester_hosting".to_string()),
            k: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let isolated_payload: SearchResult = parse_tool_payload(&isolated);
    assert!(
        isolated_payload.results.iter().all(|result| {
            result.project_id.as_deref() != Some("bester-hosting")
                && !result.text.contains("Bester restore lesson")
        }),
        "without an explicit alias, underscore scope must not silently merge with hyphen scope; got {}",
        serde_json::to_string_pretty(&isolated_payload).unwrap()
    );

    set_project_aliases(vec![ProjectAliasConfig {
        tenant_id: "fschulz".to_string(),
        project_id: "bester_hosting".to_string(),
        aliases: vec![ProjectAliasScopeConfig {
            tenant_id: "fschulz".to_string(),
            project_id: Some("bester-hosting".to_string()),
            reason: Some("project_id_separator_drift".to_string()),
        }],
    }]);

    let aliased = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "fschulz".to_string(),
            query: "Bester restore lesson".to_string(),
            project_id: Some("bester_hosting".to_string()),
            k: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let aliased_payload: SearchResult = parse_tool_payload(&aliased);
    let hit = aliased_payload
        .results
        .iter()
        .find(|result| {
            result.project_id.as_deref() == Some("bester-hosting")
                && result.text.contains("Tailscale gateway")
        })
        .unwrap_or_else(|| {
            panic!(
                "alias should retrieve the concrete hyphenated project memory; got {}",
                serde_json::to_string_pretty(&aliased_payload).unwrap()
            )
        });
    let origin = hit
        .origin
        .as_ref()
        .expect("same-tenant project rename alias should still carry origin metadata");
    assert_eq!(origin.requested_tenant_id, "fschulz");
    assert_eq!(origin.origin_tenant_id, "fschulz");
    assert_eq!(origin.origin_project_id.as_deref(), Some("bester-hosting"));
    assert_eq!(origin.alias_reason, "project_id_separator_drift");
    let expansion = aliased_payload
        .scope_expansion
        .as_ref()
        .expect("alias search should report scope expansion");
    assert_eq!(expansion.requested_project_id, "bester_hosting");
    assert_eq!(expansion.aliases.len(), 1);
}

#[tokio::test]
async fn artifact_search_compact_omits_full_payload_but_keeps_identifiers() {
    let store = make_store();

    let start: TaskArtifactResult = parse_tool_payload(
        &handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "test".to_string(),
                project_id: Some("compact_artifacts".to_string()),
                parent_task_id: None,
                agent_id: Some("planner-1".to_string()),
                session_id: None,
                goal: "Keep compact artifact search useful".to_string(),
                motivation: "Agents should fetch full artifacts only when needed".to_string(),
                hypothesis: "Compact hits keep stable identifiers".to_string(),
                scientific_question: "Can compact artifact search preserve trust metadata?"
                    .to_string(),
                dataset_refs: vec![],
                expected_outputs: vec!["compact artifact hit".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap(),
    );

    let result = handle_artifact_search(
        &store,
        TaskSearchParams {
            tenant_id: "test".to_string(),
            query: "compact artifact search".to_string(),
            k: 5,
            filters: Some(TaskSearchFiltersParams {
                project_id: Some("compact_artifacts".to_string()),
                ..Default::default()
            }),
            mode: None,
            compact: true,
            token_budget: Some(200),
            include_artifact: Some(false),
            include_matched_text: Some(false),
        },
    )
    .await
    .unwrap();

    let payload: ArtifactSearchResult = parse_tool_payload(&result);
    assert_eq!(payload.results.len(), 1);
    let hit = &payload.results[0];
    assert_eq!(hit.artifact_id, start.artifact_id);
    assert_eq!(hit.task_id, start.task_id);
    assert_eq!(hit.artifact_kind, "task_start");
    assert_eq!(hit.project_id.as_deref(), Some("compact_artifacts"));
    assert!(hit.artifact.is_none());
    assert!(hit.matched_text.is_none());
    assert_eq!(hit.trust_tier, TrustTier::CanonicalRecord);
    assert!(!hit.grounding_refs.is_empty());
    assert!(!hit.verification_hint.requires_verification);

    let budget = payload
        .budget_info
        .expect("compact artifact search should include budget info");
    assert_eq!(budget.requested_budget, Some(200));
    assert!(budget.omitted_fields.contains(&"artifact".to_string()));
    assert!(budget.omitted_fields.contains(&"matched_text".to_string()));
}

#[tokio::test]
async fn artifact_create_get_search_and_thread_flow() {
    let store = make_store();

    let start: TaskArtifactResult = parse_tool_payload(
        &handle_task_start(
            &store,
            None,
            TaskStartParams {
                tenant_id: "test".to_string(),
                project_id: Some("shared_proto".to_string()),
                parent_task_id: None,
                agent_id: Some("planner-1".to_string()),
                session_id: Some("session-a".to_string()),
                goal: "Coordinate a shared artifact thread".to_string(),
                motivation: "Multiple agents should reuse and critique the same record".to_string(),
                hypothesis: "Artifact-native collaboration reduces duplicated work".to_string(),
                scientific_question: "How should critique flow through the shared thread?"
                    .to_string(),
                dataset_refs: vec![TaskDatasetRefParams {
                    name: "repo_snapshot".to_string(),
                    version: Some("head".to_string()),
                    description: None,
                }],
                expected_outputs: vec!["thread seed".to_string()],
                entity_refs: vec![],
                provenance: None,
            },
        )
        .await
        .unwrap(),
    );

    let review: TaskArtifactResult = parse_tool_payload(
        &handle_artifact_create(
            &store,
            None,
            ArtifactCreateParams {
                tenant_id: "test".to_string(),
                artifact_kind: "review".to_string(),
                task_id: Some(start.task_id.clone()),
                project_id: Some("shared_proto".to_string()),
                parent_task_id: None,
                agent_id: Some("reviewer-1".to_string()),
                session_id: Some("session-b".to_string()),
                status: None,
                artifact_role: Some("critique".to_string()),
                challenge_id: Some("artifact_protocol".to_string()),
                thread_id: None,
                reply_to_artifact_id: Some(start.artifact_id.clone()),
                relation_kind: None,
                goal: None,
                motivation: None,
                hypothesis: None,
                scientific_question: None,
                method_summary: None,
                summary: Some(
                    "Need a clearer review and verification path for artifacts".to_string(),
                ),
                content: None,
                evidence_kind: None,
                supports_claim: None,
                blockers: vec![],
                what_worked: vec![],
                what_failed: vec!["Search still centers projection chunks".to_string()],
                validation: vec![],
                uncertainty: vec!["Exact artifact exchange semantics are still thin".to_string()],
                followups: vec!["Add artifact.search and thread inspection".to_string()],
                expected_outputs: vec![],
                related_artifact_ids: vec![],
                contributors: vec![TaskContributorParams {
                    contributor_id: "pi".to_string(),
                    display_name: Some("Principal Investigator".to_string()),
                    role: Some("human_scientist".to_string()),
                    contribution: Some("Requested critique of the seed artifact".to_string()),
                }],
                dataset_refs: vec![],
                entity_refs: vec![],
                tool_name: Some("artifact.create".to_string()),
                tool_version: None,
                command: None,
                parameters: None,
                inputs: vec![],
                outputs: vec![],
                metrics: None,
                why_chosen: None,
                confidence: Some(0.74),
                requested_action: Some("review".to_string()),
                verification_status: Some("pending".to_string()),
                compute_budget: None,
                cost_actual: None,
                data_access_level: Some("local_private".to_string()),
                policy_tags: vec!["prototype".to_string()],
                allowed_tools: vec!["task.search".to_string(), "artifact.search".to_string()],
                approval_state: Some("not_required".to_string()),
                provenance: None,
            },
        )
        .await
        .unwrap(),
    );

    let get_payload: ArtifactGetResult = parse_tool_payload(
        &handle_artifact_get(
            &store,
            ArtifactGetParams {
                tenant_id: "test".to_string(),
                artifact_id: review.artifact_id.clone(),
            },
        )
        .await
        .unwrap(),
    );
    let review_artifact = get_payload.artifact.expect("artifact should exist");
    assert_eq!(
        review_artifact.challenge_id.as_deref(),
        Some("artifact_protocol")
    );
    assert_eq!(
        review_artifact.reply_to_artifact_id.as_deref(),
        Some(start.artifact_id.as_str())
    );
    assert_eq!(review_artifact.requested_action.as_deref(), Some("review"));
    assert_eq!(
        review_artifact.verification_status.as_deref(),
        Some("pending")
    );
    assert_eq!(review_artifact.thread_key(), start.task_id.as_str());
    assert_eq!(review_artifact.contributors.len(), 1);

    let thread_payload: ArtifactThreadResult = parse_tool_payload(
        &handle_artifact_list_thread(
            &store,
            ArtifactListThreadParams {
                tenant_id: "test".to_string(),
                thread_id: None,
                artifact_id: Some(review.artifact_id.clone()),
            },
        )
        .await
        .unwrap(),
    );
    assert_eq!(thread_payload.thread_id, start.task_id);
    assert_eq!(thread_payload.artifacts.len(), 2);

    let search_payload: ArtifactSearchResult = parse_tool_payload(
        &handle_artifact_search(
            &store,
            TaskSearchParams {
                tenant_id: "test".to_string(),
                query: "clearer review path".to_string(),
                k: 5,
                filters: Some(TaskSearchFiltersParams {
                    task_id: None,
                    artifact_kind: Some("review".to_string()),
                    status: None,
                    challenge_id: Some("artifact_protocol".to_string()),
                    thread_id: None,
                    reply_to_artifact_id: Some(start.artifact_id.clone()),
                    artifact_role: Some("critique".to_string()),
                    dataset_name: None,
                    dataset_version: None,
                    entity_name: None,
                    entity_type: None,
                    tool_name: None,
                    project_id: Some("shared_proto".to_string()),
                    agent_id: None,
                    session_id: None,
                    requested_action: Some("review".to_string()),
                    verification_status: Some("pending".to_string()),
                    relation_kind: Some("reviews".to_string()),
                }),
                mode: None,
                compact: false,
                token_budget: None,
                include_artifact: None,
                include_matched_text: None,
            },
        )
        .await
        .unwrap(),
    );
    assert_eq!(search_payload.results.len(), 1);
    assert_eq!(
        search_payload.results[0]
            .artifact
            .as_ref()
            .expect("artifact should be included in full mode")
            .artifact_id,
        review.artifact_id
    );
    assert_eq!(
        search_payload.results[0].trust_tier,
        TrustTier::CanonicalRecord
    );
    assert!(!search_payload.results[0].grounding_refs.is_empty());
    assert!(
        !search_payload.results[0]
            .verification_hint
            .requires_verification
    );

    let task_search_payload: SearchResult = parse_tool_payload(
        &handle_task_search(
            &store,
            TaskSearchParams {
                tenant_id: "test".to_string(),
                query: "clearer review path".to_string(),
                k: 5,
                filters: Some(TaskSearchFiltersParams {
                    task_id: Some(thread_payload.thread_id),
                    artifact_kind: Some("review".to_string()),
                    status: None,
                    challenge_id: Some("artifact_protocol".to_string()),
                    thread_id: None,
                    reply_to_artifact_id: Some(start.artifact_id),
                    artifact_role: Some("critique".to_string()),
                    dataset_name: None,
                    dataset_version: None,
                    entity_name: None,
                    entity_type: None,
                    tool_name: None,
                    project_id: Some("shared_proto".to_string()),
                    agent_id: None,
                    session_id: None,
                    requested_action: Some("review".to_string()),
                    verification_status: Some("pending".to_string()),
                    relation_kind: Some("reviews".to_string()),
                }),
                mode: None,
                compact: false,
                token_budget: None,
                include_artifact: None,
                include_matched_text: None,
            },
        )
        .await
        .unwrap(),
    );
    assert!(!task_search_payload.results.is_empty());
    assert!(task_search_payload
        .results
        .iter()
        .all(|result| result.artifact.is_some()));
    assert_eq!(
        task_search_payload.results.iter().find_map(|result| {
            result
                .artifact
                .as_ref()
                .and_then(|artifact| artifact.artifact_role.as_deref())
        }),
        Some("critique")
    );
}

#[tokio::test]
async fn task_start_stores_canonical_artifact_and_projection_chunks() {
    let store = make_store();

    let result = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "test".to_string(),
            project_id: Some("proj_alpha".to_string()),
            parent_task_id: None,
            agent_id: Some("agent-1".to_string()),
            session_id: Some("session-7".to_string()),
            goal: "Quantify the stress-response regulon".to_string(),
            motivation: "The regulator mechanism is unresolved".to_string(),
            hypothesis: "Sigma factor S drives the induced genes".to_string(),
            scientific_question: "Which genes increase after the perturbation?".to_string(),
            dataset_refs: vec![TaskDatasetRefParams {
                name: "rna_seq".to_string(),
                version: Some("v1".to_string()),
                description: None,
            }],
            expected_outputs: vec!["differential expression table".to_string()],
            entity_refs: vec![TaskEntityRefParams {
                name: "RpoS".to_string(),
                entity_type: "protein".to_string(),
                role: Some("candidate regulator".to_string()),
            }],
            provenance: Some(TaskProvenanceParams {
                tool_name: Some("codex".to_string()),
                ..Default::default()
            }),
        },
    )
    .await
    .unwrap();

    let payload: TaskArtifactResult = parse_tool_payload(&result);
    assert!(!payload.artifact_id.is_empty());
    assert!(!payload.task_id.is_empty());
    assert!(!payload.projection_chunk_ids.is_empty());

    let tenant = TenantId::new("test").unwrap();
    let stored = store
        .get_task_artifact(&tenant, &payload.artifact_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.goal.as_deref(),
        Some("Quantify the stress-response regulon")
    );
    assert_eq!(stored.dataset_refs.len(), 1);

    let search = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "stress-response regulon".to_string(),
            project_id: Some("proj_alpha".to_string()),
            k: 10,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let search_payload: SearchResult = parse_tool_payload(&search);
    assert!(!search_payload.results.is_empty());
    assert!(search_payload.results.iter().any(|result| {
        result
            .tags
            .iter()
            .any(|tag| tag.starts_with("task:kind:task_start"))
    }));
    assert!(search_payload
        .results
        .iter()
        .any(|result| result.trust_tier == TrustTier::CanonicalRecord));
    assert!(search_payload
        .results
        .iter()
        .any(|result| !result.grounding_refs.is_empty()));
}

#[tokio::test]
async fn task_finish_stores_failed_and_validation_projections() {
    let store = make_store();

    let result = handle_task_finish(
        &store,
        None,
        TaskFinishParams {
            tenant_id: "test".to_string(),
            task_id: "task-123".to_string(),
            project_id: Some("proj_alpha".to_string()),
            agent_id: Some("agent-1".to_string()),
            session_id: Some("session-7".to_string()),
            status: Some("completed".to_string()),
            goal: Some("Quantify the stress-response regulon".to_string()),
            scientific_question: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            what_worked: vec!["Re-running with stricter QC stabilized the hit list".to_string()],
            what_failed: vec!["The first alignment preset over-trimmed reads".to_string()],
            validation: vec!["Independent replicate confirmed the top genes".to_string()],
            uncertainty: vec!["One replicate remains borderline".to_string()],
            followups: vec!["Collect an additional replicate".to_string()],
            confidence: Some(0.78),
            provenance: None,
        },
    )
    .await
    .unwrap();

    let payload: TaskArtifactResult = parse_tool_payload(&result);
    let tenant = TenantId::new("test").unwrap();
    let stored = store
        .get_task_artifact(&tenant, &payload.artifact_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.task_id, "task-123");
    assert_eq!(stored.confidence, Some(0.78));

    let search = handle_memory_search(
        &store,
        SearchParams {
            tenant_id: "test".to_string(),
            query: "over-trimmed reads".to_string(),
            project_id: Some("proj_alpha".to_string()),
            k: 10,
            filters: None,
            debug_tiers: None,
            mode: None,
            include_superseded: None,
            include_expired: None,
            include_history: None,
            oversample_factor: None,
            expand_event_siblings: false,
            compact: false,
            token_budget: None,
            include_text: None,
            include_artifact: None,
            suppress_usage_event: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let search_payload: SearchResult = parse_tool_payload(&search);
    assert!(search_payload.results.iter().any(|result| {
        result
            .tags
            .iter()
            .any(|tag| tag.starts_with("task:projection:failed"))
    }));
}

#[tokio::test]
async fn task_finish_rejects_out_of_range_confidence() {
    let store = make_store();

    let result = handle_task_finish(
        &store,
        None,
        TaskFinishParams {
            tenant_id: "test".to_string(),
            task_id: "task-123".to_string(),
            project_id: None,
            agent_id: None,
            session_id: None,
            status: None,
            goal: None,
            scientific_question: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            what_worked: vec![],
            what_failed: vec![],
            validation: vec![],
            uncertainty: vec![],
            followups: vec![],
            confidence: Some(1.1),
            provenance: None,
        },
    )
    .await;

    assert!(matches!(result, Err(McpError::InvalidParams(_))));
}

#[tokio::test]
async fn context_brief_project_generates_digest_artifact() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_a".to_string(),
            project_id: Some("proj_alpha".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "Ship the project brief".to_string(),
            motivation: "New agents need a concise resume surface".to_string(),
            hypothesis: "A persisted project brief will reduce context-search noise".to_string(),
            scientific_question: "Can a digest summarize current task state?".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["brief artifact".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    handle_task_finish(
        &store,
        None,
        TaskFinishParams {
            tenant_id: "tenant_a".to_string(),
            task_id: start_payload.task_id.clone(),
            project_id: Some("proj_alpha".to_string()),
            agent_id: None,
            session_id: None,
            status: None,
            goal: None,
            scientific_question: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            what_worked: vec!["Digest summarization reduced retrieval fan-out".to_string()],
            what_failed: vec!["Raw chunk search alone was noisy".to_string()],
            validation: vec!["Project brief response returned one active task".to_string()],
            uncertainty: vec![],
            followups: vec!["Bias memory.search toward project digests".to_string()],
            confidence: Some(0.9),
            provenance: None,
        },
    )
    .await
    .unwrap();

    let result = handle_context_brief_project(
        &store,
        ProjectBriefParams {
            tenant_id: "tenant_a".to_string(),
            project_id: "proj_alpha".to_string(),
            query: "".to_string(),
            k: 10,
            include_related_projects: true,
        },
    )
    .await
    .unwrap();

    let payload: ProjectBriefResult = parse_tool_payload(&result);
    assert_eq!(payload.artifact.artifact_kind, ArtifactKind::Digest);
    assert_eq!(
        payload.artifact.artifact_role.as_deref(),
        Some(DIGEST_ROLE_PROJECT_BRIEF)
    );
    assert_eq!(payload.brief.project_id, "proj_alpha");
    assert_eq!(payload.trust_tier, TrustTier::CompiledDigestHint);
    assert!(payload.verification_hint.requires_verification);
    assert!(!payload.grounding_refs.is_empty());
    assert!(
        !payload.brief.recent_completed_tasks.is_empty() || !payload.brief.active_tasks.is_empty()
    );
}

#[tokio::test]
async fn artifact_verify_reports_canonical_support_and_can_persist_record() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_verify".to_string(),
            project_id: Some("proj_verify".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "Verify grounding boundary".to_string(),
            motivation: "Need an explicit trust boundary".to_string(),
            hypothesis: "Canonical artifacts should ground the claim".to_string(),
            scientific_question: "Can artifact.verify recover direct canonical support?"
                .to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["verification result".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    handle_task_finish(
        &store,
        None,
        TaskFinishParams {
            tenant_id: "tenant_verify".to_string(),
            task_id: start_payload.task_id.clone(),
            project_id: Some("proj_verify".to_string()),
            agent_id: None,
            session_id: None,
            status: None,
            goal: None,
            scientific_question: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            what_worked: vec![
                "Canonical artifacts are the trust anchor for grounded claims".to_string(),
            ],
            what_failed: vec![],
            validation: vec![
                "Grounding should prefer canonical artifacts over digests".to_string()
            ],
            uncertainty: vec![],
            followups: vec![],
            confidence: Some(0.9),
            provenance: None,
        },
    )
    .await
    .unwrap();

    let result = handle_artifact_verify(
        &store,
        ArtifactVerifyParams {
            tenant_id: "tenant_verify".to_string(),
            claim: "canonical artifacts are the trust anchor".to_string(),
            project_id: Some("proj_verify".to_string()),
            task_id: Some(start_payload.task_id.clone()),
            thread_id: None,
            candidate_artifact_ids: vec![],
            k: 8,
            include_digests: false,
            create_artifact: true,
            record_task_id: Some(start_payload.task_id.clone()),
            agent_id: None,
        },
    )
    .await
    .unwrap();

    let payload: ArtifactVerifyResult = parse_tool_payload(&result);
    assert_eq!(
        payload.grounding_status,
        GroundingStatus::CanonicallyGrounded
    );
    assert!(!payload.supporting_artifacts.is_empty());
    assert!(payload.conflicting_artifacts.is_empty());
    let verification_artifact = payload
        .verification_artifact
        .expect("verification artifact should be persisted");
    assert_eq!(
        verification_artifact.artifact_kind,
        ArtifactKind::Verification
    );
    assert_eq!(
        verification_artifact.verification_status.as_deref(),
        Some("canonically_grounded")
    );
}

#[tokio::test]
async fn artifact_verify_returns_digest_only_when_only_unbacked_digest_matches() {
    let store = make_store();

    // `artifact.create` rejects `artifact_kind = digest` (digests are
    // server-generated via memory.compact to prevent ID-based overwrite
    // of canonical digests). Use the server-side `persist_digest_artifact`
    // path directly to set up the test fixture.
    let tenant = TenantId::new("tenant_digest").unwrap();
    let mut digest = TaskArtifact::new_digest(
        tenant.clone(),
        "digest_task_project_brief::proj_digest",
        "project_brief::proj_digest",
        "project_brief",
    );
    digest.project_id = ProjectId::from("proj_digest");
    digest.summary = Some("Digest-only hint about an isolated semantic summary".to_string());
    let digest = persist_digest_artifact(&store, digest)
        .await
        .expect("server-side digest persist must succeed");

    let result = handle_artifact_verify(
        &store,
        ArtifactVerifyParams {
            tenant_id: "tenant_digest".to_string(),
            claim: "isolated semantic summary".to_string(),
            project_id: Some("proj_digest".to_string()),
            task_id: None,
            thread_id: None,
            candidate_artifact_ids: vec![digest.artifact_id],
            k: 8,
            include_digests: false,
            create_artifact: false,
            record_task_id: None,
            agent_id: None,
        },
    )
    .await
    .unwrap();

    let payload: ArtifactVerifyResult = parse_tool_payload(&result);
    assert_eq!(payload.grounding_status, GroundingStatus::DigestOnly);
    assert!(payload.supporting_artifacts.is_empty());
    assert_eq!(payload.consulted_digests.len(), 1);
}

/// Regression test for the tenant-isolation default:
/// `scoped_tenants_for_project` must NOT widen across tenants when
/// `allow_cross_tenant_project_fallback` is false (the v0.3.1 default).
/// The legacy sweep leaked tenant B's project-scoped artifacts to any
/// caller in tenant A that guessed the same `project_id`.
#[tokio::test]
async fn scoped_tenants_respects_isolation_default() {
    let _flag_guard = with_fallback_flag().await;
    let store = make_store();

    // Seed two tenants with the same project_id.
    handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_a".to_string(),
            project_id: Some("shared".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "A's work".to_string(),
            motivation: "m".to_string(),
            hypothesis: "h".to_string(),
            scientific_question: "q".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["o".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_b".to_string(),
            project_id: Some("shared".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "B's work".to_string(),
            motivation: "m".to_string(),
            hypothesis: "h".to_string(),
            scientific_question: "q".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["o".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    // Default (flag off): tenant A should see ONLY its own tenant.
    set_cross_tenant_project_fallback(false);
    let scoped =
        scoped_tenants_for_project(&store, &TenantId::new("tenant_a").unwrap(), Some("shared"))
            .await
            .unwrap();
    assert_eq!(
        scoped,
        vec![TenantId::new("tenant_a").unwrap()],
        "default isolation must not widen across tenants"
    );

    // Opt-in (flag on): should widen to include tenant_b.
    set_cross_tenant_project_fallback(true);
    let scoped =
        scoped_tenants_for_project(&store, &TenantId::new("tenant_a").unwrap(), Some("shared"))
            .await
            .unwrap();
    assert!(
        scoped.contains(&TenantId::new("tenant_b").unwrap()),
        "flag-on must widen retrieval to other tenants sharing the project_id"
    );

    // Reset global state so sibling tests see the default.
    set_cross_tenant_project_fallback(false);
}

/// Phase 2.5: `task.progress` and `task.add_evidence` emit ONE
/// projection per call (the base summary) instead of the legacy
/// fanout of 2-3 kind-specific chunks. task.start / task.finish /
/// task.run_start / task.run_finish keep the full fanout because
/// their kind-specific projections carry tool/command text that
/// downstream filters rely on.
#[tokio::test]
async fn task_progress_emits_single_projection_chunk() {
    let store = make_store();

    // Seed a task so progress has something to reply to.
    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "amplification".to_string(),
            project_id: Some("proj".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "measure write amplification".to_string(),
            motivation: String::new(),
            hypothesis: String::new(),
            scientific_question: String::new(),
            dataset_refs: vec![],
            expected_outputs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    let progress = handle_task_progress(
        &store,
        None,
        TaskProgressParams {
            tenant_id: "amplification".to_string(),
            task_id: start_payload.task_id.clone(),
            project_id: Some("proj".to_string()),
            agent_id: None,
            session_id: None,
            summary: "investigated legacy fanout".to_string(),
            blockers: vec!["waiting on review".to_string()],
            failed_attempts: vec![],
            next_step: "cut projection count".to_string(),
            dataset_refs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let progress_payload: TaskArtifactResult = parse_tool_payload(&progress);

    // Before Phase 2.5 this value was 2 (TaskSummary base +
    // blocker/followup fanout). After the cut it must be exactly 1.
    assert_eq!(
        progress_payload.projection_chunk_ids.len(),
        1,
        "task.progress must emit exactly one projection chunk; \
         write amplification regression if this grows"
    );
}

/// Phase 2.1 (Codex coverage gap): the file-arm of
/// `resolve_tenant_id`. With `$MEMD_DEFAULT_TENANT` cleared and a
/// pinned `~/.memd/default_tenant` file, the file's contents must
/// win over the literal `"default"` fallback. Also verifies that
/// env still overrides the file when both are present.
#[tokio::test]
async fn resolve_tenant_id_reads_pinned_default_tenant_file() {
    let _flag_guard = with_fallback_flag().await;
    let previous_env = std::env::var("MEMD_DEFAULT_TENANT").ok();
    let previous_home = std::env::var("HOME").ok();

    let tmp = tempfile::tempdir().unwrap();
    let memd_dir = tmp.path().join(".memd");
    std::fs::create_dir_all(&memd_dir).unwrap();
    std::fs::write(memd_dir.join("default_tenant"), "  file_pinned_tenant\n").unwrap();

    // SAFETY: tests serialized via `with_fallback_flag()`.
    unsafe {
        std::env::remove_var("MEMD_DEFAULT_TENANT");
        std::env::set_var("HOME", tmp.path());
    }

    // Explicit empty → env empty → file wins.
    let resolved = resolve_tenant_id("").unwrap();
    assert_eq!(
        resolved.as_str(),
        "file_pinned_tenant",
        "file arm must take precedence over the literal `default` fallback"
    );

    // When both env and file are present, env must win.
    unsafe { std::env::set_var("MEMD_DEFAULT_TENANT", "env_wins") };
    let resolved_env = resolve_tenant_id("").unwrap();
    assert_eq!(resolved_env.as_str(), "env_wins");

    // Restore environment.
    unsafe {
        if let Some(prev) = previous_home {
            std::env::set_var("HOME", prev);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(prev) = previous_env {
            std::env::set_var("MEMD_DEFAULT_TENANT", prev);
        } else {
            std::env::remove_var("MEMD_DEFAULT_TENANT");
        }
    }
}

/// Phase 3.4 regression: a `task.add_evidence` write must mark
/// the evidence / highlight / project_brief digests dirty on the
/// writer side. The dirty tracker is a process-global singleton,
/// so this test holds the policy-flag mutex (which already
/// serializes other tests that manipulate globals) to get
/// exclusive access, then drains the tracker before and after.
#[tokio::test]
async fn task_add_evidence_marks_evidence_digests_dirty() {
    use crate::task_memory::digest_dirty::{global as dirty_tracker, DigestDirtyKey};
    use crate::task_memory::{
        DIGEST_ROLE_EVIDENCE_LIBRARY, DIGEST_ROLE_HIGHLIGHT_LIBRARY, DIGEST_ROLE_PROJECT_BRIEF,
    };

    // Serialize with sibling tests that manipulate other global
    // state (e.g., the cross-tenant fallback flag). This also
    // prevents concurrent writer paths from other tests from
    // polluting our dirty-tracker snapshot.
    let _flag_guard = with_fallback_flag().await;
    let _ = dirty_tracker().drain_dirty();

    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "dirty_ev".to_string(),
            project_id: Some("proj_dirty".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "phase 3.4 writer-dirty test".to_string(),
            motivation: String::new(),
            hypothesis: String::new(),
            scientific_question: String::new(),
            dataset_refs: vec![],
            expected_outputs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    // task.add_evidence should flag the evidence + highlight +
    // project_brief digests as dirty.
    handle_task_add_evidence(
        &store,
        None,
        TaskAddEvidenceParams {
            tenant_id: "dirty_ev".to_string(),
            task_id: start_payload.task_id,
            project_id: Some("proj_dirty".to_string()),
            agent_id: None,
            session_id: None,
            summary: "sentinel evidence".to_string(),
            evidence_kind: "unit_test".to_string(),
            supports_claim: Some(true),
            metric_name: None,
            metric_value: None,
            metrics: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    // Tracker is a process-global, so other concurrent tests may
    // also contribute entries. Check our specific (tenant,
    // project, role) triples are present rather than asserting
    // the total count.
    for role in [
        DIGEST_ROLE_EVIDENCE_LIBRARY,
        DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        DIGEST_ROLE_PROJECT_BRIEF,
    ] {
        let key = DigestDirtyKey {
            tenant_id: "dirty_ev".to_string(),
            project_id: Some("proj_dirty".to_string()),
            role: role.to_string(),
        };
        assert!(
            dirty_tracker().contains(&key),
            "{} digest must be marked dirty after task.add_evidence; \
             current dirty entries: {:?}",
            role,
            dirty_tracker().drain_dirty(),
        );
    }
}

/// Phase 2.2: `task.start` accepts only `{goal}` as the
/// hard-required surface — motivation, hypothesis, and the rest
/// default to empty. An agent that just wants to log "I started
/// working on X" should not be forced to invent fields.
#[tokio::test]
async fn task_start_accepts_minimal_goal_only_payload() {
    let _flag_guard = with_fallback_flag().await;
    // Point HOME at an empty temp dir so no pinned
    // `~/.memd/default_tenant` file from the developer machine
    // redirects the implicit default.
    let tmp = tempfile::tempdir().unwrap();
    let previous_home = std::env::var("HOME").ok();
    unsafe { std::env::set_var("HOME", tmp.path()) };
    let previous_env = std::env::var("MEMD_DEFAULT_TENANT").ok();
    unsafe { std::env::remove_var("MEMD_DEFAULT_TENANT") };

    let store = make_store();

    // Exactly the minimum: no tenant_id, no motivation, no
    // hypothesis, etc.
    let params: TaskStartParams = serde_json::from_value(json!({
        "goal": "Minimal start scenario"
    }))
    .expect("task.start must deserialize from just `{goal}`");

    let result = handle_task_start(&store, None, params).await.unwrap();
    let payload: TaskArtifactResult = parse_tool_payload(&result);

    // With env cleared and no pinned file, the resolver falls
    // back to the literal "default" tenant.
    let artifact = store
        .get_task_artifact(&TenantId::new("default").unwrap(), &payload.artifact_id)
        .await
        .unwrap()
        .expect("artifact must land in the `default` tenant");
    assert_eq!(artifact.goal.as_deref(), Some("Minimal start scenario"));

    // Restore env for sibling tests.
    if let Some(prev) = previous_home {
        unsafe { std::env::set_var("HOME", prev) };
    } else {
        unsafe { std::env::remove_var("HOME") };
    }
    if let Some(prev) = previous_env {
        unsafe { std::env::set_var("MEMD_DEFAULT_TENANT", prev) };
    }
}

/// Phase 2.1: `tenant_id` resolution falls through an ordered chain
/// of sources. Explicit value wins; otherwise `$MEMD_DEFAULT_TENANT`
/// is consulted; otherwise `~/.memd/default_tenant` (not covered
/// here to avoid touching `$HOME`); finally the literal `"default"`
/// is used.
///
/// Test is serialized via `with_fallback_flag()` because it
/// manipulates process env vars.
#[tokio::test]
async fn resolve_tenant_id_falls_back_through_env_and_literal_default() {
    let _flag_guard = with_fallback_flag().await;
    let previous = std::env::var("MEMD_DEFAULT_TENANT").ok();

    // Explicit non-empty wins even when env is set.
    // SAFETY: tests are serialized via the fallback-flag mutex.
    unsafe { std::env::set_var("MEMD_DEFAULT_TENANT", "env_default") };
    let explicit = resolve_tenant_id("explicit_tenant").unwrap();
    assert_eq!(explicit.as_str(), "explicit_tenant");

    // Empty explicit + env set → env wins.
    let env_resolved = resolve_tenant_id("").unwrap();
    assert_eq!(env_resolved.as_str(), "env_default");

    // Empty explicit + unset env (and presumably no pinned file in
    // the test environment) → literal "default".
    unsafe { std::env::remove_var("MEMD_DEFAULT_TENANT") };
    // Point HOME at an empty temp dir so the file-lookup arm cannot
    // find a pinned value left over from the developer machine.
    let tmp = tempfile::tempdir().unwrap();
    let previous_home = std::env::var("HOME").ok();
    unsafe { std::env::set_var("HOME", tmp.path()) };

    let fallback = resolve_tenant_id("   ").unwrap();
    assert_eq!(fallback.as_str(), "default");

    // Restore environment for sibling tests.
    if let Some(prev) = previous_home {
        unsafe { std::env::set_var("HOME", prev) };
    } else {
        unsafe { std::env::remove_var("HOME") };
    }
    if let Some(prev) = previous {
        unsafe { std::env::set_var("MEMD_DEFAULT_TENANT", prev) };
    }
}

/// Writer-identity resolution in v0.3.1 is explicit-only:
/// non-empty explicit → that value, else → anonymous (`None`). The
/// previous prototype maintained a process-global default derived
/// from `initialize.clientInfo` but that was unsound across shared
/// HTTP sessions (identity bleed + re-initialize forgery). See the
/// comment on `resolved_agent_id`.
#[test]
fn resolved_agent_id_uses_explicit_value_or_anonymous() {
    assert_eq!(
        resolved_agent_id(Some("codex@0.12")),
        Some("codex@0.12".to_string()),
        "non-empty explicit identifier is returned as-is"
    );
    assert!(
        resolved_agent_id(Some("   ")).is_none(),
        "whitespace-only explicit value must NOT masquerade as an identity"
    );
    assert!(
        resolved_agent_id(Some("")).is_none(),
        "empty string must be treated as anonymous"
    );
    assert!(
        resolved_agent_id(None).is_none(),
        "absent agent_id is anonymous; the countersignature path will refuse to promote"
    );
}

/// End-to-end: a `task.start` without an explicit `agent_id`
/// persists an anonymous artifact in v0.3.1. Identity auto-fill
/// from session state is deferred to Phase 2.
#[tokio::test]
async fn task_start_without_explicit_agent_id_stays_anonymous() {
    let store = make_store();

    let start_value = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "writer_anon".to_string(),
            project_id: Some("proj".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "test anonymous write".to_string(),
            motivation: "no identity supplied".to_string(),
            hypothesis: "anonymous writes stay anonymous".to_string(),
            scientific_question: "does it stay None?".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["ok".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let payload: TaskArtifactResult = parse_tool_payload(&start_value);

    let canonical = store
        .get_task_artifact(&TenantId::new("writer_anon").unwrap(), &payload.artifact_id)
        .await
        .unwrap()
        .expect("artifact must be persisted");
    assert!(
        canonical.agent_id.is_none(),
        "artifact must remain anonymous when no agent_id is supplied; \
         got {:?}",
        canonical.agent_id
    );

    // Explicit agent_id still persists as-is.
    let start_explicit = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "writer_anon".to_string(),
            project_id: Some("proj".to_string()),
            parent_task_id: None,
            agent_id: Some("planner-override".to_string()),
            session_id: None,
            goal: "explicit".to_string(),
            motivation: "m".to_string(),
            hypothesis: "h".to_string(),
            scientific_question: "q".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["ok".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let explicit_payload: TaskArtifactResult = parse_tool_payload(&start_explicit);
    let canonical_explicit = store
        .get_task_artifact(
            &TenantId::new("writer_anon").unwrap(),
            &explicit_payload.artifact_id,
        )
        .await
        .unwrap()
        .expect("artifact must be persisted");
    assert_eq!(
        canonical_explicit.agent_id.as_deref(),
        Some("planner-override")
    );
}

/// End-to-end trust-tier test: a single-agent `artifact.create` with
/// `artifact_kind = "verification"` and agent-labelled fields must
/// NOT produce a `VerifiedRecord`. Only a countersignature from a
/// distinct `agent_id` can promote trust.
#[tokio::test]
async fn single_writer_verification_is_not_verified_record() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "trust_solo".to_string(),
            project_id: Some("proj".to_string()),
            parent_task_id: None,
            agent_id: Some("solo".to_string()),
            session_id: None,
            goal: "test solo".to_string(),
            motivation: "m".to_string(),
            hypothesis: "h".to_string(),
            scientific_question: "q".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["ok".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    // Same writer "solo" tries to self-verify via artifact.create.
    let verify_value = handle_artifact_create(
        &store,
        None,
        artifact_params_minimal(
            "trust_solo",
            "verification",
            &start_payload.task_id,
            Some("solo"),
            Some(&start_payload.artifact_id),
            "looks good to me",
            Some(true),
            Some("verified"),
            Some("approved"),
        ),
    )
    .await
    .unwrap();
    let verify_payload: TaskArtifactResult = parse_tool_payload(&verify_value);

    let persisted = store
        .get_task_artifact(
            &TenantId::new("trust_solo").unwrap(),
            &verify_payload.artifact_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        derive_artifact_trust_tier(&persisted),
        TrustTier::CanonicalRecord,
        "single-writer verification cannot be VerifiedRecord"
    );
}

/// Positive test: a verification artifact written by a DIFFERENT
/// agent, replying to the original and explicitly supporting the
/// claim, is promoted to `VerifiedRecord`.
#[tokio::test]
async fn distinct_writer_countersignature_produces_verified_record() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "trust_pair".to_string(),
            project_id: Some("proj".to_string()),
            parent_task_id: None,
            agent_id: Some("author".to_string()),
            session_id: None,
            goal: "test pair".to_string(),
            motivation: "m".to_string(),
            hypothesis: "h".to_string(),
            scientific_question: "q".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["ok".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    // A DIFFERENT agent verifies.
    let verify_value = handle_artifact_create(
        &store,
        None,
        artifact_params_minimal(
            "trust_pair",
            "verification",
            &start_payload.task_id,
            Some("reviewer"),
            Some(&start_payload.artifact_id),
            "independently reproduced",
            Some(true),
            None,
            None,
        ),
    )
    .await
    .unwrap();
    let verify_payload: TaskArtifactResult = parse_tool_payload(&verify_value);

    let persisted = store
        .get_task_artifact(
            &TenantId::new("trust_pair").unwrap(),
            &verify_payload.artifact_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        derive_artifact_trust_tier(&persisted),
        TrustTier::VerifiedRecord,
        "countersignature from a distinct agent_id must promote trust"
    );
}

/// Codex-review regression (v0.3.1): the old process-global
/// `SESSION_DEFAULT_AGENT_ID` let a single client reinitialize as a
/// different persona and forge a countersignature by writing an
/// anonymous reply that the server backfilled with the new default.
/// The fix removes the default entirely — anonymous writes stay
/// anonymous, and the countersignature check refuses to promote.
#[tokio::test]
async fn anonymous_verification_never_promotes_to_verified() {
    let store = make_store();

    // Author writes with agent_id = "alice".
    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "trust_forge".to_string(),
            project_id: Some("proj".to_string()),
            parent_task_id: None,
            agent_id: Some("alice".to_string()),
            session_id: None,
            goal: "anti-forgery scenario".to_string(),
            motivation: "m".to_string(),
            hypothesis: "h".to_string(),
            scientific_question: "q".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["ok".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    // "Verification" submitted WITHOUT an explicit agent_id — this is
    // what the old default-backfill path would have silently
    // attributed to whichever client most recently called initialize.
    let verify = handle_artifact_create(
        &store,
        None,
        artifact_params_minimal(
            "trust_forge",
            "verification",
            &start_payload.task_id,
            None, // <-- anonymous
            Some(&start_payload.artifact_id),
            "I say it's fine",
            Some(true),
            None,
            None,
        ),
    )
    .await
    .unwrap();
    let verify_payload: TaskArtifactResult = parse_tool_payload(&verify);

    let persisted = store
        .get_task_artifact(
            &TenantId::new("trust_forge").unwrap(),
            &verify_payload.artifact_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        persisted.agent_id.is_none(),
        "anonymous write must stay anonymous; got {:?}",
        persisted.agent_id
    );
    assert_eq!(
        derive_artifact_trust_tier(&persisted),
        TrustTier::CanonicalRecord,
        "anonymous verification must never produce VerifiedRecord"
    );
}

/// Negative test: a reviewer who explicitly REJECTS the claim
/// (`supports_claim = false`) must NOT promote trust, even with a
/// distinct agent_id.
#[tokio::test]
async fn distinct_writer_explicit_rejection_does_not_promote() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "trust_reject".to_string(),
            project_id: Some("proj".to_string()),
            parent_task_id: None,
            agent_id: Some("author".to_string()),
            session_id: None,
            goal: "test reject".to_string(),
            motivation: "m".to_string(),
            hypothesis: "h".to_string(),
            scientific_question: "q".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["ok".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    let verify_value = handle_artifact_create(
        &store,
        None,
        artifact_params_minimal(
            "trust_reject",
            "review",
            &start_payload.task_id,
            Some("reviewer"),
            Some(&start_payload.artifact_id),
            "could not reproduce",
            Some(false),
            None,
            None,
        ),
    )
    .await
    .unwrap();
    let verify_payload: TaskArtifactResult = parse_tool_payload(&verify_value);

    let persisted = store
        .get_task_artifact(
            &TenantId::new("trust_reject").unwrap(),
            &verify_payload.artifact_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        derive_artifact_trust_tier(&persisted),
        TrustTier::CanonicalRecord,
        "explicit rejection must leave the reviewer's artifact at canonical"
    );
}

// Test builder mirrors the subset of protocol fields varied by trust tests.
#[allow(clippy::too_many_arguments)]
fn artifact_params_minimal(
    tenant_id: &str,
    artifact_kind: &str,
    task_id: &str,
    agent_id: Option<&str>,
    reply_to_artifact_id: Option<&str>,
    summary: &str,
    supports_claim: Option<bool>,
    verification_status: Option<&str>,
    approval_state: Option<&str>,
) -> ArtifactCreateParams {
    ArtifactCreateParams {
        tenant_id: tenant_id.to_string(),
        artifact_kind: artifact_kind.to_string(),
        task_id: Some(task_id.to_string()),
        project_id: None,
        parent_task_id: None,
        agent_id: agent_id.map(|s| s.to_string()),
        session_id: None,
        status: None,
        artifact_role: None,
        challenge_id: None,
        thread_id: None,
        reply_to_artifact_id: reply_to_artifact_id.map(|s| s.to_string()),
        relation_kind: None,
        goal: None,
        motivation: None,
        hypothesis: None,
        scientific_question: None,
        method_summary: None,
        summary: Some(summary.to_string()),
        content: None,
        evidence_kind: None,
        supports_claim,
        blockers: vec![],
        what_worked: vec![],
        what_failed: vec![],
        validation: vec![],
        uncertainty: vec![],
        followups: vec![],
        expected_outputs: vec![],
        related_artifact_ids: vec![],
        contributors: vec![],
        dataset_refs: vec![],
        entity_refs: vec![],
        tool_name: None,
        tool_version: None,
        command: None,
        parameters: None,
        inputs: vec![],
        outputs: vec![],
        metrics: None,
        why_chosen: None,
        confidence: None,
        requested_action: None,
        verification_status: verification_status.map(|s| s.to_string()),
        compute_budget: None,
        cost_actual: None,
        data_access_level: None,
        policy_tags: vec![],
        allowed_tools: vec![],
        approval_state: approval_state.map(|s| s.to_string()),
        provenance: None,
    }
}

/// Regression test for the digest-forgery mitigation: `artifact.create`
/// must reject any attempt to write `artifact_kind = "digest"`. Digests
/// are server-generated and have deterministic IDs; accepting
/// agent-authored digests lets any caller overwrite the canonical
/// `project_brief` / `failure_library` / etc. artifacts.
#[tokio::test]
async fn artifact_create_rejects_agent_authored_digest() {
    let store = make_store();

    let err = handle_artifact_create(
        &store,
        None,
        ArtifactCreateParams {
            tenant_id: "tenant_forge".to_string(),
            artifact_kind: "digest".to_string(),
            task_id: None,
            project_id: Some("proj_forge".to_string()),
            parent_task_id: None,
            agent_id: Some("attacker".to_string()),
            session_id: None,
            status: None,
            artifact_role: Some("project_brief".to_string()),
            challenge_id: None,
            thread_id: None,
            reply_to_artifact_id: None,
            relation_kind: None,
            goal: None,
            motivation: None,
            hypothesis: None,
            scientific_question: None,
            method_summary: None,
            summary: Some("forged brief that overwrites the real digest".to_string()),
            content: None,
            evidence_kind: None,
            supports_claim: None,
            blockers: vec![],
            what_worked: vec![],
            what_failed: vec![],
            validation: vec![],
            uncertainty: vec![],
            followups: vec![],
            expected_outputs: vec![],
            related_artifact_ids: vec![],
            contributors: vec![],
            dataset_refs: vec![],
            entity_refs: vec![],
            tool_name: None,
            tool_version: None,
            command: None,
            parameters: None,
            inputs: vec![],
            outputs: vec![],
            metrics: None,
            why_chosen: None,
            confidence: None,
            requested_action: None,
            verification_status: None,
            compute_budget: None,
            cost_actual: None,
            data_access_level: None,
            policy_tags: vec![],
            allowed_tools: vec![],
            approval_state: None,
            provenance: None,
        },
    )
    .await
    .expect_err("agent-authored digests must be rejected");

    match err {
        McpError::InvalidParams(msg) => {
            assert!(
                msg.contains("digests are server-generated"),
                "error message should explain digest policy, got: {}",
                msg
            );
        }
        other => panic!("expected InvalidParams, got: {:?}", other),
    }
}

/// Phase 0 of the memd-wiki v2 plan: the `content` field is
/// exclusively for `wiki_page` artifacts. A non-empty `content`
/// submitted with any other `artifact_kind` must be rejected at
/// the MCP boundary with a clear `InvalidParams` message — this
/// keeps the storage-row invariant "content is Some iff kind is
/// WikiPage" honest so downstream consumers (rendering, lint,
/// digest builders) can rely on it.
#[tokio::test]
async fn artifact_create_rejects_content_on_non_wiki_page_kind() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_wiki_content".to_string(),
            project_id: Some("memd".to_string()),
            parent_task_id: None,
            agent_id: Some("author-1".to_string()),
            session_id: None,
            goal: "Exercise wiki_page content invariant".to_string(),
            motivation: "Phase 0 trust boundary".to_string(),
            hypothesis: "Non-WikiPage kinds cannot carry content".to_string(),
            scientific_question: "Does the MCP validator reject misplaced content?".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["rejection".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    let mut params = artifact_params_minimal(
        "tenant_wiki_content",
        "task_progress",
        &start_payload.task_id,
        Some("author-1"),
        None,
        "progress update",
        None,
        None,
        None,
    );
    params.content = Some("# stray markdown body".to_string());

    let err = handle_artifact_create(&store, None, params)
        .await
        .expect_err("non-wiki_page kinds must not accept content");
    match err {
        McpError::InvalidParams(msg) => {
            assert!(
                msg.contains("wiki_page") && msg.contains("content"),
                "error should explain content is wiki_page-only; got: {msg}"
            );
            assert!(
                msg.contains("task_progress"),
                "error should name the rejected kind; got: {msg}"
            );
        }
        other => panic!("expected InvalidParams, got: {other:?}"),
    }
}

/// Phase 1 of memd-wiki v2: when `artifact_kind = wiki_page`, the
/// MCP validator enforces (a) non-empty `related_artifact_ids`,
/// (b) `summary` ≤ 500 bytes, (c) `artifact_role` ∈ {concept,
/// entity}, (d) `content` ≤ 256KB. Each rule gets one negative
/// case; the positive case is covered by
/// `wiki_page_verification_child_promotes_child_not_parent`.
#[tokio::test]
async fn artifact_create_validates_wiki_page_shape() {
    let store = make_store();

    fn wiki_params_with(
        related_artifact_ids: Vec<String>,
        summary: &str,
        artifact_role: Option<&str>,
        content: Option<String>,
    ) -> ArtifactCreateParams {
        let mut params = artifact_params_minimal(
            "tenant_wiki_validate",
            "wiki_page",
            "task-wiki-validate",
            Some("author-a"),
            None,
            summary,
            None,
            None,
            None,
        );
        params.artifact_role = artifact_role.map(|s| s.to_string());
        params.related_artifact_ids = related_artifact_ids;
        params.content = content;
        params
    }

    // (a) related_artifact_ids empty → reject.
    let err = handle_artifact_create(
        &store,
        None,
        wiki_params_with(vec![], "ok", Some("concept"), Some("body".to_string())),
    )
    .await
    .expect_err("wiki_page must require grounding");
    match err {
        McpError::InvalidParams(msg) => assert!(
            msg.contains("related_artifact_ids"),
            "error should name the failing field; got: {msg}"
        ),
        other => panic!("expected InvalidParams, got: {other:?}"),
    }

    // (a') related_artifact_ids contains an empty string → reject.
    let err = handle_artifact_create(
        &store,
        None,
        wiki_params_with(
            vec!["   ".to_string()],
            "ok",
            Some("concept"),
            Some("body".to_string()),
        ),
    )
    .await
    .expect_err("wiki_page grounding entries must not be blank");
    match err {
        McpError::InvalidParams(msg) => assert!(
            msg.contains("related_artifact_ids[0]"),
            "error should name the offending index; got: {msg}"
        ),
        other => panic!("expected InvalidParams, got: {other:?}"),
    }

    // (b) summary > 500 bytes → reject.
    let huge_summary = "s".repeat(super::WIKI_PAGE_MAX_SUMMARY_BYTES + 1);
    let err = handle_artifact_create(
        &store,
        None,
        wiki_params_with(
            vec!["019".to_string()],
            &huge_summary,
            Some("concept"),
            Some("body".to_string()),
        ),
    )
    .await
    .expect_err("wiki_page summary size must be capped");
    match err {
        McpError::InvalidParams(msg) => assert!(
            msg.contains("summary") && msg.contains("500"),
            "error should mention summary + cap; got: {msg}"
        ),
        other => panic!("expected InvalidParams, got: {other:?}"),
    }

    // (c) unknown artifact_role → reject.
    let err = handle_artifact_create(
        &store,
        None,
        wiki_params_with(
            vec!["019".to_string()],
            "ok",
            Some("not-a-role"),
            Some("body".to_string()),
        ),
    )
    .await
    .expect_err("wiki_page role allowlist");
    match err {
        McpError::InvalidParams(msg) => assert!(
            msg.contains("concept") && msg.contains("entity"),
            "error should list allowed roles; got: {msg}"
        ),
        other => panic!("expected InvalidParams, got: {other:?}"),
    }

    // (c') missing artifact_role → reject.
    let err = handle_artifact_create(
        &store,
        None,
        wiki_params_with(
            vec!["019".to_string()],
            "ok",
            None,
            Some("body".to_string()),
        ),
    )
    .await
    .expect_err("wiki_page role is required");
    match err {
        McpError::InvalidParams(msg) => assert!(
            msg.contains("artifact_role"),
            "error should name artifact_role; got: {msg}"
        ),
        other => panic!("expected InvalidParams, got: {other:?}"),
    }

    // (d) content > MAX_CONTENT_BYTES → reject.
    let huge_content = "x".repeat(super::WIKI_PAGE_MAX_CONTENT_BYTES + 1);
    let err = handle_artifact_create(
        &store,
        None,
        wiki_params_with(
            vec!["019".to_string()],
            "ok",
            Some("concept"),
            Some(huge_content),
        ),
    )
    .await
    .expect_err("wiki_page content size must be capped");
    match err {
        McpError::InvalidParams(msg) => assert!(
            msg.contains("content") && msg.contains("262144"),
            "error should mention content + 256KB cap (in bytes); got: {msg}"
        ),
        other => panic!("expected InvalidParams, got: {other:?}"),
    }

    // Positive case: a well-formed WikiPage is accepted.
    let good = handle_artifact_create(
        &store,
        None,
        wiki_params_with(
            vec!["01999999-0000-0000-0000-000000000000".to_string()],
            "OK summary",
            Some("entity"),
            Some("# body".to_string()),
        ),
    )
    .await
    .expect("well-formed wiki_page must be accepted");
    let payload: TaskArtifactResult = parse_tool_payload(&good);
    let stored = store
        .get_task_artifact(
            &TenantId::new("tenant_wiki_validate").unwrap(),
            &payload.artifact_id,
        )
        .await
        .unwrap()
        .expect("artifact persisted");
    assert_eq!(stored.artifact_kind, ArtifactKind::WikiPage);
    assert_eq!(stored.artifact_role.as_deref(), Some("entity"));
    assert_eq!(stored.content.as_deref(), Some("# body"));
}

/// Phase 0 (codex-folded §4.2 of the plan): a distinct-writer
/// `Verification` artifact that replies to a `WikiPage` is itself
/// promoted to `VerifiedRecord` via the existing countersignature
/// path, but the WikiPage's own `promotion_state` / trust tier
/// never change. This test nails down BOTH halves: the child
/// promotes, the parent stays at `CanonicalRecord`.
#[tokio::test]
async fn wiki_page_verification_child_promotes_child_not_parent() {
    let store = make_store();
    let tenant = TenantId::new("wiki_child_promote").unwrap();

    // Author a WikiPage.
    let mut page_params = artifact_params_minimal(
        tenant.as_str(),
        "wiki_page",
        "task-wiki-promote",
        Some("author-alpha"),
        None,
        "Verification boundary concept page.",
        None,
        None,
        None,
    );
    page_params.artifact_role = Some("concept".to_string());
    page_params.content =
        Some("# Verification boundary\n\nLLM-authored concept page body.".to_string());
    page_params.related_artifact_ids = vec!["0199".to_string()];

    let page_value = handle_artifact_create(&store, None, page_params)
        .await
        .unwrap();
    let page_payload: TaskArtifactResult = parse_tool_payload(&page_value);

    let page = store
        .get_task_artifact(&tenant, &page_payload.artifact_id)
        .await
        .unwrap()
        .expect("wiki_page was persisted");
    assert_eq!(page.artifact_kind, ArtifactKind::WikiPage);
    assert_eq!(
        derive_artifact_trust_tier(&page),
        TrustTier::CanonicalRecord,
        "fresh wiki_page must start at CanonicalRecord"
    );

    // A distinct writer files a Verification countersigning the page.
    let verify_value = handle_artifact_create(
        &store,
        None,
        artifact_params_minimal(
            tenant.as_str(),
            "verification",
            "task-wiki-promote",
            Some("reviewer-beta"),
            Some(&page_payload.artifact_id),
            "Independently confirmed the claim.",
            Some(true),
            Some("verified"),
            Some("approved"),
        ),
    )
    .await
    .unwrap();
    let verify_payload: TaskArtifactResult = parse_tool_payload(&verify_value);

    // The child verification is promoted to VerifiedRecord.
    let verify = store
        .get_task_artifact(&tenant, &verify_payload.artifact_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        derive_artifact_trust_tier(&verify),
        TrustTier::VerifiedRecord,
        "distinct-writer verification replying to wiki_page must promote to VerifiedRecord"
    );

    // The parent wiki_page stays at CanonicalRecord forever — the
    // promotion path targets the child, not the parent.
    let parent_after = store
        .get_task_artifact(&tenant, &page_payload.artifact_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        derive_artifact_trust_tier(&parent_after),
        TrustTier::CanonicalRecord,
        "wiki_page trust tier must remain CanonicalRecord after a verifying child"
    );
    assert_ne!(
        parent_after.promotion_state,
        crate::types::PromotionState::Verified,
        "wiki_page promotion_state must not upgrade via a child's countersignature"
    );
}

#[tokio::test]
async fn artifact_verify_marks_same_task_negative_marker_as_conflict() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_conflict".to_string(),
            project_id: Some("proj_conflict".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "Exercise conflict detection".to_string(),
            motivation: "Need narrow same-scope conflict checks".to_string(),
            hypothesis: "Explicit negative markers should create a conflict".to_string(),
            scientific_question: "Can artifact.verify detect obvious same-task disagreement?"
                .to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["conflict result".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    handle_artifact_create(
        &store,
        None,
        ArtifactCreateParams {
            tenant_id: "tenant_conflict".to_string(),
            artifact_kind: "evidence".to_string(),
            task_id: Some(start_payload.task_id.clone()),
            project_id: Some("proj_conflict".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            status: None,
            artifact_role: None,
            challenge_id: None,
            thread_id: Some(start_payload.task_id.clone()),
            reply_to_artifact_id: None,
            relation_kind: None,
            goal: None,
            motivation: None,
            hypothesis: None,
            scientific_question: None,
            method_summary: None,
            summary: Some("The digest planner is reliable for scoped retrieval".to_string()),
            content: None,
            evidence_kind: Some("integration_test".to_string()),
            supports_claim: Some(true),
            blockers: vec![],
            what_worked: vec![],
            what_failed: vec![],
            validation: vec!["Scoped retrieval stayed stable".to_string()],
            uncertainty: vec![],
            followups: vec![],
            expected_outputs: vec![],
            related_artifact_ids: vec![],
            contributors: vec![],
            dataset_refs: vec![],
            entity_refs: vec![],
            tool_name: None,
            tool_version: None,
            command: None,
            parameters: None,
            inputs: vec![],
            outputs: vec![],
            metrics: None,
            why_chosen: None,
            confidence: None,
            requested_action: None,
            verification_status: None,
            compute_budget: None,
            cost_actual: None,
            data_access_level: None,
            policy_tags: vec![],
            allowed_tools: vec![],
            approval_state: None,
            provenance: None,
        },
    )
    .await
    .unwrap();

    handle_artifact_create(
        &store,
        None,
        ArtifactCreateParams {
            tenant_id: "tenant_conflict".to_string(),
            artifact_kind: "verification".to_string(),
            task_id: Some(start_payload.task_id.clone()),
            project_id: Some("proj_conflict".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            status: None,
            artifact_role: Some("claim_grounding".to_string()),
            challenge_id: None,
            thread_id: Some(start_payload.task_id.clone()),
            reply_to_artifact_id: None,
            relation_kind: None,
            goal: None,
            motivation: None,
            hypothesis: None,
            scientific_question: None,
            method_summary: None,
            summary: Some(
                "The digest planner is not reliable when validation is absent".to_string(),
            ),
            content: None,
            evidence_kind: None,
            supports_claim: Some(false),
            blockers: vec![],
            what_worked: vec![],
            what_failed: vec!["Missing validation breaks reliability".to_string()],
            validation: vec![],
            uncertainty: vec![],
            followups: vec![],
            expected_outputs: vec![],
            related_artifact_ids: vec![],
            contributors: vec![],
            dataset_refs: vec![],
            entity_refs: vec![],
            tool_name: None,
            tool_version: None,
            command: None,
            parameters: None,
            inputs: vec![],
            outputs: vec![],
            metrics: None,
            why_chosen: None,
            confidence: None,
            requested_action: None,
            verification_status: Some("conflicted".to_string()),
            compute_budget: None,
            cost_actual: None,
            data_access_level: None,
            policy_tags: vec![],
            allowed_tools: vec![],
            approval_state: None,
            provenance: None,
        },
    )
    .await
    .unwrap();

    let result = handle_artifact_verify(
        &store,
        ArtifactVerifyParams {
            tenant_id: "tenant_conflict".to_string(),
            claim: "digest planner reliable".to_string(),
            project_id: Some("proj_conflict".to_string()),
            task_id: Some(start_payload.task_id),
            thread_id: None,
            candidate_artifact_ids: vec![],
            k: 8,
            include_digests: false,
            create_artifact: false,
            record_task_id: None,
            agent_id: None,
        },
    )
    .await
    .unwrap();

    let payload: ArtifactVerifyResult = parse_tool_payload(&result);
    assert_eq!(payload.grounding_status, GroundingStatus::Conflicted);
    assert!(!payload.supporting_artifacts.is_empty());
    assert!(!payload.conflicting_artifacts.is_empty());
}

#[tokio::test]
async fn context_brief_project_does_not_rewrite_unchanged_digest() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_a".to_string(),
            project_id: Some("proj_alpha".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "Ship the project brief".to_string(),
            motivation: "New agents need a concise resume surface".to_string(),
            hypothesis: "A persisted project brief will reduce context-search noise".to_string(),
            scientific_question: "Can a digest summarize current task state?".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["brief artifact".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    handle_task_finish(
        &store,
        None,
        TaskFinishParams {
            tenant_id: "tenant_a".to_string(),
            task_id: start_payload.task_id,
            project_id: Some("proj_alpha".to_string()),
            agent_id: None,
            session_id: None,
            status: None,
            goal: None,
            scientific_question: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            what_worked: vec!["Digest summarization reduced retrieval fan-out".to_string()],
            what_failed: vec!["Raw chunk search alone was noisy".to_string()],
            validation: vec!["Project brief response returned one active task".to_string()],
            uncertainty: vec![],
            followups: vec!["Bias memory.search toward project digests".to_string()],
            confidence: Some(0.9),
            provenance: None,
        },
    )
    .await
    .unwrap();

    let first = handle_context_brief_project(
        &store,
        ProjectBriefParams {
            tenant_id: "tenant_a".to_string(),
            project_id: "proj_alpha".to_string(),
            query: "".to_string(),
            k: 10,
            include_related_projects: true,
        },
    )
    .await
    .unwrap();
    let first_payload: ProjectBriefResult = parse_tool_payload(&first);
    let chunks_after_first = store
        .stats(&TenantId::new("tenant_a").unwrap())
        .await
        .unwrap()
        .total_chunks;

    let second = handle_context_brief_project(
        &store,
        ProjectBriefParams {
            tenant_id: "tenant_a".to_string(),
            project_id: "proj_alpha".to_string(),
            query: "".to_string(),
            k: 10,
            include_related_projects: true,
        },
    )
    .await
    .unwrap();
    let second_payload: ProjectBriefResult = parse_tool_payload(&second);
    let chunks_after_second = store
        .stats(&TenantId::new("tenant_a").unwrap())
        .await
        .unwrap()
        .total_chunks;

    assert_eq!(
        first_payload.artifact.artifact_id,
        second_payload.artifact.artifact_id
    );
    assert_eq!(
        first_payload.artifact.timestamp_created,
        second_payload.artifact.timestamp_created
    );
    assert_eq!(chunks_after_first, chunks_after_second);
}

#[tokio::test]
async fn digest_persistence_replaces_changed_projections_and_retires_orphans() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("tenant_digest_replace").unwrap();
    let first_digest =
        project_brief_digest_fixture(&tenant, "proj_digest_replace", "first digest summary", 1);
    let first_projection_text = build_task_projections(&first_digest)[0].chunk.text.clone();

    let orphan = MemoryChunk::new(tenant.clone(), &first_projection_text, ChunkType::Summary)
        .with_project(ProjectId::new(Some("proj_digest_replace".to_string())));
    let orphan_id = store.add(orphan).await.unwrap();

    let persisted_first = persist_digest_artifact(&store, first_digest)
        .await
        .expect("initial digest persist should succeed");
    let filters = TaskSearchFilters {
        artifact_kind: Some(ArtifactKind::Digest),
        artifact_role: Some(DIGEST_ROLE_PROJECT_BRIEF.to_string()),
        project_id: Some("proj_digest_replace".to_string()),
        ..Default::default()
    };
    let first_links = store
        .search_task_projection_chunk_ids(&tenant, &filters, 20)
        .await
        .unwrap();
    assert!(!first_links.is_empty());

    let orphan_meta = store
        .metadata()
        .get(&tenant, &orphan_id)
        .unwrap()
        .expect("orphan digest projection should still have metadata");
    assert_eq!(orphan_meta.status, ChunkStatus::Superseded);
    assert_eq!(orphan_meta.lifecycle.tier, MemoryTier::History);

    let second_digest = project_brief_digest_fixture(
        &tenant,
        "proj_digest_replace",
        "second digest summary with changed content",
        2,
    );
    let persisted_second = persist_digest_artifact(&store, second_digest)
        .await
        .expect("changed digest persist should succeed");
    assert_eq!(persisted_first.artifact_id, persisted_second.artifact_id);

    let second_links = store
        .search_task_projection_chunk_ids(&tenant, &filters, 20)
        .await
        .unwrap();
    assert!(!second_links.is_empty());
    assert_ne!(first_links, second_links);

    for old_id in first_links {
        let meta = store
            .metadata()
            .get(&tenant, &old_id)
            .unwrap()
            .expect("old digest projection metadata should remain");
        assert_eq!(meta.status, ChunkStatus::Superseded);
        assert_eq!(meta.lifecycle.tier, MemoryTier::History);
    }
    for new_id in second_links {
        let meta = store
            .metadata()
            .get(&tenant, &new_id)
            .unwrap()
            .expect("current digest projection metadata should remain");
        assert_eq!(meta.status, ChunkStatus::Final);
        assert_eq!(meta.lifecycle.tier, MemoryTier::LongTerm);
    }
}

#[tokio::test]
async fn empty_project_brief_digest_is_not_persisted() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("tenant_empty_project_brief").unwrap();

    let (artifact, brief) = ensure_project_brief_digest(&store, &tenant, "proj_empty_brief", true)
        .await
        .unwrap();

    assert!(brief.active_tasks.is_empty());
    assert!(brief.recent_completed_tasks.is_empty());
    assert!(brief.recent_failures.is_empty());
    assert!(brief.recent_decisions.is_empty());
    assert!(brief.evidence_highlights.is_empty());
    assert!(is_empty_generated_digest_artifact(&artifact));
    assert!(store
        .get_task_artifact(&tenant, &artifact.artifact_id)
        .await
        .unwrap()
        .is_none());

    let links = store
        .search_task_projection_chunk_ids(
            &tenant,
            &TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_PROJECT_BRIEF.to_string()),
                project_id: Some("proj_empty_brief".to_string()),
                ..Default::default()
            },
            20,
        )
        .await
        .unwrap();
    assert!(links.is_empty());
}

#[tokio::test]
async fn empty_library_digest_is_not_persisted() {
    let (store, _dir) = make_persistent_store();
    let tenant = TenantId::new("tenant_empty_library").unwrap();

    let (artifact, failures) =
        ensure_failure_library_digest(&store, &tenant, Some("proj_empty_library"))
            .await
            .unwrap();

    assert!(failures.is_empty());
    assert!(is_empty_generated_digest_artifact(&artifact));
    assert!(store
        .get_task_artifact(&tenant, &artifact.artifact_id)
        .await
        .unwrap()
        .is_none());

    let links = store
        .search_task_projection_chunk_ids(
            &tenant,
            &TaskSearchFilters {
                artifact_kind: Some(ArtifactKind::Digest),
                artifact_role: Some(DIGEST_ROLE_FAILURE_LIBRARY.to_string()),
                project_id: Some("proj_empty_library".to_string()),
                ..Default::default()
            },
            20,
        )
        .await
        .unwrap();
    assert!(links.is_empty());
}

#[tokio::test]
async fn artifact_find_failures_returns_library_and_failure_hits() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_b".to_string(),
            project_id: Some("proj_beta".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "Exercise failure library".to_string(),
            motivation: "Need failure-first recall".to_string(),
            hypothesis: "what_failed fields should be surfaced as reusable failures".to_string(),
            scientific_question: "Can failure digests summarize recent problems?".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["failure library".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    handle_task_progress(
        &store,
        None,
        TaskProgressParams {
            tenant_id: "tenant_b".to_string(),
            task_id: start_payload.task_id.clone(),
            project_id: Some("proj_beta".to_string()),
            agent_id: None,
            session_id: None,
            summary: "Compilation failed in the digest path".to_string(),
            blockers: vec!["Digest query planner missing project brief candidates".to_string()],
            failed_attempts: vec!["Raw search mode returned only generic chunks".to_string()],
            next_step: "Add digest-aware candidate collection".to_string(),
            dataset_refs: vec![],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();

    let result = handle_artifact_find_failures(
        &store,
        ArtifactLibraryParams {
            tenant_id: "tenant_b".to_string(),
            project_id: Some("proj_beta".to_string()),
            query: "digest planner".to_string(),
            k: 10,
        },
    )
    .await
    .unwrap();

    let payload: FailureSearchResult = parse_tool_payload(&result);
    assert_eq!(payload.artifact.artifact_kind, ArtifactKind::Digest);
    assert_eq!(
        payload.artifact.artifact_role.as_deref(),
        Some(DIGEST_ROLE_FAILURE_LIBRARY)
    );
    assert!(!payload.results.is_empty());
    assert!(payload.results[0].summary.contains("Digest"));
}

#[tokio::test]
async fn artifact_find_highlights_returns_ranked_lessons_without_rewriting_unchanged_digest() {
    let _flag_guard = with_fallback_flag().await;
    let store = make_store();

    let first = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_h".to_string(),
            project_id: Some("proj_highlight".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "Capture reusable agent lessons".to_string(),
            motivation: "Need a high-signal highlight library".to_string(),
            hypothesis: "Validated repeated tactics should surface as highlights".to_string(),
            scientific_question: "Can highlight digests rank future-agent lessons?".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["highlight library".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let first_payload: TaskArtifactResult = parse_tool_payload(&first);
    let first_task_id = first_payload.task_id.clone();

    handle_task_finish(
        &store,
        None,
        TaskFinishParams {
            tenant_id: "tenant_h".to_string(),
            task_id: first_payload.task_id,
            project_id: Some("proj_highlight".to_string()),
            agent_id: None,
            session_id: None,
            status: None,
            goal: None,
            scientific_question: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            what_worked: vec!["Use digest persistence idempotence".to_string()],
            what_failed: vec!["Rewriting unchanged digests creates retrieval noise".to_string()],
            validation: vec!["Repeated refreshes do not add chunks".to_string()],
            uncertainty: vec![],
            followups: vec![],
            confidence: Some(0.85),
            provenance: None,
        },
    )
    .await
    .unwrap();

    let second = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_h".to_string(),
            project_id: Some("proj_highlight".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "Reconfirm reusable agent lessons".to_string(),
            motivation: "Need repetition for stronger promotion".to_string(),
            hypothesis: "Repeated tactics should rank above one-off notes".to_string(),
            scientific_question: "Do repeated successful lessons outrank one-offs?".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["highlight library".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let second_payload: TaskArtifactResult = parse_tool_payload(&second);
    let second_task_id = second_payload.task_id.clone();

    handle_task_finish(
        &store,
        None,
        TaskFinishParams {
            tenant_id: "tenant_h".to_string(),
            task_id: second_payload.task_id,
            project_id: Some("proj_highlight".to_string()),
            agent_id: None,
            session_id: None,
            status: None,
            goal: None,
            scientific_question: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            what_worked: vec!["Use digest persistence idempotence".to_string()],
            what_failed: vec!["Rewriting unchanged digests creates retrieval noise".to_string()],
            validation: vec!["Repeated refreshes do not add chunks".to_string()],
            uncertainty: vec![],
            followups: vec![],
            confidence: Some(0.9),
            provenance: None,
        },
    )
    .await
    .unwrap();

    let first = handle_artifact_find_highlights(
        &store,
        ArtifactLibraryParams {
            tenant_id: "tenant_h".to_string(),
            project_id: Some("proj_highlight".to_string()),
            query: "".to_string(),
            k: 10,
        },
    )
    .await
    .unwrap();
    let first_payload: HighlightSearchViewResult = parse_tool_payload(&first);
    let chunks_after_first = store
        .stats(&TenantId::new("tenant_h").unwrap())
        .await
        .unwrap()
        .total_chunks;

    let second = handle_artifact_find_highlights(
        &store,
        ArtifactLibraryParams {
            tenant_id: "tenant_h".to_string(),
            project_id: Some("proj_highlight".to_string()),
            query: "".to_string(),
            k: 10,
        },
    )
    .await
    .unwrap();
    let second_payload: HighlightSearchViewResult = parse_tool_payload(&second);
    let chunks_after_second = store
        .stats(&TenantId::new("tenant_h").unwrap())
        .await
        .unwrap()
        .total_chunks;

    assert_eq!(first_payload.artifact.artifact_kind, ArtifactKind::Digest);
    assert_eq!(
        first_payload.artifact.artifact_role.as_deref(),
        Some(DIGEST_ROLE_HIGHLIGHT_LIBRARY)
    );
    assert!(!first_payload.results.is_empty());
    assert_eq!(first_payload.results[0].category, "tactic");
    assert!(first_payload.results[0]
        .summary
        .contains("digest persistence idempotence"));
    assert_eq!(first_payload.results[0].support_count, 2);
    let summary = first_payload
        .artifact
        .summary
        .as_deref()
        .unwrap_or_default();
    assert!(summary.contains(&format!("task:id:{}", first_task_id)));
    assert!(summary.contains(&format!("task:id:{}", second_task_id)));
    assert_eq!(
        first_payload.artifact.timestamp_created,
        second_payload.artifact.timestamp_created
    );
    assert_eq!(chunks_after_first, chunks_after_second);
}

#[tokio::test]
async fn memory_compact_can_refresh_digests_without_storage_compaction() {
    let store = make_store();

    let start = handle_task_start(
        &store,
        None,
        TaskStartParams {
            tenant_id: "tenant_c".to_string(),
            project_id: Some("proj_gamma".to_string()),
            parent_task_id: None,
            agent_id: None,
            session_id: None,
            goal: "Prepare digest-only compaction".to_string(),
            motivation: "Need on-demand digest refreshes".to_string(),
            hypothesis:
                "memory.compact should rebuild digests even when storage compaction is skipped"
                    .to_string(),
            scientific_question: "Can digest rebuild run without tombstone thresholds?".to_string(),
            dataset_refs: vec![],
            expected_outputs: vec!["digest rebuild".to_string()],
            entity_refs: vec![],
            provenance: None,
        },
    )
    .await
    .unwrap();
    let start_payload: TaskArtifactResult = parse_tool_payload(&start);

    handle_task_finish(
        &store,
        None,
        TaskFinishParams {
            tenant_id: "tenant_c".to_string(),
            task_id: start_payload.task_id,
            project_id: Some("proj_gamma".to_string()),
            agent_id: None,
            session_id: None,
            status: None,
            goal: None,
            scientific_question: None,
            dataset_refs: vec![],
            entity_refs: vec![],
            what_worked: vec!["Digest rebuild can be triggered explicitly".to_string()],
            what_failed: vec!["No storage compaction threshold was exceeded".to_string()],
            validation: vec!["Compaction response returned digest artifact ids".to_string()],
            uncertainty: vec![],
            followups: vec![],
            confidence: Some(0.8),
            provenance: None,
        },
    )
    .await
    .unwrap();

    let result = handle_memory_compact(
        &store,
        CompactParams {
            tenant_id: "tenant_c".to_string(),
            force: false,
            project_id: Some("proj_gamma".to_string()),
            digest_modes: Some(vec![QueryMode::BriefProject, QueryMode::FindFailures]),
            force_digest_rebuild: true,
        },
    )
    .await
    .unwrap();

    let payload: Value = parse_tool_payload(&result);
    assert_eq!(payload["status"].as_str(), Some("completed"));
    assert!(payload["digest_artifacts"]
        .as_array()
        .map(|items| !items.is_empty())
        .unwrap_or(false));
}
