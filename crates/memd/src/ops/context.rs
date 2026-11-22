use super::*;

/// Handle context.list_subsystems tool call.
pub async fn handle_context_list_subsystems<S: Store>(
    store: &S,
    params: ContextListSubsystemsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(500);
    let prefix = params
        .prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    info!(tenant_id = %tenant_id, prefix = ?prefix, limit = limit, "context.list_subsystems");

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut summaries: HashMap<String, (usize, HashSet<String>)> = HashMap::new();

    for chunk in chunks {
        let subsystems = tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX);
        for subsystem in subsystems {
            if let Some(prefix) = prefix {
                if !subsystem.starts_with(prefix) {
                    continue;
                }
            }

            let entry = summaries.entry(subsystem).or_insert((0, HashSet::new()));
            entry.0 += 1;

            if let Some(path) = chunk.source.path.as_deref() {
                entry.1.insert(path.to_string());
            }
            for file_tag in tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX) {
                entry.1.insert(file_tag);
            }
        }
    }

    let mut subsystem_summaries: Vec<SubsystemSummary> = summaries
        .into_iter()
        .map(|(key, (chunk_count, files))| SubsystemSummary {
            key,
            chunk_count,
            file_count: files.len(),
        })
        .collect();
    subsystem_summaries.sort_by(|a, b| a.key.cmp(&b.key));
    subsystem_summaries.truncate(limit);

    format_mcp_response(&ContextListSubsystemsResult {
        subsystems: subsystem_summaries,
    })
}

/// Handle context.get_files_for_subsystem tool call
pub async fn handle_context_get_files_for_subsystem<S: Store>(
    store: &S,
    params: ContextGetFilesForSubsystemParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let subsystem_key = params.subsystem_key.trim();
    if subsystem_key.is_empty() {
        return Err(McpError::InvalidParams(
            "subsystem_key must not be empty".to_string(),
        ));
    }
    let limit = params.limit.min(2_000);

    info!(tenant_id = %tenant_id, subsystem_key = subsystem_key, limit = limit, "context.get_files_for_subsystem");

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut files = HashSet::new();

    for chunk in chunks {
        if !chunk_matches_subsystem(&chunk, subsystem_key) {
            continue;
        }

        if let Some(path) = chunk.source.path.as_deref() {
            files.insert(path.to_string());
        }
        for file_tag in tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX) {
            files.insert(file_tag);
        }
    }

    let mut files: Vec<String> = files.into_iter().collect();
    files.sort();
    files.truncate(limit);

    format_mcp_response(&ContextGetFilesForSubsystemResult {
        subsystem_key: subsystem_key.to_string(),
        files,
    })
}

/// Handle context.find_relevant_context tool call
pub async fn handle_context_find_relevant_context<S: Store>(
    store: &S,
    params: ContextFindRelevantContextParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    let subsystem_keys: Vec<String> = params
        .subsystem_keys
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let has_filters = !subsystem_keys.is_empty();
    let fetch_k = adaptive_fetch_k(params.k, &params.task, has_filters);

    info!(
        tenant_id = %tenant_id,
        task = %params.task,
        k = params.k,
        include_hot = params.include_hot,
        subsystem_keys = subsystem_keys.len(),
        fetch_k = fetch_k,
        "context.find_relevant_context"
    );

    let mut dedupe = HashSet::new();
    let mut results = Vec::new();
    let mut hot_included = false;

    if params.include_hot {
        let (mut hot_chunks, timed_out) = collect_all_chunks_until_deadline(
            store,
            &tenant_id,
            20_000,
            Duration::from_millis(HOT_CONTEXT_SCAN_TIMEOUT_MS),
        )
        .await?;
        if timed_out {
            warn!(
                tenant_id = %tenant_id,
                scanned_chunks = hot_chunks.len(),
                timeout_ms = HOT_CONTEXT_SCAN_TIMEOUT_MS,
                "context.find_relevant_context hot scan timed out; continuing with retrieval results"
            );
        }
        hot_chunks.retain(|chunk| {
            has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT)
                && chunk_matches_any_subsystem(chunk, &subsystem_keys)
        });
        let hot_cap = hot_chunks.len();
        hot_chunks = apply_visibility_filter(
            store,
            hot_chunks.into_iter().map(|chunk| (chunk, 1.0)).collect(),
            &VisibilityPolicy::default(),
            hot_cap,
        )
        .await
        .into_iter()
        .map(|(chunk, _)| chunk)
        .collect();
        hot_chunks.sort_by_key(|chunk| std::cmp::Reverse(chunk.timestamp_created));

        for chunk in hot_chunks.into_iter().take(params.k.min(5)) {
            let id = chunk.chunk_id.to_string();
            if dedupe.insert(id) {
                hot_included = true;
                results.push(chunk_to_result(&chunk, 1.0, Some("hot".to_string()), None));
            }
        }
    }

    let scored_chunks = store
        .search_with_scores(&tenant_id, &params.task, fetch_k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    // Hide superseded/expired/history chunks, matching memory.search.
    let visible_cap = scored_chunks.len();
    let scored_chunks = apply_visibility_filter(
        store,
        scored_chunks,
        &VisibilityPolicy::default(),
        visible_cap,
    )
    .await;

    for (chunk, score) in scored_chunks {
        if !is_context_chunk(&chunk) {
            continue;
        }
        if !params.include_hot && has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            continue;
        }
        if !chunk_matches_any_subsystem(&chunk, &subsystem_keys) {
            continue;
        }

        let id = chunk.chunk_id.to_string();
        if !dedupe.insert(id) {
            continue;
        }

        let source_tier = if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            Some("hot".to_string())
        } else if has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD) {
            Some("cold".to_string())
        } else {
            None
        };
        results.push(chunk_to_result(&chunk, score, source_tier, None));
        if results.len() >= params.k {
            break;
        }
    }

    format_mcp_response(&ContextFindRelevantContextResult {
        results,
        hot_included,
    })
}

/// Handle context.suggest_agent tool call
pub async fn handle_context_suggest_agent<S: Store>(
    store: &S,
    params: ContextSuggestAgentParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    let changed_files: Vec<String> = params
        .changed_files
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();

    info!(
        tenant_id = %tenant_id,
        task = %params.task,
        changed_files = changed_files.len(),
        k = params.k,
        "context.suggest_agent"
    );

    #[derive(Default)]
    struct AgentScore {
        score: f32,
        reasons: HashSet<String>,
        matched_triggers: HashSet<String>,
    }

    let task_lower = params.task.to_ascii_lowercase();
    let task_tokens: Vec<String> = params
        .task
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() >= 3)
        .map(|token| token.to_ascii_lowercase())
        .collect();

    let chunks = collect_all_chunks(store, &tenant_id, 50_000).await?;
    let mut scores: HashMap<String, AgentScore> = HashMap::new();

    for chunk in chunks {
        let agent_names = tag_values(&chunk.tags, TAG_CTX_AGENT_PREFIX);
        if agent_names.is_empty() {
            continue;
        }

        let chunk_text = chunk.text.to_ascii_lowercase();
        let triggers = tag_values(&chunk.tags, TAG_CTX_TRIGGER_PREFIX);
        let subsystem_tags = tag_values(&chunk.tags, TAG_CTX_SUBSYSTEM_PREFIX);
        let file_tags = tag_values(&chunk.tags, TAG_CTX_FILE_PREFIX);

        for agent_name in agent_names {
            let mut score = 0.1f32;
            let mut reasons = HashSet::new();
            let mut matched_triggers = HashSet::new();

            let lexical_hits = task_tokens
                .iter()
                .filter(|token| chunk_text.contains(token.as_str()))
                .count();
            if lexical_hits > 0 {
                score += lexical_hits as f32 * 0.03;
                reasons.insert(format!("keyword_overlap:{}", lexical_hits));
            }

            if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
                score += 0.05;
                reasons.insert("hot_tier_profile".to_string());
            }

            for subsystem in &subsystem_tags {
                if task_lower.contains(&subsystem.to_ascii_lowercase()) {
                    score += 0.15;
                    reasons.insert(format!("subsystem_match:{}", subsystem));
                }
            }

            for trigger in &triggers {
                for changed_file in &changed_files {
                    if wildcard_match(trigger, changed_file) {
                        score += 0.6;
                        matched_triggers.insert(format!("{} -> {}", trigger, changed_file));
                    }
                }
            }

            for file_tag in &file_tags {
                for changed_file in &changed_files {
                    if wildcard_match(file_tag, changed_file)
                        || changed_file.contains(file_tag)
                        || file_tag.contains(changed_file)
                    {
                        score += 0.2;
                        reasons.insert(format!("file_match:{}", file_tag));
                    }
                }
            }

            let entry = scores.entry(agent_name).or_default();
            if score > entry.score {
                entry.score = score;
            }
            entry.reasons.extend(reasons);
            entry.matched_triggers.extend(matched_triggers);
        }
    }

    let considered_agents = scores.len();
    let mut recommendations: Vec<AgentSuggestion> = scores
        .into_iter()
        .map(|(agent_name, score)| {
            let mut reasons: Vec<String> = score.reasons.into_iter().collect();
            reasons.sort();
            let mut matched_triggers: Vec<String> = score.matched_triggers.into_iter().collect();
            matched_triggers.sort();

            AgentSuggestion {
                agent_name,
                score: score.score,
                reasons,
                matched_triggers,
            }
        })
        .collect();

    recommendations.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.agent_name.cmp(&b.agent_name))
    });
    recommendations.truncate(params.k);

    format_mcp_response(&ContextSuggestAgentResult {
        recommendations,
        considered_agents,
    })
}

/// Handle context.get_hot_context tool call
pub async fn handle_context_get_hot_context<S: Store>(
    store: &S,
    params: ContextGetHotContextParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;

    info!(tenant_id = %tenant_id, k = params.k, "context.get_hot_context");

    let mut chunks = collect_all_chunks(store, &tenant_id, 20_000).await?;
    chunks.retain(|chunk| has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT));
    chunks.sort_by_key(|chunk| std::cmp::Reverse(chunk.timestamp_created));

    // Hide superseded/expired/history chunks, matching memory.search.
    // apply_visibility_filter stops at `k`, so it does at most k overlay
    // lookups over the recency-sorted candidates.
    let scored: Vec<(MemoryChunk, f32)> = chunks.into_iter().map(|c| (c, 1.0)).collect();
    let visible =
        apply_visibility_filter(store, scored, &VisibilityPolicy::default(), params.k).await;

    let results: Vec<ChunkResult> = visible
        .iter()
        .map(|(chunk, score)| chunk_to_result(chunk, *score, Some("hot".to_string()), None))
        .collect();

    format_mcp_response(&ContextGetHotContextResult { results })
}
