use super::*;

/// Handle memory.stats tool call.
pub async fn handle_memory_stats<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: StatsParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(tenant_id = %tenant_id, "memory.stats");

    let store_stats: StoreStats = store
        .stats(&tenant_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    // Get disk stats if tenant_manager is available
    let disk_stats = tenant_manager.and_then(|tm| {
        tm.tenant_disk_stats(&tenant_id)
            .ok()
            .map(|ds| DiskStatsResult {
                total_bytes: ds.total_bytes,
                segment_count: ds.segment_count,
            })
    });

    // Get compaction metrics if available
    let compaction = store
        .get_compaction_metrics(&tenant_id)
        .ok()
        .map(|m| CompactionStatsResult {
            tombstone_ratio: m.tombstone_ratio,
            active_chunks: m.active_chunks,
            deleted_chunks: m.deleted_chunks,
            segment_count: m.segment_count,
            hnsw_staleness: m.hnsw_staleness,
            hnsw_cache_size: m.hnsw_cache_size,
            hnsw_index_size: m.hnsw_index_size,
            needs_compaction: m.tombstone_ratio > 0.20
                || m.segment_count > 10
                || m.hnsw_staleness > 0.15,
        });

    format_mcp_response(&StatsResult {
        total_chunks: store_stats.total_chunks,
        active_chunks: store_stats.active_chunks,
        candidate_chunks: store_stats.candidate_chunks,
        deleted_chunks: store_stats.deleted_chunks,
        chunk_types: store_stats.chunk_types,
        chunk_types_active: store_stats.chunk_types_active,
        chunk_types_deleted: store_stats.chunk_types_deleted,
        chunk_types_all: store_stats.chunk_types_all,
        disk_stats,
        compaction,
    })
}

fn percentile_u64(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (p * (sorted.len() - 1) / 100).min(sorted.len() - 1);
    sorted[idx]
}

fn build_latency_health(metrics: &MetricsCollector, include_recent: bool) -> LatencyHealthResult {
    if !include_recent {
        return LatencyHealthResult::default();
    }
    let mut totals = metrics
        .get_recent_queries(1000)
        .into_iter()
        .map(|query| query.total_ms)
        .collect::<Vec<_>>();
    totals.sort_unstable();
    LatencyHealthResult {
        recent_search_count: totals.len(),
        p50_total_ms: percentile_u64(&totals, 50),
        p95_total_ms: percentile_u64(&totals, 95),
        p99_total_ms: percentile_u64(&totals, 99),
    }
}

fn warnings_for_health(
    snapshot: &StoreHealthSnapshot,
    latency: &LatencyHealthResult,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if snapshot.counts.total_chunks == 0 {
        warnings.push("no chunks found for requested scope".to_string());
    }
    if snapshot.duplicates.duplicate_row_ratio > 0.20 {
        warnings.push(format!(
            "high duplicate row ratio ({:.1}%)",
            snapshot.duplicates.duplicate_row_ratio * 100.0
        ));
    }
    if snapshot.index_coverage.indexed_percentage > 0.0
        && snapshot.index_coverage.indexed_percentage < 95.0
    {
        warnings.push(format!(
            "low index coverage ({:.1}%)",
            snapshot.index_coverage.indexed_percentage
        ));
    }
    if snapshot.payload.p95_canonical_text_bytes > 8_000 {
        warnings.push(format!(
            "high p95 canonical text payload ({} bytes)",
            snapshot.payload.p95_canonical_text_bytes
        ));
    }
    if latency.p95_total_ms > 5_000 {
        warnings.push(format!(
            "high recent p95 search latency ({} ms)",
            latency.p95_total_ms
        ));
    }
    warnings
}

async fn fallback_health_snapshot<S: Store>(
    store: &S,
    tenant_id: &TenantId,
) -> Result<StoreHealthSnapshot, McpError> {
    let stats = store
        .stats(tenant_id)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    Ok(StoreHealthSnapshot {
        counts: HealthCounts {
            active_chunks: stats.active_chunks,
            deleted_chunks: stats.deleted_chunks,
            total_chunks: stats.total_chunks,
            ..Default::default()
        },
        chunk_types_active: stats.chunk_types_active,
        chunk_types_all: stats.chunk_types_all,
        ..Default::default()
    })
}

async fn scoped_health_snapshot<S: Store>(
    store: &S,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    duplicate_limit: usize,
) -> Result<StoreHealthSnapshot, McpError> {
    match store
        .health_snapshot(tenant_id, project_id, duplicate_limit)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        Some(snapshot) => Ok(snapshot),
        None => fallback_health_snapshot(store, tenant_id).await,
    }
}

fn parse_dream_digest_modes(raw: Option<&[String]>) -> Result<Vec<QueryMode>, McpError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let mut modes = Vec::with_capacity(raw.len());
    for mode in raw {
        let parsed = match mode.trim() {
            "generic" => QueryMode::Generic,
            "brief_project" => QueryMode::BriefProject,
            "resume_task" => QueryMode::ResumeTask,
            "find_failures" => QueryMode::FindFailures,
            "find_decisions" => QueryMode::FindDecisions,
            "find_evidence" => QueryMode::FindEvidence,
            "find_highlights" => QueryMode::FindHighlights,
            other => {
                return Err(McpError::InvalidParams(format!(
                    "invalid digest mode '{}'",
                    other
                )));
            }
        };
        modes.push(parsed);
    }
    Ok(modes)
}

fn build_dream_report_artifact(
    tenant_id: &TenantId,
    project_id: Option<&str>,
    report: &DreamReport,
    related_artifact_ids: Vec<String>,
    now_ms: i64,
) -> TaskArtifact {
    let scope_key = build_scope_key(project_id, tenant_id, DIGEST_ROLE_DREAM_REPORT);
    let (artifact_id, task_id, digest_key) =
        stable_digest_identity(DIGEST_ROLE_DREAM_REPORT, &scope_key);
    let mut artifact = TaskArtifact::new_digest(
        tenant_id.clone(),
        task_id,
        digest_key,
        DIGEST_ROLE_DREAM_REPORT,
    );
    artifact.artifact_id = artifact_id;
    artifact.project_id = ProjectId::from(project_id.map(str::to_string));
    artifact.summary = Some(format!(
        "Dream maintenance report for {} retired {} duplicate projections; duplicate row ratio {:.4} -> {:.4}.",
        project_id.unwrap_or(tenant_id.as_str()),
        report.applied_actions.len(),
        report.before.health.duplicates.duplicate_row_ratio,
        report.after.health.duplicates.duplicate_row_ratio
    ));
    artifact.method_summary = Some(
        "Deterministic maintenance report over metadata lifecycle, digest projection duplicates, and physical compaction results.".to_string(),
    );
    artifact.related_artifact_ids = related_artifact_ids;
    artifact.metrics = Some(json!({
        "planned_actions": report.planned_actions.len(),
        "applied_actions": report.applied_actions.len(),
        "before_duplicate_row_ratio": report.before.health.duplicates.duplicate_row_ratio,
        "after_duplicate_row_ratio": report.after.health.duplicates.duplicate_row_ratio,
        "estimated_hidden_payload_bytes": report.reclaimed.estimated_hidden_payload_bytes,
        "metadata_bytes_reclaimed": report.reclaimed.metadata_bytes,
        "sparse_index_bytes_reclaimed": report.reclaimed.sparse_index_bytes,
        "tenant_bytes_reclaimed": report.reclaimed.tenant_bytes
    }));
    artifact.validation = report.verification.clone();
    artifact.outputs = vec![serde_json::to_string(report).unwrap_or_default()];
    artifact.source_updated_at_ms = Some(now_ms);
    artifact
}

/// Handle memory.dream tool call.
pub async fn handle_memory_dream<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: DreamParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }
    let Some(persistent) = store.as_persistent() else {
        return Err(McpError::ToolError(
            "memory.dream requires a persistent store".to_string(),
        ));
    };

    let digest_modes = parse_dream_digest_modes(params.digest_modes.as_deref())?;
    let before_health =
        scoped_health_snapshot(store, &tenant_id, params.project_id.as_deref(), 10).await?;
    let before_disk = disk_snapshot(tenant_manager, &tenant_id);
    let mut warnings = Vec::new();
    if let Some(warning) = unsupported_exact_safe_warning(params.duplicate_strategy) {
        warnings.push(warning);
    }

    let mut planned_actions = match params.duplicate_strategy {
        crate::maintenance::DuplicateStrategy::None => Vec::new(),
        crate::maintenance::DuplicateStrategy::DigestProjections
        | crate::maintenance::DuplicateStrategy::ExactSafe => {
            plan_duplicate_projection_retirements(
                store,
                persistent,
                &tenant_id,
                params.project_id.as_deref(),
                params.max_actions,
            )
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
        }
    };

    let rewrite_blocked = params.physical.rewrite_segments;
    if rewrite_blocked {
        planned_actions.push(DreamAction::rewrite_segments_unsupported());
        warnings.push(
            "rewrite_segments is not supported until the shadow-copy segment rewrite phase lands"
                .to_string(),
        );
    }

    let now_ms = current_time_ms();
    let mut applied_actions = Vec::new();
    let mut physical = PhysicalCompactionResult::default();
    let mut digest_artifacts = Vec::new();

    if !params.dry_run && !rewrite_blocked {
        applied_actions = apply_lifecycle_actions(persistent, &tenant_id, &planned_actions, now_ms)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;

        if params.physical.prune_sparse_index && !applied_actions.is_empty() {
            match prune_sparse_index_for_actions(persistent, &tenant_id, &applied_actions) {
                Ok(count) => physical.sparse_pruned_chunks = count,
                Err(err) => warnings.push(format!("sparse index prune failed: {}", err)),
            }
        }

        if params.physical.run_store_compaction {
            match persistent.run_compaction(&tenant_id) {
                Ok(_) => physical.store_compaction_ran = true,
                Err(err) => warnings.push(format!("store compaction skipped: {}", err)),
            }
        }

        if params.physical.vacuum_metadata {
            let before_pages = persistent.metadata().page_count_snapshot().ok();
            if let Err(err) = persistent.metadata().checkpoint_wal() {
                warnings.push(format!("metadata WAL checkpoint failed: {}", err));
            }
            match persistent.metadata().vacuum() {
                Ok(()) => {
                    physical.metadata_vacuum_ran = true;
                    if let (Some((before_page_count, before_freelist)), Ok((after_page_count, _))) =
                        (before_pages, persistent.metadata().page_count_snapshot())
                    {
                        if after_page_count >= before_page_count && before_freelist > 0 {
                            warnings.push(
                                "metadata VACUUM completed but did not reduce page count"
                                    .to_string(),
                            );
                        }
                    }
                }
                Err(err) => warnings.push(format!("metadata VACUUM failed: {}", err)),
            }
        }

        if !applied_actions.is_empty() || !digest_modes.is_empty() {
            digest_artifacts = rebuild_requested_digests(
                store,
                &tenant_id,
                params.project_id.as_deref(),
                &digest_modes,
            )
            .await?;
        }
    }

    let after_health =
        scoped_health_snapshot(store, &tenant_id, params.project_id.as_deref(), 10).await?;
    let after_disk = disk_snapshot(tenant_manager, &tenant_id);
    let before = DreamStateSnapshot {
        health: before_health,
        disk: before_disk.clone(),
    };
    let after = DreamStateSnapshot {
        health: after_health,
        disk: after_disk.clone(),
    };
    let mut reclaimed = build_reclaimed(&before_disk, &after_disk, &applied_actions);
    if params.dry_run {
        reclaimed.estimated_hidden_payload_bytes = estimated_hidden_payload_bytes(&planned_actions);
    }
    let mut verification = Vec::new();
    if params.dry_run {
        verification.push("dry-run performed no lifecycle or disk mutations".to_string());
    }
    if !params.dry_run && !rewrite_blocked {
        verification.push(format!(
            "applied {} lifecycle retirements",
            applied_actions.len()
        ));
        verification.push(format!(
            "duplicate row ratio {:.4} -> {:.4}",
            before.health.duplicates.duplicate_row_ratio,
            after.health.duplicates.duplicate_row_ratio
        ));
    }
    if params.physical.rewrite_segments {
        verification.push("segment rewrite was blocked before mutation".to_string());
    }
    if !params.dry_run && applied_actions.is_empty() && !rewrite_blocked {
        warnings.push("no duplicate digest projections were eligible for retirement".to_string());
    }
    if !params.dry_run
        && reclaimed.metadata_bytes == 0
        && reclaimed.sparse_index_bytes == 0
        && reclaimed.tenant_bytes == 0
        && reclaimed.estimated_hidden_payload_bytes > 0
    {
        warnings.push(
            "safe profile hid duplicate payloads but did not reclaim append-only segment bytes"
                .to_string(),
        );
    }

    let mut report = DreamReport {
        status: status_for_report(rewrite_blocked, params.dry_run),
        scope: DreamScope {
            tenant_id: tenant_id.to_string(),
            project_id: params.project_id.clone(),
        },
        policy: DreamPolicy::from_params(&params),
        before,
        planned_actions,
        applied_actions,
        after,
        summary_artifacts: digest_artifacts,
        archive_artifacts: Vec::new(),
        physical,
        reclaimed,
        warnings,
        verification,
    };

    if !params.dry_run && !rewrite_blocked && !report.applied_actions.is_empty() {
        let project_artifacts =
            load_project_artifacts(store, &tenant_id, params.project_id.as_deref(), 500).await?;
        let mut related_ids = related_artifact_ids_from_actions(&report.applied_actions);
        related_ids.extend(related_artifact_ids_from_project_artifacts(
            &project_artifacts,
        ));
        related_ids.sort();
        related_ids.dedup();
        let report_artifact_id = build_dream_report_artifact(
            &tenant_id,
            params.project_id.as_deref(),
            &report,
            related_ids.clone(),
            now_ms,
        )
        .artifact_id;
        report.summary_artifacts.push(report_artifact_id);
        let report_artifact = build_dream_report_artifact(
            &tenant_id,
            params.project_id.as_deref(),
            &report,
            related_ids,
            now_ms,
        );
        let persisted = persist_digest_artifact(store, report_artifact).await?;
        if !report.summary_artifacts.contains(&persisted.artifact_id) {
            report.summary_artifacts.push(persisted.artifact_id);
        }
        report.summary_artifacts.sort();
        report.summary_artifacts.dedup();
    }

    format_mcp_response(&report)
}

/// Handle memory.health tool call
pub async fn handle_memory_health<S: Store>(
    store: &S,
    metrics: &MetricsCollector,
    params: HealthParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    if let Some(project_id) = params.project_id.as_deref() {
        validate_identifier("project_id", project_id)?;
    }
    let duplicate_limit = if params.include_examples {
        params.duplicate_limit.min(100)
    } else {
        0
    };
    let snapshot = match store
        .health_snapshot(&tenant_id, params.project_id.as_deref(), duplicate_limit)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?
    {
        Some(snapshot) => snapshot,
        None => fallback_health_snapshot(store, &tenant_id).await?,
    };
    let latency = build_latency_health(metrics, params.include_recent);
    let warnings = warnings_for_health(&snapshot, &latency);
    let aliases = params
        .project_id
        .as_deref()
        .map(|project_id| configured_project_aliases(&tenant_id, project_id))
        .unwrap_or_default();

    format_mcp_response(&MemoryHealthResult {
        scope: HealthScopeResult {
            tenant_id: tenant_id.to_string(),
            project_id: params.project_id,
            aliases,
        },
        counts: snapshot.counts,
        chunk_types: ChunkTypeHealthResult {
            active: snapshot.chunk_types_active,
            all: snapshot.chunk_types_all,
        },
        duplicates: snapshot.duplicates,
        index_coverage: snapshot.index_coverage,
        payload: snapshot.payload,
        latency,
        warnings,
    })
}

/// Handle memory.metrics tool call
pub fn handle_memory_metrics(
    metrics: &MetricsCollector,
    index_stats: HashMap<String, IndexStats>,
    params: MetricsParams,
) -> Result<Value, McpError> {
    info!(
        tenant_id = ?params.tenant_id,
        include_recent = params.include_recent,
        include_tiered = params.include_tiered,
        "memory.metrics"
    );

    // Filter index stats by tenant if specified. `memory.metrics`
    // intentionally keeps strict semantics here: if the caller passed a
    // tenant_id, it must parse — we do NOT fall back to the default so
    // an empty string doesn't silently show all tenants.
    let filtered_stats = if let Some(ref tenant_id_str) = params.tenant_id {
        let tenant_id = validate_tenant_id(tenant_id_str)?;
        index_stats
            .into_iter()
            .filter(|(k, _)| k == tenant_id.as_str())
            .collect()
    } else {
        index_stats
    };

    let mut snapshot = metrics.snapshot(filtered_stats);

    if !params.include_recent {
        snapshot.recent_queries.clear();
        snapshot.token_usage.recent_tool_calls.clear();
    }

    // Clear tiered stats if not requested
    if !params.include_tiered {
        snapshot.tiered = Default::default();
    }

    format_mcp_response(&snapshot)
}

/// Handle memory.compact tool call
pub async fn handle_memory_compact<S: Store>(
    store: &S,
    params: CompactParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    info!(
        tenant_id = %tenant_id,
        force = params.force,
        project_id = ?params.project_id,
        force_digest_rebuild = params.force_digest_rebuild,
        "memory.compact"
    );

    let digest_modes = params.digest_modes.clone().unwrap_or_default();
    let should_rebuild_digests = params.force_digest_rebuild || !digest_modes.is_empty();

    // Phase 3.4: before checking thresholds, drain the writer-side
    // dirty tracker and regenerate any digests that were flagged by
    // `task.add_evidence` / `task.finish` / `artifact.create`. This
    // gives operators a knob — `memory.compact` — to actually action
    // the writer-driven invalidations without also paying the cost of
    // a full storage compaction. Any explicit `digest_modes` or
    // `force_digest_rebuild` below still runs as before.
    let dirty_digests_swept = sweep_dirty_digests(store).await;
    if dirty_digests_swept > 0 {
        debug!(
            swept = dirty_digests_swept,
            "Phase 3.4: regenerated dirty digests flagged by writer paths"
        );
    }

    if params.force {
        // Force compaction regardless of thresholds
        let result = store
            .run_compaction(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;

        info!(
            tenant_id = %tenant_id,
            tombstones = result.tombstones_processed,
            hnsw_rebuilt = result.hnsw_rebuild.is_some(),
            segments_merged = result.segment_merge.is_some(),
            cache_invalidated = result.cache_entries_invalidated,
            duration_ms = result.duration.as_millis(),
            "compaction completed (forced)"
        );

        let digest_artifacts = if should_rebuild_digests {
            rebuild_requested_digests(
                store,
                &tenant_id,
                params.project_id.as_deref(),
                &digest_modes,
            )
            .await?
        } else {
            Vec::new()
        };

        return format_mcp_response(&json!({
            "status": "completed",
            "tombstones_processed": result.tombstones_processed,
            "hnsw_rebuild": result.hnsw_rebuild.map(|r| json!({
                "embeddings_processed": r.embeddings_processed,
                "embeddings_included": r.embeddings_included,
                "embeddings_excluded": r.embeddings_excluded,
                "duration_ms": r.duration.as_millis()
            })),
            "segment_merge": result.segment_merge.map(|r| json!({
                "segments_before": r.segments_before,
                "segments_after": r.segments_after,
                "segments_merged": r.segments_merged,
                "duration_ms": r.duration.as_millis()
                })),
                "cache_entries_invalidated": result.cache_entries_invalidated,
                "duration_ms": result.duration.as_millis(),
                "digest_artifacts": digest_artifacts
        }));
    }

    // Check thresholds first
    match store.run_compaction_if_needed(&tenant_id) {
        Ok(Some(result)) => {
            info!(
                tenant_id = %tenant_id,
                tombstones = result.tombstones_processed,
                hnsw_rebuilt = result.hnsw_rebuild.is_some(),
                segments_merged = result.segment_merge.is_some(),
                cache_invalidated = result.cache_entries_invalidated,
                duration_ms = result.duration.as_millis(),
                "compaction completed"
            );

            let digest_artifacts = if should_rebuild_digests {
                rebuild_requested_digests(
                    store,
                    &tenant_id,
                    params.project_id.as_deref(),
                    &digest_modes,
                )
                .await?
            } else {
                Vec::new()
            };

            format_mcp_response(&json!({
                "status": "completed",
                "tombstones_processed": result.tombstones_processed,
                "hnsw_rebuild": result.hnsw_rebuild.map(|r| json!({
                    "embeddings_processed": r.embeddings_processed,
                    "embeddings_included": r.embeddings_included,
                    "embeddings_excluded": r.embeddings_excluded,
                    "duration_ms": r.duration.as_millis()
                })),
                "segment_merge": result.segment_merge.map(|r| json!({
                    "segments_before": r.segments_before,
                    "segments_after": r.segments_after,
                    "segments_merged": r.segments_merged,
                    "duration_ms": r.duration.as_millis()
                })),
                "cache_entries_invalidated": result.cache_entries_invalidated,
                "duration_ms": result.duration.as_millis(),
                "digest_artifacts": digest_artifacts
            }))
        }
        Ok(None) => {
            debug!(tenant_id = %tenant_id, "compaction skipped - thresholds not exceeded");

            let digest_artifacts = if should_rebuild_digests {
                rebuild_requested_digests(
                    store,
                    &tenant_id,
                    params.project_id.as_deref(),
                    &digest_modes,
                )
                .await?
            } else {
                Vec::new()
            };

            format_mcp_response(&json!({
                "status": if digest_artifacts.is_empty() { "skipped" } else { "completed" },
                "reason": if digest_artifacts.is_empty() { "No compaction needed - all thresholds below limits" } else { "Storage compaction skipped; digests refreshed" },
                "digest_artifacts": digest_artifacts
            }))
        }
        Err(e) => Err(McpError::ToolError(e.to_string())),
    }
}

/// Handle memory.consolidate_episode tool call
pub async fn handle_memory_consolidate_episode<S: Store>(
    store: &S,
    params: ConsolidateEpisodeParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    validate_episode_id(&params.episode_id)?;

    if params.max_chunks == 0 {
        return Err(McpError::InvalidParams(
            "max_chunks must be greater than 0".to_string(),
        ));
    }

    let mut episode_chunks =
        collect_episode_chunks(store, &tenant_id, &params.episode_id, params.max_chunks).await?;
    if episode_chunks.is_empty() {
        return Err(McpError::ToolError(format!(
            "no chunks found for episode '{}'",
            params.episode_id
        )));
    }

    episode_chunks.sort_by_key(|chunk| chunk.timestamp_created);
    let summary_text = build_episode_summary_text(&params.episode_id, &episode_chunks);
    let inherited_tags = vec![
        make_episode_tag(&params.episode_id),
        "episode_summary:true".to_string(),
        format!("episode_source_chunks:{}", episode_chunks.len()),
    ];
    let source_projects = episode_chunks
        .iter()
        .map(|chunk| chunk.project_id.as_option().map(str::to_string))
        .collect::<HashSet<_>>();
    let (project_id, relation) = if params.retain_source_chunks {
        (
            None,
            crate::consolidate::journal::LineageRelation::DerivesFrom,
        )
    } else {
        if source_projects.len() != 1 {
            return Err(McpError::InvalidParams(
                "retain_source_chunks=false requires every episode source to share one project scope"
                    .to_string(),
            ));
        }
        (
            source_projects.into_iter().next().flatten(),
            crate::consolidate::journal::LineageRelation::Supersedes,
        )
    };
    let entry = crate::consolidate::prompt::ConsolidatedEntry {
        text: summary_text.clone(),
        supersedes: episode_chunks
            .iter()
            .map(|chunk| chunk.chunk_id.to_string())
            .collect(),
        agent_action: "Reuse the latest validated episode result before repeating this work."
            .to_string(),
        evidence: episode_chunks
            .iter()
            .map(|chunk| chunk.chunk_id.to_string())
            .collect(),
        confidence: 1.0,
        priority: 7,
    };
    let persistent = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.consolidate_episode requires a persistent store".to_string())
    })?;
    let execution = crate::consolidate::service::execute_consolidation(
        persistent,
        &tenant_id,
        project_id.as_deref(),
        std::slice::from_ref(&entry),
        relation,
        "episode_deterministic",
        &inherited_tags,
        &format!("episode:{}", params.episode_id),
        &summary_text,
    )
    .await
    .map_err(|error| McpError::ToolError(error.to_string()))?;
    if execution.state != crate::consolidate::journal::ConsolidationState::Committed {
        return Err(McpError::ToolError(format!(
            "episode consolidation run {} stopped in state {}",
            execution.run_id, execution.state
        )));
    }
    let summary_chunk_id = execution
        .candidate_chunk_ids
        .first()
        .ok_or_else(|| McpError::ToolError("episode consolidation wrote no summary".to_string()))?;

    store.record_usage_event(UsageEvent {
        op: UsageOp::Consolidate,
        tenant: Some(tenant_id.to_string()),
        project: None,
        outcome: "ok".to_string(),
        chunk_count: Some(episode_chunks.len() as i64),
        bytes: None,
        detail: None,
    });

    format_mcp_response(&ConsolidateEpisodeResult {
        summary_chunk_id: summary_chunk_id.to_string(),
        source_chunk_count: episode_chunks.len(),
        retained_source_chunks: params.retain_source_chunks,
        run_id: execution.run_id.to_string(),
    })
}

/// Parameters for memory.export_markdown (Track G2).
#[derive(Debug, Deserialize)]
pub struct ExportMarkdownParams {
    #[serde(default)]
    pub tenant_id: String,
    /// Optional project filter — when set, only chunks under this
    /// project are exported.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Maximum chunks to read from metadata. Defaults to 10_000;
    /// callers can raise it for whole-tenant exports.
    #[serde(default = "default_export_limit")]
    pub limit: usize,
}

fn default_export_limit() -> usize {
    10_000
}

/// Handle memory.export_markdown (Track G2). Read-only — never writes
/// to disk; the CLI (G3) consumes the returned `{path, content}`
/// tuples and writes them on the user's machine.
pub async fn handle_memory_export_markdown<S: Store>(
    store: &S,
    params: ExportMarkdownParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.export_markdown requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    // SQL-level project filter when scoped, so a noisy tenant doesn't
    // starve the scoped export by burning the row budget on rows from
    // other projects (Codex round-1 G2 MEDIUM finding).
    let project_filter = params.project_id.as_deref();
    let metas = if let Some(pid) = project_filter {
        ps.metadata()
            .list_recent_for_project(&tenant_id, Some(pid), params.limit)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    } else {
        ps.metadata()
            .list(&tenant_id, params.limit, 0)
            .map_err(|e| McpError::ToolError(e.to_string()))?
    };

    let mut chunks: Vec<MemoryChunk> = Vec::with_capacity(metas.len());
    for meta in metas {
        if meta.status != ChunkStatus::Final || meta.lifecycle.superseded_by.is_some() {
            continue;
        }
        if let Some(pid) = project_filter {
            if meta.project_id.as_deref() != Some(pid) {
                continue;
            }
        }
        match store
            .get(&tenant_id, &meta.chunk_id)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?
        {
            Some(chunk) => chunks.push(chunk),
            None => continue,
        }
    }

    let files = crate::markdown_export::render_markdown_tree(&chunks);
    let payload: Vec<serde_json::Value> = files
        .into_iter()
        .map(|f| serde_json::json!({ "path": f.path, "content": f.content }))
        .collect();

    format_mcp_response(&serde_json::json!({ "files": payload }))
}

/// Parameters for memory.find_near_duplicates (Track D5).
///
/// Read-only preview that mirrors the candidates
/// `memory.add(supersede_near_duplicates=...)` would actually link.
/// Pool sizes and scope semantics match the write path exactly so the
/// preview never reports a candidate the write path would miss (or
/// vice versa) — Codex round-1 D5 MEDIUM finding.
#[derive(Debug, Deserialize)]
pub struct FindNearDuplicatesParams {
    #[serde(default)]
    pub tenant_id: String,
    pub text: String,
    #[serde(rename = "type", default = "default_doc_type")]
    pub chunk_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// When set, also returns trigram-Jaccard candidates with score ≥
    /// this threshold over the same FUZZY_RECENT_POOL_SIZE pool the
    /// write path uses. Absent = exact-only.
    #[serde(default)]
    pub fuzzy_threshold: Option<f32>,
    /// `"project"` (default) restricts the candidate pool to rows with
    /// the same project_id (incl. project_id IS NULL when the probe
    /// has no project). `"tenant"` widens to the whole tenant.
    #[serde(default)]
    pub scope: Option<String>,
}

fn default_doc_type() -> String {
    "doc".into()
}

/// Handle memory.find_near_duplicates (Track D5). Read-only.
pub async fn handle_memory_find_near_duplicates<S: Store>(
    store: &S,
    params: FindNearDuplicatesParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.find_near_duplicates requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    let chunk_type = parse_chunk_type(&params.chunk_type)?;

    let scope_project = match params.scope.as_deref().unwrap_or("project") {
        "project" => true,
        "tenant" => false,
        other => {
            return Err(McpError::InvalidParams(format!(
                "scope: expected 'project' or 'tenant', got '{other}'"
            )));
        }
    };

    let canonical = crate::store::supersession::canonicalize_for_type(&params.text, chunk_type);
    let project_filter = if scope_project {
        params.project_id.as_deref()
    } else {
        None
    };

    // Exact: SQL pre-filters by canonical, so a Rust post-filter to
    // honour project_id IS NULL is cheap and safe.
    let exact_metas = ps
        .metadata()
        .list_by_canonical_text(&tenant_id, project_filter, &canonical)
        .map_err(|e| McpError::ToolError(e.to_string()))?;
    let exact: Vec<String> = exact_metas
        .into_iter()
        .filter(|m| {
            // Live-head only: don't surface previously-superseded rows
            // (mirrors compute_dedup_candidates semantics).
            m.status == ChunkStatus::Final && m.lifecycle.superseded_by.is_none()
        })
        .filter(|m| {
            !scope_project
                || params.project_id.is_none() && m.project_id.is_none()
                || m.project_id.as_deref() == params.project_id.as_deref()
        })
        .map(|m| m.chunk_id.to_string())
        .collect();

    // Fuzzy: optional. Pool size is fixed at FUZZY_RECENT_POOL_SIZE
    // so the preview's candidate set is exactly the one
    // `compute_dedup_candidates` would consider on the write path
    // (Codex round-1 D5 MEDIUM finding). Emits (chunk_id, similarity)
    // pairs ordered by score desc.
    let mut fuzzy_pairs: Vec<(String, f32)> = Vec::new();
    if let Some(threshold) = params.fuzzy_threshold {
        let limit = crate::dedup::FUZZY_RECENT_POOL_SIZE;
        let metas = if scope_project && params.project_id.is_none() {
            ps.metadata()
                .list_recent_with_null_project(&tenant_id, limit)
                .map_err(|e| McpError::ToolError(e.to_string()))?
        } else {
            ps.metadata()
                .list_recent_for_project(&tenant_id, project_filter, limit)
                .map_err(|e| McpError::ToolError(e.to_string()))?
        };
        for m in metas {
            if !(m.status == ChunkStatus::Final && m.lifecycle.superseded_by.is_none()) {
                continue;
            }
            if scope_project
                && !(params.project_id.is_none() && m.project_id.is_none()
                    || m.project_id.as_deref() == params.project_id.as_deref())
            {
                continue;
            }
            let other = m.canonical_text.as_deref().unwrap_or("");
            let score = crate::store::supersession::jaccard_trigram_score(&canonical, other);
            if score >= threshold {
                fuzzy_pairs.push((m.chunk_id.to_string(), score));
            }
        }
        fuzzy_pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }

    let fuzzy: Vec<serde_json::Value> = fuzzy_pairs
        .into_iter()
        .map(|(id, sim)| serde_json::json!({ "chunk_id": id, "similarity": sim }))
        .collect();

    format_mcp_response(&serde_json::json!({
        "exact_matches": exact,
        "fuzzy_matches": fuzzy,
    }))
}

// ----------------------------------------------------------------
// Track F5 — OMF MCP handlers.
// ----------------------------------------------------------------

/// Parameters for memory.export_omf (Track F5).
#[derive(Debug, Deserialize, Default)]
pub struct ExportOmfParams {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// Include history-tier rows in the export (false = live-only).
    #[serde(default)]
    pub include_history: bool,
    /// When absent, defaults to true (matches `ExportOptions`).
    #[serde(default)]
    pub include_superseded: Option<bool>,
    /// When absent, defaults to true (matches `ExportOptions`).
    #[serde(default)]
    pub include_expired: Option<bool>,
}

/// Handle memory.export_omf (Track F5). Read-only.
pub async fn handle_memory_export_omf<S: Store>(
    store: &S,
    params: ExportOmfParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.export_omf requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    let opts = crate::omf::export::ExportOptions {
        project_id: params.project_id,
        include_history: params.include_history,
        include_superseded: params.include_superseded.unwrap_or(true),
        include_expired: params.include_expired.unwrap_or(true),
    };
    let doc = crate::omf::export::export_omf(ps, &tenant_id, opts)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&json!({ "document": doc }))
}

/// Parameters for memory.preview_omf_import (Track F5).
#[derive(Debug, Deserialize)]
pub struct PreviewOmfImportParams {
    #[serde(default)]
    pub tenant_id: String,
    /// The OMF document to preview. Required.
    pub document: crate::omf::OmfDocument,
    /// Include items whose top-level status is "archived"/"expired".
    /// Defaults to true (matches `ImportOptions`).
    #[serde(default)]
    pub include_archived: Option<bool>,
    /// Optional fuzzy threshold. Absent = exact-only.
    #[serde(default)]
    pub fuzzy_threshold: Option<f32>,
}

/// Handle memory.preview_omf_import (Track F5). Read-only dry-run.
pub async fn handle_memory_preview_omf_import<S: Store>(
    store: &S,
    params: PreviewOmfImportParams,
) -> Result<Value, McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.preview_omf_import requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    let opts = crate::omf::import::ImportOptions {
        include_archived: params.include_archived.unwrap_or(true),
        fuzzy_threshold: params.fuzzy_threshold,
    };
    let preview = crate::omf::import::preview_omf_import(ps, &tenant_id, &params.document, opts)
        .await
        .map_err(|e| McpError::ToolError(e.to_string()))?;

    format_mcp_response(&json!({
        "total": preview.total,
        "to_import": preview.to_import,
        "duplicates": preview.duplicates,
        "filtered": preview.filtered,
        "unscoped": preview.unscoped,
        "by_project": preview.by_project,
    }))
}

/// Parameters for memory.import_omf (Track F5).
#[derive(Debug, Deserialize)]
pub struct ImportOmfParams {
    #[serde(default)]
    pub tenant_id: String,
    /// The OMF document to import. Required.
    pub document: crate::omf::OmfDocument,
    /// Include items whose top-level status is "archived"/"expired".
    /// Defaults to true.
    #[serde(default)]
    pub include_archived: Option<bool>,
    /// Optional fuzzy threshold. Absent = exact-only.
    #[serde(default)]
    pub fuzzy_threshold: Option<f32>,
}

/// Handle memory.import_omf (Track F5).
///
/// Returns both the formatted MCP response and a list of
/// `PostWriteEvent`s — one per newly imported chunk — so the server
/// dispatch arm can run structural indexing identically to how
/// memory.add_batch + memory.supersede already do.
pub async fn handle_memory_import_omf<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    params: ImportOmfParams,
) -> Result<(Value, Vec<PostWriteEvent>), McpError> {
    let ps = store.as_persistent().ok_or_else(|| {
        McpError::ToolError("memory.import_omf requires a persistent store".into())
    })?;
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant_id)
            .map_err(|e| McpError::ToolError(e.to_string()))?;
    }

    let opts = crate::omf::import::ImportOptions {
        include_archived: params.include_archived.unwrap_or(true),
        fuzzy_threshold: params.fuzzy_threshold,
    };
    let (result, imported) =
        crate::omf::import::import_omf_with_events(ps, &tenant_id, &params.document, opts)
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;

    let tenant_id_str = tenant_id.to_string();
    let events: Vec<PostWriteEvent> = imported
        .into_iter()
        .map(|ic| PostWriteEvent::from_imported_chunk(ic, &tenant_id_str))
        .collect();
    store.record_usage_event(UsageEvent {
        op: UsageOp::ImportOmf,
        tenant: Some(tenant_id.to_string()),
        project: None,
        outcome: "ok".to_string(),
        chunk_count: Some(result.imported as i64),
        bytes: None,
        detail: None,
    });

    let response = format_mcp_response(&json!({
        "total": result.total,
        "imported": result.imported,
        "duplicates": result.duplicates,
        "skipped": result.skipped,
    }))?;
    Ok((response, events))
}
