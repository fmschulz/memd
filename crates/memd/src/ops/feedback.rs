use super::*;

/// Handle memory.feedback tool call.
pub async fn handle_memory_feedback<S: Store>(
    store: &S,
    params: FeedbackParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;
    let query = params.query.trim();
    if query.is_empty() {
        return Err(McpError::InvalidParams(
            "query must not be empty".to_string(),
        ));
    }
    let relevance = parse_relevance_label(&params.relevance)?;

    let chunk = store
        .get(&tenant_id, &chunk_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let Some(chunk) = chunk else {
        return Err(McpError::InvalidParams(
            "chunk_id not found for tenant".to_string(),
        ));
    };

    let timestamp_ms = current_time_ms();
    let feedback = FeedbackEntry::new_scoped(
        tenant_id,
        chunk.project_id.as_option().map(str::to_string),
        query.to_string(),
        chunk_id,
        relevance,
        timestamp_ms,
    );
    store
        .add_feedback(feedback)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&FeedbackResult { stored: true })
}

/// Record an explicit outcome against chunks rendered in one retrieval episode.
pub async fn handle_memory_record_outcome<S: Store>(
    store: &S,
    params: RecordOutcomeParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let episode_id = RetrievalEpisodeId::parse(&params.episode_id)
        .map_err(|error| McpError::InvalidParams(error.to_string()))?;
    let outcome = OutcomeKind::parse(params.outcome.trim())
        .map_err(|error| McpError::InvalidParams(error.to_string()))?;
    let verifier = OutcomeVerifier::parse(params.verifier_type.trim())
        .map_err(|error| McpError::InvalidParams(error.to_string()))?;
    let used_chunk_ids = params
        .used_chunk_ids
        .iter()
        .map(|chunk_id| validate_chunk_id(chunk_id))
        .collect::<Result<Vec<_>, _>>()?;
    let harmful_chunk_ids = params
        .harmful_chunk_ids
        .iter()
        .map(|chunk_id| validate_chunk_id(chunk_id))
        .collect::<Result<Vec<_>, _>>()?;
    let timestamp_ms = params.event_time_ms.unwrap_or_else(current_time_ms);
    let event = OutcomeEvent::new(
        episode_id.clone(),
        outcome,
        verifier,
        used_chunk_ids,
        harmful_chunk_ids,
        params.evidence_reference,
        timestamp_ms,
    );
    store
        .record_outcome(&tenant_id, event.clone())
        .await
        .map_err(|error| McpError::InvalidParams(error.to_string()))?;
    format_mcp_response(&RecordOutcomeResult {
        event_id: event.event_id.to_string(),
        episode_id: episode_id.to_string(),
        stored: true,
        ranking_eligible: event.ranking_eligible,
    })
}
