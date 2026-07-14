use super::*;

pub async fn handle_memory_search<S: Store>(
    store: &S,
    params: SearchParams,
) -> Result<Value, McpError> {
    let tenant_id = resolve_tenant_id(&params.tenant_id)?;
    ProjectId::validate_opt(params.project_id.as_deref())
        .map_err(|e| McpError::InvalidParams(e.to_string()))?;
    validate_search_k(params.k)?;
    if params.ranking_time_ms.is_some_and(|value| value < 0) {
        return Err(McpError::InvalidParams(
            "ranking_time_ms must be non-negative".to_string(),
        ));
    }
    let parsed_filters = parse_search_filters(params.filters.as_ref())?;
    let debug_tiers = params.debug_tiers.unwrap_or(false);
    let mode = params.mode.unwrap_or_default();
    let project_id_filter = params.project_id.as_deref();
    let scope_expansion = scope_expansion_for(&tenant_id, project_id_filter);
    let has_filters = has_active_search_filters(project_id_filter, &parsed_filters);
    let (visibility_policy, oversample_factor) = resolve_visibility_and_oversample(&params);
    let policy_mode = params.ranking_policy.unwrap_or(RankingPolicyMode::Shadow);
    let candidate_multiplier = params.candidate_multiplier.unwrap_or(4).clamp(1, 20);
    let outcome_candidate_cap = params
        .k
        .saturating_mul(candidate_multiplier)
        .clamp(params.k, 200);
    // Pre-visibility trim headroom: `apply_search_filters` takes a cap so
    // pass the larger lifecycle/outcome candidate bound here. The final
    // served list is still trimmed to k after visibility and source dedup.
    let pre_visibility_cap = params
        .k
        .saturating_mul(oversample_factor)
        .max(outcome_candidate_cap)
        .min(200);
    let fetch_k = adaptive_fetch_k(params.k, &params.query, has_filters)
        .max(pre_visibility_cap)
        .min(200);
    let project_scopes = project_scopes_for_project(store, &tenant_id, project_id_filter).await?;

    info!(
        tenant_id = %tenant_id,
        query = %params.query,
        k = params.k,
        fetch_k = fetch_k,
        debug_tiers = debug_tiers,
        "memory.search"
    );

    // Use search_with_tier_info if debug_tiers is requested
    if debug_tiers {
        let (scored_chunks, timing) = search_with_tier_info_for_project_scopes(
            store,
            &project_scopes,
            &params.query,
            fetch_k,
            params.ranking_time_ms,
        )
        .await?;
        let exact_candidates = exact_lexical_candidates_for_project_scopes(
            store,
            &project_scopes,
            &params.query,
            params.k.min(fetch_k),
        )
        .await?;
        let preferred = summary_preferred_results_for_project_scopes(
            store,
            &project_scopes,
            &params.query,
            mode,
            params.k.min(8),
            params.ranking_time_ms,
        )
        .await?;
        let mut merged = merge_preferred_and_raw(
            preferred,
            merge_scored_chunk_lists(vec![exact_candidates, scored_chunks], fetch_k),
            fetch_k,
        );
        if should_run_lexical_overlap_rescue(&merged, &params.query, params.k) {
            let lexical_candidates = lexical_overlap_candidates_for_project_scopes(
                store,
                &project_scopes,
                &params.query,
                params.k.min(fetch_k),
            )
            .await?;
            merged = merge_scored_chunk_lists(vec![lexical_candidates, merged], fetch_k);
        }
        let mut scored_chunks =
            apply_search_filters(merged, None, &parsed_filters, pre_visibility_cap);
        let mut timing = timing;
        let mut repair_info = None;

        if scored_chunks.is_empty() && !params.query.is_empty() {
            if let Some(repaired_query) = normalize_query_for_repair(&params.query) {
                let (repair_scored, repair_timing) = search_with_tier_info_for_project_scopes(
                    store,
                    &project_scopes,
                    &repaired_query,
                    fetch_k,
                    params.ranking_time_ms,
                )
                .await?;
                let repaired_filtered =
                    apply_search_filters(repair_scored, None, &parsed_filters, pre_visibility_cap);
                let repaired = !repaired_filtered.is_empty();
                if repaired {
                    scored_chunks = repaired_filtered;
                    timing = repair_timing;
                }
                repair_info = Some(RepairInfo {
                    attempted: true,
                    repaired,
                    original_query: params.query.clone(),
                    repaired_query: Some(repaired_query),
                });
            }
        }

        if mode == QueryMode::Generic {
            scored_chunks = suppress_generated_digest_projection_chunks(scored_chunks);
        }

        // Resolve lifecycle visibility over the expanded pool. Outcome
        // shadow scoring observes this pool; source dedup and final top-k
        // selection happen afterward so discarded fragments remain auditable.
        let episode_candidates = apply_visibility_filter(
            store,
            scored_chunks,
            &visibility_policy,
            outcome_candidate_cap,
        )
        .await;
        let episode_pool = outcome_rank_candidate_pool(
            store,
            &tenant_id,
            project_id_filter,
            episode_candidates,
            policy_mode,
            params.ranking_time_ms,
        )
        .await?;
        let mut scored_chunks = if params.dedupe_by_source {
            dedupe_scored_chunks_by_source_uri(episode_pool.served.clone())
        } else {
            episode_pool.served.clone()
        };
        scored_chunks.truncate(params.k);
        if params.render_event_time {
            render_observed_time_into_text(&mut scored_chunks);
        }

        debug!(
            results_count = scored_chunks.len(),
            "search completed with tier info"
        );

        // Pre-budget retrieval count: scope_status's wider_scope_hits probe
        // must key on whether the search itself fell short of k, not on how
        // many rows survived token-budget packing in shape_memory_results.
        let retrieved_count = scored_chunks.len();

        // Build tier debug info if timing is available
        let tier_info = timing.map(|t| {
            let source_tier = if t.cache_lookup_ms > 0 && t.hot_tier_ms == 0 && t.warm_tier_ms == 0
            {
                "cache".to_string()
            } else if t.hot_tier_ms > 0 && t.warm_tier_ms == 0 {
                "hot".to_string()
            } else if t.warm_tier_ms > 0 {
                "warm".to_string()
            } else {
                "hybrid".to_string()
            };

            let cache_hit = t.cache_lookup_ms > 0 && t.hot_tier_ms == 0 && t.warm_tier_ms == 0;
            let hot_tier_hit = t.hot_tier_ms > 0 && t.warm_tier_ms == 0;

            TierDebugInfo {
                source_tier,
                cache_hit,
                hot_tier_hit,
                cache_lookup_ms: t.cache_lookup_ms,
                hot_tier_ms: t.hot_tier_ms,
                warm_tier_ms: t.warm_tier_ms,
            }
        });

        // Determine source tier per result based on scoring heuristics
        // If we have tier_info, derive per-result tier from overall timing
        let default_tier = tier_info.as_ref().map(|t| t.source_tier.clone());

        let artifacts = resolve_artifacts_for_ranked_chunks(store, &scored_chunks).await?;
        let mut results = build_chunk_results(
            store,
            &scored_chunks,
            default_tier,
            &artifacts,
            params.expand_event_siblings,
            &visibility_policy,
        )
        .await?;
        annotate_chunk_origins(&mut results, &tenant_id, scope_expansion.as_ref());

        let (results, budget_info) = shape_memory_results(results, &params);
        let (retrieval_episode_id, ranking_policy) =
            if params.suppress_retrieval_episode || params.ranking_time_ms.is_some() {
                (None, None)
            } else {
                let (episode_id, policy) = record_search_retrieval_episode(
                    store,
                    &tenant_id,
                    project_id_filter,
                    &params.query,
                    mode,
                    params.k,
                    policy_mode,
                    params.task_id.clone(),
                    params.thread_id.clone(),
                    &episode_pool,
                    &results,
                )
                .await?;
                (Some(episode_id), Some(policy))
            };
        record_search_usage_event(store, &tenant_id, &params, results.len());
        let scope_status = scope_status_for_search(
            store,
            &tenant_id,
            project_id_filter,
            &params.query,
            params.k,
            retrieved_count,
            &parsed_filters,
            &visibility_policy,
            params.ranking_time_ms,
        )
        .await;
        return format_mcp_response(&SearchResult {
            results,
            retrieval_episode_id,
            ranking_policy,
            budget_info,
            scope_expansion,
            tier_info,
            repair_info,
            scope_status: Some(scope_status),
        });
    }

    // Standard path without tier info
    let scored_chunks = search_with_scores_for_project_scopes(
        store,
        &project_scopes,
        &params.query,
        fetch_k,
        params.ranking_time_ms,
    )
    .await?;
    let exact_candidates = exact_lexical_candidates_for_project_scopes(
        store,
        &project_scopes,
        &params.query,
        params.k.min(fetch_k),
    )
    .await?;
    let preferred = summary_preferred_results_for_project_scopes(
        store,
        &project_scopes,
        &params.query,
        mode,
        params.k.min(8),
        params.ranking_time_ms,
    )
    .await?;
    let mut merged = merge_preferred_and_raw(
        preferred,
        merge_scored_chunk_lists(vec![exact_candidates, scored_chunks], fetch_k),
        fetch_k,
    );
    if should_run_lexical_overlap_rescue(&merged, &params.query, params.k) {
        let lexical_candidates = lexical_overlap_candidates_for_project_scopes(
            store,
            &project_scopes,
            &params.query,
            params.k.min(fetch_k),
        )
        .await?;
        merged = merge_scored_chunk_lists(vec![lexical_candidates, merged], fetch_k);
    }
    let mut scored_chunks = apply_search_filters(merged, None, &parsed_filters, pre_visibility_cap);
    let mut repair_info = None;

    if scored_chunks.is_empty() && !params.query.is_empty() {
        if let Some(repaired_query) = normalize_query_for_repair(&params.query) {
            let repair_scored = search_with_scores_for_project_scopes(
                store,
                &project_scopes,
                &repaired_query,
                fetch_k,
                params.ranking_time_ms,
            )
            .await?;
            let repaired_filtered =
                apply_search_filters(repair_scored, None, &parsed_filters, pre_visibility_cap);
            let repaired = !repaired_filtered.is_empty();
            if repaired {
                scored_chunks = repaired_filtered;
            }
            repair_info = Some(RepairInfo {
                attempted: true,
                repaired,
                original_query: params.query.clone(),
                repaired_query: Some(repaired_query),
            });
        }
    }

    if mode == QueryMode::Generic {
        scored_chunks = suppress_generated_digest_projection_chunks(scored_chunks);
    }

    let episode_candidates = apply_visibility_filter(
        store,
        scored_chunks,
        &visibility_policy,
        outcome_candidate_cap,
    )
    .await;
    let episode_pool = outcome_rank_candidate_pool(
        store,
        &tenant_id,
        project_id_filter,
        episode_candidates,
        policy_mode,
        params.ranking_time_ms,
    )
    .await?;
    let mut scored_chunks = if params.dedupe_by_source {
        dedupe_scored_chunks_by_source_uri(episode_pool.served.clone())
    } else {
        episode_pool.served.clone()
    };
    scored_chunks.truncate(params.k);
    if params.render_event_time {
        render_observed_time_into_text(&mut scored_chunks);
    }

    debug!(results_count = scored_chunks.len(), "search completed");

    // Pre-budget retrieval count for scope_status (see the tier-debug
    // branch above): the wider_scope_hits probe keys on search shortage,
    // not on token-budget packing.
    let retrieved_count = scored_chunks.len();

    let artifacts = resolve_artifacts_for_ranked_chunks(store, &scored_chunks).await?;
    let mut results = build_chunk_results(
        store,
        &scored_chunks,
        None,
        &artifacts,
        params.expand_event_siblings,
        &visibility_policy,
    )
    .await?;
    annotate_chunk_origins(&mut results, &tenant_id, scope_expansion.as_ref());

    let (results, budget_info) = shape_memory_results(results, &params);
    let (retrieval_episode_id, ranking_policy) =
        if params.suppress_retrieval_episode || params.ranking_time_ms.is_some() {
            (None, None)
        } else {
            let (episode_id, policy) = record_search_retrieval_episode(
                store,
                &tenant_id,
                project_id_filter,
                &params.query,
                mode,
                params.k,
                policy_mode,
                params.task_id.clone(),
                params.thread_id.clone(),
                &episode_pool,
                &results,
            )
            .await?;
            (Some(episode_id), Some(policy))
        };
    record_search_usage_event(store, &tenant_id, &params, results.len());
    let scope_status = scope_status_for_search(
        store,
        &tenant_id,
        project_id_filter,
        &params.query,
        params.k,
        retrieved_count,
        &parsed_filters,
        &visibility_policy,
        params.ranking_time_ms,
    )
    .await;
    format_mcp_response(&SearchResult {
        results,
        retrieval_episode_id,
        ranking_policy,
        budget_info,
        scope_expansion,
        tier_info: None,
        repair_info,
        scope_status: Some(scope_status),
    })
}
