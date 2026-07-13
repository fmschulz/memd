use super::*;

/// Handle task.start tool call.
pub async fn handle_task_start<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskStartParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    // `goal` remains the only hard-required field on task.start in
    // v0.3.1+ (see Phase 2.2). motivation/hypothesis/scientific_question
    // became optional — they can be empty strings when the caller has
    // nothing to say; richer task records still fill them in.
    validate_identifier("goal", &params.goal)?;
    if let Some(parent_task_id) = params.parent_task_id.as_deref() {
        validate_identifier("parent_task_id", parent_task_id)?;
    }

    info!(
        tenant_id = %tenant_id,
        goal = %params.goal,
        "task.start"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_start(tenant_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.parent_task_id = params.parent_task_id;
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.goal = Some(params.goal);
    artifact.motivation = Some(params.motivation);
    artifact.hypothesis = Some(params.hypothesis);
    artifact.scientific_question = Some(params.scientific_question);
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.expected_outputs = params.expected_outputs;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    finalize_artifact_for_storage(&mut artifact);
    let projections = build_task_projections(&artifact);
    let result = store
        .add_task_artifact(artifact, projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.finish tool call.
pub async fn handle_task_finish<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskFinishParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    // Confidence is optional in v0.3.1+; only validate when supplied.
    if let Some(confidence) = params.confidence {
        validate_confidence(confidence)?;
    }

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        "task.finish"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_finish(tenant_id, params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.status = Some(params.status.unwrap_or_else(|| "completed".to_string()));
    artifact.goal = params.goal;
    artifact.scientific_question = params.scientific_question;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.what_worked = params.what_worked;
    artifact.what_failed = params.what_failed;
    artifact.validation = params.validation;
    artifact.uncertainty = params.uncertainty;
    artifact.followups = params.followups;
    // `confidence` is optional in v0.3.1+; only attach when the caller
    // actually asserted a value.
    artifact.confidence = params.confidence;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    finalize_artifact_for_storage(&mut artifact);
    let projections = build_task_projections(&artifact);
    // Capture scope for the dirty-digest hook before `artifact` moves
    // into the store.
    let tenant_for_dirty = artifact.tenant_id.clone();
    let project_for_dirty = artifact.project_id.as_option().map(str::to_string);
    let result = store
        .add_task_artifact(artifact, projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Phase 3.4: task.finish rolls up what_worked / what_failed /
    // validation, which are exactly the inputs to the failure,
    // highlight, and project_brief digests.
    mark_task_finish_digests_dirty(&tenant_for_dirty, project_for_dirty.as_deref());

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.progress tool call.
pub async fn handle_task_progress<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskProgressParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("summary", &params.summary)?;
    validate_identifier("next_step", &params.next_step)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        "task.progress"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_task_progress(tenant_id, params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.summary = Some(params.summary);
    artifact.blockers = params.blockers;
    artifact.what_failed = params.failed_attempts;
    artifact.followups = vec![params.next_step];
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    finalize_artifact_for_storage(&mut artifact);
    let result = store
        .add_task_artifact(
            artifact.clone(),
            // Phase 2.5: high-frequency task.* handlers emit one
            // projection per call (the base summary) instead of the
            // legacy 4-7 fanout. See
            // `build_task_projections_minimal` for rationale.
            build_task_projections_minimal(&artifact),
        )
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.run_start tool call.
pub async fn handle_task_run_start<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskRunStartParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("tool_name", &params.tool_name)?;
    validate_identifier("command", &params.command)?;
    validate_identifier("why_chosen", &params.why_chosen)?;
    if params.inputs.is_empty() {
        return Err(McpError::InvalidParams(
            "inputs must not be empty".to_string(),
        ));
    }

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        tool_name = %params.tool_name,
        "task.run_start"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_run_start(tenant_id, params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.summary = params.summary;
    artifact.tool_name = Some(params.tool_name);
    artifact.tool_version = params.tool_version;
    artifact.command = Some(params.command);
    artifact.why_chosen = Some(params.why_chosen);
    artifact.parameters = Some(params.parameters);
    artifact.inputs = params.inputs;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    if artifact.provenance.tool_name.is_none() {
        artifact.provenance.tool_name = artifact.tool_name.clone();
    }
    if artifact.provenance.tool_version.is_none() {
        artifact.provenance.tool_version = artifact.tool_version.clone();
    }

    finalize_artifact_for_storage(&mut artifact);
    // run_start keeps full projections because the separate Run
    // projection carries tool/command/parameters content that
    // retrieval filters rely on (see task_search_filters_exactly_by_tool_and_dataset).
    let result = store
        .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.run_finish tool call.
pub async fn handle_task_run_finish<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskRunFinishParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("status", &params.status)?;
    validate_identifier("notes", &params.notes)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        status = %params.status,
        "task.run_finish"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let mut artifact = TaskArtifact::new_run_finish(tenant_id, params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    artifact.status = Some(params.status);
    artifact.tool_name = params.tool_name;
    artifact.tool_version = params.tool_version;
    artifact.command = params.command;
    artifact.outputs = params.outputs;
    artifact.metrics = params.metrics;
    artifact.summary = Some(params.notes);
    artifact.validation = params.validation;
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    if artifact.provenance.tool_name.is_none() {
        artifact.provenance.tool_name = artifact.tool_name.clone();
    }
    if artifact.provenance.tool_version.is_none() {
        artifact.provenance.tool_version = artifact.tool_version.clone();
    }

    finalize_artifact_for_storage(&mut artifact);
    // run_finish keeps full projections so tool/outputs/metrics are
    // still indexed as retrievable text for tool-name filters.
    let result = store
        .add_task_artifact(artifact.clone(), build_task_projections(&artifact))
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.add_evidence tool call.
pub async fn handle_task_add_evidence<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: TaskAddEvidenceParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_identifier("summary", &params.summary)?;
    validate_identifier("evidence_kind", &params.evidence_kind)?;

    info!(
        tenant_id = %tenant_id,
        task_id = %params.task_id,
        evidence_kind = %params.evidence_kind,
        "task.add_evidence"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    // Keep `tenant_id` available for the post-write dirty-digest hook.
    let mut artifact = TaskArtifact::new_evidence(tenant_id.clone(), params.task_id);
    artifact.thread_id = Some(artifact.task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id);
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.session_id = params.session_id;
    // Summary is optional in v0.3.1+; only set when non-empty so
    // downstream `score_text_candidate` does not index a bogus empty
    // string.
    artifact.summary = (!params.summary.is_empty()).then_some(params.summary);
    artifact.evidence_kind = Some(params.evidence_kind);
    artifact.supports_claim = params.supports_claim;
    artifact.metrics = match (params.metric_name, params.metric_value, params.metrics) {
        (_, _, Some(metrics)) => Some(metrics),
        (Some(metric_name), Some(metric_value), None) => Some(json!({
            "metric_name": metric_name,
            "metric_value": metric_value,
        })),
        _ => None,
    };
    artifact.dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    artifact.entity_refs = entity_params_to_refs(params.entity_refs)?;
    artifact.provenance = params_to_task_provenance(params.provenance);
    artifact.tool_name = artifact.provenance.tool_name.clone();
    artifact.tool_version = artifact.provenance.tool_version.clone();

    finalize_artifact_for_storage(&mut artifact);
    let result = store
        .add_task_artifact(
            artifact.clone(),
            // Phase 2.5: high-frequency task.* handlers emit one
            // projection per call (the base summary) instead of the
            // legacy 4-7 fanout. See
            // `build_task_projections_minimal` for rationale.
            build_task_projections_minimal(&artifact),
        )
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Phase 3.4: evidence writes invalidate the evidence library,
    // highlight library (which ranks evidence-backed lessons), and
    // project brief (which summarizes evidence density).
    mark_evidence_related_digests_dirty(&tenant_id, artifact.project_id.as_option());

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Phase 3.4: mark every digest whose view depends on evidence
/// content as dirty. Called from `task.add_evidence` and from the
/// artifact.create path when the kind influences evidence aggregation.
fn mark_evidence_related_digests_dirty(tenant_id: &TenantId, project_id: Option<&str>) {
    let project = project_id.map(str::to_string);
    for role in [
        crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY,
        crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF,
    ] {
        crate::task_memory::digest_dirty::mark_dirty(tenant_id.to_string(), project.clone(), role);
    }
}

/// Phase 3.4: mark digests affected by decision/review/revision
/// artifact writes.
fn mark_decision_related_digests_dirty(tenant_id: &TenantId, project_id: Option<&str>) {
    let project = project_id.map(str::to_string);
    for role in [
        crate::task_memory::DIGEST_ROLE_DECISION_LIBRARY,
        crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF,
    ] {
        crate::task_memory::digest_dirty::mark_dirty(tenant_id.to_string(), project.clone(), role);
    }
}

/// Phase 3.4: `task.finish` captures `what_failed` / `validation` /
/// `what_worked` / `followups`, which feed ALL four canonical-data
/// digest families (`infer_failure_items`, `infer_decision_items`,
/// `infer_evidence_items`, `infer_highlight_items` in `task_memory::digests`
/// all consume `TaskFinish`). Mark every one dirty so the sweeper
/// refreshes the full set; dropping decision/evidence here was a
/// Codex-flagged coverage hole.
fn mark_task_finish_digests_dirty(tenant_id: &TenantId, project_id: Option<&str>) {
    let project = project_id.map(str::to_string);
    for role in [
        crate::task_memory::DIGEST_ROLE_FAILURE_LIBRARY,
        crate::task_memory::DIGEST_ROLE_DECISION_LIBRARY,
        crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY,
        crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY,
        crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF,
    ] {
        crate::task_memory::digest_dirty::mark_dirty(tenant_id.to_string(), project.clone(), role);
    }
}

/// Handle artifact.create tool call.
pub async fn handle_artifact_create<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: ArtifactCreateParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let artifact_kind =
        ArtifactKind::from_str(&params.artifact_kind).map_err(McpError::InvalidParams)?;

    // Digest artifacts are server-generated by the compaction runner /
    // memory.compact path (via `persist_digest_artifact`). Because their
    // IDs are deterministic on (role, scope), accepting client-authored
    // digests lets any caller overwrite the project's canonical digest
    // artifacts (`project_brief`, `failure_library`, …). Reject them at
    // the boundary — the only legitimate way to refresh a digest is via
    // `memory.compact`.
    if artifact_kind == ArtifactKind::Digest {
        return Err(McpError::InvalidParams(
            "artifact.create: digests are server-generated; \
             use memory.compact to refresh digest artifacts"
                .to_string(),
        ));
    }

    if let Some(confidence) = params.confidence {
        validate_confidence(confidence)?;
    }
    if let Some(reply_to_artifact_id) = params.reply_to_artifact_id.as_deref() {
        validate_identifier("reply_to_artifact_id", reply_to_artifact_id)?;
    }

    info!(
        tenant_id = %tenant_id,
        artifact_kind = %artifact_kind.as_str(),
        "artifact.create"
    );

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let parent_artifact = if let Some(reply_to_artifact_id) = params.reply_to_artifact_id.as_deref()
    {
        store
            .get_task_artifact(&tenant_id, reply_to_artifact_id)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        None
    };

    let explicit_task_id = match params.task_id.as_deref() {
        Some(task_id) => {
            validate_identifier("task_id", task_id)?;
            Some(task_id.to_string())
        }
        None => None,
    };
    let task_id = explicit_task_id.clone().or_else(|| {
        parent_artifact
            .as_ref()
            .map(|artifact| artifact.task_id.clone())
    });

    let mut artifact = match artifact_kind {
        ArtifactKind::TaskStart => {
            let mut artifact = TaskArtifact::new_task_start(tenant_id.clone());
            if let Some(task_id) = explicit_task_id.clone() {
                artifact.task_id = task_id;
            }
            artifact
        }
        ArtifactKind::TaskProgress => TaskArtifact::new_task_progress(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::RunStart => TaskArtifact::new_run_start(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::RunFinish => TaskArtifact::new_run_finish(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::Evidence => TaskArtifact::new_evidence(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::Review => TaskArtifact::new_review(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams("task_id is required for review artifacts".to_string())
            })?,
        ),
        ArtifactKind::Revision => TaskArtifact::new_revision(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams("task_id is required for revision artifacts".to_string())
            })?,
        ),
        ArtifactKind::Verification => TaskArtifact::new_verification(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for verification artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::Decision => TaskArtifact::new_decision(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams("task_id is required for decision artifacts".to_string())
            })?,
        ),
        ArtifactKind::Digest => {
            let role = params.artifact_role.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "artifact_role is required for digest artifacts".to_string(),
                )
            })?;
            let digest_scope = params
                .project_id
                .clone()
                .or_else(|| task_id.clone())
                .unwrap_or_else(|| "tenant".to_string());
            let (artifact_id, synthetic_task_id, digest_key) =
                crate::task_memory::stable_digest_identity(&role, &digest_scope);
            let mut artifact =
                TaskArtifact::new_digest(tenant_id.clone(), synthetic_task_id, digest_key, role);
            artifact.artifact_id = artifact_id;
            artifact
        }
        ArtifactKind::TaskFinish => TaskArtifact::new_task_finish(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams(
                    "task_id is required for non-task_start artifacts".to_string(),
                )
            })?,
        ),
        ArtifactKind::WikiPage => TaskArtifact::new_wiki_page(
            tenant_id.clone(),
            task_id.clone().ok_or_else(|| {
                McpError::InvalidParams("task_id is required for wiki_page artifacts".to_string())
            })?,
        ),
    };

    // Phase 0 trust boundary: `content` is only allowed on `wiki_page`
    // kinds. Reject non-empty `content` on every other kind at the
    // MCP boundary so stored rows carry a consistent invariant
    // (validator elsewhere, e.g. digests.rs, can treat `content ==
    // Some(_)` as `kind == WikiPage` without needing a fallback).
    if let Some(content) = params.content.as_ref() {
        if !content.is_empty() && artifact_kind != ArtifactKind::WikiPage {
            return Err(McpError::InvalidParams(format!(
                "artifact.create: `content` is only accepted on `wiki_page` artifacts; \
                 got artifact_kind={}",
                artifact_kind.as_str()
            )));
        }
    }

    // Phase 1 WikiPage-specific validation. These checks live at the
    // MCP boundary so the stored row always honors the contract — the
    // Python compiler (Phase 2) and lint (Phase 3) can trust it
    // without re-validating.
    if artifact_kind == ArtifactKind::WikiPage {
        validate_wiki_page_params(&params)?;
    }

    let inherited_project_id = parent_artifact
        .as_ref()
        .and_then(|artifact| artifact.project_id.as_option().map(str::to_string));
    let inherited_thread_id = parent_artifact
        .as_ref()
        .map(|artifact| artifact.thread_key().to_string());
    let inherited_challenge_id = parent_artifact
        .as_ref()
        .and_then(|artifact| artifact.challenge_id.clone());

    let dataset_refs = dataset_params_to_refs(params.dataset_refs)?;
    let entity_refs = entity_params_to_refs(params.entity_refs)?;
    let contributors = contributor_params_to_refs(params.contributors)?;
    let provenance = params_to_task_provenance(params.provenance);

    artifact.tool_name = params.tool_name;
    artifact.tool_version = params.tool_version;
    artifact.command = params.command;
    artifact.parameters = params.parameters;
    artifact.inputs = params.inputs;
    artifact.outputs = params.outputs;
    artifact.metrics = params.metrics;
    artifact.why_chosen = params.why_chosen;
    artifact.goal = params.goal;
    artifact.motivation = params.motivation;
    artifact.hypothesis = params.hypothesis;
    artifact.scientific_question = params.scientific_question;
    artifact.method_summary = params.method_summary;
    artifact.summary = params.summary;
    artifact.content = params.content;
    artifact.evidence_kind = params.evidence_kind;
    artifact.supports_claim = params.supports_claim;
    artifact.blockers = params.blockers;
    artifact.what_worked = params.what_worked;
    artifact.what_failed = params.what_failed;
    artifact.validation = params.validation;
    artifact.uncertainty = params.uncertainty;
    artifact.followups = params.followups;
    artifact.expected_outputs = params.expected_outputs;
    artifact.related_artifact_ids = params.related_artifact_ids;
    artifact.confidence = params.confidence;
    artifact.requested_action = params.requested_action;
    artifact.verification_status = params.verification_status;
    artifact.compute_budget = params.compute_budget;
    artifact.cost_actual = params.cost_actual;
    artifact.data_access_level = params.data_access_level;
    artifact.policy_tags = params.policy_tags;
    artifact.allowed_tools = params.allowed_tools;
    artifact.approval_state = params.approval_state;

    let relation_kind = params.relation_kind.or_else(|| {
        if params.reply_to_artifact_id.is_some() {
            Some(match artifact_kind {
                ArtifactKind::Review => "reviews".to_string(),
                ArtifactKind::Revision => "revises".to_string(),
                ArtifactKind::Verification => "verifies".to_string(),
                _ => "reply_to".to_string(),
            })
        } else {
            None
        }
    });
    let thread_id = params
        .thread_id
        .or(inherited_thread_id)
        .or_else(|| Some(artifact.task_id.clone()));

    apply_common_artifact_fields(
        &mut artifact,
        CommonArtifactFields {
            project_id: params.project_id.or(inherited_project_id),
            parent_task_id: params.parent_task_id,
            agent_id: resolved_agent_id(params.agent_id.as_deref()),
            session_id: params.session_id,
            status: params.status,
            artifact_role: params.artifact_role,
            challenge_id: params.challenge_id.or(inherited_challenge_id),
            thread_id,
            reply_to_artifact_id: params.reply_to_artifact_id,
            relation_kind,
            dataset_refs,
            entity_refs,
            contributors,
            provenance,
        },
    );

    finalize_artifact_for_storage(&mut artifact);
    // If this artifact countersigns a prior canonical artifact written
    // by a different agent, upgrade the promotion state to Verified.
    // This is the ONLY path that produces `VerifiedRecord` trust today.
    promote_if_countersigned(store, &mut artifact).await?;
    let projections = build_task_projections(&artifact);
    // Capture scope + kind for the Phase 3.4 dirty-digest hook before
    // the artifact moves into the store.
    let tenant_for_dirty = artifact.tenant_id.clone();
    let project_for_dirty = artifact.project_id.as_option().map(str::to_string);
    let kind_for_dirty = artifact.artifact_kind;
    let validation_for_dirty: Vec<String> = artifact.validation.clone();
    let result = store
        .add_task_artifact(artifact, projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Phase 3.4: decisions, reviews, verifications invalidate the
    // decision/highlight/project_brief libraries. Evidence artifacts
    // invalidate the evidence family. Additionally (Codex follow-up):
    // any artifact with non-empty `validation` also feeds the evidence
    // library via `infer_evidence_items`, so we dirty that family too
    // even for review/decision/verification kinds when validation is
    // present. `revision` is intentionally narrower — revisions are
    // meta-edits and don't flow into the decision/evidence aggregates.
    match kind_for_dirty {
        ArtifactKind::Decision | ArtifactKind::Review | ArtifactKind::Verification => {
            mark_decision_related_digests_dirty(&tenant_for_dirty, project_for_dirty.as_deref());
        }
        ArtifactKind::Evidence => {
            mark_evidence_related_digests_dirty(&tenant_for_dirty, project_for_dirty.as_deref());
        }
        ArtifactKind::Revision => {
            // Revisions only touch the thread structure + highlight
            // ranking, not the library content directly.
            crate::task_memory::digest_dirty::mark_dirty(
                tenant_for_dirty.to_string(),
                project_for_dirty.clone(),
                crate::task_memory::DIGEST_ROLE_HIGHLIGHT_LIBRARY,
            );
            crate::task_memory::digest_dirty::mark_dirty(
                tenant_for_dirty.to_string(),
                project_for_dirty.clone(),
                crate::task_memory::DIGEST_ROLE_PROJECT_BRIEF,
            );
        }
        _ => {}
    }
    // Any artifact that carries validation flows into the evidence
    // library regardless of kind.
    if !validation_for_dirty.is_empty() && !matches!(kind_for_dirty, ArtifactKind::Evidence) {
        crate::task_memory::digest_dirty::mark_dirty(
            tenant_for_dirty.to_string(),
            project_for_dirty.clone(),
            crate::task_memory::DIGEST_ROLE_EVIDENCE_LIBRARY,
        );
    }

    format_mcp_response(&TaskArtifactResult {
        task_id: result.task_id,
        artifact_id: result.artifact_id,
        projection_chunk_ids: result.projection_chunk_ids,
    })
}

/// Handle task.get tool call.
pub async fn handle_task_get<S: Store>(
    store: &S,
    params: TaskGetParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;

    let artifacts = store
        .list_task_artifacts(&tenant_id, &params.task_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&TaskGetResult {
        task_id: params.task_id,
        artifacts,
    })
}

/// Handle artifact.get tool call.
pub async fn handle_artifact_get<S: Store>(
    store: &S,
    params: ArtifactGetParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("artifact_id", &params.artifact_id)?;

    let artifact = store
        .get_task_artifact(&tenant_id, &params.artifact_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&ArtifactGetResult { artifact })
}

/// Handle task.search tool call.
pub async fn handle_task_search<S: Store>(
    store: &S,
    params: TaskSearchParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    let filters = parse_task_search_filters(params.filters.as_ref())?;
    let mode = params.mode.unwrap_or_default();
    let scope_expansion = scope_expansion_for(&tenant_id, filters.project_id.as_deref());
    let has_filters = has_active_task_filters(&filters);
    let candidate_limit = if has_filters {
        params.k.saturating_mul(20).clamp(50, 1000)
    } else {
        params.k.saturating_mul(25).clamp(100, 1000)
    };
    let scoped_tenants =
        scoped_tenants_for_project(store, &tenant_id, filters.project_id.as_deref()).await?;

    let mut chunk_ids = if mode != QueryMode::Generic {
        candidate_chunk_ids_for_tenants_and_mode(
            store,
            &scoped_tenants,
            mode,
            &filters,
            candidate_limit,
        )
        .await?
    } else {
        Vec::new()
    };
    let base_chunk_ids = search_task_projection_chunk_ids_for_tenants(
        store,
        &scoped_tenants,
        &filters,
        candidate_limit,
    )
    .await?;
    let mut seen = chunk_ids.iter().cloned().collect::<HashSet<_>>();
    for chunk_id in base_chunk_ids {
        if seen.insert(chunk_id.clone()) {
            chunk_ids.push(chunk_id);
        }
    }
    let mut ranked_lists = Vec::with_capacity(scoped_tenants.len());
    for tenant in &scoped_tenants {
        ranked_lists.push(
            store
                .rerank_chunks_for_query(tenant, &params.query, &chunk_ids, params.k)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
    }
    let ranked = merge_scored_chunk_lists(ranked_lists, params.k);
    let artifacts = resolve_artifacts_for_ranked_chunks(store, &ranked).await?;
    let mut results = ranked
        .iter()
        .map(|(chunk, score)| {
            chunk_to_result(
                chunk,
                *score,
                None,
                artifacts.get(&chunk.chunk_id.to_string()).cloned(),
            )
        })
        .collect::<Vec<_>>();
    annotate_chunk_origins(&mut results, &tenant_id, scope_expansion.as_ref());

    format_mcp_response(&SearchResult {
        results,
        retrieval_episode_id: None,
        ranking_policy: None,
        budget_info: None,
        scope_expansion,
        tier_info: None,
        repair_info: None,
        scope_status: None,
    })
}

async fn search_artifacts_internal<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    query: &str,
    k: usize,
    filters: &TaskSearchFilters,
    mode: QueryMode,
) -> Result<Vec<ArtifactSearchHit>, McpError> {
    let has_filters = has_active_task_filters(filters);
    let candidate_limit = if has_filters {
        k.saturating_mul(20).clamp(50, 1000)
    } else {
        k.saturating_mul(25).clamp(100, 1000)
    };
    let scoped_tenants =
        scoped_tenants_for_project(store, tenant_id, filters.project_id.as_deref()).await?;

    let mut chunk_ids = if mode != QueryMode::Generic {
        candidate_chunk_ids_for_tenants_and_mode(
            store,
            &scoped_tenants,
            mode,
            filters,
            candidate_limit,
        )
        .await?
    } else {
        Vec::new()
    };
    let base_chunk_ids = search_task_projection_chunk_ids_for_tenants(
        store,
        &scoped_tenants,
        filters,
        candidate_limit,
    )
    .await?;
    let mut seen = chunk_ids.iter().cloned().collect::<HashSet<_>>();
    for chunk_id in base_chunk_ids {
        if seen.insert(chunk_id.clone()) {
            chunk_ids.push(chunk_id);
        }
    }
    let mut ranked_lists = Vec::with_capacity(scoped_tenants.len());
    for tenant in &scoped_tenants {
        ranked_lists.push(
            store
                .rerank_chunks_for_query(tenant, query, &chunk_ids, candidate_limit)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?,
        );
    }
    let ranked = merge_scored_chunk_lists(ranked_lists, candidate_limit);
    let artifacts = resolve_artifacts_for_ranked_chunks(store, &ranked).await?;

    let mut seen = HashSet::new();
    let mut results = Vec::new();
    for (chunk, score) in ranked {
        let Some(artifact) = artifacts.get(&chunk.chunk_id.to_string()).cloned() else {
            continue;
        };
        if !seen.insert(artifact.artifact_id.clone()) {
            continue;
        }
        results.push(build_artifact_search_hit(artifact, score, Some(&chunk)));
        if results.len() >= k {
            break;
        }
    }

    Ok(results)
}

/// Handle artifact.search tool call.
pub async fn handle_artifact_search<S: Store>(
    store: &S,
    params: TaskSearchParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    let filters = parse_task_search_filters(params.filters.as_ref())?;
    let mode = params.mode.unwrap_or_default();
    let scope_expansion = scope_expansion_for(&tenant_id, filters.project_id.as_deref());
    let mut results =
        search_artifacts_internal(store, &tenant_id, &params.query, params.k, &filters, mode)
            .await?;
    annotate_artifact_origins(&mut results, &tenant_id, scope_expansion.as_ref());

    let (results, budget_info) = shape_artifact_results(results, &params);
    format_mcp_response(&ArtifactSearchResult {
        results,
        budget_info,
        scope_expansion,
    })
}

/// Handle artifact.list_thread tool call.
pub async fn handle_artifact_list_thread<S: Store>(
    store: &S,
    params: ArtifactListThreadParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    let thread_id = match (params.thread_id, params.artifact_id) {
        (Some(thread_id), _) => {
            validate_identifier("thread_id", &thread_id)?;
            thread_id
        }
        (None, Some(artifact_id)) => {
            validate_identifier("artifact_id", &artifact_id)?;
            let artifact = store
                .get_task_artifact(&tenant_id, &artifact_id)
                .await
                .map_err(|e| McpError::ToolError(e.to_string()))?
                .ok_or_else(|| McpError::ToolError("artifact not found".to_string()))?;
            artifact.thread_key().to_string()
        }
        (None, None) => {
            return Err(McpError::InvalidParams(
                "artifact.list_thread requires thread_id or artifact_id".to_string(),
            ));
        }
    };

    let artifacts = store
        .list_thread_artifacts(&tenant_id, &thread_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&ArtifactThreadResult {
        thread_id,
        artifacts,
    })
}

fn dedupe_grounding_refs(refs: impl IntoIterator<Item = GroundingRef>) -> Vec<GroundingRef> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for reference in refs {
        if seen.insert(reference.artifact_id.clone()) {
            out.push(reference);
        }
    }
    out
}

fn dedupe_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn grounding_status_label(status: GroundingStatus) -> &'static str {
    match status {
        GroundingStatus::VerifiedRecord => "verified_record",
        GroundingStatus::CanonicallyGrounded => "canonically_grounded",
        GroundingStatus::DigestOnly => "digest_only",
        GroundingStatus::InsufficientGrounding => "insufficient_grounding",
        GroundingStatus::Conflicted => "conflicted",
    }
}

fn grounding_confidence(
    status: GroundingStatus,
    support_count: usize,
    conflict_count: usize,
) -> f32 {
    let support_boost = (support_count.min(3) as f32) * 0.03;
    match status {
        GroundingStatus::VerifiedRecord => (0.92 + support_boost).min(0.99),
        GroundingStatus::CanonicallyGrounded => (0.82 + support_boost).min(0.94),
        GroundingStatus::DigestOnly => 0.45,
        GroundingStatus::InsufficientGrounding => 0.12,
        GroundingStatus::Conflicted => {
            let penalty = (conflict_count.min(3) as f32) * 0.04;
            (0.38 - penalty).max(0.18)
        }
    }
}

async fn digest_wrapper_metadata<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    artifact: &TaskArtifact,
) -> Result<(TrustTier, Vec<GroundingRef>, VerificationHint), McpError> {
    let trust_tier = derive_artifact_trust_tier(artifact);
    let mut grounding_refs = resolve_grounding_refs_by_artifact_ids(
        store,
        tenant_id,
        artifact.project_id.as_option(),
        &artifact.related_artifact_ids,
        12,
    )
    .await?;
    if grounding_refs.is_empty() {
        grounding_refs.push(build_grounding_ref(artifact, None));
    }
    let verification_hint = verification_hint_for_trust_tier(trust_tier);
    Ok((trust_tier, grounding_refs, verification_hint))
}

fn artifact_matches_conflict_scope(
    artifact: &TaskArtifact,
    project_id: Option<&str>,
    task_id: Option<&str>,
    thread_id: Option<&str>,
    support_task_ids: &HashSet<String>,
    support_thread_ids: &HashSet<String>,
) -> bool {
    if let Some(project_id) = project_id {
        if artifact.project_id.as_option() != Some(project_id) {
            return false;
        }
    }
    if let Some(task_id) = task_id {
        return artifact.task_id == task_id;
    }
    if let Some(thread_id) = thread_id {
        return artifact.thread_key() == thread_id;
    }

    support_task_ids.contains(&artifact.task_id)
        || support_thread_ids.contains(artifact.thread_key())
}

struct VerificationEvidence<'a> {
    grounding_status: GroundingStatus,
    confidence: f32,
    supporting_artifacts: &'a [GroundingRef],
    conflicting_artifacts: &'a [GroundingRef],
    consulted_digests: &'a [GroundingRef],
    notes: &'a [String],
}

async fn persist_verification_artifact<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    params: &ArtifactVerifyParams,
    evidence: VerificationEvidence<'_>,
) -> Result<TaskArtifact, McpError> {
    let task_id = params
        .record_task_id
        .clone()
        .or_else(|| params.task_id.clone())
        .or_else(|| {
            evidence
                .supporting_artifacts
                .first()
                .map(|reference| reference.task_id.clone())
        })
        .or_else(|| {
            evidence
                .conflicting_artifacts
                .first()
                .map(|reference| reference.task_id.clone())
        })
        .ok_or_else(|| {
            McpError::InvalidParams(
                "create_artifact=true requires record_task_id, task_id, or canonically grounded artifacts".to_string(),
            )
        })?;

    let mut artifact = TaskArtifact::new_verification(tenant_id.clone(), task_id.clone());
    artifact.project_id = ProjectId::from(params.project_id.clone());
    // Attribute the verification record to the caller's agent_id when
    // supplied. Without this the artifact is anonymous, and the
    // countersignature promotion in `promote_if_countersigned` cannot
    // elevate it to `VerifiedRecord` — a deliberate safeguard that
    // keeps self-attributed "verifications" from laundering trust.
    artifact.agent_id = resolved_agent_id(params.agent_id.as_deref());
    artifact.artifact_role = Some("claim_grounding".to_string());
    artifact.summary = Some(format!(
        "Claim grounding status: {}. Claim: {}",
        grounding_status_label(evidence.grounding_status),
        params.claim
    ));
    artifact.validation = dedupe_strings(
        evidence
            .supporting_artifacts
            .iter()
            .map(|reference| format!("Supporting artifact: {}", reference.artifact_id))
            .chain(evidence.notes.iter().cloned()),
    );
    artifact.what_failed = dedupe_strings(
        evidence
            .conflicting_artifacts
            .iter()
            .map(|reference| format!("Conflicting artifact: {}", reference.artifact_id))
            .chain(match evidence.grounding_status {
                GroundingStatus::DigestOnly => Some(
                    "Only digest artifacts were found; no canonical artifact directly grounded the claim.".to_string(),
                ),
                GroundingStatus::InsufficientGrounding => Some(
                    "No canonical artifact directly grounded the claim.".to_string(),
                ),
                _ => None,
            }),
    );
    artifact.related_artifact_ids = dedupe_strings(
        evidence
            .supporting_artifacts
            .iter()
            .map(|reference| reference.artifact_id.clone())
            .chain(
                evidence
                    .conflicting_artifacts
                    .iter()
                    .map(|reference| reference.artifact_id.clone()),
            )
            .chain(
                evidence
                    .consulted_digests
                    .iter()
                    .map(|reference| reference.artifact_id.clone()),
            ),
    );
    artifact.confidence = Some(evidence.confidence);
    artifact.verification_status =
        Some(grounding_status_label(evidence.grounding_status).to_string());
    artifact.thread_id = params
        .thread_id
        .clone()
        .or_else(|| {
            evidence
                .supporting_artifacts
                .first()
                .map(|reference| reference.thread_id.clone())
        })
        .or_else(|| {
            evidence
                .conflicting_artifacts
                .first()
                .map(|reference| reference.thread_id.clone())
        })
        .or_else(|| Some(task_id.clone()));

    finalize_artifact_for_storage(&mut artifact);
    // Match handle_artifact_create: the verification record can only be
    // treated as a VerifiedRecord after a distinct-writer countersignature
    // check; otherwise it stays at `Canonical`.
    promote_if_countersigned(store, &mut artifact).await?;
    let projections = build_task_projections(&artifact);
    store
        .add_task_artifact(artifact.clone(), projections)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    Ok(artifact)
}

/// Handle artifact.verify tool call.
pub async fn handle_artifact_verify<S: Store>(
    store: &S,
    params: ArtifactVerifyParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if params.claim.trim().is_empty() {
        return Err(McpError::InvalidParams(
            "claim must not be empty".to_string(),
        ));
    }
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }
    if let Some(task_id) = params.task_id.as_deref() {
        validate_identifier("task_id", task_id)?;
    }
    if let Some(thread_id) = params.thread_id.as_deref() {
        validate_identifier("thread_id", thread_id)?;
    }
    if let Some(record_task_id) = params.record_task_id.as_deref() {
        validate_identifier("record_task_id", record_task_id)?;
    }
    for artifact_id in &params.candidate_artifact_ids {
        validate_identifier("candidate_artifact_ids", artifact_id)?;
    }

    let lookup_tenants =
        artifact_lookup_tenants(store, &tenant_id, params.project_id.as_deref()).await?;
    let mut seen_artifacts = HashSet::new();
    let mut explicit_digest_candidate = false;
    let candidate_hits = if params.candidate_artifact_ids.is_empty() {
        let filters = TaskSearchFilters {
            project_id: params.project_id.clone(),
            task_id: params.task_id.clone(),
            thread_id: params.thread_id.clone(),
            ..Default::default()
        };
        search_artifacts_internal(
            store,
            &tenant_id,
            &params.claim,
            params.k,
            &filters,
            QueryMode::Generic,
        )
        .await?
    } else {
        let mut hits = Vec::new();
        for artifact_id in &params.candidate_artifact_ids {
            let Some(artifact) =
                get_artifact_by_id_in_scope(store, &lookup_tenants, artifact_id).await?
            else {
                continue;
            };
            if !seen_artifacts.insert(artifact.artifact_id.clone()) {
                continue;
            }
            if derive_artifact_trust_tier(&artifact) == TrustTier::CompiledDigestHint {
                explicit_digest_candidate = true;
            }
            hits.push(build_artifact_search_hit(
                artifact.clone(),
                artifact_claim_score(&artifact, &params.claim),
                None,
            ));
        }
        hits
    };

    let mut canonical_hits = Vec::new();
    let mut digest_hits = Vec::new();
    for hit in candidate_hits {
        if derive_artifact_trust_tier(artifact_hit_record(&hit)) == TrustTier::CompiledDigestHint {
            digest_hits.push(hit);
        } else {
            canonical_hits.push(hit);
        }
    }

    let mut notes = Vec::new();
    if canonical_hits.is_empty() && !digest_hits.is_empty() {
        let expanded_ids = digest_hits
            .iter()
            .flat_map(|hit| {
                artifact_hit_record(hit)
                    .related_artifact_ids
                    .iter()
                    .cloned()
            })
            .collect::<Vec<_>>();
        let expanded_refs = resolve_grounding_refs_by_artifact_ids(
            store,
            &tenant_id,
            params.project_id.as_deref(),
            &expanded_ids,
            params.k.saturating_mul(2),
        )
        .await?;
        if !expanded_refs.is_empty() {
            notes.push(format!(
                "Expanded {} canonical artifact references from digest candidates.",
                expanded_refs.len()
            ));
        }
        for reference in expanded_refs {
            let Some(artifact) =
                get_artifact_by_id_in_scope(store, &lookup_tenants, &reference.artifact_id).await?
            else {
                continue;
            };
            if seen_artifacts.insert(artifact.artifact_id.clone()) {
                canonical_hits.push(build_artifact_search_hit(
                    artifact.clone(),
                    artifact_claim_score(&artifact, &params.claim),
                    None,
                ));
            }
        }
    }

    let supporting_hits = canonical_hits
        .iter()
        .filter(|hit| artifact_supports_claim(artifact_hit_record(hit), &params.claim, hit.score))
        .collect::<Vec<_>>();
    let support_task_ids = supporting_hits
        .iter()
        .map(|hit| artifact_hit_record(hit).task_id.clone())
        .collect::<HashSet<_>>();
    let support_thread_ids = supporting_hits
        .iter()
        .map(|hit| artifact_hit_record(hit).thread_key().to_string())
        .collect::<HashSet<_>>();

    let conflicting_hits = if supporting_hits.is_empty() {
        Vec::new()
    } else {
        canonical_hits
            .iter()
            .filter(|hit| artifact_has_negative_marker(artifact_hit_record(hit)))
            .filter(|hit| {
                artifact_matches_conflict_scope(
                    artifact_hit_record(hit),
                    params.project_id.as_deref(),
                    params.task_id.as_deref(),
                    params.thread_id.as_deref(),
                    &support_task_ids,
                    &support_thread_ids,
                )
            })
            .collect::<Vec<_>>()
    };

    let supporting_artifacts = dedupe_grounding_refs(
        supporting_hits
            .iter()
            .flat_map(|hit| hit.grounding_refs.clone()),
    );
    let conflicting_artifacts = dedupe_grounding_refs(
        conflicting_hits
            .iter()
            .flat_map(|hit| hit.grounding_refs.clone()),
    );
    let consulted_digests =
        if params.include_digests || supporting_artifacts.is_empty() || explicit_digest_candidate {
            dedupe_grounding_refs(
                digest_hits
                    .iter()
                    .flat_map(|hit| hit.grounding_refs.clone()),
            )
        } else {
            Vec::new()
        };

    if !digest_hits.is_empty() {
        notes.push(
            "Digest artifacts were consulted as compiled hints and not counted as primary evidence."
                .to_string(),
        );
    }
    if !conflicting_artifacts.is_empty() {
        notes.push(
            "Conflict detection is intentionally narrow in v1 and only uses explicit same-scope negative markers."
                .to_string(),
        );
    }

    let grounding_status = if !supporting_artifacts.is_empty() && !conflicting_artifacts.is_empty()
    {
        GroundingStatus::Conflicted
    } else if !supporting_artifacts.is_empty() {
        if supporting_hits.iter().any(|hit| {
            derive_artifact_trust_tier(artifact_hit_record(hit)) == TrustTier::VerifiedRecord
        }) {
            GroundingStatus::VerifiedRecord
        } else {
            GroundingStatus::CanonicallyGrounded
        }
    } else if !consulted_digests.is_empty() {
        GroundingStatus::DigestOnly
    } else {
        GroundingStatus::InsufficientGrounding
    };
    let confidence = grounding_confidence(
        grounding_status,
        supporting_artifacts.len(),
        conflicting_artifacts.len(),
    );
    let verification_artifact = if params.create_artifact {
        Some(
            persist_verification_artifact(
                store,
                &tenant_id,
                &params,
                VerificationEvidence {
                    grounding_status,
                    confidence,
                    supporting_artifacts: &supporting_artifacts,
                    conflicting_artifacts: &conflicting_artifacts,
                    consulted_digests: &consulted_digests,
                    notes: &notes,
                },
            )
            .await?,
        )
    } else {
        None
    };

    format_mcp_response(&ArtifactVerifyResult {
        claim: params.claim,
        grounding_status,
        confidence,
        supporting_artifacts,
        conflicting_artifacts,
        consulted_digests,
        notes,
        verification_artifact,
    })
}

pub async fn handle_context_brief_project<S: Store>(
    store: &S,
    params: ProjectBriefParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("project_id", &params.project_id)?;
    validate_search_k(params.k)?;

    let (artifact, mut brief) = ensure_project_brief_digest(
        store,
        &tenant_id,
        &params.project_id,
        params.include_related_projects,
    )
    .await?;

    if !params.query.trim().is_empty() {
        sort_ranked_items(&mut brief.recent_failures, &params.query, |item| {
            (item.summary.clone(), item.timestamp_created, false)
        });
        sort_ranked_items(&mut brief.recent_decisions, &params.query, |item| {
            (item.summary.clone(), item.timestamp_created, item.explicit)
        });
        sort_ranked_items(&mut brief.evidence_highlights, &params.query, |item| {
            (item.summary.clone(), item.timestamp_created, false)
        });
        brief.recent_failures.truncate(params.k.min(10));
        brief.recent_decisions.truncate(params.k.min(10));
        brief.evidence_highlights.truncate(params.k.min(10));
    }
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&ProjectBriefResult {
        artifact,
        brief,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_task_resume<S: Store>(
    store: &S,
    params: TaskResumeParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_identifier("task_id", &params.task_id)?;
    validate_search_k(params.k)?;

    let (artifact, mut resume) =
        ensure_task_resume_digest(store, &tenant_id, &params.task_id).await?;

    if !params.query.trim().is_empty() {
        sort_ranked_items(&mut resume.recent_runs, &params.query, |item| {
            (
                format!(
                    "{} {} {}",
                    item.tool_name.clone().unwrap_or_default(),
                    item.command.clone().unwrap_or_default(),
                    item.status.clone().unwrap_or_default()
                ),
                item.timestamp_created,
                false,
            )
        });
        resume.recent_runs.truncate(params.k.min(5));
    }
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&TaskResumeResult {
        artifact,
        resume,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_artifact_find_failures<S: Store>(
    store: &S,
    params: ArtifactLibraryParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }

    let (artifact, mut results) =
        ensure_failure_library_digest(store, &tenant_id, params.project_id.as_deref()).await?;
    sort_ranked_items(&mut results, &params.query, |item| {
        (item.summary.clone(), item.timestamp_created, false)
    });
    results.truncate(params.k);
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&FailureSearchResult {
        artifact,
        results,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_artifact_find_decisions<S: Store>(
    store: &S,
    params: ArtifactLibraryParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }

    let (artifact, mut results) =
        ensure_decision_library_digest(store, &tenant_id, params.project_id.as_deref()).await?;
    sort_ranked_items(&mut results, &params.query, |item| {
        (item.summary.clone(), item.timestamp_created, item.explicit)
    });
    results.truncate(params.k);
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&DecisionSearchViewResult {
        artifact,
        results,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_artifact_find_evidence<S: Store>(
    store: &S,
    params: ArtifactLibraryParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }

    let (artifact, mut results) =
        ensure_evidence_library_digest(store, &tenant_id, params.project_id.as_deref()).await?;
    sort_ranked_items(&mut results, &params.query, |item| {
        (item.summary.clone(), item.timestamp_created, false)
    });
    results.truncate(params.k);
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&EvidenceSearchViewResult {
        artifact,
        results,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}

pub async fn handle_artifact_find_highlights<S: Store>(
    store: &S,
    params: ArtifactLibraryParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_search_k(params.k)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }

    let (artifact, mut results) =
        ensure_highlight_library_digest(store, &tenant_id, params.project_id.as_deref()).await?;
    sort_highlight_items(&mut results, &params.query);
    results.truncate(params.k);
    let (trust_tier, grounding_refs, verification_hint) =
        digest_wrapper_metadata(store, &tenant_id, &artifact).await?;

    format_mcp_response(&HighlightSearchViewResult {
        artifact,
        results,
        trust_tier,
        grounding_refs,
        verification_hint,
    })
}
