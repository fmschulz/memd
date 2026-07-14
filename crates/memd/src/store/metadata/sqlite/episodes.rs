use super::*;

pub(super) fn sql_usize(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<usize> {
    usize::try_from(row.get::<_, i64>(column)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

pub(super) fn row_to_retrieval_episode(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<RetrievalEpisode> {
    Ok(RetrievalEpisode {
        episode_id: RetrievalEpisodeId::parse(&row.get::<_, String>(0)?)
            .map_err(|error| sql_decode_error(0, error))?,
        tenant_id: TenantId::new(row.get::<_, String>(1)?)
            .map_err(|error| sql_decode_error(1, error))?,
        project_id: row.get(2)?,
        query_hash: row.get(3)?,
        query_mode: row.get(4)?,
        requested_k: sql_usize(row, 5)?,
        fetched_k: sql_usize(row, 6)?,
        rendered_k: sql_usize(row, 7)?,
        policy_version: row.get(8)?,
        policy_mode: RankingPolicyMode::parse(&row.get::<_, String>(9)?)
            .map_err(|error| sql_decode_error(9, error))?,
        task_id: row.get(10)?,
        thread_id: row.get(11)?,
        created_at_ms: row.get(12)?,
        expires_at_ms: row.get(13)?,
    })
}

pub(super) fn row_to_retrieval_episode_item(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<RetrievalEpisodeItem> {
    Ok(RetrievalEpisodeItem {
        episode_id: RetrievalEpisodeId::parse(&row.get::<_, String>(0)?)
            .map_err(|error| sql_decode_error(0, error))?,
        chunk_id: ChunkId::parse(&row.get::<_, String>(1)?)
            .map_err(|error| sql_decode_error(1, error))?,
        origin_tenant_id: TenantId::new(row.get::<_, String>(2)?)
            .map_err(|error| sql_decode_error(2, error))?,
        origin_project_id: row.get(3)?,
        original_rank: sql_usize(row, 4)?,
        original_score: row.get(5)?,
        lane_scores_json: row.get(6)?,
        outcome_adjustment: row.get(7)?,
        served_rank: row
            .get::<_, Option<i64>>(8)?
            .map(|value| {
                usize::try_from(value).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })
            })
            .transpose()?,
        shadow_rank: row
            .get::<_, Option<i64>>(9)?
            .map(|value| {
                usize::try_from(value).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })
            })
            .transpose()?,
        rendered: row.get::<_, i64>(10)? != 0,
        source_dedup_group: row.get(11)?,
    })
}

pub(super) fn parse_chunk_id_json(column: usize, text: String) -> rusqlite::Result<Vec<ChunkId>> {
    let values = serde_json::from_str::<Vec<String>>(&text)
        .map_err(|error| sql_decode_error(column, error))?;
    values
        .into_iter()
        .map(|value| ChunkId::parse(&value).map_err(|error| sql_decode_error(column, error)))
        .collect()
}

pub(super) fn row_to_outcome_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutcomeEvent> {
    Ok(OutcomeEvent {
        event_id: OutcomeEventId::parse(&row.get::<_, String>(0)?)
            .map_err(|error| sql_decode_error(0, error))?,
        episode_id: RetrievalEpisodeId::parse(&row.get::<_, String>(1)?)
            .map_err(|error| sql_decode_error(1, error))?,
        outcome: OutcomeKind::parse(&row.get::<_, String>(2)?)
            .map_err(|error| sql_decode_error(2, error))?,
        verifier: OutcomeVerifier::parse(&row.get::<_, String>(3)?)
            .map_err(|error| sql_decode_error(3, error))?,
        used_chunk_ids: parse_chunk_id_json(4, row.get(4)?)?,
        harmful_chunk_ids: parse_chunk_id_json(5, row.get(5)?)?,
        evidence_reference: row.get(6)?,
        ranking_eligible: row.get::<_, i64>(7)? != 0,
        timestamp_ms: row.get(8)?,
    })
}

pub(super) fn query_retrieval_episode_items(
    conn: &Connection,
    episode_id: &RetrievalEpisodeId,
) -> Result<Vec<RetrievalEpisodeItem>> {
    let mut statement = conn.prepare(
        "SELECT episode_id, chunk_id, origin_tenant_id, origin_project_id,
                original_rank, original_score, lane_scores_json,
                outcome_adjustment, served_rank, shadow_rank, rendered,
                source_dedup_group
         FROM retrieval_episode_items
         WHERE episode_id = ?1
         ORDER BY original_rank ASC",
    )?;
    let rows = statement.query_map(
        rusqlite::params![episode_id.to_string()],
        row_to_retrieval_episode_item,
    )?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

impl SqliteMetadataStore {
    /// Insert one retrieval episode and all candidate items atomically.
    pub fn insert_retrieval_episode(
        &self,
        episode: &RetrievalEpisode,
        items: &[RetrievalEpisodeItem],
    ) -> Result<()> {
        validate_retrieval_episode(episode, items)?;
        let mut conn = self.pool.get();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        for item in items {
            let stored_project = tx
                .query_row(
                    "SELECT project_id FROM chunks
                     WHERE tenant_id = ?1 AND chunk_id = ?2",
                    rusqlite::params![item.origin_tenant_id.as_str(), item.chunk_id.to_string()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?;
            let Some(stored_project) = stored_project else {
                return Err(MemdError::ValidationError(format!(
                    "retrieval episode chunk {} does not belong to its origin tenant",
                    item.chunk_id
                )));
            };
            if stored_project != item.origin_project_id {
                return Err(MemdError::ValidationError(format!(
                    "retrieval episode chunk {} origin project does not match",
                    item.chunk_id
                )));
            }
        }

        tx.execute(
            "INSERT INTO retrieval_episodes (
                 episode_id, tenant_id, project_id, query_hash, query_mode,
                 requested_k, fetched_k, rendered_k, policy_version, policy_mode,
                 task_id, thread_id, created_at_ms, expires_at_ms
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
             )",
            rusqlite::params![
                episode.episode_id.to_string(),
                episode.tenant_id.as_str(),
                episode.project_id.as_deref(),
                &episode.query_hash,
                &episode.query_mode,
                episode.requested_k as i64,
                episode.fetched_k as i64,
                episode.rendered_k as i64,
                &episode.policy_version,
                episode.policy_mode.as_str(),
                episode.task_id.as_deref(),
                episode.thread_id.as_deref(),
                episode.created_at_ms,
                episode.expires_at_ms,
            ],
        )?;
        {
            let mut statement = tx.prepare(
                "INSERT INTO retrieval_episode_items (
                     episode_id, chunk_id, origin_tenant_id, origin_project_id,
                     original_rank, original_score, lane_scores_json,
                     outcome_adjustment, served_rank, shadow_rank, rendered,
                     source_dedup_group
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for item in items {
                statement.execute(rusqlite::params![
                    item.episode_id.to_string(),
                    item.chunk_id.to_string(),
                    item.origin_tenant_id.as_str(),
                    item.origin_project_id.as_deref(),
                    item.original_rank as i64,
                    item.original_score,
                    &item.lane_scores_json,
                    item.outcome_adjustment,
                    item.served_rank.map(|rank| rank as i64),
                    item.shadow_rank.map(|rank| rank as i64),
                    i64::from(item.rendered),
                    item.source_dedup_group.as_deref(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Load one retrieval episode and its candidate items by tenant and ID.
    pub fn get_retrieval_episode(
        &self,
        tenant_id: &TenantId,
        episode_id: &RetrievalEpisodeId,
    ) -> Result<Option<(RetrievalEpisode, Vec<RetrievalEpisodeItem>)>> {
        let conn = self.pool.get();
        let episode = conn
            .query_row(
                "SELECT episode_id, tenant_id, project_id, query_hash, query_mode,
                        requested_k, fetched_k, rendered_k, policy_version, policy_mode,
                        task_id, thread_id, created_at_ms, expires_at_ms
                 FROM retrieval_episodes
                 WHERE tenant_id = ?1 AND episode_id = ?2",
                rusqlite::params![tenant_id.as_str(), episode_id.to_string()],
                row_to_retrieval_episode,
            )
            .optional()?;
        let Some(episode) = episode else {
            return Ok(None);
        };
        let items = query_retrieval_episode_items(&conn, episode_id)?;
        Ok(Some((episode, items)))
    }

    /// Atomically replace the final served/rendered projection for an episode.
    pub fn finalize_retrieval_episode(
        &self,
        tenant_id: &TenantId,
        episode_id: &RetrievalEpisodeId,
        rendered_chunk_ids: &[ChunkId],
    ) -> Result<()> {
        let mut conn = self.pool.get();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut episode = tx
            .query_row(
                "SELECT episode_id, tenant_id, project_id, query_hash, query_mode,
                        requested_k, fetched_k, rendered_k, policy_version, policy_mode,
                        task_id, thread_id, created_at_ms, expires_at_ms
                 FROM retrieval_episodes
                 WHERE tenant_id = ?1 AND episode_id = ?2",
                rusqlite::params![tenant_id.as_str(), episode_id.to_string()],
                row_to_retrieval_episode,
            )
            .optional()?
            .ok_or_else(|| {
                MemdError::ValidationError(format!(
                    "unknown retrieval episode {episode_id} for tenant"
                ))
            })?;
        let mut items = query_retrieval_episode_items(&tx, episode_id)?;
        crate::store::outcome::apply_rendered_order(&mut episode, &mut items, rendered_chunk_ids)?;
        tx.execute(
            "UPDATE retrieval_episodes SET rendered_k = ?3
             WHERE tenant_id = ?1 AND episode_id = ?2",
            rusqlite::params![
                tenant_id.as_str(),
                episode_id.to_string(),
                episode.rendered_k as i64
            ],
        )?;
        tx.execute(
            "UPDATE retrieval_episode_items
             SET served_rank = NULL, rendered = 0
             WHERE episode_id = ?1",
            rusqlite::params![episode_id.to_string()],
        )?;
        for (rank, chunk_id) in rendered_chunk_ids.iter().enumerate() {
            tx.execute(
                "UPDATE retrieval_episode_items
                 SET served_rank = ?3, rendered = 1
                 WHERE episode_id = ?1 AND chunk_id = ?2",
                rusqlite::params![episode_id.to_string(), chunk_id.to_string(), rank as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Validate and insert one immutable outcome event.
    pub fn insert_outcome_event(&self, tenant_id: &TenantId, event: &OutcomeEvent) -> Result<()> {
        let mut conn = self.pool.get();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let episode = tx
            .query_row(
                "SELECT episode_id, tenant_id, project_id, query_hash, query_mode,
                        requested_k, fetched_k, rendered_k, policy_version, policy_mode,
                        task_id, thread_id, created_at_ms, expires_at_ms
                 FROM retrieval_episodes
                 WHERE tenant_id = ?1 AND episode_id = ?2",
                rusqlite::params![tenant_id.as_str(), event.episode_id.to_string()],
                row_to_retrieval_episode,
            )
            .optional()?
            .ok_or_else(|| {
                MemdError::ValidationError(format!(
                    "unknown retrieval episode {} for tenant",
                    event.episode_id
                ))
            })?;
        let items = query_retrieval_episode_items(&tx, &event.episode_id)?;
        validate_outcome_event(tenant_id, &episode, &items, event)?;
        let used = serde_json::to_string(
            &event
                .used_chunk_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )?;
        let harmful = serde_json::to_string(
            &event
                .harmful_chunk_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )?;
        tx.execute(
            "INSERT INTO outcome_events (
                 event_id, episode_id, outcome, verifier_type,
                 used_chunk_ids_json, harmful_chunk_ids_json, evidence_reference,
                 ranking_eligible, timestamp_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                event.event_id.to_string(),
                event.episode_id.to_string(),
                event.outcome.as_str(),
                event.verifier.as_str(),
                used,
                harmful,
                event.evidence_reference.as_deref(),
                i64::from(event.ranking_eligible),
                event.timestamp_ms,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// List immutable outcomes for one tenant-scoped episode.
    pub fn list_outcome_events_for_episode(
        &self,
        tenant_id: &TenantId,
        episode_id: &RetrievalEpisodeId,
    ) -> Result<Vec<OutcomeEvent>> {
        let conn = self.pool.get();
        let exists = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM retrieval_episodes
                 WHERE tenant_id = ?1 AND episode_id = ?2
             )",
            rusqlite::params![tenant_id.as_str(), episode_id.to_string()],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !exists {
            return Ok(Vec::new());
        }
        let mut statement = conn.prepare(
            "SELECT o.event_id, o.episode_id, o.outcome, o.verifier_type,
                    o.used_chunk_ids_json, o.harmful_chunk_ids_json,
                    o.evidence_reference, o.ranking_eligible, o.timestamp_ms
             FROM outcome_events o
             JOIN retrieval_episodes e ON e.episode_id = o.episode_id
             WHERE o.episode_id = ?1 AND e.tenant_id = ?2
             ORDER BY o.timestamp_ms ASC, o.event_id ASC",
        )?;
        let rows = statement.query_map(
            rusqlite::params![episode_id.to_string(), tenant_id.as_str()],
            row_to_outcome_event,
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Aggregate the newest eligible attribution per episode/chunk into
    /// decayed priors scoped to the tenant/project that requested retrieval.
    /// An aliased chunk therefore learns only inside the requester's scope;
    /// its origin tenant cannot be influenced by another tenant's outcomes.
    pub fn outcome_priors(
        &self,
        scope_tenant_id: &TenantId,
        scope_project_id: Option<&str>,
        chunk_ids: &[ChunkId],
        now_ms: i64,
    ) -> Result<Vec<OutcomePrior>> {
        let requested = chunk_ids.iter().cloned().collect::<HashSet<_>>();
        if requested.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.pool.get();
        let mut statement = conn.prepare(
            "SELECT o.episode_id, i.chunk_id, o.outcome,
                    o.used_chunk_ids_json, o.harmful_chunk_ids_json,
                    o.timestamp_ms
             FROM outcome_events o
             JOIN retrieval_episode_items i ON i.episode_id = o.episode_id
             JOIN retrieval_episodes e ON e.episode_id = o.episode_id
             WHERE o.ranking_eligible = 1
               AND e.tenant_id = ?1
               AND ((?2 IS NULL AND e.project_id IS NULL)
                    OR e.project_id = ?2)
               AND o.timestamp_ms <= ?3
             ORDER BY o.timestamp_ms DESC, o.event_id DESC",
        )?;
        let rows = statement.query_map(
            rusqlite::params![scope_tenant_id.as_str(), scope_project_id, now_ms],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;
        let mut priors = HashMap::<ChunkId, OutcomePrior>::new();
        let mut seen_episode_chunks = HashSet::<(String, ChunkId)>::new();
        for row in rows {
            let (episode_id, chunk_id, outcome, used_json, harmful_json, timestamp_ms) = row?;
            let Ok(chunk_id) = ChunkId::parse(&chunk_id) else {
                continue;
            };
            if !requested.contains(&chunk_id) {
                continue;
            }
            let outcome = OutcomeKind::parse(&outcome)?;
            let attributed = if outcome.credits_used() {
                parse_chunk_id_json(3, used_json)?
            } else if outcome.credits_harmful() {
                parse_chunk_id_json(4, harmful_json)?
            } else {
                continue;
            };
            if !attributed.contains(&chunk_id)
                || !seen_episode_chunks.insert((episode_id, chunk_id.clone()))
            {
                continue;
            }
            priors
                .entry(chunk_id.clone())
                .or_insert_with(|| OutcomePrior::new(chunk_id.clone()))
                .add(
                    outcome.credits_used(),
                    decayed_outcome_weight(timestamp_ms, now_ms),
                    timestamp_ms,
                );
        }
        let mut priors = priors.into_values().collect::<Vec<_>>();
        priors.sort_by_key(|prior| prior.chunk_id.to_string());
        Ok(priors)
    }

    /// Per-chunk rendered exposure counts from structured retrieval episodes.
    pub fn retrieval_exposure_stats_since(
        &self,
        since_ms: i64,
        tenant_id: Option<&str>,
        project_id: Option<&str>,
    ) -> Result<Vec<(String, u32, i64)>> {
        let conn = self.pool.get();
        let mut sql = String::from(
            "SELECT i.chunk_id, COUNT(*) AS rendered_count, MAX(e.created_at_ms)
             FROM retrieval_episode_items i
             JOIN retrieval_episodes e ON e.episode_id = i.episode_id
             WHERE i.rendered = 1 AND e.created_at_ms >= ?1",
        );
        let mut params = vec![rusqlite::types::Value::Integer(since_ms)];
        if let Some(tenant_id) = tenant_id {
            sql.push_str(" AND e.tenant_id = ?");
            params.push(rusqlite::types::Value::Text(tenant_id.to_string()));
        }
        if let Some(project_id) = project_id {
            sql.push_str(" AND e.project_id = ?");
            params.push(rusqlite::types::Value::Text(project_id.to_string()));
        }
        sql.push_str(" GROUP BY i.chunk_id ORDER BY rendered_count DESC, i.chunk_id ASC");
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
            let count = row.get::<_, i64>(1)?.clamp(0, i64::from(u32::MAX)) as u32;
            Ok((row.get::<_, String>(0)?, count, row.get::<_, i64>(2)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
