use super::*;

/// Parameters for memory.supersede
#[derive(Debug, Deserialize)]
pub struct SupersedeParams {
    #[serde(default)]
    pub tenant_id: String,
    pub old_chunk_id: String,
    pub new_text: String,
    #[serde(rename = "type")]
    pub chunk_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub source: Option<SourceParams>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Handle memory.supersede tool call.
///
/// Atomically supersedes an existing chunk with a new version via
/// `PersistentStore::supersede_chunk`. Returns both the formatted MCP
/// response and a `PostWriteEvent` so the server dispatch arm can run
/// structural indexing for the new chunk (mirroring memory.add).
pub async fn handle_memory_supersede<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: SupersedeParams,
) -> Result<(Value, PostWriteEvent), McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.supersede requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        old_chunk_id = %params.old_chunk_id,
        new_text_len = params.new_text.len(),
        "memory.supersede"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let old_id = ChunkId::parse(&params.old_chunk_id)
        .map_err(|e| McpError::InvalidParams(format!("old_chunk_id: {e}")))?;
    let chunk_type = parse_chunk_type(&params.chunk_type)?;

    // Capture source_path before `params.source` is consumed by
    // params_to_source — `SourceParams` is not Clone, so we lift the
    // path out by reference first and own it for the post-write event.
    let source_path = params.source.as_ref().and_then(|s| s.path.clone());

    let mut new_chunk = MemoryChunk::new(tenant_id.clone(), &params.new_text, chunk_type);
    if let Some(project_id) = params.project_id.clone() {
        new_chunk = new_chunk.with_project(ProjectId::new(Some(project_id)));
    }
    new_chunk = new_chunk.with_source(params_to_source(params.source));
    if !params.tags.is_empty() {
        new_chunk = new_chunk.with_tags(params.tags.clone());
    }

    let admission = crate::write_service::prepare_write(PrepareWriteRequest {
        chunk_type,
        text: &new_chunk.text,
        tags: &new_chunk.tags,
        ingestion_mode: crate::types::IngestionMode::Document,
        expires_at_ms: None,
        review_after_ms: None,
    });
    if admission.is_rejected() {
        return Err(McpError::InvalidParams(format!(
            "memory.supersede rejected by quality gate: {}",
            admission.outcome.reason
        )));
    }
    new_chunk = admission.apply_to_chunk(new_chunk);
    let lifecycle_delta = admission.lifecycle_delta();
    let (new_id, stored_chunk_ids) = ps
        .supersede_chunk_with_lifecycle_and_stored_ids(
            &tenant_id,
            &old_id,
            new_chunk,
            lifecycle_delta,
        )
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    info!(new_chunk_id = %new_id, old_chunk_id = %old_id, "chunk superseded");

    let event = PostWriteEvent {
        tenant_id: tenant_id.to_string(),
        chunk_id: new_id.clone(),
        chunk_type: params.chunk_type.clone(),
        project_id: params.project_id,
        source_path,
        text: params.new_text.clone(),
    };
    let response = format_mcp_response(&json!({
        "new_chunk_id": new_id.to_string(),
        "new_stored_chunk_ids": stored_chunk_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "old_chunk_id": old_id.to_string(),
        "admission_decision": admission.decision(),
        "admission_reason": admission.outcome.reason,
        "admission_warning": admission.outcome.warning,
    }))?;
    Ok((response, event))
}

/// Parameters for memory.set_expiry (Track C6).
///
/// The nested `Option<Option<i64>>` encodes triple-state:
/// - field absent → outer `None` → leave the overlay unchanged.
/// - field present and `null` → `Some(None)` → clear the overlay
///   field.
/// - field present with a value → `Some(Some(v))` → set the field.
#[derive(Debug, Deserialize, Default)]
pub struct SetExpiryParams {
    #[serde(default)]
    pub tenant_id: String,
    pub chunk_id: String,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub expires_at_ms: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub review_after_ms: Option<Option<i64>>,
}

/// Custom deserializer that preserves the "field present but null"
/// signal serde would otherwise collapse to `Option<Option<T>>::None`.
///
/// `#[serde(default)]` alone turns an absent field AND an explicit
/// `null` into the same value (both `None`), which defeats the
/// triple-state contract on `memory.set_expiry`. Wrapping the field
/// with `deserialize_with = "deserialize_some"` makes `null` round-trip
/// as `Some(None)` (clear) while keeping absent fields as `None` (leave).
fn deserialize_some<'de, T, D>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Handle memory.set_expiry tool call (Track C6).
///
/// Updates the `expires_at_ms` and/or `review_after_ms` overlay fields
/// on an existing chunk and bumps the tenant cache version when at
/// least one field changed. Refuses to run on non-persistent stores
/// because the overlay table only exists on `PersistentStore`.
pub async fn handle_memory_set_expiry<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: SetExpiryParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.set_expiry requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_id = ChunkId::parse(&params.chunk_id)
        .map_err(|e| McpError::InvalidParams(format!("chunk_id: {e}")))?;

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    // Reject a no-op payload so callers that forgot to send either
    // field get an explicit error instead of a silently-succeeding
    // cache bump.
    if params.expires_at_ms.is_none() && params.review_after_ms.is_none() {
        return Err(McpError::InvalidParams(
            "memory.set_expiry requires at least one of expires_at_ms / review_after_ms".into(),
        ));
    }

    info!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        set_expires = params.expires_at_ms.is_some(),
        set_review = params.review_after_ms.is_some(),
        "memory.set_expiry"
    );

    let delta = LifecycleDelta {
        expires_at_ms: params.expires_at_ms,
        review_after_ms: params.review_after_ms,
        lifecycle_updated_at_ms: Some(current_time_ms()),
        ..Default::default()
    };

    // Single atomic UPDATE whose rowcount drives the response. Fails
    // closed on both non-existent chunk IDs AND cross-tenant access
    // (the tenant filter is part of the UPDATE's WHERE, so a wrong
    // tenant matches zero rows and returns `Ok(false)` here). No
    // preflight read, so no TOCTOU window.
    let updated = ps
        .update_lifecycle_if_exists(&tenant_id, &chunk_id, &delta)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    if !updated {
        return Err(McpError::ToolError(format!(
            "memory.set_expiry: chunk {chunk_id} not found in tenant {tenant_id}"
        )));
    }

    format_mcp_response(&json!({
        "chunk_id": chunk_id.to_string(),
        "updated": true,
    }))
}

/// Handle memory.get tool call.
///
/// Resolves the authoritative lifecycle overlay before applying the requested
/// visibility policy. Hidden chunks omit their payload but expose status and
/// tier so callers can decide whether to retry with an include flag.
pub async fn handle_memory_get<S: Store>(store: &S, params: GetParams) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;

    debug!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        "memory.get"
    );

    let resolved = match store
        .get_with_lifecycle(&tenant_id, &chunk_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        Some(r) => r,
        None => {
            debug!(chunk_id = %chunk_id, "chunk not found");
            store.record_usage_event(UsageEvent {
                op: UsageOp::Get,
                tenant: Some(tenant_id.to_string()),
                project: None,
                outcome: "zero_hits".to_string(),
                chunk_count: Some(0),
                bytes: None,
                detail: None,
            });
            return format_mcp_response(&json!({ "found": false }));
        }
    };
    store.record_usage_event(UsageEvent {
        op: UsageOp::Get,
        tenant: Some(tenant_id.to_string()),
        project: None,
        outcome: "hits:1".to_string(),
        chunk_count: Some(1),
        bytes: None,
        detail: None,
    });

    let policy = VisibilityPolicy {
        include_superseded: params.include_superseded.unwrap_or(false),
        include_expired: params.include_expired.unwrap_or(false),
        include_history: params.include_history.unwrap_or(false),
    };

    // Single consolidation point for the lifecycle visibility rule —
    // `is_visible_at` covers status, tier, and the wall-clock
    // `expires_at_ms` window. B1 (search filter) and C3/C4 (tiering)
    // share this method so the rule never drifts between call sites.
    let now_ms = current_time_ms();
    if !policy.is_visible_at(resolved.status, &resolved.lifecycle, now_ms) {
        // `hidden_reason` tells the caller which `include_*` flag would
        // flip this row visible, so an agent that got `{hidden:true}`
        // can retry with the right knob without having to triangulate
        // from status + tier + expires_at_ms.
        //
        // Precedence MUST mirror `VisibilityPolicy::is_visible_at`
        // exactly, otherwise this discriminator reports a flag that
        // wouldn't actually unhide the row. The policy hides in the
        // order: status → tier → wall-clock expiry. `Deleted` rows
        // never reach this branch because `get_with_lifecycle` filters
        // them upstream; `Error` rows do reach here because the store
        // layer returns them (they are hidden by `is_visible`'s
        // status arm), and we report them as `"error"` — there is no
        // `include_error` knob, but the discriminator still describes
        // the state accurately instead of falling through to a wrong
        // bucket like `"history"`.
        use crate::types::{ChunkStatus, MemoryTier};
        let reason = match resolved.status {
            ChunkStatus::Candidate => "candidate",
            ChunkStatus::Superseded => "superseded",
            ChunkStatus::Expired => "expired",
            ChunkStatus::Error => "error",
            // At this point the status arm of `is_visible_at` accepted
            // the row, so the hide must be tier-based or clock-based.
            // Check tier first to match the policy's own order.
            _ if resolved.lifecycle.tier == MemoryTier::History => "history",
            _ if resolved
                .lifecycle
                .expires_at_ms
                .is_some_and(|t| t <= now_ms) =>
            {
                "expired"
            }
            // Unreachable: if none of the above, the row would have
            // been visible. Keep a defensive fallback rather than
            // panicking so a future policy change can't take the
            // handler down.
            _ => "unknown",
        };
        info!(
            chunk_id = %chunk_id,
            status = %resolved.status,
            tier = %resolved.lifecycle.tier,
            reason = reason,
            "memory.get hidden by visibility policy"
        );
        return format_mcp_response(&json!({
            "found": true,
            "hidden": true,
            "status": resolved.status.to_string(),
            "tier": resolved.lifecycle.tier.to_string(),
            "hidden_reason": reason,
        }));
    }

    info!(chunk_id = %chunk_id, "chunk found");
    format_mcp_response(&json!({
        "found": true,
        "chunk": resolved.chunk,
        "lifecycle": resolved.lifecycle,
        "status": resolved.status.to_string(),
    }))
}

/// Handle memory.delete tool call
pub async fn handle_memory_delete<S: Store>(
    store: &S,
    params: DeleteParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_id = validate_chunk_id(&params.chunk_id)?;

    info!(
        tenant_id = %tenant_id,
        chunk_id = %chunk_id,
        "memory.delete"
    );

    let deleted = store
        .delete(&tenant_id, &chunk_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    if deleted {
        info!(chunk_id = %chunk_id, "chunk deleted");
    } else {
        warn!(chunk_id = %chunk_id, "chunk not found for deletion");
    }
    store.record_usage_event(UsageEvent {
        op: UsageOp::Delete,
        tenant: Some(tenant_id.to_string()),
        project: None,
        outcome: if deleted { "ok" } else { "not_found" }.to_string(),
        chunk_count: Some(if deleted { 1 } else { 0 }),
        bytes: None,
        detail: None,
    });

    format_mcp_response(&DeleteResult { deleted })
}
