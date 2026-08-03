use super::action::*;
use super::collect::{canonical_or_lexical_path, read_text_capped};
use super::*;
use crate::ops::configured_project_aliases;
use crate::store::OutcomePrior;
use crate::types::ChunkStatus;
use std::collections::HashSet;

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
    /// Constant 1 since scan-first selection replaced the multi-query
    /// fan-out; kept for struct stability.
    #[allow(dead_code)]
    pub(super) occurrences: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(super) struct PriorityBreakdown {
    pub(super) explicit: f32,
    pub(super) kind_weight: f32,
    pub(super) type_weight: f32,
    pub(super) recurrence: f32,
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

/// One tenant-wide metadata scan replacing the former canned-query
/// fan-out: every stored chunk is a candidate, partitioned into
/// (project, machine-wide) by `chunk.project_id == active_project`.
/// Configured project aliases count as the active project so
/// historically mis-scoped chunks stay readable, matching the scope
/// expansion the search path applies.
pub(super) async fn scan_takeaway_candidates<S: Store>(
    store: &S,
    tenant: &TenantId,
    active_project: Option<&str>,
) -> Result<(
    Vec<Takeaway>,
    Vec<Takeaway>,
    Vec<MemoryMdCandidateExplanation>,
)> {
    let alias_scopes = active_project
        .map(|project| configured_project_aliases(tenant, project))
        .unwrap_or_default();
    let mut project_takeaways = Vec::new();
    let mut global_takeaways = Vec::new();
    let mut explanations = Vec::new();
    let mut offset = 0usize;
    let mut raw_rank = 0usize;
    while offset < READABLE_SCAN_MAX_METADATA_ROWS {
        let limit = READABLE_SCAN_PAGE_SIZE.min(READABLE_SCAN_MAX_METADATA_ROWS - offset);
        let chunks = store.list_chunks(tenant, limit, offset).await?;
        if chunks.is_empty() {
            break;
        }
        let fetched = chunks.len();
        for chunk in chunks {
            raw_rank += 1;
            let is_project = chunk.project_id.as_option() == active_project
                || alias_scopes.iter().any(|alias| {
                    alias.origin_tenant_id == chunk.tenant_id.as_str()
                        && alias.origin_project_id.as_deref() == chunk.project_id.as_option()
                });
            let section = if is_project {
                "project"
            } else {
                "machine_wide"
            };
            let takeaway = Takeaway {
                chunk_id: chunk.chunk_id.to_string(),
                tenant_id: chunk.tenant_id.to_string(),
                project_id: chunk.project_id.as_option().map(str::to_string),
                text: chunk.text.trim().to_string(),
                score: 0.0,
                priority: 0.0,
                chunk_type: chunk.chunk_type.to_string(),
                timestamp_created: chunk.timestamp_created,
                tags: chunk.tags.clone(),
                sources: BTreeSet::from(["scan".to_string()]),
                occurrences: 1,
            };
            let mut explanation = MemoryMdCandidateExplanation {
                section: section.to_string(),
                source: "scan".to_string(),
                query: String::new(),
                mode: "scan".to_string(),
                raw_rank,
                chunk_id: takeaway.chunk_id.clone(),
                tenant_id: takeaway.tenant_id.clone(),
                project_id: takeaway.project_id.clone(),
                chunk_type: takeaway.chunk_type.clone(),
                timestamp_created: takeaway.timestamp_created,
                search_score: takeaway.score,
                priority_score: None,
                priority_breakdown: None,
                display_status: "candidate".to_string(),
                filter_reason: None,
                display_rank: None,
                generated_digest: is_generated_digest_takeaway(&takeaway.tags),
                quality_flags: Vec::new(),
                topic_key: None,
                tags: takeaway.tags.clone(),
                matched_sources: vec!["scan".to_string()],
            };
            match scan_candidate_filter_reason(chunk.status, &takeaway.tags, &takeaway.text) {
                Some(reason) => {
                    explanation.display_status = "filtered".to_string();
                    explanation.filter_reason = Some(reason.to_string());
                    explanation.quality_flags.push(reason.to_string());
                    explanations.push(explanation);
                }
                None => {
                    explanations.push(explanation);
                    if is_project {
                        project_takeaways.push(takeaway);
                    } else {
                        global_takeaways.push(takeaway);
                    }
                }
            }
        }
        if fetched < limit {
            break;
        }
        offset = offset.saturating_add(limit);
    }
    Ok((project_takeaways, global_takeaways, explanations))
}

/// Admission predicate for the tenant-wide scan. Mirrors the filters
/// the former per-query candidate merge applied, plus a lifecycle
/// check because `list_chunks` (unlike search) can surface
/// superseded/expired/error chunks.
pub(super) fn scan_candidate_filter_reason(
    status: ChunkStatus,
    tags: &[String],
    text: &str,
) -> Option<&'static str> {
    if matches!(
        status,
        ChunkStatus::Candidate
            | ChunkStatus::Deleted
            | ChunkStatus::Error
            | ChunkStatus::Superseded
            | ChunkStatus::Expired
    ) {
        return Some("not_visible");
    }
    if text.is_empty() {
        return Some("empty_text");
    }
    if is_generated_digest_takeaway(tags) {
        return Some("generated_digest_wrapper");
    }
    if is_fragment_like_candidate(tags, text) {
        return Some("fragment_like");
    }
    // Defence-in-depth: skip anything still carrying a
    // `kind:superseded` tag so consolidated output never competes
    // with the raw chunks it replaced.
    if tags.iter().any(|tag| tag.starts_with("kind:superseded")) {
        return Some("superseded_tag");
    }
    // Ephemeral-admitted writes (write admission stamps
    // `admission:ephemeral` alongside the History lifecycle tier) are
    // short-lived hidden context; search hides them via the default
    // visibility policy, so the scan must too.
    if tags.iter().any(|tag| tag == "admission:ephemeral") {
        return Some("ephemeral_admission");
    }
    None
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

/// One repo file's token set for the repo-novelty gate. `path` is
/// relative to the project dir so suppression reasons stay readable.
#[derive(Debug)]
pub(in crate::cli) struct RepoDoc {
    pub(super) path: String,
    pub(super) tokens: HashSet<String>,
}

/// Bounds for the repo-novelty index: at most this many files, each
/// read through `read_text_capped` at 256 KiB.
const REPO_INDEX_MAX_FILES: usize = 200;
const REPO_INDEX_READ_CAP_BYTES: u64 = 256 * 1024;

/// Build the repo-novelty index once per refresh: token sets for the
/// markdown an agent already reads for free — `tasks/**/*.md`,
/// `docs/handoffs/*.md` (not `_archive/`), and the root `README.md` /
/// `CLAUDE.md` / `AGENTS.md`. The generated output file itself and
/// anything under `.memd/` are excluded; otherwise the previous
/// refresh's `memory.md` would cover every takeaway it rendered and
/// the next refresh would suppress them all. A missing or unreadable
/// project dir yields an empty index, which makes the gate a no-op.
pub(in crate::cli) fn build_repo_index(project_dir: &Path, output_path: &Path) -> Vec<RepoDoc> {
    let output_norm = canonical_or_lexical_path(output_path);
    let mut files = Vec::new();
    collect_markdown_recursive(&project_dir.join("tasks"), 0, &mut files);
    // Top level only, on purpose: `docs/handoffs/_archive/` stays out.
    if let Ok(entries) = fs::read_dir(project_dir.join("docs/handoffs")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                files.push(path);
            }
        }
    }
    for name in ["README.md", "CLAUDE.md", "AGENTS.md"] {
        let path = project_dir.join(name);
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    files.dedup();

    let mut index = Vec::new();
    for path in files.into_iter().take(REPO_INDEX_MAX_FILES) {
        if canonical_or_lexical_path(&path) == output_norm
            || path.components().any(|part| part.as_os_str() == ".memd")
        {
            continue;
        }
        let Ok((text, _truncated)) = read_text_capped(&path, REPO_INDEX_READ_CAP_BYTES) else {
            continue;
        };
        let tokens = repo_novelty_tokens(&text);
        if tokens.is_empty() {
            continue;
        }
        let path = path
            .strip_prefix(project_dir)
            .unwrap_or(&path)
            .display()
            .to_string();
        index.push(RepoDoc { path, tokens });
    }
    index
}

fn collect_markdown_recursive(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    // Depth cap guards against symlink cycles inside `tasks/`.
    if depth > 8 || files.len() >= REPO_INDEX_MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= REPO_INDEX_MAX_FILES {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_recursive(&path, depth + 1, files);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
}

/// Shared tokenizer for repo docs and takeaways: lowercase runs of
/// `[a-z0-9_./-]` of length >= 5, minus common filler words. Keeping
/// `_./-` glued means paths, flags, and hostnames survive as single
/// high-signal tokens.
pub(super) fn repo_novelty_tokens(text: &str) -> HashSet<String> {
    text.to_ascii_lowercase()
        .split(|ch: char| {
            !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '.' | '/' | '-'))
        })
        .filter(|token| token.len() >= 5 && !REPO_NOVELTY_STOPWORDS.contains(token))
        .map(str::to_string)
        .collect()
}

const REPO_NOVELTY_STOPWORDS: &[&str] = &[
    "about", "added", "after", "again", "always", "because", "before", "being", "between", "could",
    "doing", "during", "every", "having", "other", "should", "since", "still", "their", "there",
    "these", "those", "through", "under", "until", "using", "where", "which", "while", "without",
    "would",
];

/// The repo-novelty gate: `memory.md` slots are reserved for facts no
/// repo file holds, so a takeaway mostly contained in one indexed doc
/// is suppressed — its content is already free at session start.
/// Covered means >= 0.6 of the takeaway's tokens appear in a single
/// doc. Takeaways with fewer than 8 tokens carry too little signal
/// for a containment test, and user-pinned lessons at or above
/// `USER_PRESERVE_PRIORITY_THRESHOLD` always survive; neither is ever
/// suppressed. Pure set intersection against the prebuilt index — no
/// file reads here.
pub(super) fn suppress_repo_covered(
    takeaways: &mut Vec<Takeaway>,
    index: &[RepoDoc],
) -> HashMap<String, String> {
    let mut suppressed = HashMap::new();
    if index.is_empty() {
        return suppressed;
    }
    for takeaway in takeaways.iter() {
        if user_priority_at_least(&takeaway.tags, USER_PRESERVE_PRIORITY_THRESHOLD) {
            continue;
        }
        if let Some(path) = repo_doc_covering(&takeaway.text, index) {
            suppressed.insert(takeaway.chunk_id.clone(), format!("covered_by_repo:{path}"));
        }
    }
    takeaways.retain(|takeaway| !suppressed.contains_key(&takeaway.chunk_id));
    suppressed
}

/// Containment predicate behind the repo-novelty gate, shared with the
/// outcome scanner: the covering doc's path when >= 0.6 of `text`'s
/// tokens appear in a single indexed doc. Texts with fewer than 8
/// tokens carry too little signal for a containment test and are
/// never covered.
pub(in crate::cli) fn repo_doc_covering<'a>(text: &str, index: &'a [RepoDoc]) -> Option<&'a str> {
    let tokens = repo_novelty_tokens(text);
    if tokens.len() < 8 {
        return None;
    }
    index
        .iter()
        .find(|doc| {
            let overlap = tokens.intersection(&doc.tokens).count();
            overlap as f32 / tokens.len() as f32 >= 0.6
        })
        .map(|doc| doc.path.as_str())
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

pub(super) fn recompute_union_priorities(
    project_takeaways: &mut [Takeaway],
    global_takeaways: &mut [Takeaway],
    hit_stats: &HashMap<String, HitStats>,
    outcome_priors: &HashMap<String, OutcomePrior>,
    now_ms: i64,
) -> HashMap<String, PriorityBreakdown> {
    let tag_counts = recurring_tag_counts(project_takeaways.iter().chain(global_takeaways.iter()));
    let mut breakdowns = HashMap::new();
    for takeaway in project_takeaways
        .iter_mut()
        .chain(global_takeaways.iter_mut())
    {
        let breakdown =
            priority_breakdown(takeaway, &tag_counts, hit_stats, outcome_priors, now_ms);
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
    outcome_priors: &HashMap<String, OutcomePrior>,
    now_ms: i64,
) -> f32 {
    priority_breakdown(takeaway, tag_counts, hit_stats, outcome_priors, now_ms).total
}

pub(super) fn priority_breakdown(
    takeaway: &Takeaway,
    tag_counts: &HashMap<String, usize>,
    hit_stats: &HashMap<String, HitStats>,
    outcome_priors: &HashMap<String, OutcomePrior>,
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
    let library_bonus = if takeaway.tags.iter().any(|tag| {
        tag == "task:role:highlight_library"
            || tag == "task:role:project_brief"
            || tag.starts_with("kind:consolidated")
    }) {
        15.0
    } else {
        0.0
    };

    // Exposure is not evidence of usefulness; hit stats only feed the
    // staleness check below. Utility comes from verified, decayed outcome
    // priors: each net positive credit is worth 4 points, capped at 12 so a
    // well-used chunk lands in the decision tier without overriding explicit
    // priority.
    let utility = outcome_priors
        .get(&takeaway.chunk_id)
        .map(|prior| (prior.positive_weight - prior.negative_weight).clamp(0.0, 3.0) * 4.0)
        .unwrap_or(0.0);
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
        + library_bonus
        + utility
        + staleness_penalty;

    PriorityBreakdown {
        explicit,
        kind_weight,
        type_weight,
        recurrence,
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
