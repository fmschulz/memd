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

/// Handle context.search_context_documents tool call.
///
/// Phase 2.4 consolidation: operators should prefer
/// `memory.search` with `mode = "generic"` plus tag filters for new
/// integrations. `context.search_context_documents` still offers a
/// context-doc-specific return shape and remains supported for
/// existing callers, but we emit a deprecation-style log each call so
/// the usage is visible in telemetry.
pub async fn handle_context_search_documents<S: Store>(
    store: &S,
    params: ContextSearchDocumentsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    warn!(
        tool = "context.search_context_documents",
        "deprecated: prefer memory.search with tag filters / mode"
    );

    let tier = params
        .tier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(tier) = tier {
        if tier != "hot" && tier != "cold" {
            return Err(McpError::InvalidParams(
                "tier must be one of: hot, cold".to_string(),
            ));
        }
    }

    let subsystem_key = params
        .subsystem_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_filters = subsystem_key.is_some() || tier.is_some();
    let fetch_k = adaptive_fetch_k(params.k, &params.query, has_filters);

    info!(
        tenant_id = %tenant_id,
        query = %params.query,
        k = params.k,
        fetch_k = fetch_k,
        subsystem_key = ?subsystem_key,
        tier = ?tier,
        "context.search_context_documents"
    );

    let scored_chunks = store
        .search_with_scores(&tenant_id, &params.query, fetch_k)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    // Hide superseded/expired/history chunks, matching memory.search /
    // memory.get. This deprecated handler has no include_* knobs, so the
    // default policy (active-only) applies.
    let visible_cap = scored_chunks.len();
    let scored_chunks = apply_visibility_filter(
        store,
        scored_chunks,
        &VisibilityPolicy::default(),
        visible_cap,
    )
    .await;

    let mut filtered = Vec::new();
    for (chunk, score) in scored_chunks {
        if !is_context_chunk(&chunk) {
            continue;
        }
        if let Some(subsystem_key) = subsystem_key {
            if !chunk_matches_subsystem(&chunk, subsystem_key) {
                continue;
            }
        }
        if !chunk_matches_tier(&chunk, tier) {
            continue;
        }

        let source_tier = if has_exact_tag(&chunk.tags, TAG_CTX_TIER_HOT) {
            Some("hot".to_string())
        } else if has_exact_tag(&chunk.tags, TAG_CTX_TIER_COLD) {
            Some("cold".to_string())
        } else {
            None
        };
        filtered.push(chunk_to_result(&chunk, score, source_tier, None));
        if filtered.len() >= params.k {
            break;
        }
    }

    format_mcp_response(&ContextSearchDocumentsResult { results: filtered })
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

// ---------- Structural Query Handlers ----------

use crate::structural::{CallerInfo, ImportInfo, SymbolLocation, SymbolQueryService};

/// Handle code.find_definition tool call
pub fn handle_find_definition(
    query_service: &SymbolQueryService,
    params: FindDefinitionParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        "code.find_definition"
    );

    let locations = query_service
        .find_symbol_definition(&tenant_id, &params.name, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = locations.len(), "find_definition completed");

    let definitions: Vec<SymbolLocationResult> = locations
        .into_iter()
        .map(symbol_location_to_result)
        .collect();

    format_mcp_response(&FindDefinitionResult { definitions })
}

/// Handle code.find_references tool call
pub fn handle_find_references(
    query_service: &SymbolQueryService,
    params: FindReferencesParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        "code.find_references"
    );

    let locations = query_service
        .find_references(&tenant_id, &params.name, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = locations.len(), "find_references completed");

    let references: Vec<SymbolLocationResult> = locations
        .into_iter()
        .map(symbol_location_to_result)
        .collect();

    format_mcp_response(&FindReferencesResult { references })
}

/// Handle code.find_callers tool call
pub fn handle_find_callers(
    query_service: &SymbolQueryService,
    params: FindCallersParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    // Clamp depth to 1-3
    let depth = params.depth.clamp(1, 3);

    info!(
        tenant_id = %tenant_id,
        name = %params.name,
        depth = depth,
        "code.find_callers"
    );

    let caller_infos = query_service
        .find_callers(
            &tenant_id,
            &params.name,
            depth,
            params.project_id.as_deref(),
        )
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = caller_infos.len(), "find_callers completed");

    let callers: Vec<CallerInfoResult> = caller_infos
        .into_iter()
        .map(caller_info_to_result)
        .collect();

    format_mcp_response(&FindCallersResult { callers })
}

/// Handle code.find_imports tool call
pub fn handle_find_imports(
    query_service: &SymbolQueryService,
    params: FindImportsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        module = %params.module,
        "code.find_imports"
    );

    let import_infos = query_service
        .find_imports(&tenant_id, &params.module, params.project_id.as_deref())
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = import_infos.len(), "find_imports completed");

    let imports: Vec<ImportInfoResult> = import_infos
        .into_iter()
        .map(import_info_to_result)
        .collect();

    format_mcp_response(&FindImportsResult { imports })
}

/// Convert SymbolLocation to result type
fn symbol_location_to_result(loc: SymbolLocation) -> SymbolLocationResult {
    SymbolLocationResult {
        file_path: loc.file_path,
        name: loc.name,
        kind: loc.kind.as_str().to_string(),
        line_start: loc.line_start,
        line_end: loc.line_end,
        col_start: loc.col_start,
        col_end: loc.col_end,
        signature: loc.signature,
        docstring: loc.docstring,
        visibility: loc.visibility,
        language: loc.language,
    }
}

/// Convert CallerInfo to result type
fn caller_info_to_result(info: CallerInfo) -> CallerInfoResult {
    CallerInfoResult {
        caller_name: info.caller_name,
        caller_file: info.caller_file,
        call_line: info.call_line,
        call_col: info.call_col,
        caller_kind: info.caller_kind.as_str().to_string(),
        depth: info.depth,
    }
}

/// Convert ImportInfo to result type
fn import_info_to_result(info: ImportInfo) -> ImportInfoResult {
    ImportInfoResult {
        importing_file: info.importing_file,
        import_line: info.import_line,
        alias: info.alias,
    }
}

// ---------- Trace Query Handlers ----------

use crate::structural::{
    parse_iso_datetime, ErrorResult, FrameInfo, TimeRange as StructuralTimeRange, ToolCallResult,
    TraceQueryService,
};

/// Result type for debug.find_tool_calls
#[derive(Debug, Serialize, Deserialize)]
pub struct FindToolCallsResult {
    pub tool_calls: Vec<ToolCallResult>,
    pub total_count: usize,
}

/// Result type for debug.find_errors
#[derive(Debug, Serialize, Deserialize)]
pub struct FindErrorsResult {
    pub errors: Vec<ErrorResultResponse>,
    pub total_count: usize,
}

/// Error result with optional frames
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResultResponse {
    pub trace_id: i64,
    pub error_signature: String,
    pub error_message: String,
    pub timestamp_ms: i64,
    pub timestamp_formatted: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<FrameInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Convert ErrorResult to response, optionally including frames
fn error_to_response(error: ErrorResult, include_frames: bool) -> ErrorResultResponse {
    ErrorResultResponse {
        trace_id: error.trace_id,
        error_signature: error.error_signature,
        error_message: error.error_message,
        timestamp_ms: error.timestamp_ms,
        timestamp_formatted: error.timestamp_formatted,
        frames: if include_frames {
            Some(error.frames)
        } else {
            None
        },
        session_id: error.session_id,
    }
}

/// Parse time range from optional ISO 8601 strings
fn parse_trace_time_range(
    time_from: Option<&str>,
    time_to: Option<&str>,
) -> Result<Option<StructuralTimeRange>, McpError> {
    let from_ms = match time_from {
        Some(s) => Some(parse_iso_datetime(s).map_err(|e| McpError::InvalidParams(e.to_string()))?),
        None => None,
    };
    let to_ms = match time_to {
        Some(s) => Some(parse_iso_datetime(s).map_err(|e| McpError::InvalidParams(e.to_string()))?),
        None => None,
    };

    if from_ms.is_none() && to_ms.is_none() {
        Ok(None)
    } else {
        Ok(Some(StructuralTimeRange { from_ms, to_ms }))
    }
}

/// Handle debug.find_tool_calls tool call
pub fn handle_find_tool_calls(
    trace_service: &TraceQueryService,
    params: FindToolCallsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(100);

    // Parse time range
    let time_range =
        parse_trace_time_range(params.time_from.as_deref(), params.time_to.as_deref())?;

    info!(
        tenant_id = %tenant_id,
        tool_name = ?params.tool_name,
        session_id = ?params.session_id,
        errors_only = params.errors_only,
        limit = limit,
        "debug.find_tool_calls"
    );

    let tool_calls = if params.errors_only {
        trace_service
            .find_tool_calls_with_errors(&tenant_id, time_range)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        trace_service
            .find_tool_calls(
                &tenant_id,
                params.tool_name.as_deref(),
                time_range,
                params.session_id.as_deref(),
                limit,
            )
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    debug!(
        results_count = tool_calls.len(),
        "find_tool_calls completed"
    );

    let total_count = tool_calls.len();
    format_mcp_response(&FindToolCallsResult {
        tool_calls,
        total_count,
    })
}

/// Handle debug.find_errors tool call
pub fn handle_find_errors(
    trace_service: &TraceQueryService,
    params: FindErrorsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let limit = params.limit.min(100);

    // Parse time range
    let time_range =
        parse_trace_time_range(params.time_from.as_deref(), params.time_to.as_deref())?;

    info!(
        tenant_id = %tenant_id,
        error_signature = ?params.error_signature,
        function_name = ?params.function_name,
        file_path = ?params.file_path,
        limit = limit,
        "debug.find_errors"
    );

    let error_results = trace_service
        .find_errors(
            &tenant_id,
            params.error_signature.as_deref(),
            params.function_name.as_deref(),
            params.file_path.as_deref(),
            time_range,
            limit,
        )
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    debug!(results_count = error_results.len(), "find_errors completed");

    let total_count = error_results.len();
    let errors: Vec<ErrorResultResponse> = error_results
        .into_iter()
        .map(|e| error_to_response(e, params.include_frames))
        .collect();

    format_mcp_response(&FindErrorsResult {
        errors,
        total_count,
    })
}
