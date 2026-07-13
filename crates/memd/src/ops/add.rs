use super::*;

/// Handle memory.add tool call
pub async fn handle_memory_add<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: AddParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    ProjectId::validate_opt(params.project_id.as_deref())
        .map_err(|e| McpError::InvalidParams(e.to_string()))?;
    let chunk_type = parse_chunk_type(&params.chunk_type)?;

    info!(
        tenant_id = %tenant_id,
        chunk_type = %chunk_type,
        text_len = params.text.len(),
        "memory.add"
    );

    // Snapshot before the write: a brand-new tenant is reported in the
    // payload so a typo'd --tenant-id doesn't silently fork a new silo.
    let created_tenant = match store.list_tenants().await {
        Ok(tenants) if !tenants.iter().any(|t| t == &tenant_id) => Some(true),
        _ => None,
    };

    // Ensure tenant directory exists if tenant_manager is available
    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut chunk = MemoryChunk::new(tenant_id.clone(), &params.text, chunk_type);

    // Apply optional fields
    if let Some(project_id) = &params.project_id {
        chunk = chunk.with_project(ProjectId::new(Some(project_id.clone())));
    }

    if let Some(episode_id) = &params.episode_id {
        validate_episode_id(episode_id)?;
        let mut tags = chunk.tags.clone();
        tags.push(make_episode_tag(episode_id));
        chunk = chunk.with_tags(tags);
    }

    chunk = chunk.with_source(params_to_source(params.source));

    if let Some(ms) = params.event_time_ms {
        chunk.timestamp_observed = Some(ms);
    }

    if !params.tags.is_empty() {
        let mut tags = chunk.tags.clone();
        tags.extend(params.tags);
        chunk = chunk.with_tags(tags);
    }
    let caller_tags_for_dedupe = chunk.tags.clone();

    let ingestion_mode = parse_ingestion_mode(params.mode.as_deref())?;
    let mut admission = resolve_write_admission(
        chunk.chunk_type,
        &chunk.text,
        &chunk.tags,
        ingestion_mode,
        params.expires_at_ms,
        params.review_after_ms,
    );
    if admission.is_rejected() {
        record_add_usage_event(
            store,
            &tenant_id,
            params.project_id.clone(),
            format!("rejected:{}", admission.outcome.reason),
            0,
            chunk.text.len(),
        );
        return Err(McpError::InvalidParams(format!(
            "memory.add rejected by quality gate: {}",
            admission.outcome.reason
        )));
    }
    drop_optional_default_retention_for_in_memory_store(store, &mut admission);
    record_add_usage_event(
        store,
        &tenant_id,
        params.project_id.clone(),
        add_usage_outcome(&admission).to_string(),
        1,
        chunk.text.len(),
    );
    chunk = apply_admission_tags(chunk, &admission);

    // PreparedWrite has already applied normalized tags, priority, and mode.
    let lifecycle_delta = admission_lifecycle_delta(&admission);
    let has_lifecycle = !lifecycle_delta.is_empty();
    let caller_requested_lifecycle =
        params.expires_at_ms.is_some() || params.review_after_ms.is_some();
    let resolved_dedup = match params.supersede_near_duplicates.as_ref() {
        Some(spec) => {
            crate::dedup::resolve_spec(spec).map_err(|e| McpError::ToolError(e.to_string()))?
        }
        None => None,
    };

    if resolved_dedup.is_none() && !caller_requested_lifecycle {
        if let Some(existing_id) =
            find_default_content_duplicate(store, &chunk, &caller_tags_for_dedupe).await?
        {
            info!(chunk_id = %existing_id, "reused existing exact content duplicate");
            return format_mcp_response(&AddResult {
                chunk_id: existing_id.to_string(),
                dedupe_decision: Some("reused_existing_exact_content".to_string()),
                deduped_existing_id: Some(existing_id.to_string()),
                admission_decision: Some(admission_decision_string(&admission)),
                admission_reason: Some(admission.outcome.reason.clone()),
                admission_warning: admission.outcome.warning.clone(),
                lifecycle_tier: admission_lifecycle_tier_string(&admission),
                expires_at_ms: admission.retention.expires_at_ms,
                review_after_ms: admission.retention.review_after_ms,
                created_tenant: None,
            });
        }
    }

    // Track D path: when dedup is requested, find candidates first
    // (read-only on the store), then atomically supersede each one with
    // the new chunk via PersistentStore::supersede_chunk. The
    // supersede_chunk call already writes the new chunk + the
    // supersession edge in one logical op, so we drive the loop from
    // here rather than calling Store::add separately.
    if let Some(cfg) = resolved_dedup {
        let ps = store.as_persistent().ok_or_else(|| {
            McpError::ToolError(
                "memory.add with supersede_near_duplicates requires a persistent store".into(),
            )
        })?;
        let project_scope = chunk.project_id.as_option().map(|s| s.to_string());
        let candidates = crate::dedup::compute_dedup_candidates(
            ps,
            &chunk.tenant_id,
            &chunk.text,
            chunk.chunk_type,
            project_scope.as_deref(),
            &cfg,
        )?;

        // Snapshot tenant_id before `chunk` is consumed by either
        // dedup branch.
        let tenant_id_for_extras = chunk.tenant_id.clone();

        let new_chunk_id = if candidates.is_empty() {
            // No prior matches — fall back to a normal add. Lifecycle
            // overlay still applies if requested.
            if has_lifecycle {
                ps.add_chunk_with_lifecycle(chunk, lifecycle_delta.clone())
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?
            } else {
                store
                    .add(chunk)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?
            }
        } else {
            // Atomically replace the FIRST candidate with the new chunk
            // — `supersede_chunk` writes the payload + supersession
            // edge in one logical op. `compute_dedup_candidates`
            // already filtered to live-head rows (status=Final,
            // superseded_by=None), so the head-only guard inside
            // supersede_chunk will not fail-closed on stale candidates
            // (Codex round-1 D3 HIGH-2).
            let first_old = &candidates[0];
            let new_id = ps
                .supersede_chunk(&tenant_id_for_extras, first_old, chunk)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?;

            // Codex round-1 D3 HIGH-1: supersede_chunk does not carry a
            // lifecycle delta through, so the requested temporal overlay
            // (expires_at_ms / review_after_ms) is dropped on the
            // matched-dedup path. Apply it explicitly to the new
            // chunk_id so the dedup-vs-no-dedup behaviour is identical
            // when temporal fields are present.
            if has_lifecycle {
                let mut delta = lifecycle_delta.clone();
                if delta.lifecycle_updated_at_ms.is_none() {
                    delta.lifecycle_updated_at_ms = Some(current_time_ms());
                }
                ps.update_lifecycle(&tenant_id_for_extras, &new_id, &delta)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?;
            }
            new_id
        };

        // We only atomically superseded the FIRST candidate via
        // `supersede_chunk` — the call only handles a 1:1 edge.
        // Additional candidates (rare: only when the prior state
        // already contained multiple live-head duplicates of the same
        // canonical, e.g. a legacy backlog or a concurrent
        // no-dedup writer) are intentionally left untouched. The
        // response reflects exactly what changed so callers don't
        // think they got a stronger guarantee than supersede_chunk
        // actually delivers. A follow-up dedup run will clean up the
        // remaining duplicates one at a time.
        let superseded_ids = if candidates.is_empty() {
            Vec::new()
        } else {
            vec![candidates[0].to_string()]
        };

        info!(
            chunk_id = %new_chunk_id,
            superseded_total = candidates.len(),
            superseded_linked = superseded_ids.len(),
            "chunk added with dedup"
        );
        return format_mcp_response(&serde_json::json!({
            "chunk_id": new_chunk_id.to_string(),
            "superseded_ids": superseded_ids,
            "admission_decision": admission_decision_string(&admission),
            "admission_reason": admission.outcome.reason.clone(),
            "admission_warning": admission.outcome.warning.clone(),
            "lifecycle_tier": admission_lifecycle_tier_string(&admission),
            "expires_at_ms": admission.retention.expires_at_ms,
            "review_after_ms": admission.retention.review_after_ms,
        }));
    }

    let chunk_id = if has_lifecycle {
        // Temporal overlay requires the persistent-store write path that
        // updates the lifecycle row in the same logical op. Non-persistent
        // stores (used only by a small handful of tests) have no overlay
        // table, so we refuse rather than silently dropping the fields.
        let ps = store.as_persistent().ok_or_else(|| {
            McpError::ToolError(
                "memory.add with temporal fields requires a persistent store".into(),
            )
        })?;
        ps.add_chunk_with_lifecycle(chunk, lifecycle_delta)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        store
            .add(chunk)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    info!(chunk_id = %chunk_id, "chunk added");

    format_mcp_response(&AddResult {
        chunk_id: chunk_id.to_string(),
        dedupe_decision: None,
        deduped_existing_id: None,
        admission_decision: Some(admission_decision_string(&admission)),
        admission_reason: Some(admission.outcome.reason.clone()),
        admission_warning: admission.outcome.warning.clone(),
        lifecycle_tier: admission_lifecycle_tier_string(&admission),
        expires_at_ms: admission.retention.expires_at_ms,
        review_after_ms: admission.retention.review_after_ms,
        created_tenant,
    })
}

/// Handle memory.add_batch tool call
pub async fn handle_memory_add_batch<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: AddBatchParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    for chunk in &params.chunks {
        ProjectId::validate_opt(chunk.project_id.as_deref())
            .map_err(|e| McpError::InvalidParams(e.to_string()))?;
    }

    info!(
        tenant_id = %tenant_id,
        count = params.chunks.len(),
        "memory.add_batch"
    );

    // Ensure tenant directory exists if tenant_manager is available
    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    // Resolve the optional Track D dedup spec once for the whole batch.
    let resolved_dedup = match params.supersede_near_duplicates.as_ref() {
        Some(spec) => {
            crate::dedup::resolve_spec(spec).map_err(|e| McpError::ToolError(e.to_string()))?
        }
        None => None,
    };

    // Track D path: when dedup is requested, fall out of the batched
    // fast path entirely and treat each chunk independently — same
    // contract as D3 on memory.add. The response gains a parallel
    // `superseded_ids` array of arrays so callers can correlate.
    if let Some(cfg) = resolved_dedup {
        let ps = store.as_persistent().ok_or_else(|| {
            McpError::ToolError(
                "memory.add_batch with supersede_near_duplicates requires a persistent store"
                    .into(),
            )
        })?;

        // Pre-pass: consume params.chunks once and build the (chunk,
        // delta, has_lifecycle, project_id) tuples up front so
        // validation failures abort cleanly without committing half a
        // batch. SourceParams is not Clone, so we have to move it out
        // of chunk_params here rather than borrow inside the second
        // pass.
        let mut prepared: Vec<(
            MemoryChunk,
            LifecycleDelta,
            bool,
            Option<String>,
            ResolvedAdmission,
        )> = Vec::with_capacity(params.chunks.len());
        for chunk_params in params.chunks {
            let chunk_type = parse_chunk_type(&chunk_params.chunk_type)?;
            let project_id_for_dedup = chunk_params.project_id.clone();
            let mut chunk = MemoryChunk::new(tenant_id.clone(), &chunk_params.text, chunk_type);
            if let Some(project_id) = &chunk_params.project_id {
                chunk = chunk.with_project(ProjectId::new(Some(project_id.clone())));
            }
            if let Some(episode_id) = &chunk_params.episode_id {
                validate_episode_id(episode_id)?;
                let mut tags = chunk.tags.clone();
                tags.push(make_episode_tag(episode_id));
                chunk = chunk.with_tags(tags);
            }
            chunk = chunk.with_source(params_to_source(chunk_params.source));
            if let Some(ms) = chunk_params.event_time_ms {
                chunk.timestamp_observed = Some(ms);
            }
            if !chunk_params.tags.is_empty() {
                let mut tags = chunk.tags.clone();
                tags.extend(chunk_params.tags);
                chunk = chunk.with_tags(tags);
            }
            // Track E: per-chunk mode + conversation default review window.
            let ingestion_mode = parse_ingestion_mode(chunk_params.mode.as_deref())?;
            let mut admission = resolve_write_admission(
                chunk.chunk_type,
                &chunk.text,
                &chunk.tags,
                ingestion_mode,
                chunk_params.expires_at_ms,
                chunk_params.review_after_ms,
            );
            if admission.is_rejected() {
                return Err(McpError::InvalidParams(format!(
                    "memory.add_batch rejected chunk {} by quality gate: {}",
                    prepared.len(),
                    admission.outcome.reason
                )));
            }
            drop_optional_default_retention_for_in_memory_store(store, &mut admission);
            chunk = apply_admission_tags(chunk, &admission);
            chunk = chunk.with_ingestion_mode(ingestion_mode);
            let delta = admission_lifecycle_delta(&admission);
            let has_lifecycle = !delta.is_empty();
            prepared.push((chunk, delta, has_lifecycle, project_id_for_dedup, admission));
        }

        // Second pass: per-chunk dedup-or-add. Failures still leave
        // earlier rows committed, matching the existing add_batch
        // failure contract.
        let mut chunk_ids: Vec<String> = Vec::with_capacity(prepared.len());
        let mut superseded_ids: Vec<Vec<String>> = Vec::with_capacity(prepared.len());
        let mut admission_decisions = Vec::with_capacity(prepared.len());
        let mut admission_reasons = Vec::with_capacity(prepared.len());
        for (chunk, delta, has_lifecycle, project_id, admission) in prepared {
            let bytes = chunk.text.len();
            let candidates = crate::dedup::compute_dedup_candidates(
                ps,
                &tenant_id,
                &chunk.text,
                chunk.chunk_type,
                project_id.as_deref(),
                &cfg,
            )?;
            let new_id = if candidates.is_empty() {
                if has_lifecycle {
                    ps.add_chunk_with_lifecycle(chunk, delta.clone())
                        .await
                        .map_err(|e| McpError::ToolError(e.to_string()))?
                } else {
                    store
                        .add(chunk)
                        .await
                        .map_err(|e| McpError::ToolError(e.to_string()))?
                }
            } else {
                let first_old = &candidates[0];
                let new_id = ps
                    .supersede_chunk(&tenant_id, first_old, chunk)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?;
                if has_lifecycle {
                    let mut d = delta.clone();
                    if d.lifecycle_updated_at_ms.is_none() {
                        d.lifecycle_updated_at_ms = Some(current_time_ms());
                    }
                    ps.update_lifecycle(&tenant_id, &new_id, &d)
                        .await
                        .map_err(|e| McpError::ToolError(e.to_string()))?;
                }
                new_id
            };
            // Mirror D3: only the first candidate is actually linked
            // by supersede_chunk; report only what we changed.
            let linked = if candidates.is_empty() {
                Vec::new()
            } else {
                vec![candidates[0].to_string()]
            };
            // Record the add event only after the write commits, so an
            // aborted batch (later chunk rejected/failed) never inflates
            // `memd report` growth with an admitted chunk that was never
            // stored. Every chunk in this branch is inserted or superseded
            // (a new chunk id either way), so all are counted.
            record_add_usage_event(
                store,
                &tenant_id,
                project_id.clone(),
                add_usage_outcome(&admission).to_string(),
                1,
                bytes,
            );
            chunk_ids.push(new_id.to_string());
            superseded_ids.push(linked);
            admission_decisions.push(admission_decision_string(&admission));
            admission_reasons.push(admission.outcome.reason);
        }

        info!(count = chunk_ids.len(), "batch add (with dedup) completed");
        return format_mcp_response(&serde_json::json!({
            "chunk_ids": chunk_ids,
            "superseded_ids": superseded_ids,
            "admission_decisions": admission_decisions,
            "admission_reasons": admission_reasons,
        }));
    }

    // If any chunk carries a temporal overlay field, fall out of the
    // Track E: pre-pass over every chunk to apply ingestion_mode +
    // conversation-mode review default. This decides per-chunk whether
    // a lifecycle delta is required and produces the (chunk, delta)
    // tuples both branches consume. Batches without any per-chunk
    // lifecycle (no expires_at_ms / review_after_ms / conversation
    // mode) keep the bulk `store.add_batch` fast path unchanged.
    let mut prepared: Vec<(
        MemoryChunk,
        LifecycleDelta,
        bool,
        ResolvedAdmission,
        Vec<String>,
        bool,
    )> = Vec::with_capacity(params.chunks.len());
    // Deferred add-event metadata (project, bytes, outcome) per chunk.
    // Usage events are recorded only after the writes commit below, so an
    // aborted batch never inflates `memd report` growth.
    let mut add_event_meta: Vec<(Option<String>, usize, String)> =
        Vec::with_capacity(params.chunks.len());
    for chunk_params in params.chunks {
        let chunk_type = parse_chunk_type(&chunk_params.chunk_type)?;
        let caller_requested_lifecycle =
            chunk_params.expires_at_ms.is_some() || chunk_params.review_after_ms.is_some();
        let mut chunk = MemoryChunk::new(tenant_id.clone(), &chunk_params.text, chunk_type);
        if let Some(project_id) = &chunk_params.project_id {
            chunk = chunk.with_project(ProjectId::new(Some(project_id.clone())));
        }
        if let Some(episode_id) = &chunk_params.episode_id {
            validate_episode_id(episode_id)?;
            let mut tags = chunk.tags.clone();
            tags.push(make_episode_tag(episode_id));
            chunk = chunk.with_tags(tags);
        }
        chunk = chunk.with_source(params_to_source(chunk_params.source));
        if let Some(ms) = chunk_params.event_time_ms {
            chunk.timestamp_observed = Some(ms);
        }
        if !chunk_params.tags.is_empty() {
            let mut tags = chunk.tags.clone();
            tags.extend(chunk_params.tags);
            chunk = chunk.with_tags(tags);
        }
        let caller_tags_for_dedupe = chunk.tags.clone();
        let ingestion_mode = parse_ingestion_mode(chunk_params.mode.as_deref())?;
        let mut admission = resolve_write_admission(
            chunk.chunk_type,
            &chunk.text,
            &chunk.tags,
            ingestion_mode,
            chunk_params.expires_at_ms,
            chunk_params.review_after_ms,
        );
        if admission.is_rejected() {
            return Err(McpError::InvalidParams(format!(
                "memory.add_batch rejected chunk {} by quality gate: {}",
                prepared.len(),
                admission.outcome.reason
            )));
        }
        drop_optional_default_retention_for_in_memory_store(store, &mut admission);
        // Capture the add event now; it is recorded after the write commits
        // (see below), keyed positionally to chunk_ids/dedupe_decisions.
        add_event_meta.push((
            chunk_params.project_id.clone(),
            chunk.text.len(),
            add_usage_outcome(&admission).to_string(),
        ));
        chunk = apply_admission_tags(chunk, &admission);
        chunk = chunk.with_ingestion_mode(ingestion_mode);
        let delta = admission_lifecycle_delta(&admission);
        let has_lifecycle = !delta.is_empty();
        prepared.push((
            chunk,
            delta,
            has_lifecycle,
            admission,
            caller_tags_for_dedupe,
            caller_requested_lifecycle,
        ));
    }
    let any_lifecycle = prepared.iter().any(|(_, _, hl, _, _, _)| *hl);
    let admission_decisions = prepared
        .iter()
        .map(|(_, _, _, admission, _, _)| admission_decision_string(admission))
        .collect::<Vec<_>>();
    let admission_reasons = prepared
        .iter()
        .map(|(_, _, _, admission, _, _)| admission.outcome.reason.clone())
        .collect::<Vec<_>>();

    let prepared_len = prepared.len();
    let mut dedupe_decisions = Vec::with_capacity(prepared_len);
    let mut deduped_existing_ids = Vec::with_capacity(prepared_len);
    let mut any_default_deduped = false;

    let chunk_ids = if any_lifecycle {
        let ps = store.as_persistent().ok_or_else(|| {
            McpError::ToolError(
                "memory.add_batch with temporal fields requires a persistent store".into(),
            )
        })?;
        let mut ids = Vec::with_capacity(prepared.len());
        for (chunk, delta, has_lifecycle, _, caller_tags, caller_requested_lifecycle) in prepared {
            if !caller_requested_lifecycle {
                if let Some(existing_id) =
                    find_default_content_duplicate(store, &chunk, &caller_tags).await?
                {
                    let existing_id_string = existing_id.to_string();
                    ids.push(existing_id);
                    dedupe_decisions.push("reused_existing_exact_content".to_string());
                    deduped_existing_ids.push(Some(existing_id_string));
                    any_default_deduped = true;
                    continue;
                }
            }

            let id = if has_lifecycle {
                ps.add_chunk_with_lifecycle(chunk, delta)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?
            } else {
                store
                    .add(chunk)
                    .await
                    .map_err(|e| McpError::ToolError(e.to_string()))?
            };
            ids.push(id);
            dedupe_decisions.push("inserted".to_string());
            deduped_existing_ids.push(None);
        }
        ids
    } else if store.as_persistent().is_some() {
        dedupe_decisions = vec!["inserted".to_string(); prepared_len];
        deduped_existing_ids = vec![None; prepared_len];

        let mut ids_by_position: Vec<Option<ChunkId>> = vec![None; prepared_len];
        let mut pending_insert_positions = Vec::new();
        let mut pending_insert_chunks = Vec::new();
        let mut pending_for_dedupe: Vec<(usize, MemoryChunk)> = Vec::new();
        let mut pending_aliases: Vec<(usize, usize)> = Vec::new();

        for (position, (chunk, _, _, _, caller_tags, _)) in prepared.into_iter().enumerate() {
            if let Some(existing_id) =
                find_default_content_duplicate(store, &chunk, &caller_tags).await?
            {
                let existing_id_string = existing_id.to_string();
                ids_by_position[position] = Some(existing_id);
                dedupe_decisions[position] = "reused_existing_exact_content".to_string();
                deduped_existing_ids[position] = Some(existing_id_string);
                any_default_deduped = true;
                continue;
            }

            let mut pending_duplicate_position = None;
            if !default_content_dedupe_exempt(&chunk, &caller_tags) {
                for (prior_position, prior_chunk) in &pending_for_dedupe {
                    if prior_chunk.text == chunk.text
                        && prior_chunk.chunk_type == chunk.chunk_type
                        && prior_chunk.project_id == chunk.project_id
                        && (chunk.source == Source::empty() || chunk.source == prior_chunk.source)
                        && caller_tags_already_preserved(prior_chunk, &caller_tags)
                    {
                        pending_duplicate_position = Some(*prior_position);
                        break;
                    }
                }
            }

            if let Some(prior_position) = pending_duplicate_position {
                pending_aliases.push((position, prior_position));
                dedupe_decisions[position] = "reused_existing_exact_content".to_string();
                any_default_deduped = true;
                continue;
            }

            pending_insert_positions.push(position);
            pending_for_dedupe.push((position, chunk.clone()));
            pending_insert_chunks.push(chunk);
        }

        let inserted_ids = store
            .add_batch(pending_insert_chunks)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;
        if inserted_ids.len() != pending_insert_positions.len() {
            return Err(McpError::ToolError(format!(
                "memory.add_batch inserted {} chunks but expected {}",
                inserted_ids.len(),
                pending_insert_positions.len()
            )));
        }
        for (position, id) in pending_insert_positions.into_iter().zip(inserted_ids) {
            ids_by_position[position] = Some(id);
        }
        for (position, prior_position) in pending_aliases {
            let id = ids_by_position
                .get(prior_position)
                .and_then(|id| id.clone())
                .ok_or_else(|| {
                    McpError::ToolError(
                        "memory.add_batch missing inserted id for within-batch duplicate".into(),
                    )
                })?;
            deduped_existing_ids[position] = Some(id.to_string());
            ids_by_position[position] = Some(id);
        }

        ids_by_position
            .into_iter()
            .enumerate()
            .map(|(position, maybe_id)| {
                maybe_id.ok_or_else(|| {
                    McpError::ToolError(format!(
                        "memory.add_batch did not produce chunk id for position {position}"
                    ))
                })
            })
            .collect::<std::result::Result<Vec<_>, McpError>>()?
    } else {
        dedupe_decisions = vec!["inserted".to_string(); prepared_len];
        deduped_existing_ids = vec![None; prepared_len];
        // No lifecycle overlay anywhere → bulk path. The chunks already
        // carry the per-row ingestion_mode label (set in the pre-pass);
        // store.add_batch threads that through to ChunkMetadata.
        let chunks: Vec<MemoryChunk> = prepared.into_iter().map(|(c, _, _, _, _, _)| c).collect();
        store
            .add_batch(chunks)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    // Record one add usage event per committed chunk, now that the writes
    // have succeeded — deferring past the writes means an aborted batch never
    // inflates `memd report` growth. Chunks reused via exact-content dedup
    // wrote nothing new, so they are skipped. add_event_meta is positionally
    // aligned with dedupe_decisions and chunk_ids.
    for (i, (project, bytes, outcome)) in add_event_meta.into_iter().enumerate() {
        let reused = dedupe_decisions
            .get(i)
            .is_some_and(|d| d == "reused_existing_exact_content");
        if reused {
            continue;
        }
        record_add_usage_event(store, &tenant_id, project, outcome, 1, bytes);
    }

    info!(count = chunk_ids.len(), "batch add completed");

    format_mcp_response(&AddBatchResult {
        chunk_ids: chunk_ids.iter().map(|id| id.to_string()).collect(),
        dedupe_decisions: any_default_deduped.then_some(dedupe_decisions),
        deduped_existing_ids: any_default_deduped.then_some(deduped_existing_ids),
        admission_decisions: Some(admission_decisions),
        admission_reasons: Some(admission_reasons),
    })
}
