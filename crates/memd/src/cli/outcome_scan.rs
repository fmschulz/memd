//! Passive outcome scanner over Codex session logs.
//!
//! Codex rollout files interleave tool-call inputs (commands, patches)
//! with tool-call outputs. A memd search rendered in an output carries a
//! `retrieval_episode_id` plus the served chunk IDs; a distinctive
//! literal from a served chunk showing up in a *later* tool-call input
//! of the same session is passive evidence the memory was used. Only
//! inputs count as usage — scanning outputs would credit "the agent was
//! shown the chunk again". Verified hits become `external_tool`
//! `accepted` outcome events, which feed `outcome_priors` and the
//! `memory.md` utility term.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

use super::memory_md::{build_repo_index, repo_doc_covering, RepoDoc};
use super::paths::absolutize_project_dir;
use super::scope;
use crate::error::{MemdError, Result};
use crate::store::{
    OutcomeEvent, OutcomeEventId, OutcomeKind, OutcomeVerifier, RetrievalEpisodeId, Store,
};
use crate::types::{ChunkId, TenantId};

/// Shorter literals ("src/main.rs", "--force") are too common to tie a
/// command back to one served chunk.
const MIN_LITERAL_LEN: usize = 12;
const MAX_LITERALS_PER_CHUNK: usize = 10;
const MAX_EXAMPLES: usize = 5;
const MAX_EXAMPLE_EXCERPT_CHARS: usize = 160;
/// Scanned file -> mtime map keeping re-runs from re-writing events.
const STATE_FILE: &str = ".memd/data/outcome_scan_state.json";
const SESSION_DIR_MAX_DEPTH: usize = 8;

#[derive(Debug)]
pub(super) struct OutcomeScanOptions {
    pub(super) project_dir: PathBuf,
    pub(super) sessions_dir: Option<PathBuf>,
    pub(super) since_days: u64,
    pub(super) dry_run: bool,
}

/// One tool-call output that rendered a retrieval episode.
#[derive(Debug)]
struct Serve {
    line: usize,
    episode_id: String,
    chunk_ids: Vec<String>,
}

/// One tool-call input: the executed command or patch text.
#[derive(Debug)]
struct Action {
    line: usize,
    text: String,
}

#[derive(Debug, Default)]
struct SessionSignals {
    serves: Vec<Serve>,
    actions: Vec<Action>,
}

#[derive(Debug)]
struct Hit {
    chunk_id: String,
    literal: String,
    serve_line: usize,
    action_line: usize,
    action_excerpt: String,
}

pub(super) async fn run_outcome_scan<S: Store>(
    store: &S,
    options: OutcomeScanOptions,
) -> Result<Value> {
    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let (tenant_id, _project_id) = scope::resolve_required(&project_dir, None, None)?;
    let tenant = TenantId::new(&tenant_id)?;
    let sessions_dir = match options.sessions_dir {
        Some(dir) => dir,
        None => dirs::home_dir()
            .ok_or_else(|| {
                MemdError::ValidationError(
                    "cannot resolve the home directory for the default sessions dir; pass --sessions-dir".to_string(),
                )
            })?
            .join(".codex")
            .join("sessions"),
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let cutoff_ms = now_ms.saturating_sub((options.since_days as i64).saturating_mul(86_400_000));

    let state_path = project_dir.join(STATE_FILE);
    let mut state = load_scan_state(&state_path);

    let mut session_files = Vec::new();
    collect_jsonl_files(&sessions_dir, 0, &mut session_files);
    session_files.sort();

    // The exclusion path mirrors the memory.md refresh default: indexing
    // the generated memory.md would mark every rendered chunk repo-covered.
    let repo_index: Vec<RepoDoc> = build_repo_index(&project_dir, &project_dir.join("memory.md"));

    let mut files_scanned = 0usize;
    let mut files_skipped_unchanged = 0usize;
    let mut files_skipped_age = 0usize;
    let mut files_unreadable = 0usize;
    let mut files_with_serves = 0usize;
    let mut serves_found = 0usize;
    let mut chunks_skipped_missing = 0usize;
    let mut chunks_skipped_repo_covered = 0usize;
    let mut candidate_hits = 0usize;
    let mut events_written = 0usize;
    let mut events_planned = 0usize;
    let mut skipped_missing_episode = 0usize;
    let mut skipped_already_recorded = 0usize;
    let mut skipped_not_rendered = 0usize;
    let mut skipped_outside_window = 0usize;
    let mut record_errors = 0usize;
    let mut served_chunk_ids = BTreeSet::new();
    let mut examples = Vec::new();
    // Per-chunk literal cache across files; empty literals mean "never
    // credit this chunk" (missing, repo-covered, or nothing distinctive).
    let mut literal_cache: HashMap<String, Vec<String>> = HashMap::new();

    for path in &session_files {
        let Ok(metadata) = fs::metadata(path) else {
            files_unreadable += 1;
            continue;
        };
        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        if mtime_ms < cutoff_ms {
            files_skipped_age += 1;
            continue;
        }
        let state_key = path.display().to_string();
        if state.get(&state_key) == Some(&mtime_ms) {
            files_skipped_unchanged += 1;
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            files_unreadable += 1;
            continue;
        };
        files_scanned += 1;
        // Cheap pre-filter on the raw JSONL: most sessions quote the memd
        // rules (which mention `retrieval_episode_id: null`) without ever
        // rendering an episode, so require an actual episode UUID before
        // paying for a full JSON parse.
        if find_episode_ids(&text).is_empty() {
            if !options.dry_run {
                state.insert(state_key, mtime_ms);
            }
            continue;
        }

        let signals = parse_session(&text);
        if signals.serves.is_empty() {
            if !options.dry_run {
                state.insert(state_key, mtime_ms);
            }
            continue;
        }
        files_with_serves += 1;
        serves_found += signals.serves.len();

        for serve in &signals.serves {
            for chunk_id in &serve.chunk_ids {
                served_chunk_ids.insert(chunk_id.clone());
                if literal_cache.contains_key(chunk_id) {
                    continue;
                }
                let literals = match resolve_chunk_literals(
                    store,
                    &tenant,
                    chunk_id,
                    &project_dir,
                    &repo_index,
                )
                .await?
                {
                    ChunkLiterals::Missing => {
                        chunks_skipped_missing += 1;
                        Vec::new()
                    }
                    ChunkLiterals::RepoCovered => {
                        chunks_skipped_repo_covered += 1;
                        Vec::new()
                    }
                    ChunkLiterals::Literals(literals) => literals,
                };
                literal_cache.insert(chunk_id.clone(), literals);
            }
        }

        let hits = compute_hits(&signals, &literal_cache);
        candidate_hits += hits.len();
        // Excerpts quote raw commands and patches, which can carry secrets.
        // They exist so an operator can judge precision, so they are printed
        // only under --dry-run and never on the path that writes events.
        if options.dry_run {
            for (_, hit) in hits
                .iter()
                .take(MAX_EXAMPLES.saturating_sub(examples.len()))
            {
                examples.push(json!({
                    "session": path.file_name().map(|name| name.to_string_lossy().into_owned()),
                    "chunk_id": hit.chunk_id,
                    "literal": hit.literal,
                    "serve_line": hit.serve_line,
                    "action_line": hit.action_line,
                    "action_excerpt": hit.action_excerpt,
                }));
            }
        }

        // One event per episode per session file, crediting every hit chunk.
        let mut per_episode: BTreeMap<String, Vec<&Hit>> = BTreeMap::new();
        for (episode_id, hit) in &hits {
            per_episode.entry(episode_id.clone()).or_default().push(hit);
        }
        for (episode_id, episode_hits) in per_episode {
            let Ok(episode_uuid) = RetrievalEpisodeId::parse(&episode_id) else {
                continue;
            };
            let Some((episode, items)) =
                store.get_retrieval_episode(&tenant, &episode_uuid).await?
            else {
                skipped_missing_episode += 1;
                continue;
            };
            let session_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            // The state file alone is not enough: Codex appends to a live
            // session all day, so its mtime keeps changing and the same
            // episode would be credited on every run. The store is the
            // authority — one `codex:<session>` event per episode, ever.
            let evidence_prefix = format!("codex:{session_name}:");
            if store
                .list_outcomes_for_episode(&tenant, &episode_uuid)
                .await?
                .iter()
                .any(|existing| {
                    existing
                        .evidence_reference
                        .as_deref()
                        .is_some_and(|reference| reference.starts_with(&evidence_prefix))
                })
            {
                skipped_already_recorded += 1;
                continue;
            }
            // record_outcome only accepts chunks rendered in the episode;
            // served output text can quote other chunk IDs (citations,
            // supersedes tags), so intersect up front instead of losing the
            // whole event to one stray ID.
            let rendered = items
                .iter()
                .filter(|item| item.rendered)
                .map(|item| item.chunk_id.to_string())
                .collect::<HashSet<_>>();
            let used_chunk_ids = episode_hits
                .iter()
                .map(|hit| hit.chunk_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|chunk_id| rendered.contains(chunk_id))
                .filter_map(|chunk_id| ChunkId::parse(&chunk_id).ok())
                .collect::<Vec<_>>();
            if used_chunk_ids.is_empty() {
                skipped_not_rendered += 1;
                continue;
            }
            // The session file's mtime is the honest usage time; outside the
            // episode retention window record_outcome would reject it.
            if mtime_ms < episode.created_at_ms || mtime_ms > episode.expires_at_ms {
                skipped_outside_window += 1;
                continue;
            }
            let evidence_line = episode_hits
                .iter()
                .map(|hit| hit.action_line)
                .min()
                .expect("per-episode hit group is non-empty");
            let event = OutcomeEvent {
                event_id: OutcomeEventId::new(),
                episode_id: episode_uuid,
                outcome: OutcomeKind::Accepted,
                verifier: OutcomeVerifier::ExternalTool,
                used_chunk_ids,
                harmful_chunk_ids: Vec::new(),
                evidence_reference: Some(format!("codex:{session_name}:{evidence_line}")),
                ranking_eligible: true,
                timestamp_ms: mtime_ms,
            };
            if options.dry_run {
                events_planned += 1;
            } else {
                match store.record_outcome(&tenant, event).await {
                    Ok(()) => events_written += 1,
                    Err(error) => {
                        record_errors += 1;
                        tracing::warn!(?error, episode_id, "outcome-scan record_outcome failed");
                    }
                }
            }
        }

        if !options.dry_run {
            state.insert(state_key, mtime_ms);
        }
    }

    if !options.dry_run {
        save_scan_state(&state_path, &state)?;
    }

    Ok(json!({
        "tenant_id": tenant.to_string(),
        "sessions_dir": sessions_dir,
        "dry_run": options.dry_run,
        "files_found": session_files.len(),
        "files_scanned": files_scanned,
        "files_skipped_unchanged": files_skipped_unchanged,
        "files_skipped_age": files_skipped_age,
        "files_unreadable": files_unreadable,
        "files_with_serves": files_with_serves,
        "serves_found": serves_found,
        "served_chunks": served_chunk_ids.len(),
        "chunks_skipped_missing": chunks_skipped_missing,
        "chunks_skipped_repo_covered": chunks_skipped_repo_covered,
        "candidate_hits": candidate_hits,
        "events_planned": events_planned,
        "events_written": events_written,
        "events_skipped": {
            "missing_episode": skipped_missing_episode,
            "already_recorded": skipped_already_recorded,
            "not_rendered": skipped_not_rendered,
            "outside_window": skipped_outside_window,
            "record_error": record_errors,
        },
        "examples": examples,
    }))
}

enum ChunkLiterals {
    Missing,
    RepoCovered,
    Literals(Vec<String>),
}

async fn resolve_chunk_literals<S: Store>(
    store: &S,
    tenant: &TenantId,
    chunk_id: &str,
    project_dir: &Path,
    repo_index: &[RepoDoc],
) -> Result<ChunkLiterals> {
    let Ok(parsed) = ChunkId::parse(chunk_id) else {
        return Ok(ChunkLiterals::Missing);
    };
    let Some(chunk) = store.get(tenant, &parsed).await? else {
        return Ok(ChunkLiterals::Missing);
    };
    // Repo-novelty gate: a chunk a repo file already covers proves nothing
    // about memory usefulness — the agent reads those files anyway.
    if repo_doc_covering(&chunk.text, repo_index).is_some() {
        return Ok(ChunkLiterals::RepoCovered);
    }
    let literals = extract_literals(&chunk.text)
        .into_iter()
        .filter(|literal| !literal_is_repo_path(project_dir, literal))
        .collect();
    Ok(ChunkLiterals::Literals(literals))
}

fn collect_jsonl_files(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > SESSION_DIR_MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, depth + 1, files);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            files.push(path);
        }
    }
}

fn load_scan_state(path: &Path) -> HashMap<String, i64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_scan_state(path: &Path, state: &HashMap<String, i64>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(&state)?)?;
    Ok(())
}

/// One pass over a session file; line order is time order. Tool-call
/// inputs become actions, tool-call outputs are inspected only for
/// served retrieval episodes — never for literal hits.
fn parse_session(text: &str) -> SessionSignals {
    let mut signals = SessionSignals::default();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(payload) = value.get("payload") else {
            continue;
        };
        match payload.get("type").and_then(Value::as_str) {
            // `custom_tool_call` carries the command in `input` (e.g. a
            // `tools.exec_command({"cmd": ...})` wrapper); `function_call`
            // carries a JSON string in `arguments`.
            Some("custom_tool_call") | Some("function_call") => {
                let action_text = payload
                    .get("input")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("arguments").and_then(Value::as_str));
                if let Some(action_text) = action_text {
                    signals.actions.push(Action {
                        line: line_number,
                        text: action_text.to_string(),
                    });
                }
            }
            Some("custom_tool_call_output") | Some("function_call_output") => {
                let Some(output) = payload.get("output") else {
                    continue;
                };
                let mut rendered = String::new();
                collect_strings(output, &mut rendered);
                let episode_ids = find_episode_ids(&rendered);
                if episode_ids.is_empty() {
                    continue;
                }
                let chunk_ids = find_uuids(&rendered)
                    .into_iter()
                    .filter(|uuid| !episode_ids.contains(uuid))
                    .collect::<Vec<_>>();
                for episode_id in episode_ids {
                    signals.serves.push(Serve {
                        line: line_number,
                        episode_id,
                        chunk_ids: chunk_ids.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    signals
}

fn collect_strings(value: &Value, out: &mut String) {
    match value {
        Value::String(text) => {
            out.push_str(text);
            out.push('\n');
        }
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

/// Episode IDs are the UUID directly following a `retrieval_episode_id`
/// key through quoting/escaping noise only. `retrieval_episode_id: null`
/// (fixed-clock searches and quoted documentation) yields nothing.
fn find_episode_ids(text: &str) -> BTreeSet<String> {
    const KEY: &str = "retrieval_episode_id";
    let mut ids = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut search = 0;
    while let Some(found) = text[search..].find(KEY) {
        let mut pos = search + found + KEY.len();
        while pos < bytes.len() && matches!(bytes[pos], b'"' | b'\\' | b':' | b' ' | b'=' | b'`') {
            pos += 1;
        }
        if let Some(uuid) = uuid_at(bytes, pos) {
            ids.insert(uuid);
        }
        search += found + KEY.len();
    }
    ids
}

fn find_uuids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut index = 0;
    while index + 36 <= bytes.len() {
        if let Some(uuid) = uuid_at(bytes, index) {
            if seen.insert(uuid.clone()) {
                found.push(uuid);
            }
            index += 36;
        } else {
            index += 1;
        }
    }
    found
}

/// Lowercase hyphenated UUID at `start`, rejected when embedded in a
/// longer hex run on either side.
fn uuid_at(bytes: &[u8], start: usize) -> Option<String> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let is_hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    if start > 0 && is_hex(bytes[start - 1]) {
        return None;
    }
    let mut pos = start;
    for (group, length) in GROUPS.iter().enumerate() {
        if group > 0 {
            if bytes.get(pos) != Some(&b'-') {
                return None;
            }
            pos += 1;
        }
        for _ in 0..*length {
            match bytes.get(pos) {
                Some(&byte) if is_hex(byte) => pos += 1,
                _ => return None,
            }
        }
    }
    if bytes.get(pos).is_some_and(|byte| is_hex(*byte)) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[start..pos]).into_owned())
}

/// Distinctive literals of one chunk text: backtick-quoted spans,
/// whitespace tokens containing `/`, and `--flags`; kept when at least
/// `MIN_LITERAL_LEN` long and carrying at least one of `/ 0-9 _ . -`.
fn extract_literals(chunk_text: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut seen = HashSet::new();
    let push = |literal: &str, literals: &mut Vec<String>, seen: &mut HashSet<String>| {
        if literals.len() < MAX_LITERALS_PER_CHUNK
            && literal_qualifies(literal)
            && seen.insert(literal.to_string())
        {
            literals.push(literal.to_string());
        }
    };
    for (index, span) in chunk_text.split('`').enumerate() {
        if index % 2 == 1 {
            push(span.trim(), &mut literals, &mut seen);
        }
    }
    for token in chunk_text.split_whitespace() {
        let token = token
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`'
                )
            })
            .trim_end_matches('.');
        if token.contains('/') || token.starts_with("--") {
            push(token, &mut literals, &mut seen);
        }
    }
    literals
}

fn literal_qualifies(literal: &str) -> bool {
    literal.len() >= MIN_LITERAL_LEN
        && literal
            .chars()
            .any(|ch| ch.is_ascii_digit() || matches!(ch, '/' | '_' | '.' | '-'))
}

/// A literal resolving to a path in the repo working tree is weak
/// evidence: the agent could have found it by reading the repo instead
/// of the memory.
fn literal_is_repo_path(project_dir: &Path, literal: &str) -> bool {
    let candidate = Path::new(literal);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        project_dir.join(candidate)
    };
    resolved.starts_with(project_dir) && resolved.exists()
}

/// A literal counts as a hit for its chunk only when it appears in an
/// action after the serve line and in no action before it — anything
/// already in play before the serve was not learned from the memory.
///
/// Serves are walked newest-first and each `(chunk, literal)` pair is
/// credited at most once per session, so a chunk served by several
/// episodes yields one hit attributed to the nearest preceding serve
/// rather than one positive outcome per episode.
fn compute_hits(
    signals: &SessionSignals,
    literals_by_chunk: &HashMap<String, Vec<String>>,
) -> Vec<(String, Hit)> {
    let mut hits = Vec::new();
    let mut credited = HashSet::new();
    let mut serves = signals.serves.iter().collect::<Vec<_>>();
    serves.sort_by_key(|serve| std::cmp::Reverse(serve.line));
    for serve in serves {
        for chunk_id in &serve.chunk_ids {
            let Some(literals) = literals_by_chunk.get(chunk_id) else {
                continue;
            };
            for literal in literals {
                let seen_before = signals
                    .actions
                    .iter()
                    .any(|action| action.line < serve.line && action.text.contains(literal));
                if seen_before {
                    continue;
                }
                let Some(action) = signals
                    .actions
                    .iter()
                    .find(|action| action.line > serve.line && action.text.contains(literal))
                else {
                    continue;
                };
                if !credited.insert((chunk_id.clone(), literal.clone())) {
                    continue;
                }
                hits.push((
                    serve.episode_id.clone(),
                    Hit {
                        chunk_id: chunk_id.clone(),
                        literal: literal.clone(),
                        serve_line: serve.line,
                        action_line: action.line,
                        action_excerpt: excerpt_around(&action.text, literal),
                    },
                ));
            }
        }
    }
    hits
}

fn excerpt_around(text: &str, literal: &str) -> String {
    let center = text.find(literal).unwrap_or(0);
    let mut start = center.saturating_sub(MAX_EXAMPLE_EXCERPT_CHARS / 2);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (center + literal.len() + MAX_EXAMPLE_EXCERPT_CHARS / 2).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    text[start..end].replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_extraction_keeps_backticks_paths_and_flags() {
        let text = "Fix lives in `crates/memd/src/cli/args.rs` — search with \
                    --dedupe-by-source; run scripts/run_bench.sh, then check \
                    docs/handoffs/2026-07-13_reliable.md. Short `x.rs` and \
                    --force and plain twelvecharword do not qualify.";
        let literals = extract_literals(text);
        assert!(literals.contains(&"crates/memd/src/cli/args.rs".to_string()));
        assert!(literals.contains(&"--dedupe-by-source".to_string()));
        assert!(literals.contains(&"scripts/run_bench.sh".to_string()));
        assert!(literals.contains(&"docs/handoffs/2026-07-13_reliable.md".to_string()));
        assert!(!literals.iter().any(|l| l == "x.rs"), "below min length");
        assert!(!literals.iter().any(|l| l == "--force"), "below min length");
        assert!(
            !literals.iter().any(|l| l == "twelvecharword"),
            "no /0-9_.- character and not a path/flag/backtick span"
        );

        let many = (0..20)
            .map(|i| format!("dir{i}/file_number_{i}.rs"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(extract_literals(&many).len(), MAX_LITERALS_PER_CHUNK);
    }

    fn serve(line: usize, episode: &str, chunks: &[&str]) -> Serve {
        Serve {
            line,
            episode_id: episode.to_string(),
            chunk_ids: chunks.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn action(line: usize, text: &str) -> Action {
        Action {
            line,
            text: text.to_string(),
        }
    }

    #[test]
    fn hit_requires_use_after_serve_and_absence_before() {
        let signals = SessionSignals {
            serves: vec![serve(10, "ep-1", &["chunk-a", "chunk-b"])],
            actions: vec![
                action(5, "cat docs/known_before_serve.md"),
                action(
                    20,
                    "bash scripts/learned_from_memory.sh docs/known_before_serve.md",
                ),
            ],
        };
        let literals = HashMap::from([
            (
                "chunk-a".to_string(),
                vec!["scripts/learned_from_memory.sh".to_string()],
            ),
            (
                "chunk-b".to_string(),
                vec!["docs/known_before_serve.md".to_string()],
            ),
        ]);
        let hits = compute_hits(&signals, &literals);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.chunk_id, "chunk-a");
        assert_eq!(hits[0].1.literal, "scripts/learned_from_memory.sh");
        assert_eq!(hits[0].1.serve_line, 10);
        assert_eq!(hits[0].1.action_line, 20);
    }

    #[test]
    fn repeated_serves_credit_the_nearest_one_once() {
        let signals = SessionSignals {
            serves: vec![
                serve(10, "ep-early", &["chunk-a"]),
                serve(20, "ep-late", &["chunk-a"]),
            ],
            actions: vec![action(30, "bash scripts/learned_from_memory.sh")],
        };
        let literals = HashMap::from([(
            "chunk-a".to_string(),
            vec!["scripts/learned_from_memory.sh".to_string()],
        )]);
        let hits = compute_hits(&signals, &literals);
        assert_eq!(hits.len(), 1, "one use must not credit two episodes");
        assert_eq!(hits[0].0, "ep-late", "credit the nearest preceding serve");

        // With the use between the two serves, the later serve is vetoed by
        // its own before-window and the earlier one takes the credit.
        let signals = SessionSignals {
            serves: vec![
                serve(10, "ep-early", &["chunk-a"]),
                serve(20, "ep-late", &["chunk-a"]),
            ],
            actions: vec![action(15, "bash scripts/learned_from_memory.sh")],
        };
        let hits = compute_hits(&signals, &literals);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "ep-early");
    }

    #[test]
    fn tool_outputs_never_count_as_actions() {
        // Line 1 serves episode + chunk; line 2 is another tool OUTPUT that
        // repeats the literal (the agent being shown the chunk again); line 3
        // is a real input. Only line 3 may count as usage.
        let episode = "019f0000-0000-7000-8000-000000000001";
        let chunk = "019f0000-0000-7000-8000-000000000002";
        let session = format!(
            concat!(
                "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"custom_tool_call_output\",",
                "\"output\":[{{\"type\":\"input_text\",\"text\":\"chunk {chunk} ... ",
                "\\\"retrieval_episode_id\\\": \\\"{episode}\\\"\"}}]}}}}\n",
                "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"custom_tool_call_output\",",
                "\"output\":\"rerendered scripts/from_memory_only.sh\"}}}}\n",
                "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call\",",
                "\"arguments\":\"{{\\\"cmd\\\":\\\"bash scripts/from_memory_only.sh\\\"}}\"}}}}\n",
            ),
            chunk = chunk,
            episode = episode,
        );
        let signals = parse_session(&session);
        assert_eq!(signals.serves.len(), 1);
        assert_eq!(signals.serves[0].episode_id, episode);
        assert_eq!(signals.serves[0].chunk_ids, vec![chunk.to_string()]);
        assert_eq!(
            signals.actions.len(),
            1,
            "outputs must never become actions"
        );
        assert_eq!(signals.actions[0].line, 3);

        let literals = HashMap::from([(
            chunk.to_string(),
            vec!["scripts/from_memory_only.sh".to_string()],
        )]);
        let hits = compute_hits(&signals, &literals);
        assert_eq!(hits.len(), 1, "the input on line 3 is genuine usage");
        assert_eq!(hits[0].1.action_line, 3);
    }

    #[test]
    fn null_episode_id_is_not_a_serve() {
        let session = concat!(
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",",
            "\"output\":\"\\\"retrieval_episode_id\\\": null\"}}\n",
        );
        assert!(parse_session(session).serves.is_empty());
    }

    #[test]
    fn repo_path_literals_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path();
        std::fs::create_dir_all(project_dir.join("src")).unwrap();
        std::fs::write(project_dir.join("src/real_file_on_disk.rs"), "x").unwrap();

        assert!(literal_is_repo_path(
            project_dir,
            "src/real_file_on_disk.rs"
        ));
        assert!(literal_is_repo_path(
            project_dir,
            project_dir
                .join("src/real_file_on_disk.rs")
                .to_str()
                .unwrap(),
        ));
        assert!(!literal_is_repo_path(project_dir, "src/not_on_disk.rs"));
        // Absolute paths outside the working tree stay usable as evidence.
        assert!(!literal_is_repo_path(
            Path::new("/nonexistent-project"),
            project_dir
                .join("src/real_file_on_disk.rs")
                .to_str()
                .unwrap(),
        ));
    }
}
