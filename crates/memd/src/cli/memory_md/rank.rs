use super::action::*;
use super::*;

#[derive(Debug, Clone)]
pub(super) struct Takeaway {
    pub(super) chunk_id: String,
    pub(super) tenant_id: String,
    pub(super) project_id: Option<String>,
    pub(super) text: String,
    pub(super) score: f32,
    pub(super) priority: f32,
    pub(super) chunk_type: String,
    pub(super) timestamp_created: i64,
    pub(super) tags: Vec<String>,
    pub(super) sources: BTreeSet<String>,
    pub(super) occurrences: usize,
}

#[derive(Debug, Clone)]
pub(super) struct RankedTakeawayCollection {
    pub(super) takeaways: Vec<Takeaway>,
    pub(super) explanations: Vec<MemoryMdCandidateExplanation>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct PriorityBreakdown {
    pub(super) explicit: f32,
    pub(super) kind_weight: f32,
    pub(super) type_weight: f32,
    pub(super) recurrence: f32,
    pub(super) multi_query: f32,
    pub(super) search_score: f32,
    pub(super) library_bonus: f32,
    pub(super) utility: f32,
    pub(super) staleness_penalty: f32,
    pub(super) total: f32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct MemoryMdCandidateExplanation {
    pub(super) section: String,
    pub(super) source: String,
    pub(super) query: String,
    pub(super) mode: String,
    pub(super) raw_rank: usize,
    pub(super) chunk_id: String,
    pub(super) tenant_id: String,
    pub(super) project_id: Option<String>,
    pub(super) chunk_type: String,
    pub(super) timestamp_created: i64,
    pub(super) search_score: f32,
    pub(super) priority_score: Option<f32>,
    pub(super) priority_breakdown: Option<PriorityBreakdown>,
    pub(super) display_status: String,
    pub(super) filter_reason: Option<String>,
    pub(super) display_rank: Option<usize>,
    pub(super) generated_digest: bool,
    pub(super) quality_flags: Vec<String>,
    pub(super) topic_key: Option<String>,
    pub(super) tags: Vec<String>,
    pub(super) matched_sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TakeawayCategory {
    pub(super) heading: &'static str,
    pub(super) reason: &'static str,
    pub(super) order: u8,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn collect_ranked_takeaways_with_explanations<S: Store>(
    store: &S,
    tenant_id: &str,
    project_id: Option<&str>,
    queries: &[(CliQueryMode, &str, &str)],
    candidate_k: usize,
    limit: usize,
    hit_stats: &HashMap<String, HitStats>,
    section: &str,
) -> Result<RankedTakeawayCollection> {
    let mut by_chunk: HashMap<String, Takeaway> = HashMap::new();
    let mut explanations = Vec::new();

    for (mode, source, query) in queries {
        let payload = cli_search_payload_silent(
            store,
            tenant_id.to_string(),
            project_id.map(str::to_string),
            (*query).to_string(),
            candidate_k,
            false,
            None,
            *mode,
            false,
            false,
            false,
        )
        .await?;
        merge_payload_candidates(
            &mut by_chunk,
            &mut explanations,
            &payload,
            section,
            source,
            query,
            *mode,
        );
    }

    let tag_counts = recurring_tag_counts(by_chunk.values());
    let now_ms = now_ms() as i64;
    let mut breakdowns = HashMap::new();
    let mut takeaways = by_chunk
        .into_values()
        .map(|mut takeaway| {
            let breakdown = priority_breakdown(&takeaway, &tag_counts, hit_stats, now_ms);
            takeaway.priority = breakdown.total;
            breakdowns.insert(takeaway.chunk_id.clone(), breakdown);
            takeaway
        })
        .collect::<Vec<_>>();
    let scored_takeaways = takeaways.clone();
    let mut suppressed_reasons = suppress_finishes_covered_by_libraries(&mut takeaways)
        .into_iter()
        .map(|id| (id, "covered_by_library".to_string()))
        .collect::<HashMap<_, _>>();
    suppressed_reasons.extend(filter_startup_takeaways(&mut takeaways));
    takeaways.sort_by(|left, right| {
        right
            .priority
            .partial_cmp(&left.priority)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
    });
    let displayed_ids = takeaways
        .iter()
        .take(limit)
        .enumerate()
        .map(|(idx, takeaway)| (takeaway.chunk_id.clone(), idx + 1))
        .collect::<HashMap<_, _>>();
    takeaways.truncate(limit);
    finalize_candidate_explanations(
        &mut explanations,
        &scored_takeaways,
        &suppressed_reasons,
        &displayed_ids,
        &breakdowns,
    );
    Ok(RankedTakeawayCollection {
        takeaways,
        explanations,
    })
}

/// Drop raw `task:kind:task_finish` takeaways already represented by
/// a `highlight_library` or `project_brief` digest. The digest's
/// summary line `Covers tasks: task:id:<X>, ...` lists the source
/// task IDs (emitted in `ensure_highlight_library_digest` and
/// `build_project_brief_digest_artifact`). User-tagged lessons with
/// explicit `priority:N` >= USER_PRESERVE_PRIORITY_THRESHOLD survive.
///
/// The covered set is keyed by `(project_id, task_id)` so a digest
/// from one project never suppresses a same-id finish from another —
/// this matters for the machine-wide section of `memory.md`, which
/// spans projects.
pub(super) fn suppress_finishes_covered_by_libraries(
    takeaways: &mut Vec<Takeaway>,
) -> BTreeSet<String> {
    let covered: BTreeSet<(Option<String>, String)> = takeaways
        .iter()
        .filter(|takeaway| is_library_digest(&takeaway.tags))
        .flat_map(|takeaway| {
            extract_covered_task_ids(&takeaway.text)
                .into_iter()
                .map(|id| (takeaway.project_id.clone(), id))
                .collect::<Vec<_>>()
        })
        .collect();
    if covered.is_empty() {
        return BTreeSet::new();
    }
    let suppressed = takeaways
        .iter()
        .filter(|takeaway| is_suppressible_finish(takeaway, &covered))
        .map(|takeaway| takeaway.chunk_id.clone())
        .collect::<BTreeSet<_>>();
    takeaways.retain(|takeaway| !suppressed.contains(&takeaway.chunk_id));
    suppressed
}

pub(super) fn filter_startup_takeaways(takeaways: &mut Vec<Takeaway>) -> HashMap<String, String> {
    let mut suppressed = HashMap::new();
    for takeaway in takeaways.iter() {
        let reason = if has_boilerplate_action(takeaway) {
            Some("boilerplate_action")
        } else {
            None
        };
        if let Some(reason) = reason {
            suppressed.insert(takeaway.chunk_id.clone(), reason.to_string());
        }
    }
    takeaways.retain(|takeaway| !suppressed.contains_key(&takeaway.chunk_id));
    suppressed
}

pub(super) fn takeaway_preferred(candidate: &Takeaway, current: &Takeaway) -> bool {
    candidate
        .priority
        .partial_cmp(&current.priority)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| candidate.timestamp_created.cmp(&current.timestamp_created))
        .then_with(|| current.chunk_id.cmp(&candidate.chunk_id))
        .is_gt()
}

pub(super) fn topic_key(takeaway: &Takeaway) -> String {
    let mut tag_key = takeaway
        .tags
        .iter()
        .filter(|tag| tag.starts_with("topic:") || tag.starts_with("task:id:"))
        .cloned()
        .collect::<Vec<_>>();
    tag_key.sort();
    tag_key.dedup();
    if !tag_key.is_empty() {
        return tag_key.into_iter().take(4).collect::<Vec<_>>().join("|");
    }

    normalized_topic_terms(&takeaway.text)
}

pub(super) type ScopedSuppressionReasons = HashMap<(String, String), String>;

pub(super) fn assign_active_project_candidates(
    project_takeaways: &mut Vec<Takeaway>,
    global_takeaways: &mut Vec<Takeaway>,
    project_explanations: &mut Vec<MemoryMdCandidateExplanation>,
    global_explanations: &mut Vec<MemoryMdCandidateExplanation>,
    active_project_id: Option<&str>,
) {
    let Some(active_project_id) = active_project_id else {
        return;
    };

    let mut retained_global = Vec::with_capacity(global_takeaways.len());
    for takeaway in global_takeaways.drain(..) {
        if takeaway.project_id.as_deref() == Some(active_project_id) {
            if let Some(existing) = project_takeaways
                .iter_mut()
                .find(|existing| existing.chunk_id == takeaway.chunk_id)
            {
                merge_takeaway_evidence(existing, takeaway);
            } else {
                project_takeaways.push(takeaway);
            }
        } else {
            retained_global.push(takeaway);
        }
    }
    *global_takeaways = retained_global;

    let mut retained_explanations = Vec::with_capacity(global_explanations.len());
    for mut explanation in global_explanations.drain(..) {
        if explanation.project_id.as_deref() == Some(active_project_id) {
            explanation.section = "project".to_string();
            project_explanations.push(explanation);
        } else {
            retained_explanations.push(explanation);
        }
    }
    *global_explanations = retained_explanations;
}

pub(super) fn merge_takeaway_evidence(existing: &mut Takeaway, incoming: Takeaway) {
    existing.score = existing.score.max(incoming.score);
    existing.priority = existing.priority.max(incoming.priority);
    existing.timestamp_created = existing.timestamp_created.max(incoming.timestamp_created);
    existing.occurrences = existing.occurrences.saturating_add(incoming.occurrences);
    existing.sources.extend(incoming.sources);
    for tag in incoming.tags {
        if !existing.tags.contains(&tag) {
            existing.tags.push(tag);
        }
    }
}

pub(super) fn recompute_union_priorities(
    project_takeaways: &mut [Takeaway],
    global_takeaways: &mut [Takeaway],
    hit_stats: &HashMap<String, HitStats>,
    now_ms: i64,
) -> HashMap<String, PriorityBreakdown> {
    let tag_counts = recurring_tag_counts(project_takeaways.iter().chain(global_takeaways.iter()));
    let mut breakdowns = HashMap::new();
    for takeaway in project_takeaways
        .iter_mut()
        .chain(global_takeaways.iter_mut())
    {
        let breakdown = priority_breakdown(takeaway, &tag_counts, hit_stats, now_ms);
        takeaway.priority = breakdown.total;
        breakdowns.insert(takeaway.chunk_id.clone(), breakdown);
    }
    breakdowns
}

pub(super) fn suppress_unrelated_machine_takeaways(
    global_takeaways: &mut Vec<Takeaway>,
) -> ScopedSuppressionReasons {
    let suppressed = global_takeaways
        .iter()
        .filter(|takeaway| !is_machine_wide_startup_relevant(takeaway))
        .map(|takeaway| {
            (
                ("machine_wide".to_string(), takeaway.chunk_id.clone()),
                "machine_wide_unrelated".to_string(),
            )
        })
        .collect::<ScopedSuppressionReasons>();
    global_takeaways.retain(|takeaway| {
        !suppressed.contains_key(&("machine_wide".to_string(), takeaway.chunk_id.clone()))
    });
    suppressed
}

pub(super) fn sort_takeaways(takeaways: &mut [Takeaway]) {
    takeaways.sort_by(|left, right| {
        right
            .priority
            .partial_cmp(&left.priority)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.timestamp_created.cmp(&left.timestamp_created))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
}

pub(super) fn dedupe_memory_md_union(
    project_takeaways: &mut Vec<Takeaway>,
    global_takeaways: &mut Vec<Takeaway>,
) -> ScopedSuppressionReasons {
    let components = duplicate_components(project_takeaways, global_takeaways);
    let mut suppressed = ScopedSuppressionReasons::new();

    for component in components
        .into_iter()
        .filter(|component| component.len() > 1)
    {
        let winner = component
            .iter()
            .copied()
            .reduce(|current, candidate| {
                if union_member_preferred(candidate, current, project_takeaways, global_takeaways) {
                    candidate
                } else {
                    current
                }
            })
            .expect("duplicate component is non-empty");
        let winner_takeaway = union_member_takeaway(winner, project_takeaways, global_takeaways);

        for member in component.into_iter().filter(|member| *member != winner) {
            let takeaway = union_member_takeaway(member, project_takeaways, global_takeaways);
            suppressed.insert(
                (
                    union_member_section(member).to_string(),
                    takeaway.chunk_id.clone(),
                ),
                duplicate_reason(takeaway, winner_takeaway),
            );
        }
    }

    project_takeaways.retain(|takeaway| {
        !suppressed.contains_key(&("project".to_string(), takeaway.chunk_id.clone()))
    });
    global_takeaways.retain(|takeaway| {
        !suppressed.contains_key(&("machine_wide".to_string(), takeaway.chunk_id.clone()))
    });
    suppressed
}

pub(super) fn duplicate_components(
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
) -> Vec<Vec<(bool, usize)>> {
    let members = (0..project_takeaways.len())
        .map(|index| (true, index))
        .chain((0..global_takeaways.len()).map(|index| (false, index)))
        .collect::<Vec<_>>();
    let mut parent = (0..members.len()).collect::<Vec<_>>();

    for left in 0..members.len() {
        for right in (left + 1)..members.len() {
            let left_takeaway =
                union_member_takeaway(members[left], project_takeaways, global_takeaways);
            let right_takeaway =
                union_member_takeaway(members[right], project_takeaways, global_takeaways);
            if takeaways_duplicate(left_takeaway, right_takeaway) {
                union_components(&mut parent, left, right);
            }
        }
    }

    let mut by_root: BTreeMap<usize, Vec<(bool, usize)>> = BTreeMap::new();
    for (flat_index, member) in members.into_iter().enumerate() {
        let root = find_component(&mut parent, flat_index);
        by_root.entry(root).or_default().push(member);
    }
    by_root.into_values().collect()
}

pub(super) fn find_component(parent: &mut [usize], mut index: usize) -> usize {
    while parent[index] != index {
        parent[index] = parent[parent[index]];
        index = parent[index];
    }
    index
}

pub(super) fn union_components(parent: &mut [usize], left: usize, right: usize) {
    let left_root = find_component(parent, left);
    let right_root = find_component(parent, right);
    if left_root != right_root {
        parent[right_root] = left_root;
    }
}

pub(super) fn union_member_takeaway<'a>(
    member: (bool, usize),
    project_takeaways: &'a [Takeaway],
    global_takeaways: &'a [Takeaway],
) -> &'a Takeaway {
    if member.0 {
        &project_takeaways[member.1]
    } else {
        &global_takeaways[member.1]
    }
}

pub(super) fn union_member_section(member: (bool, usize)) -> &'static str {
    if member.0 {
        "project"
    } else {
        "machine_wide"
    }
}

pub(super) fn union_member_preferred(
    candidate: (bool, usize),
    current: (bool, usize),
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
) -> bool {
    if candidate.0 != current.0 {
        return candidate.0;
    }
    takeaway_preferred(
        union_member_takeaway(candidate, project_takeaways, global_takeaways),
        union_member_takeaway(current, project_takeaways, global_takeaways),
    )
}

pub(super) fn takeaways_duplicate(left: &Takeaway, right: &Takeaway) -> bool {
    left.chunk_id == right.chunk_id
        || topic_equivalent(left, right)
        || lineage_equivalent(left, right)
}

pub(super) fn topic_equivalent(left: &Takeaway, right: &Takeaway) -> bool {
    let left_explicit = explicit_topic_keys(left);
    let right_explicit = explicit_topic_keys(right);
    if !left_explicit.is_empty() && !right_explicit.is_empty() {
        return left_explicit.iter().any(|key| right_explicit.contains(key));
    }

    let left_terms = normalized_topic_word_set(&left.text);
    let right_terms = normalized_topic_word_set(&right.text);
    if left_terms.len() < 4 || right_terms.len() < 4 {
        return false;
    }
    let intersection = left_terms.intersection(&right_terms).count();
    let union = left_terms.union(&right_terms).count();
    union > 0 && intersection as f32 / union as f32 >= 0.75
}

pub(super) fn explicit_topic_keys(takeaway: &Takeaway) -> BTreeSet<String> {
    takeaway
        .tags
        .iter()
        .filter(|tag| tag.starts_with("topic:") || tag.starts_with("task:id:"))
        .cloned()
        .collect()
}

pub(super) fn normalized_topic_word_set(text: &str) -> BTreeSet<String> {
    normalized_topic_terms(text)
        .split('-')
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

pub(super) fn lineage_equivalent(left: &Takeaway, right: &Takeaway) -> bool {
    let left_ids = lineage_identity_set(left);
    let right_ids = lineage_identity_set(right);
    left_ids.iter().any(|id| right_ids.contains(id))
}

pub(super) fn lineage_identity_set(takeaway: &Takeaway) -> BTreeSet<String> {
    let mut ids = BTreeSet::from([takeaway.chunk_id.clone()]);
    for tag in &takeaway.tags {
        let Some(raw_ids) = tag
            .strip_prefix("supersedes:")
            .or_else(|| tag.strip_prefix("derives_from:"))
        else {
            continue;
        };
        ids.extend(
            raw_ids
                .split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
        );
    }
    ids
}

pub(super) fn duplicate_reason(candidate: &Takeaway, winner: &Takeaway) -> String {
    if candidate.chunk_id == winner.chunk_id {
        format!("duplicate_exact:{}", winner.chunk_id)
    } else if topic_equivalent(candidate, winner) {
        format!("duplicate_topic:{}", topic_key(winner))
    } else if lineage_equivalent(candidate, winner) {
        format!("duplicate_lineage:{}", winner.chunk_id)
    } else {
        format!("duplicate_cluster:{}", winner.chunk_id)
    }
}

pub(super) fn reconcile_candidate_explanations(
    explanations: &mut [MemoryMdCandidateExplanation],
    section: &str,
    displayed_takeaways: &[Takeaway],
    suppressed: &ScopedSuppressionReasons,
    breakdowns: &HashMap<String, PriorityBreakdown>,
) {
    let displayed = displayed_takeaways
        .iter()
        .enumerate()
        .map(|(index, takeaway)| (takeaway.chunk_id.as_str(), index + 1))
        .collect::<HashMap<_, _>>();
    for explanation in explanations {
        let can_reconcile = explanation.filter_reason.is_none()
            || explanation.filter_reason.as_deref() == Some("below_display_limit");
        if !can_reconcile {
            continue;
        }
        if let Some(breakdown) = breakdowns.get(&explanation.chunk_id) {
            explanation.priority_score = Some(breakdown.total);
            explanation.priority_breakdown = Some(breakdown.clone());
        }
        if let Some(reason) = suppressed.get(&(section.to_string(), explanation.chunk_id.clone())) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some(reason.clone());
            if !explanation.quality_flags.contains(reason) {
                explanation.quality_flags.push(reason.clone());
            }
            explanation.display_rank = None;
        } else if let Some(rank) = displayed.get(explanation.chunk_id.as_str()) {
            explanation.display_status = "displayed".to_string();
            explanation.filter_reason = None;
            explanation.display_rank = Some(*rank);
        } else {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("below_display_limit".to_string());
            explanation.display_rank = None;
        }
    }
}

pub(super) fn normalized_topic_terms(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or(text);
    let mut words = Vec::new();
    for raw in first_line.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        let word = raw.to_ascii_lowercase();
        if word.len() < 4 || TOPIC_STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        words.push(word);
        if words.len() >= 8 {
            break;
        }
    }
    if words.is_empty() {
        summarize_text(text, 64).to_ascii_lowercase()
    } else {
        words.join("-")
    }
}

pub(super) const TOPIC_STOPWORDS: &[&str] = &[
    "after",
    "also",
    "because",
    "before",
    "from",
    "into",
    "keep",
    "lesson",
    "lessons",
    "memory",
    "project",
    "records",
    "task",
    "that",
    "this",
    "validation",
    "with",
];

/// True only for system-generated library digests. The
/// `task:status:generated` requirement guards against a user-authored
/// chunk spoofing a `task:role:*` tag to suppress real finishes.
pub(super) fn is_library_digest(tags: &[String]) -> bool {
    let generated = tags.iter().any(|tag| tag == "task:status:generated");
    let role = tags
        .iter()
        .any(|tag| tag == "task:role:highlight_library" || tag == "task:role:project_brief");
    generated && role
}

pub(super) fn is_suppressible_finish(
    takeaway: &Takeaway,
    covered: &BTreeSet<(Option<String>, String)>,
) -> bool {
    let is_finish = takeaway
        .tags
        .iter()
        .any(|tag| tag == "task:kind:task_finish");
    if !is_finish {
        return false;
    }
    if user_priority_at_least(&takeaway.tags, USER_PRESERVE_PRIORITY_THRESHOLD) {
        return false;
    }
    takeaway.tags.iter().any(|tag| {
        tag.strip_prefix("task:id:")
            .map(|id| covered.contains(&(takeaway.project_id.clone(), id.to_string())))
            .unwrap_or(false)
    })
}

/// True if `tags` carry an explicit `priority:`/`importance:` value at
/// or above `threshold`. Parsed as `f32` to mirror `explicit_priority`
/// so a decimal tag like `priority:8.5` is honoured.
pub(super) fn user_priority_at_least(tags: &[String], threshold: u8) -> bool {
    tags.iter().any(|tag| {
        let value = tag
            .strip_prefix("priority:")
            .or_else(|| tag.strip_prefix("importance:"));
        match value.and_then(|v| v.parse::<f32>().ok()) {
            Some(n) => n.is_finite() && n >= threshold as f32,
            None => false,
        }
    })
}

pub(super) fn extract_covered_task_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("Covers tasks:") else {
            continue;
        };
        for token in rest.split(',') {
            let token = token.trim();
            if let Some(id) = token.strip_prefix("task:id:") {
                let id = id.trim().trim_end_matches('.').trim_end_matches(';');
                if !id.is_empty() {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
}

pub(super) fn merge_payload_candidates(
    by_chunk: &mut HashMap<String, Takeaway>,
    explanations: &mut Vec<MemoryMdCandidateExplanation>,
    payload: &Value,
    section: &str,
    source: &str,
    query: &str,
    mode: CliQueryMode,
) {
    let Some(results) = payload.get("results").and_then(Value::as_array) else {
        return;
    };
    for (idx, result) in results.iter().enumerate() {
        let Some(chunk_id) = result.get("chunk_id").and_then(Value::as_str) else {
            continue;
        };
        let text = result
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let tags = result
            .get("tags")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut explanation =
            candidate_explanation(section, source, query, mode, idx + 1, result, &tags, text);
        if text.is_empty() {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("empty_text".to_string());
            explanations.push(explanation);
            continue;
        }
        if is_generated_digest_takeaway(&tags) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("generated_digest_wrapper".to_string());
            explanations.push(explanation);
            continue;
        }
        if is_fragment_like_candidate(&tags, text) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("fragment_like".to_string());
            explanation.quality_flags.push("fragment_like".to_string());
            explanations.push(explanation);
            continue;
        }
        // Defence-in-depth: the lifecycle visibility filter already
        // hides superseded chunks, but skip anything still carrying a
        // `kind:superseded` tag so consolidated output never competes
        // with the raw chunks it replaced.
        if tags.iter().any(|tag| tag.starts_with("kind:superseded")) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("superseded_tag".to_string());
            explanations.push(explanation);
            continue;
        }
        let score = result.get("score").and_then(Value::as_f64).unwrap_or(0.0) as f32;

        let entry = by_chunk
            .entry(chunk_id.to_string())
            .or_insert_with(|| Takeaway {
                chunk_id: chunk_id.to_string(),
                tenant_id: result
                    .get("tenant_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                project_id: result
                    .get("project_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                text: text.to_string(),
                score,
                priority: 0.0,
                chunk_type: result
                    .get("chunk_type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                timestamp_created: result
                    .get("timestamp_created")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                tags,
                sources: BTreeSet::new(),
                occurrences: 0,
            });
        entry.score = entry.score.max(score);
        entry.occurrences = entry.occurrences.saturating_add(1);
        entry.sources.insert(source.to_string());
        explanations.push(explanation);
    }
}

// This pure builder mirrors the serialized explanation record field-for-field.
#[allow(clippy::too_many_arguments)]
pub(super) fn candidate_explanation(
    section: &str,
    source: &str,
    query: &str,
    mode: CliQueryMode,
    raw_rank: usize,
    result: &Value,
    tags: &[String],
    text: &str,
) -> MemoryMdCandidateExplanation {
    let generated_digest = is_generated_digest_takeaway(tags)
        || text
            .to_ascii_lowercase()
            .contains("task digest status generated");
    let mut quality_flags = Vec::new();
    if is_fragment_like_candidate(tags, text) {
        quality_flags.push("fragment_like".to_string());
    }
    if generated_digest
        || text
            .to_ascii_lowercase()
            .contains("task digest status generated")
    {
        quality_flags.push("generated_wrapper".to_string());
    }
    MemoryMdCandidateExplanation {
        section: section.to_string(),
        source: source.to_string(),
        query: query.to_string(),
        mode: query_mode_label(mode).to_string(),
        raw_rank,
        chunk_id: result
            .get("chunk_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tenant_id: result
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        project_id: result
            .get("project_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        chunk_type: result
            .get("chunk_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        timestamp_created: result
            .get("timestamp_created")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        search_score: result.get("score").and_then(Value::as_f64).unwrap_or(0.0) as f32,
        priority_score: None,
        priority_breakdown: None,
        display_status: "candidate".to_string(),
        filter_reason: None,
        display_rank: None,
        generated_digest,
        quality_flags,
        topic_key: None,
        tags: tags.to_vec(),
        matched_sources: vec![source.to_string()],
    }
}

pub(super) fn finalize_candidate_explanations(
    explanations: &mut [MemoryMdCandidateExplanation],
    scored_takeaways: &[Takeaway],
    suppressed_reasons: &HashMap<String, String>,
    displayed_ids: &HashMap<String, usize>,
    breakdowns: &HashMap<String, PriorityBreakdown>,
) {
    let by_id = scored_takeaways
        .iter()
        .map(|takeaway| (takeaway.chunk_id.as_str(), takeaway))
        .collect::<HashMap<_, _>>();
    for explanation in explanations {
        if explanation.display_status == "filtered" {
            continue;
        }
        if let Some(takeaway) = by_id.get(explanation.chunk_id.as_str()) {
            explanation.priority_score = Some(takeaway.priority);
            explanation.priority_breakdown = breakdowns.get(&takeaway.chunk_id).cloned();
            explanation.matched_sources = takeaway.sources.iter().cloned().collect();
            explanation.topic_key = Some(topic_key(takeaway));
        }
        if let Some(reason) = suppressed_reasons.get(&explanation.chunk_id) {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some(reason.clone());
            explanation.quality_flags.push(reason.clone());
        } else if let Some(rank) = displayed_ids.get(&explanation.chunk_id) {
            explanation.display_status = "displayed".to_string();
            explanation.display_rank = Some(*rank);
        } else {
            explanation.display_status = "filtered".to_string();
            explanation.filter_reason = Some("below_display_limit".to_string());
        }
    }
}

pub(super) fn query_mode_label(mode: CliQueryMode) -> &'static str {
    match mode {
        CliQueryMode::Generic => "generic",
        CliQueryMode::BriefProject => "brief_project",
        CliQueryMode::ResumeTask => "resume_task",
        CliQueryMode::FindFailures => "find_failures",
        CliQueryMode::FindDecisions => "find_decisions",
        CliQueryMode::FindEvidence => "find_evidence",
        CliQueryMode::FindHighlights => "find_highlights",
    }
}

pub(super) fn recurring_tag_counts<'a>(
    takeaways: impl Iterator<Item = &'a Takeaway>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for takeaway in takeaways {
        for tag in takeaway.tags.iter().filter(|tag| high_signal_tag(tag)) {
            *counts.entry(tag.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

#[cfg(test)]
pub(super) fn priority_score(
    takeaway: &Takeaway,
    tag_counts: &HashMap<String, usize>,
    hit_stats: &HashMap<String, HitStats>,
    now_ms: i64,
) -> f32 {
    priority_breakdown(takeaway, tag_counts, hit_stats, now_ms).total
}

pub(super) fn priority_breakdown(
    takeaway: &Takeaway,
    tag_counts: &HashMap<String, usize>,
    hit_stats: &HashMap<String, HitStats>,
    now_ms: i64,
) -> PriorityBreakdown {
    let explicit = explicit_priority(&takeaway.tags).unwrap_or(0.0);
    let kind_weight = takeaway
        .tags
        .iter()
        .map(|tag| match tag.as_str() {
            "kind:decision" => 12.0,
            "kind:finish" => 10.0,
            "kind:evidence" => 8.0,
            "kind:run" => 5.0,
            "kind:progress" => 3.0,
            _ if tag.starts_with("ctx:file:") => 5.0,
            _ if tag.starts_with("ctx:subsystem:") => 4.0,
            _ => 0.0,
        })
        .sum::<f32>();
    let type_weight = match takeaway.chunk_type.as_str() {
        "decision" => 10.0,
        "summary" => 6.0,
        "research" => 5.0,
        "trace" => 3.0,
        "plan" => 2.0,
        _ => 0.0,
    };
    let recurrence = takeaway
        .tags
        .iter()
        .filter_map(|tag| tag_counts.get(tag))
        .map(|count| count.saturating_sub(1).min(5) as f32)
        .sum::<f32>();
    let multi_query = takeaway.occurrences.saturating_sub(1).min(4) as f32 * 3.0;
    let search_score = takeaway.score.clamp(0.0, 25.0) * 2.0;
    let library_bonus = if takeaway.tags.iter().any(|tag| {
        tag == "task:role:highlight_library"
            || tag == "task:role:project_brief"
            || tag.starts_with("kind:consolidated")
    }) {
        15.0
    } else {
        0.0
    };

    // Exposure is not evidence of usefulness. Keep it only to distinguish
    // never-exposed stale chunks; verified outcomes drive adaptive ranking.
    let utility = 0.0;
    let staleness_penalty = if !hit_stats.contains_key(&takeaway.chunk_id)
        && takeaway.timestamp_created > 0
        && now_ms.saturating_sub(takeaway.timestamp_created) > STALE_CHUNK_AGE_MS
    {
        -2.0
    } else {
        0.0
    };

    let total = explicit
        + kind_weight
        + type_weight
        + recurrence
        + multi_query
        + search_score
        + library_bonus
        + utility
        + staleness_penalty;

    PriorityBreakdown {
        explicit,
        kind_weight,
        type_weight,
        recurrence,
        multi_query,
        search_score,
        library_bonus,
        utility,
        staleness_penalty,
        total,
    }
}

pub(super) fn explicit_priority(tags: &[String]) -> Option<f32> {
    tags.iter().find_map(|tag| {
        let value = tag
            .strip_prefix("priority:")
            .or_else(|| tag.strip_prefix("importance:"))?;
        let parsed = value.parse::<f32>().ok()?;
        if !parsed.is_finite() {
            // `priority:inf` / `priority:nan` must not pin a takeaway at max
            // rank with permanent suppression immunity.
            return None;
        }
        let scaled = if parsed <= 10.0 {
            parsed * 10.0
        } else {
            parsed.min(100.0)
        };
        Some(scaled.clamp(0.0, 100.0))
    })
}

pub(super) fn high_signal_tag(tag: &str) -> bool {
    tag.starts_with("kind:")
        || tag.starts_with("ctx:")
        || tag.starts_with("priority:")
        || tag.starts_with("importance:")
}

pub(super) fn is_generated_digest_takeaway(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == "task:status:generated")
        && tags
            .iter()
            .any(|tag| tag.starts_with("task:role:") || tag.starts_with("task:digest:"))
}

pub(super) fn is_fragment_like_takeaway(takeaway: &Takeaway) -> bool {
    is_fragment_like_candidate(&takeaway.tags, &takeaway.text)
}

pub(super) fn is_fragment_like_candidate(tags: &[String], text: &str) -> bool {
    if tags.iter().any(|tag| {
        tag.strip_prefix("chunk_index:")
            .and_then(|value| value.parse::<usize>().ok())
            .map(|idx| idx > 0)
            .unwrap_or(false)
    }) {
        return true;
    }

    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with("...")
        || trimmed
            .chars()
            .next()
            .map(|ch| matches!(ch, ',' | ';' | ':' | ')' | ']' | '}'))
            .unwrap_or(false)
    {
        return true;
    }

    let lowered = trimmed.to_ascii_lowercase();
    const CONTINUATION_PREFIXES: &[&str] = &[
        "and ",
        "but ",
        "which ",
        "where ",
        "then ",
        "therefore ",
        "from there ",
        "as a result ",
    ];
    CONTINUATION_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
}

pub(super) fn has_boilerplate_action(takeaway: &Takeaway) -> bool {
    explicit_agent_action(&takeaway.text).is_none()
        && takeaway_category(takeaway).reason == "ranked project takeaway"
}

pub(super) fn is_machine_wide_startup_relevant(takeaway: &Takeaway) -> bool {
    if takeaway.project_id.is_none() {
        return true;
    }
    if user_priority_at_least(&takeaway.tags, USER_PRESERVE_PRIORITY_THRESHOLD)
        && takeaway.tags.iter().any(|tag| {
            tag == "global"
                || tag == "always"
                || tag == "machine"
                || tag == "kind:consolidated"
                || tag.starts_with("global:")
                || tag.starts_with("machine:")
        })
    {
        return true;
    }
    let lowered = takeaway.text.to_ascii_lowercase();
    lowered.contains("machine-wide")
        || lowered.contains("all projects")
        || lowered.contains("global default")
        || lowered.contains("always ")
}

pub(super) fn takeaway_category(takeaway: &Takeaway) -> TakeawayCategory {
    let lowered = takeaway.text.to_ascii_lowercase();
    let has_tag = |needle: &str| takeaway.tags.iter().any(|tag| tag == needle);
    let has_source = |needle: &str| takeaway.sources.iter().any(|source| source == needle);

    if takeaway.chunk_type == "decision"
        || has_tag("kind:decision")
        || lowered.contains("decision:")
        || lowered.contains("rationale:")
    {
        return category("Decisions", "decision or rationale");
    }
    if lowered.contains("fix:")
        || lowered.contains("validated fix")
        || lowered.contains("validated:")
        || lowered.contains("validation:")
        || lowered.contains("fixed by")
        || lowered.contains("solution:")
        || lowered.contains("passed")
        || lowered.contains("confirmed")
        || lowered.contains("reproduced")
        || lowered.contains("0 failures")
        || lowered.contains("no failures")
        || lowered.contains("resolved after")
        || lowered.contains("resolved by")
    {
        return category("Validated Fixes", "validated fix or result");
    }
    // Require a real failure signal: a bare "failure" mention ("0
    // failures") or arrival via the *_failures retrieval query is not
    // failure evidence — filing successes here inverts their meaning.
    if has_tag("kind:failure")
        || lowered.contains("root cause")
        || lowered.contains("failed because")
        || lowered.contains("failure:")
        || lowered.contains("blocker")
    {
        return category("Known Failures", "failure or root-cause evidence");
    }
    if lowered.contains("command:")
        || lowered.contains("path:")
        || lowered.contains("parameter:")
        || lowered.contains("parameters:")
        || lowered.contains("/home/")
        || lowered.contains("crates/")
        || lowered.contains("tasks/")
        || lowered.contains(".rs")
        || lowered.contains(".md")
        || lowered.contains("http://")
        || lowered.contains("https://")
    {
        return category("Commands/Paths", "command, path, or parameter evidence");
    }
    if lowered.contains("next step:")
        || lowered.contains("follow-up:")
        || lowered.contains("followup:")
        || lowered.contains("followups:")
    {
        return category("Open Follow-ups", "explicit follow-up");
    }
    if has_tag("kind:evidence") || has_tag("kind:run") || has_source("project_highlights") {
        return category("Evidence", "evidence or run result");
    }

    category("Other Takeaways", "ranked project takeaway")
}

pub(super) fn category(heading: &'static str, reason: &'static str) -> TakeawayCategory {
    let order = TAKEAWAY_CATEGORIES
        .iter()
        .find_map(|(candidate, order)| (*candidate == heading).then_some(*order))
        .unwrap_or(u8::MAX);
    TakeawayCategory {
        heading,
        reason,
        order,
    }
}
