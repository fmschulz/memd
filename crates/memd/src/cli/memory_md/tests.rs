use super::collect::*;
use super::evaluate::*;
use super::rank::*;
use super::render::*;
use super::state::*;
use super::*;
use crate::store::{
    DuplicateHealth, HealthCounts, IndexCoverageHealth, PayloadHealth, StoreHealthSnapshot,
    StoreStats,
};
use crate::types::{ChunkId, MemoryChunk};

#[test]
fn explicit_priority_scales_small_values_and_caps_large_values() {
    assert_eq!(explicit_priority(&["priority:7".to_string()]), Some(70.0));
    assert_eq!(
        explicit_priority(&["importance:120".to_string()]),
        Some(100.0)
    );
}

#[test]
fn git_state_handles_clean_dirty_no_repo_and_missing_git() {
    let no_repo = tempfile::tempdir().unwrap();
    let no_repo_state = collect_git_state(no_repo.path(), "git");
    if no_repo_state.warning.as_deref() == Some("git unavailable: executable not found") {
        return;
    }
    assert!(no_repo_state.not_git_repo);
    assert!(!no_repo_state.available);

    let missing = collect_git_state(no_repo.path(), "definitely-not-a-git-binary");
    assert!(!missing.available);
    assert!(missing
        .warning
        .as_deref()
        .unwrap_or_default()
        .contains("executable not found"));

    let repo = tempfile::tempdir().unwrap();
    let init = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .arg("init")
        .output()
        .unwrap();
    assert!(init.status.success());
    let clean = collect_git_state(repo.path(), "git");
    assert!(clean.available);
    assert_eq!(clean.clean, Some(true));

    fs::write(repo.path().join("dirty.txt"), "dirty").unwrap();
    let dirty = collect_git_state(repo.path(), "git");
    assert!(dirty.available);
    assert_eq!(dirty.clean, Some(false));
    assert!(dirty.changed_entries > 0);
}

#[test]
fn task_state_uses_nested_heading_for_first_open_action() {
    let dir = tempfile::tempdir().unwrap();
    let tasks_dir = dir.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(
        tasks_dir.join("todo.md"),
        "# Completed\n\n- [x] old\n\n# Current\n\n## Deep Work\n\n- [ ] implement nested action\n",
    )
    .unwrap();

    let scan = collect_task_state(dir.path());
    let latest = scan.latest_task.expect("latest task");
    assert_eq!(latest.heading.as_deref(), Some("Deep Work"));
    assert_eq!(scan.next_actions.len(), 1);
    assert_eq!(scan.next_actions[0].line, 9);
    assert_eq!(scan.next_actions[0].source_path, "tasks/todo.md");
}

#[test]
fn task_state_ignores_checked_boxes_and_incidental_pending_text() {
    let dir = tempfile::tempdir().unwrap();
    let tasks_dir = dir.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(
        tasks_dir.join("todo.md"),
        "# Current\n\n- [x] resolved the pending migration\n- note: this pending migration is already closed\n- [ ] real open item\n",
    )
    .unwrap();

    let scan = collect_task_state(dir.path());
    assert_eq!(scan.next_actions.len(), 1);
    assert_eq!(scan.next_actions[0].text, "real open item");
}

#[test]
fn task_source_state_distinguishes_missing_empty_and_parse_failure() {
    let missing = tempfile::tempdir().unwrap();
    assert_eq!(
        collect_task_state(missing.path()).source_state,
        TaskSourceState::Missing
    );

    let completed = tempfile::tempdir().unwrap();
    fs::create_dir_all(completed.path().join("tasks")).unwrap();
    fs::write(
        completed.path().join("tasks/todo.md"),
        "# Completed\n\n- [x] verified release\n",
    )
    .unwrap();
    assert_eq!(
        collect_task_state(completed.path()).source_state,
        TaskSourceState::ParsedNoOpenTasks
    );

    let unreadable = tempfile::tempdir().unwrap();
    fs::create_dir_all(unreadable.path().join("tasks/todo.md")).unwrap();
    assert_eq!(
        collect_task_state(unreadable.path()).source_state,
        TaskSourceState::ParseFailed
    );
}

#[test]
fn union_dedup_assigns_cross_section_duplicates_to_project() {
    let mut project = vec![make_takeaway(
        "shared",
        "Decision: keep tenant-scoped cache keys.",
        vec!["kind:decision", "topic:cache-scope"],
        "decision",
    )];
    let mut global = vec![
        make_takeaway(
            "shared",
            "Decision: keep tenant-scoped cache keys.",
            vec!["kind:decision", "topic:cache-scope"],
            "decision",
        ),
        make_takeaway(
            "paraphrase",
            "Cache scope decision for every project.",
            vec!["topic:cache-scope"],
            "summary",
        ),
    ];
    let suppressed = dedupe_memory_md_union(&mut project, &mut global);
    assert_eq!(project.len(), 1);
    assert!(global.is_empty());
    assert_eq!(suppressed.len(), 2);
}

#[test]
fn union_dedup_collapses_lineage_equivalent_items() {
    let mut project = vec![make_takeaway(
        "source-a",
        "Original cache incident resolution.",
        vec!["kind:decision"],
        "decision",
    )];
    let mut global = vec![make_takeaway(
        "consolidated-a",
        "Reusable tenant isolation rule.",
        vec!["kind:evidence", "supersedes:source-a,source-b"],
        "summary",
    )];

    let suppressed = dedupe_memory_md_union(&mut project, &mut global);

    assert_eq!(project.len(), 1);
    assert!(global.is_empty());
    assert_eq!(
        suppressed
            .get(&("machine_wide".to_string(), "consolidated-a".to_string()))
            .map(String::as_str),
        Some("duplicate_lineage:source-a")
    );
}

#[test]
fn generic_repo_tags_do_not_collapse_unrelated_takeaways() {
    let mut project = vec![make_takeaway(
        "cache",
        "Cache eviction uses bounded least recently used entries.",
        vec!["repo:memd", "kind:decision"],
        "decision",
    )];
    let mut global = vec![make_takeaway(
        "backup",
        "SQLite backup validation checks restored ledger rows.",
        vec!["repo:memd", "kind:evidence"],
        "summary",
    )];

    let suppressed = dedupe_memory_md_union(&mut project, &mut global);

    assert!(suppressed.is_empty());
    assert_eq!(project.len(), 1);
    assert_eq!(global.len(), 1);
}

#[test]
fn active_project_candidates_move_out_of_machine_section() {
    let mut project = Vec::new();
    let mut global = vec![
        make_takeaway_with_project("active", Some("project-a")),
        make_takeaway_with_project("other", Some("project-b")),
    ];
    let mut project_explanations = Vec::new();
    let mut global_explanations = Vec::new();

    assign_active_project_candidates(
        &mut project,
        &mut global,
        &mut project_explanations,
        &mut global_explanations,
        Some("project-a"),
    );

    assert_eq!(
        project
            .iter()
            .map(|takeaway| takeaway.chunk_id.as_str())
            .collect::<Vec<_>>(),
        vec!["active"]
    );
    assert_eq!(
        global
            .iter()
            .map(|takeaway| takeaway.chunk_id.as_str())
            .collect::<Vec<_>>(),
        vec!["other"]
    );
}

#[test]
fn missing_or_failed_task_source_never_passes_answerability() {
    for source_state in [TaskSourceState::Missing, TaskSourceState::ParseFailed] {
        let mut state = make_project_state();
        state.next_actions.clear();
        state.task_source_state = source_state;

        let metrics = evaluate_agent_usefulness(&state, &[], &[]);

        assert!(!metrics.no_open_tasks_detected);
        assert!(!metrics.answerability_passed);
        assert!(agent_usefulness_failures(&metrics)
            .iter()
            .any(|failure| failure.contains("task source")));
    }
}

#[test]
fn task_source_uncertainty_is_rendered_explicitly() {
    let mut missing = make_project_state();
    missing.next_actions.clear();
    missing.task_source_state = TaskSourceState::Missing;
    let mut rendered = String::new();
    render_latest_project_state(&mut rendered, &missing);
    assert!(rendered.contains("is missing; open-task state is unknown"));

    missing.task_source_state = TaskSourceState::ParseFailed;
    rendered.clear();
    render_latest_project_state(&mut rendered, &missing);
    assert!(rendered.contains("could not be parsed; open-task state is unknown"));
}

#[test]
fn handoff_state_renders_title_once_and_stable_spacing() {
    let dir = tempfile::tempdir().unwrap();
    let handoff_dir = dir.path().join("docs/handoffs");
    fs::create_dir_all(&handoff_dir).unwrap();
    fs::write(
        handoff_dir.join("2026-06-28-release.md"),
        "# Handoff 2026-06-28\n\nStatus: active\nNext step: ship release\n",
    )
    .unwrap();

    let scan = collect_handoff_state(dir.path());
    let handoff = scan.latest_handoff.expect("latest handoff");
    assert_eq!(handoff.heading.as_deref(), Some("Handoff 2026-06-28"));
    assert_eq!(handoff.text, "Status: active | Next step: ship release");

    let rendered_signal = render_state_signal(&handoff);
    assert!(!rendered_signal.contains("Handoff 2026-06-28 - Handoff 2026-06-28"));

    let mut state = make_project_state();
    state.latest_handoff = Some(handoff);
    state.latest_vcs = None;
    let mut rendered = String::new();
    render_latest_project_state(&mut rendered, &state);
    assert!(rendered.contains("- handoff: Handoff 2026-06-28 -"));
    assert!(rendered.contains("Status: active"));
    assert!(rendered.contains("Next step: ship release"));
    assert!(!rendered.contains("\n\n\n### Next Actions"));
}

#[test]
fn scope_path_normalization_warns_only_on_real_drift() {
    let dir = tempfile::tempdir().unwrap();
    let configured_same = format!("{}/.", dir.path().display());
    assert!(scope_path_drift_warning(&configured_same, dir.path()).is_none());

    let drift = dir.path().join("other");
    let warning =
        scope_path_drift_warning(drift.to_str().unwrap(), dir.path()).expect("drift warning");
    assert!(warning.contains("scope mismatch"));
}

#[test]
fn fragment_candidates_are_filtered_before_ranking() {
    let payload = serde_json::json!({
        "results": [
            {
                "chunk_id": "fragment",
                "tenant_id": "t",
                "project_id": "p",
                "text": "and then continued from a prior chunk",
                "score": 10.0,
                "chunk_type": "summary",
                "timestamp_created": 1,
                "tags": ["chunk_index:1", "kind:finish"]
            }
        ]
    });
    let mut by_chunk = HashMap::new();
    let mut explanations = Vec::new();
    merge_payload_candidates(
        &mut by_chunk,
        &mut explanations,
        &payload,
        "project",
        "project_highlights",
        "project takeaways",
        CliQueryMode::FindHighlights,
    );
    assert!(by_chunk.is_empty());
    let explanation = explanations.first().expect("explanation");
    assert_eq!(explanation.filter_reason.as_deref(), Some("fragment_like"));
    assert!(explanation
        .quality_flags
        .iter()
        .any(|flag| flag == "fragment_like"));
}

#[test]
fn conditional_sentences_are_not_filtered_as_fragments() {
    assert!(!is_fragment_like_candidate(
        &[],
        "When rerunning failed tasks, use --force to overwrite stale outputs."
    ));
    assert!(!is_fragment_like_candidate(
        &[],
        "While debugging CI, inspect the failing job log before changing code."
    ));
    assert!(!is_fragment_like_candidate(
        &[],
        "Because the release workflow publishes from main, push a release branch first."
    ));
    assert!(is_fragment_like_candidate(
        &[],
        "and then continued from a prior chunk"
    ));
}

#[cfg(unix)]
#[test]
fn command_timeout_drains_large_output_while_waiting() {
    let mut command = Command::new("sh");
    command.arg("-c").arg(
        "i=0; while [ \"$i\" -lt 20000 ]; do printf 'dirty-file-%05d xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' \"$i\"; i=$((i + 1)); done",
    );

    let output = run_command_with_timeout(command, Duration::from_secs(5)).unwrap();

    assert!(!output.timed_out);
    assert!(output.status_success);
    assert!(output.stdout.len() > 128 * 1024);
}

#[test]
fn expected_git_failure_checks_true_and_false_expectations() {
    let mut metrics = make_agent_metrics();
    metrics.git_state_present = false;
    assert_eq!(
        expected_git_failure(&metrics, true).as_deref(),
        Some("expected git state but git_state_present=false")
    );
    assert!(expected_git_failure(&metrics, false).is_none());

    metrics.git_state_present = true;
    assert_eq!(
        expected_git_failure(&metrics, false).as_deref(),
        Some("expected no git state but git_state_present=true")
    );
    assert!(expected_git_failure(&metrics, true).is_none());
}

#[test]
fn startup_filters_remove_boilerplate_before_union_dedup() {
    let mut takeaways = vec![
        make_takeaway(
            "keep",
            "Validation: MEMD_EMBED_DEVICE cpu override fixed GPU contention. Agent action: Use MEMD_EMBED_DEVICE=cpu when GPU contention blocks embedding.",
            vec!["topic:embed-device", "priority:9"],
            "summary",
        ),
        make_takeaway(
            "drop-duplicate",
            "Validation: MEMD_EMBED_DEVICE cuda override fixed GPU contention. Agent action: Use MEMD_EMBED_DEVICE when GPU contention blocks embedding.",
            vec!["topic:embed-device", "priority:8"],
            "summary",
        ),
        make_takeaway(
            "drop-boilerplate",
            "Routine note without a concrete action or category signal.",
            vec![],
            "summary",
        ),
    ];
    for (idx, takeaway) in takeaways.iter_mut().enumerate() {
        takeaway.priority = (90 - idx) as f32;
    }

    let mut suppressed = filter_startup_takeaways(&mut takeaways);
    let mut global = Vec::new();
    suppressed.extend(
        dedupe_memory_md_union(&mut takeaways, &mut global)
            .into_iter()
            .map(|((_, id), reason)| (id, reason)),
    );
    let ids = takeaways
        .iter()
        .map(|takeaway| takeaway.chunk_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["keep"]);
    assert_eq!(
        suppressed.get("drop-boilerplate").map(String::as_str),
        Some("boilerplate_action")
    );
    assert!(suppressed
        .get("drop-duplicate")
        .map(|reason| reason.starts_with("duplicate_topic:"))
        .unwrap_or(false));
}

#[tokio::test]
async fn memory_state_reports_project_scoped_unreadable_chunks() {
    let store = FakeHealthStore::new(2, Vec::new());
    let tenant = TenantId::new("t").unwrap();
    let state = collect_memory_state(&store, &tenant, Some("p")).await;
    assert_eq!(state.metadata_active_chunks, Some(2));
    assert_eq!(state.readable_active_chunks, Some(0));
    assert_eq!(state.unreadable_active_chunks, Some(2));

    let project_state = ProjectState {
        memory: state,
        ..make_project_state()
    };
    assert!(project_state_warnings(&project_state)
        .iter()
        .any(|warning| warning.contains("memory degraded: 2 active chunks")));
}

#[tokio::test]
async fn memory_state_caps_large_readable_scan_and_warns_partial() {
    let store = FakeHealthStore::new(READABLE_SCAN_MAX_METADATA_ROWS + 5, Vec::new());
    let tenant = TenantId::new("t").unwrap();
    let state = collect_memory_state(&store, &tenant, Some("p")).await;
    assert_eq!(
        state.unreadable_active_chunks,
        Some(READABLE_SCAN_MAX_METADATA_ROWS)
    );
    assert!(state
        .scan_warning
        .as_deref()
        .unwrap_or_default()
        .contains("readable memory scan partial"));
}

#[test]
fn render_memory_md_caps_long_takeaway_text() {
    let takeaway = Takeaway {
        chunk_id: "chunk-a".to_string(),
        tenant_id: "tenant-a".to_string(),
        project_id: Some("project-a".to_string()),
        text: "word ".repeat(120),
        score: 1.0,
        priority: 42.0,
        chunk_type: "summary".to_string(),
        timestamp_created: 0,
        tags: vec!["kind:finish".to_string()],
        sources: BTreeSet::from(["project_highlights".to_string()]),
        occurrences: 1,
    };
    let state = make_project_state();
    let rendered = render_memory_md(&state, &[], &[takeaway], &[]);
    assert!(rendered.contains("## Project Fact Library"));
    assert!(rendered.contains("## Agent Guidance"));
    assert!(rendered.contains("agent action: `Use this as evidence only after confirming"));
    assert!(rendered.contains("chunk-a"));
    assert!(rendered.contains("..."));
}

#[test]
fn render_memory_md_omits_empty_global_section() {
    let takeaway = Takeaway {
        chunk_id: "chunk-a".to_string(),
        tenant_id: "tenant-a".to_string(),
        project_id: Some("project-a".to_string()),
        text: "validated project lesson".to_string(),
        score: 1.0,
        priority: 42.0,
        chunk_type: "summary".to_string(),
        timestamp_created: 0,
        tags: vec!["kind:finish".to_string()],
        sources: BTreeSet::from(["project_highlights".to_string()]),
        occurrences: 1,
    };
    let state = make_project_state();
    let rendered = render_memory_md(&state, &[], &[takeaway], &[]);
    assert!(rendered.contains("## Project Fact Library"));
    assert!(!rendered.contains("## Machine-Wide Fact Library"));
}

#[test]
fn render_memory_md_groups_takeaways_by_signal_category() {
    let mut decision = make_takeaway(
        "decision",
        "Decision: keep project aliases explicit. Rationale: silent merging hides drift.",
        vec!["kind:decision"],
        "summary",
    );
    decision.priority = 80.0;
    decision.timestamp_created = 42;

    let mut fix = make_takeaway(
        "fix",
        "Validation: cargo test -p memd passed after the scoped startup change.",
        vec!["kind:finish"],
        "summary",
    );
    fix.priority = 70.0;

    let mut command = make_takeaway(
        "command",
        "Command: memd memory-md --project-dir . --output memory.md",
        vec!["kind:run"],
        "trace",
    );
    command.priority = 60.0;

    let state = make_project_state();
    let rendered = render_memory_md(&state, &[], &[command, fix, decision], &[]);

    assert!(rendered.contains("### Decisions"));
    assert!(rendered.contains("### Validated Fixes"));
    assert!(rendered.contains("### Commands/Paths"));
    assert!(rendered.contains("reason: `decision or rationale`"));
    assert!(rendered.contains("reason: `validated fix or result`"));
    assert!(rendered.contains("reason: `command, path, or parameter evidence`"));
    assert!(rendered.contains("agent action: `Apply this decision"));
    assert!(rendered.contains("agent action: `Reuse this validated fix"));
    assert!(rendered.contains("agent action: `Use or verify this command/path"));
    assert!(rendered.contains("created_unix_ms: `42`"));
}

#[test]
fn memory_md_quality_report_scores_useful_items_and_wrappers() {
    let content = r#"# memory.md

## Project Takeaways

### Decisions

1. Decision: keep project aliases explicit.
   - reason: `decision or rationale`
   - agent action: `Apply this decision when the same scope appears: keep project aliases explicit.`

### Other Takeaways

1. Task digest status generated. Summary: Highlight library for p contains 2 ranked lessons.
   - reason: `ranked project takeaway`
   - tags: `task:status:generated, task:role:highlight_library`

2. Routine status update with no reason.

## Machine-Wide Takeaways

1. Decision outside project section should not be counted.
   - reason: `decision or rationale`
"#;
    let report = evaluate_memory_md_quality(content, 10);
    assert_eq!(report.displayed_count, 3);
    assert_eq!(report.useful_count, 1);
    assert_eq!(report.generated_wrapper_count, 1);
    assert_eq!(report.missing_reason_count, 1);
    assert_eq!(report.missing_action_count, 2);
    assert!((report.useful_ratio - (1.0 / 3.0)).abs() < f64::EPSILON);
}

#[test]
fn explicit_agent_action_overrides_category_template() {
    let takeaway = make_takeaway(
        "action",
        "Decision: keep cache keys scoped. Agent action: Verify tenant and project are present before reusing cached retrieval results.",
        vec!["kind:decision"],
        "summary",
    );

    let state = make_project_state();
    let rendered = render_memory_md(&state, &[], &[takeaway], &[]);

    assert!(rendered.contains(
        "agent action: `Verify tenant and project are present before reusing cached retrieval results`"
    ));
}

#[test]
fn explicit_agent_action_skips_explanatory_marker_mentions() {
    let takeaway = make_takeaway(
        "action",
        "Validation: memory quality gate passed. Docs mention Agent action: sentence. Agent action: Write every high-priority durable memory with a concrete action sentence that tells future agents what to verify or reuse.",
        vec!["kind:finish"],
        "summary",
    );

    let state = make_project_state();
    let rendered = render_memory_md(&state, &[], &[takeaway], &[]);

    assert!(rendered.contains(
        "agent action: `Write every high-priority durable memory with a concrete action sentence that tells future agents what to verify or reuse`"
    ));
}

#[test]
fn explicit_agent_action_keeps_paths_and_versions() {
    let takeaway = make_takeaway(
        "action",
        "Installed skill bundle. Agent action: Verify future agent sessions read the refreshed ~/.agents/skills/memd skill and use memd 0.61.0 before diagnosing memory-quality behavior.",
        vec!["kind:finish"],
        "summary",
    );

    let state = make_project_state();
    let rendered = render_memory_md(&state, &[], &[takeaway], &[]);

    assert!(rendered.contains(
        "agent action: `Verify future agent sessions read the refreshed ~/.agents/skills/memd skill and use memd 0.61.0 before diagnosing memory-quality behavior`"
    ));
}

fn make_project_state() -> ProjectState {
    ProjectState {
        generated_unix_ms: 123,
        tenant_id: "tenant-a".to_string(),
        project_id: Some("project-a".to_string()),
        configured_project_dir: Some("/tmp/project-a".to_string()),
        resolved_project_dir: "/tmp/project-a".to_string(),
        scope_warnings: Vec::new(),
        git: GitState {
            available: true,
            not_git_repo: false,
            branch: Some("main".to_string()),
            clean: Some(true),
            changed_entries: 0,
            summary: "branch `main`; clean".to_string(),
            warning: None,
        },
        latest_task: Some(StateSignal {
            source_path: "tasks/todo.md".to_string(),
            line: Some(1),
            heading: Some("Current Work".to_string()),
            text: "open action: run validation".to_string(),
        }),
        latest_handoff: None,
        latest_vcs: None,
        next_actions: vec![NextAction {
            source_path: "tasks/todo.md".to_string(),
            line: 2,
            heading: Some("Current Work".to_string()),
            text: "run validation".to_string(),
        }],
        task_source_state: TaskSourceState::ParsedOpenTasks,
        memory: MemoryState::default(),
        collection_warnings: Vec::new(),
    }
}

fn make_agent_metrics() -> AgentUsefulnessMetrics {
    AgentUsefulnessMetrics {
        latest_project_state_present: true,
        scope_present: true,
        git_state_present: true,
        git_state_present_or_not_git_repo: true,
        latest_work_present: true,
        next_action_present: true,
        no_open_tasks_detected: false,
        task_source_state: TaskSourceState::ParsedOpenTasks,
        scope_health_present: true,
        memory_degraded_warning_present: false,
        fragment_count: 0,
        duplicate_cluster_count: 0,
        boilerplate_action_count: 0,
        unrelated_machine_items: 0,
        source_backed_next_actions: true,
        answerability_passed: true,
    }
}

struct FakeHealthStore {
    active_chunks: usize,
    readable_chunks: Vec<MemoryChunk>,
}

impl FakeHealthStore {
    fn new(active_chunks: usize, readable_chunks: Vec<MemoryChunk>) -> Self {
        Self {
            active_chunks,
            readable_chunks,
        }
    }
}

#[async_trait::async_trait]
impl Store for FakeHealthStore {
    async fn add(&self, chunk: MemoryChunk) -> Result<ChunkId> {
        Ok(chunk.chunk_id)
    }

    async fn add_batch(&self, chunks: Vec<MemoryChunk>) -> Result<Vec<ChunkId>> {
        Ok(chunks.into_iter().map(|chunk| chunk.chunk_id).collect())
    }

    async fn get(&self, _tenant_id: &TenantId, _chunk_id: &ChunkId) -> Result<Option<MemoryChunk>> {
        Ok(None)
    }

    async fn search(
        &self,
        _tenant_id: &TenantId,
        _query: &str,
        _k: usize,
    ) -> Result<Vec<MemoryChunk>> {
        Ok(Vec::new())
    }

    async fn list_chunks_for_project(
        &self,
        _tenant_id: &TenantId,
        _project_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MemoryChunk>> {
        Ok(self
            .readable_chunks
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn delete(&self, _tenant_id: &TenantId, _chunk_id: &ChunkId) -> Result<bool> {
        Ok(false)
    }

    async fn stats(&self, _tenant_id: &TenantId) -> Result<StoreStats> {
        Ok(StoreStats {
            active_chunks: self.active_chunks,
            ..StoreStats::default()
        })
    }

    async fn health_snapshot(
        &self,
        _tenant_id: &TenantId,
        _project_id: Option<&str>,
        _duplicate_limit: usize,
    ) -> Result<Option<StoreHealthSnapshot>> {
        Ok(Some(StoreHealthSnapshot {
            counts: HealthCounts {
                active_chunks: self.active_chunks,
                total_chunks: self.active_chunks,
                ..HealthCounts::default()
            },
            chunk_types_active: HashMap::new(),
            chunk_types_all: HashMap::new(),
            duplicates: DuplicateHealth::default(),
            index_coverage: IndexCoverageHealth::default(),
            payload: PayloadHealth::default(),
        }))
    }
}

fn make_takeaway(chunk_id: &str, text: &str, tags: Vec<&str>, chunk_type: &str) -> Takeaway {
    Takeaway {
        chunk_id: chunk_id.to_string(),
        tenant_id: "t".to_string(),
        project_id: Some("p".to_string()),
        text: text.to_string(),
        score: 0.0,
        priority: 0.0,
        chunk_type: chunk_type.to_string(),
        timestamp_created: 0,
        tags: tags.into_iter().map(str::to_string).collect(),
        sources: BTreeSet::new(),
        occurrences: 1,
    }
}

fn make_takeaway_with_project(chunk_id: &str, project_id: Option<&str>) -> Takeaway {
    let mut takeaway = make_takeaway(
        chunk_id,
        "Machine-wide reusable rule. Agent action: verify before reuse.",
        vec!["kind:decision"],
        "decision",
    );
    takeaway.project_id = project_id.map(str::to_string);
    takeaway
}

#[test]
fn success_traces_are_not_filed_under_known_failures() {
    // A fully successful trace mentioning "0 failures" used to be
    // filed under Known Failures with fabricated avoid-guidance —
    // an active meaning inversion.
    let success = make_takeaway(
        "success",
        "Trace: ran alpha ETL end-to-end, 0 failures after retry patch was applied.",
        vec!["kind:run"],
        "trace",
    );
    let cat = takeaway_category(&success);
    assert_ne!(cat.heading, "Known Failures", "got: {}", cat.heading);
    assert_eq!(cat.heading, "Validated Fixes");

    // Arrival via a *_failures retrieval query alone is not
    // failure evidence either.
    let mut via_query = make_takeaway(
        "via-query",
        "Benchmark: HNSW recall comparable to IVFFlat at this corpus size.",
        vec![],
        "summary",
    );
    via_query.sources.insert("project_failures".to_string());
    assert_ne!(takeaway_category(&via_query).heading, "Known Failures");
}

#[test]
fn real_failures_still_classify_as_known_failures() {
    let tagged = make_takeaway(
        "tagged",
        "Ingest run aborted on schema mismatch.",
        vec!["kind:failure"],
        "trace",
    );
    assert_eq!(takeaway_category(&tagged).heading, "Known Failures");

    let root_cause = make_takeaway(
        "root-cause",
        "Root cause: NFS stall truncated the segment write; job failed because fsync never returned.",
        vec![],
        "summary",
    );
    assert_eq!(takeaway_category(&root_cause).heading, "Known Failures");
}

#[test]
fn extract_covered_task_ids_parses_summary_footer() {
    let text = "Highlight library for foo contains 3 ranked lessons.\nCovers tasks: task:id:T1, task:id:T2, task:id:T3";
    let ids = extract_covered_task_ids(text);
    assert_eq!(
        ids,
        vec!["T1".to_string(), "T2".to_string(), "T3".to_string()]
    );
}

#[test]
fn task_finish_suppressed_when_covered_by_highlight() {
    let mut takeaways = vec![
        make_takeaway(
            "digest1",
            "Highlight library contains lessons.\nCovers tasks: task:id:T1, task:id:T2",
            vec!["task:role:highlight_library", "task:status:generated"],
            "summary",
        ),
        make_takeaway(
            "raw1",
            "Task T1 finished successfully.",
            vec!["task:kind:task_finish", "task:id:T1"],
            "summary",
        ),
        make_takeaway(
            "raw2",
            "Task T3 finished — not covered.",
            vec!["task:kind:task_finish", "task:id:T3"],
            "summary",
        ),
    ];
    suppress_finishes_covered_by_libraries(&mut takeaways);
    let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
    assert!(ids.contains(&"digest1"));
    assert!(!ids.contains(&"raw1"), "covered finish should be dropped");
    assert!(ids.contains(&"raw2"), "uncovered finish should survive");
}

#[test]
fn user_explicit_priority_high_survives_suppression() {
    let mut takeaways = vec![
        make_takeaway(
            "digest1",
            "Covers tasks: task:id:T1",
            vec!["task:role:highlight_library", "task:status:generated"],
            "summary",
        ),
        make_takeaway(
            "raw1",
            "Task T1 finished — but operator marked it high priority.",
            vec!["task:kind:task_finish", "task:id:T1", "priority:9"],
            "summary",
        ),
    ];
    suppress_finishes_covered_by_libraries(&mut takeaways);
    let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
    assert!(ids.contains(&"raw1"), "priority>=8 finish must survive");
}

#[test]
fn unverified_role_tag_does_not_suppress() {
    // A user-authored chunk that spoofs the role tag but lacks
    // `task:status:generated` must not suppress real finishes.
    let mut takeaways = vec![
        make_takeaway(
            "spoof",
            "Covers tasks: task:id:T1",
            vec!["task:role:highlight_library"],
            "summary",
        ),
        make_takeaway(
            "raw1",
            "Task T1 finished successfully.",
            vec!["task:kind:task_finish", "task:id:T1"],
            "summary",
        ),
    ];
    suppress_finishes_covered_by_libraries(&mut takeaways);
    let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
    assert!(ids.contains(&"raw1"), "spoofed digest must not suppress");
}

#[test]
fn cross_project_finish_is_not_suppressed() {
    // Digest belongs to project p; the finish belongs to a
    // different project and shares the task id. It must survive.
    let mut digest = make_takeaway(
        "digest_p",
        "Covers tasks: task:id:T1",
        vec!["task:role:highlight_library", "task:status:generated"],
        "summary",
    );
    digest.project_id = Some("project-a".to_string());
    let mut raw = make_takeaway(
        "raw_other",
        "Task T1 finished in a different project.",
        vec!["task:kind:task_finish", "task:id:T1"],
        "summary",
    );
    raw.project_id = Some("project-b".to_string());
    let mut takeaways = vec![digest, raw];
    suppress_finishes_covered_by_libraries(&mut takeaways);
    let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
    assert!(
        ids.contains(&"raw_other"),
        "finish from another project must not be suppressed"
    );
}

#[test]
fn decimal_priority_survives_suppression() {
    let mut takeaways = vec![
        make_takeaway(
            "digest1",
            "Covers tasks: task:id:T1",
            vec!["task:role:highlight_library", "task:status:generated"],
            "summary",
        ),
        make_takeaway(
            "raw1",
            "Task T1 finished — operator set a decimal priority.",
            vec!["task:kind:task_finish", "task:id:T1", "priority:8.5"],
            "summary",
        ),
    ];
    suppress_finishes_covered_by_libraries(&mut takeaways);
    let ids: Vec<&str> = takeaways.iter().map(|t| t.chunk_id.as_str()).collect();
    assert!(ids.contains(&"raw1"), "priority:8.5 finish must survive");
}

#[test]
fn library_bonus_outranks_raw_chunk() {
    let counts = HashMap::new();
    let hit_stats = HashMap::new();
    let library = make_takeaway(
        "digest",
        "Highlight library text.",
        vec!["task:role:highlight_library", "kind:finish"],
        "summary",
    );
    let raw = make_takeaway("raw", "Plain finish.", vec!["kind:finish"], "summary");
    let lib_score = priority_score(&library, &counts, &hit_stats, 0);
    let raw_score = priority_score(&raw, &counts, &hit_stats, 0);
    assert!(
        lib_score > raw_score + 10.0,
        "library bonus should dominate: lib={lib_score}, raw={raw_score}"
    );
}

#[test]
fn priority_score_does_not_treat_exposure_as_success() {
    let counts = HashMap::new();
    let cold = make_takeaway("cold", "text", vec!["kind:finish"], "summary");
    let hot = make_takeaway("hot", "text", vec!["kind:finish"], "summary");
    let mut hits = HashMap::new();
    hits.insert(
        "hot".to_string(),
        HitStats {
            exposure_count: 7,
            rendered_count: 7,
            last_ts_ms: 1,
        },
    );
    let cold_score = priority_score(&cold, &counts, &hits, 1);
    let hot_score = priority_score(&hot, &counts, &hits, 1);
    assert_eq!(hot_score, cold_score, "exposure must not create utility");
}

#[test]
fn priority_score_demotes_unused_old_chunks() {
    let counts = HashMap::new();
    let hits: HashMap<String, HitStats> = HashMap::new();
    let recent = {
        let mut t = make_takeaway("recent", "text", vec!["kind:finish"], "summary");
        t.timestamp_created = 1_000_000;
        t
    };
    let stale = {
        let mut t = make_takeaway("stale", "text", vec!["kind:finish"], "summary");
        t.timestamp_created = 1_000_000; // very old
        t
    };
    // now: a few hours after `recent`; ~365 days after `stale`.
    let now_recent = 1_000_000 + 3 * 3_600_000;
    let now_stale = 1_000_000 + 365 * 86_400_000;
    let recent_score = priority_score(&recent, &counts, &hits, now_recent);
    let stale_score = priority_score(&stale, &counts, &hits, now_stale);
    assert!(
        recent_score > stale_score,
        "stale chunk must be demoted: recent={recent_score}, stale={stale_score}"
    );
}

#[test]
fn generated_digest_candidates_are_skipped_from_takeaways() {
    let tags = vec![
        "task:status:generated".to_string(),
        "task:role:decision_library".to_string(),
    ];
    assert!(is_generated_digest_takeaway(&tags));
    assert!(!is_generated_digest_takeaway(&[
        "kind:decision".to_string(),
        "priority:9".to_string(),
    ]));

    let payload = serde_json::json!({
        "results": [
            {
                "chunk_id": "digest",
                "tenant_id": "t",
                "project_id": "p",
                "text": "Task digest status generated. Summary: Highlight library for p contains 2 ranked lessons.",
                "score": 25.0,
                "chunk_type": "summary",
                "timestamp_created": 10,
                "tags": tags
            },
            {
                "chunk_id": "raw",
                "tenant_id": "t",
                "project_id": "p",
                "text": "Validated fix: use the stable project scope when restoring the gateway.",
                "score": 4.0,
                "chunk_type": "summary",
                "timestamp_created": 11,
                "tags": ["kind:finish"]
            }
        ]
    });
    let mut by_chunk = HashMap::new();
    let mut explanations = Vec::new();
    merge_payload_candidates(
        &mut by_chunk,
        &mut explanations,
        &payload,
        "project",
        "project_highlights",
        "project takeaways",
        CliQueryMode::FindHighlights,
    );
    assert!(!by_chunk.contains_key("digest"));
    assert!(by_chunk.contains_key("raw"));
    assert_eq!(
        explanations
            .iter()
            .find(|item| item.chunk_id == "digest")
            .and_then(|item| item.filter_reason.as_deref()),
        Some("generated_digest_wrapper")
    );
    assert_eq!(
        explanations
            .iter()
            .find(|item| item.chunk_id == "raw")
            .map(|item| item.display_status.as_str()),
        Some("candidate")
    );
}
