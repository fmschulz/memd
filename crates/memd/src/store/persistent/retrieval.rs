use super::*;

impl PersistentStore {
    pub(super) async fn hybrid_search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        self.hybrid_search_at(tenant_id, query, k, None).await
    }

    pub(super) async fn hybrid_search_at(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        ranking_time_ms: Option<i64>,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        let query_preview_end = query
            .char_indices()
            .nth(50)
            .map_or(query.len(), |(index, _)| index);
        debug!(
            tenant_id = %tenant_id,
            query = &query[..query_preview_end],
            k = k,
            hybrid = self.hybrid_searcher.is_some(),
            dense = self.dense_searcher.is_some(),
            "hybrid_search called"
        );

        // Use real hybrid search if available, otherwise fallback
        if self.hybrid_searcher.is_some() || self.dense_searcher.is_some() {
            debug!("taking search_with_scores_real path");
            return self
                .search_with_scores_real(tenant_id, query, k, ranking_time_ms)
                .await;
        }
        // Final fallback: simple text search
        warn!("WARNING: Taking text-only fallback path - no embeddings!");
        return self.search_with_scores_impl(tenant_id, query, k).await;
    }

    pub(super) async fn search_with_scores_impl(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        // OLD FALLBACK CODE (keep for now but will be removed):
        // For now, simple implementation: list + filter
        // Real search comes in Phase 3 with embeddings
        let metadata_list = self.metadata.list(tenant_id, k * 2, 0)?;

        let mut results = Vec::new();

        for meta in metadata_list {
            if meta.status == ChunkStatus::Deleted {
                continue;
            }

            let Some(chunk) = self
                .get_chunk_for_retrieval(tenant_id, &meta.chunk_id, "text_fallback_search")
                .await?
            else {
                continue;
            };

            if query.is_empty() || chunk.text.to_lowercase().contains(&query.to_lowercase()) {
                results.push(chunk);
                if results.len() >= k {
                    break;
                }
            }
        }

        // Fallback returns results with score 1.0
        Ok(results.into_iter().map(|c| (c, 1.0)).collect())
    }

    /// Replacement for old search_with_scores - now does real hybrid search
    async fn search_with_scores_real(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
        ranking_time_ms: Option<i64>,
    ) -> Result<Vec<(MemoryChunk, f32)>> {
        debug!(
            tenant_id = %tenant_id,
            hybrid = self.hybrid_searcher.is_some(),
            dense = self.dense_searcher.is_some(),
            "search_with_scores_real called"
        );

        let total_start = Instant::now();

        // Over-fetch so the deleted/hidden-chunk drop below still yields `k`
        // live results. HNSW has no per-chunk delete, so tombstoned chunks stay
        // indexed, resolve to None in get_chunk_for_retrieval, and are dropped
        // with no backfill; without headroom a store with recent tombstones
        // returns fewer than `k` even when `k` live matches exist deeper.
        let fetch_k = k.saturating_mul(2).min(k.saturating_add(256));

        // Use hybrid search if available (combines dense + sparse)
        if let Some(ref hybrid) = self.hybrid_searcher {
            debug!("using HYBRID search path");
            let search_context = ranking_time_ms.map(|ranking_time_ms| SearchContext {
                ranking_time_ms: Some(ranking_time_ms),
                ..SearchContext::default()
            });
            let (hybrid_results, timing) = hybrid
                .search_with_timing(tenant_id, query, fetch_k, search_context.clone())
                .await?;

            let fetch_start = Instant::now();
            let mut chunk_by_id: HashMap<ChunkId, MemoryChunk> =
                HashMap::with_capacity(hybrid_results.len());
            let mut rerank_meta = Vec::with_capacity(hybrid_results.len());
            let mut base_results: Vec<HybridSearchResult> =
                Vec::with_capacity(hybrid_results.len());

            for result in hybrid_results {
                if let Some(chunk) = self
                    .get_chunk_for_retrieval(tenant_id, &result.chunk_id, "hybrid_search")
                    .await?
                {
                    if hybrid.rerank_enabled() {
                        rerank_meta.push(ChunkMetaForRerank {
                            chunk_id: result.chunk_id.clone(),
                            rrf_score: result.final_score,
                            timestamp_created: chunk.timestamp_created,
                            project_id: chunk.project_id.as_option().map(str::to_string),
                            chunk_type: chunk.chunk_type,
                            text: Some(chunk.text.clone()),
                        });
                    }
                    chunk_by_id.insert(result.chunk_id.clone(), chunk);
                    base_results.push(result);
                    if !hybrid.rerank_enabled() && base_results.len() >= k {
                        break;
                    }
                }
            }

            let reranked = if hybrid.rerank_enabled() {
                hybrid.rerank_with_metadata_for_query(
                    query,
                    base_results,
                    rerank_meta,
                    search_context,
                )
            } else {
                base_results
            };
            let mut results: Vec<(MemoryChunk, f32)> = reranked
                .into_iter()
                .filter_map(|result| {
                    chunk_by_id
                        .get(&result.chunk_id)
                        .cloned()
                        .map(|chunk| (chunk, result.final_score))
                })
                .collect();
            // Truncate to the requested `k` after the deleted-chunk drop, so
            // the over-fetch absorbs tombstones instead of returning extras.
            results.truncate(k);
            let fetch_time = fetch_start.elapsed();

            // Record query metrics (use dense time as embed time, sparse time as search time)
            self.metrics.record_query(QueryMetrics::from_timings(
                timing.dense_time,
                timing.sparse_time + timing.fusion_time,
                fetch_time,
                total_start.elapsed(),
            ));

            // Record tiered metrics if tiered search was used
            if let Some(tiered_timing) = timing.tiered.as_ref() {
                self.metrics.record_tiered_query(TieredQueryMetrics {
                    source_tier: if tiered_timing.cache_lookup_ms > 0
                        && tiered_timing.hot_tier_ms == 0
                        && tiered_timing.warm_tier_ms == 0
                    {
                        "cache".to_string()
                    } else if tiered_timing.hot_tier_ms > 0 {
                        "hot".to_string()
                    } else {
                        "warm".to_string()
                    },
                    cache_lookup_ms: tiered_timing.cache_lookup_ms,
                    hot_tier_ms: tiered_timing.hot_tier_ms,
                    warm_tier_ms: tiered_timing.warm_tier_ms,
                    cache_hit: tiered_timing.warm_tier_ms == 0 && tiered_timing.hot_tier_ms == 0,
                    hot_tier_hit: tiered_timing.hot_tier_ms > 0 && tiered_timing.warm_tier_ms == 0,
                });
            }

            return Ok(results);
        }

        // Fallback to dense-only if hybrid not available
        if let Some(ref searcher) = self.dense_searcher {
            debug!("using DENSE-ONLY search path");
            let (dense_results, embed_time, search_time) = searcher
                .search_with_timing(tenant_id, query, fetch_k)
                .await?;

            warn!(
                dense_count = dense_results.len(),
                "dense search returned results"
            );

            let fetch_start = Instant::now();
            let mut results = Vec::with_capacity(dense_results.len());
            for result in dense_results {
                if results.len() >= k {
                    break;
                }
                if let Some(chunk) = self
                    .get_chunk_for_retrieval(tenant_id, &result.chunk_id, "dense_search")
                    .await?
                {
                    results.push((chunk, result.score));
                } else {
                    warn!(chunk_id = %result.chunk_id, "FAILED to fetch chunk - get() returned None");
                }
            }
            warn!(final_count = results.len(), "chunks fetched successfully");
            let fetch_time = fetch_start.elapsed();

            // Record metrics
            self.metrics.record_query(QueryMetrics::from_timings(
                embed_time,
                search_time,
                fetch_time,
                total_start.elapsed(),
            ));

            return Ok(results);
        }

        // Fall back to text search with score 1.0
        warn!("using TEXT-ONLY fallback search (no embeddings available)");
        let chunks = self.search(tenant_id, query, k).await?;
        Ok(chunks.into_iter().map(|c| (c, 1.0)).collect())
    }

    pub(super) async fn delete_chunk(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
    ) -> Result<bool> {
        self.ensure_writable("delete_chunk")?;
        // Get metadata to find segment/ordinal
        let meta = self.metadata.get(tenant_id, chunk_id)?;
        let meta = match meta {
            Some(m) if m.status != ChunkStatus::Deleted => m,
            _ => return Ok(false),
        };

        let tenant_str = tenant_id.to_string();

        // Write to WAL
        let tenant = self.get_or_create_tenant(&tenant_str)?;
        {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);

            tenant.append_wal_delete(&tenant_str, &chunk_id.to_string(), timestamp, "delete")?;
        }

        // Update metadata status
        self.metadata.mark_deleted(tenant_id, chunk_id)?;

        // Update tombstone in segment. Phase 3.6: `mark_deleted`
        // now takes `&self` because `SegmentReader.tombstones` is a
        // `Arc<RwLock<TombstoneSet>>`. A read lock on the segments
        // map is enough — concurrent reads on other segments and
        // other active readers on this segment no longer block.
        {
            let segments = tenant.segments.read();
            if let Some(reader) = segments.get(&meta.segment_id) {
                reader.mark_deleted(meta.ordinal)?;
            }
        }

        // Remove from hybrid/sparse index and invalidate cache/hot tier
        if let Some(ref hybrid) = self.hybrid_searcher {
            if let Err(e) = hybrid.delete_chunk(tenant_id, chunk_id) {
                warn!(
                    chunk_id = %chunk_id,
                    error = %e,
                    "failed to delete chunk from hybrid searcher"
                );
            }
        }

        // Explicit cache/tier invalidation (hybrid.delete_chunk also does this)
        self.invalidate_chunk(chunk_id);

        info!(tenant_id = %tenant_str, chunk_id = %chunk_id, "chunk deleted");
        Ok(true)
    }

    pub(super) async fn get_stats(&self, tenant_id: &TenantId) -> Result<StoreStats> {
        let (active, deleted, candidates) = self.metadata.count_by_status(tenant_id)?;
        let (chunk_types_active, chunk_types_deleted, chunk_types_all) =
            self.metadata.count_chunk_types_by_status(tenant_id, None)?;

        Ok(StoreStats {
            total_chunks: active + deleted + candidates,
            active_chunks: active,
            candidate_chunks: candidates,
            deleted_chunks: deleted,
            chunk_types: chunk_types_active.clone(),
            chunk_types_active,
            chunk_types_deleted,
            chunk_types_all,
        })
    }
}
