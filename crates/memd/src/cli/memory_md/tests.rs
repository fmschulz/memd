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
use crate::types::{ChunkId, ChunkStatus, ChunkType, MemoryChunk, ProjectId};

#[test]
fn atomic_replace_overwrites_complete_file_and_cleans_temps() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("memory.md");
    fs::write(&output, "old complete file\n").unwrap();

    atomic_replace(&output, b"new complete file\n").unwrap();

    assert_eq!(fs::read_to_string(&output).unwrap(), "new complete file\n");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

    let blocked = dir.path().join("blocked.md");
    fs::create_dir(&blocked).unwrap();
    assert!(atomic_replace(&blocked, b"must not publish").is_err());
    assert!(blocked.is_dir());
    assert!(fs::read_dir(dir.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[cfg(unix)]
#[test]
fn atomic_replace_preserves_existing_output_symlink() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("generated-memory.md");
    let output = dir.path().join("memory.md");
    fs::write(&target, "old complete file\n").unwrap();
    symlink("generated-memory.md", &output).unwrap();

    atomic_replace(&output, b"new complete file\n").unwrap();

    assert!(fs::symlink_metadata(&output)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), "new complete file\n");
}

#[cfg(unix)]
#[test]
fn atomic_replace_preserves_permissions_and_creates_private_files() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.md");
    fs::write(&existing, "old\n").unwrap();
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o640)).unwrap();

    atomic_replace(&existing, b"replacement\n").unwrap();
    assert_eq!(
        fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
        0o640
    );

    let new_output = dir.path().join("new.md");
    atomic_replace(&new_output, b"new\n").unwrap();
    assert_eq!(
        fs::metadata(new_output).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn explicit_priority_scales_small_values_and_caps_large_values() {
    assert_eq!(explicit_priority(&["priority:7".to_string()]), Some(70.0));
    assert_eq!(
        explicit_priority(&["importance:120".to_string()]),
        Some(100.0)
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
fn scan_candidate_filter_reasons_gate_admission() {
    let no_tags: Vec<String> = Vec::new();
    for status in [
        ChunkStatus::Candidate,
        ChunkStatus::Deleted,
        ChunkStatus::Error,
        ChunkStatus::Superseded,
        ChunkStatus::Expired,
    ] {
        assert_eq!(
            scan_candidate_filter_reason(status, &no_tags, "Decision: keep it."),
            Some("not_visible")
        );
    }
    assert_eq!(
        scan_candidate_filter_reason(ChunkStatus::Final, &no_tags, ""),
        Some("empty_text")
    );
    let digest_tags = vec![
        "task:status:generated".to_string(),
        "task:role:highlight_library".to_string(),
    ];
    assert_eq!(
        scan_candidate_filter_reason(ChunkStatus::Final, &digest_tags, "Task digest."),
        Some("generated_digest_wrapper")
    );
    let fragment_tags = vec!["chunk_index:1".to_string()];
    assert_eq!(
        scan_candidate_filter_reason(
            ChunkStatus::Final,
            &fragment_tags,
            "and then continued from a prior chunk"
        ),
        Some("fragment_like")
    );
    let superseded_tags = vec!["kind:superseded".to_string()];
    assert_eq!(
        scan_candidate_filter_reason(ChunkStatus::Final, &superseded_tags, "Old lesson."),
        Some("superseded_tag")
    );
    assert_eq!(
        scan_candidate_filter_reason(
            ChunkStatus::Final,
            &["kind:decision".to_string()],
            "Decision: keep project scopes explicit."
        ),
        None
    );
}

#[tokio::test]
async fn scan_partitions_candidates_by_active_project_in_one_pass() {
    let tenant = TenantId::new("t").unwrap();
    let mut superseded = MemoryChunk::new(
        tenant.clone(),
        "Decision: replaced fact.",
        ChunkType::Decision,
    )
    .with_project(ProjectId::from("project-a"));
    superseded.status = ChunkStatus::Superseded;
    let chunks = vec![
        MemoryChunk::new(
            tenant.clone(),
            "Decision: active project fact.",
            ChunkType::Decision,
        )
        .with_project(ProjectId::from("project-a"))
        .with_tags(vec!["kind:decision".to_string()]),
        MemoryChunk::new(
            tenant.clone(),
            "Decision: other project fact.",
            ChunkType::Decision,
        )
        .with_project(ProjectId::from("project-b")),
        MemoryChunk::new(
            tenant.clone(),
            "Decision: tenant-wide fact.",
            ChunkType::Decision,
        ),
        superseded,
    ];
    let store = FakeHealthStore::new(chunks.len(), chunks);

    let (project, global, explanations) =
        scan_takeaway_candidates(&store, &tenant, Some("project-a"))
            .await
            .unwrap();

    assert_eq!(
        project
            .iter()
            .map(|takeaway| takeaway.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Decision: active project fact."]
    );
    assert_eq!(project[0].sources, BTreeSet::from(["scan".to_string()]));
    assert_eq!(
        global
            .iter()
            .map(|takeaway| takeaway.text.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Decision: other project fact.",
            "Decision: tenant-wide fact."
        ]
    );
    assert_eq!(explanations.len(), 4);
    assert!(explanations
        .iter()
        .all(|explanation| explanation.source == "scan"
            && explanation.mode == "scan"
            && explanation.query.is_empty()));
    assert_eq!(
        explanations
            .iter()
            .map(|explanation| explanation.raw_rank)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(
        explanations
            .iter()
            .map(|explanation| explanation.section.as_str())
            .collect::<Vec<_>>(),
        vec!["project", "machine_wide", "machine_wide", "project"]
    );
    let filtered = explanations
        .iter()
        .find(|explanation| explanation.filter_reason.is_some())
        .expect("superseded chunk explanation");
    assert_eq!(filtered.filter_reason.as_deref(), Some("not_visible"));
    assert_eq!(filtered.display_status, "filtered");
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
        sources: BTreeSet::from(["scan".to_string()]),
        occurrences: 1,
    };
    let state = make_project_state();
    let rendered = render_memory_md(&state, &[], &[takeaway], &[]);
    assert!(rendered.contains("## Project Fact Library"));
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
        sources: BTreeSet::from(["scan".to_string()]),
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
    assert!(rendered.contains("- chunk: `decision`; priority: `80.0`; tags: `kind:decision`"));
    assert!(rendered.contains("- chunk: `command`; priority: `60.0`; tags: `kind:run`"));
}

#[test]
fn memory_md_quality_report_scores_useful_items_and_wrappers() {
    let content = r#"# memory.md

## Project Fact Library

### Decisions

1. Decision: keep project aliases explicit.
   - chunk: `decision-a`; priority: `80.0`; tags: `kind:decision`

### Other Takeaways

1. Task digest status generated. Summary: Highlight library for p contains 2 ranked lessons.
   - chunk: `digest-a`; priority: `40.0`; tags: `task:status:generated, task:role:highlight_library`

2. Routine status update with no category signal.

## Machine-Wide Fact Library

1. Decision outside project section should not be counted.
"#;
    let report = evaluate_memory_md_quality(content, 10);
    assert_eq!(report.displayed_count, 3);
    assert_eq!(report.useful_count, 1);
    assert_eq!(report.generated_wrapper_count, 1);
    assert!((report.useful_ratio - (1.0 / 3.0)).abs() < f64::EPSILON);
}

fn make_project_state() -> ProjectState {
    ProjectState {
        generated_unix_ms: 123,
        tenant_id: "tenant-a".to_string(),
        project_id: Some("project-a".to_string()),
        configured_project_dir: Some("/tmp/project-a".to_string()),
        resolved_project_dir: "/tmp/project-a".to_string(),
        scope_warnings: Vec::new(),
        memory: MemoryState::default(),
        collection_warnings: Vec::new(),
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

    async fn list_chunks(
        &self,
        _tenant_id: &TenantId,
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

    // Arrival via a failure-flavoured retrieval source alone is not
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
    let priors = HashMap::new();
    let lib_score = priority_score(&library, &counts, &hit_stats, &priors, 0);
    let raw_score = priority_score(&raw, &counts, &hit_stats, &priors, 0);
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
    let priors = HashMap::new();
    let cold_score = priority_score(&cold, &counts, &hits, &priors, 1);
    let hot_score = priority_score(&hot, &counts, &hits, &priors, 1);
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
    let priors = HashMap::new();
    let recent_score = priority_score(&recent, &counts, &hits, &priors, now_recent);
    let stale_score = priority_score(&stale, &counts, &hits, &priors, now_stale);
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
    assert_eq!(
        scan_candidate_filter_reason(
            ChunkStatus::Final,
            &tags,
            "Task digest status generated. Summary: Highlight library for p contains 2 ranked lessons."
        ),
        Some("generated_digest_wrapper")
    );
}

#[test]
fn repo_novelty_tokenizer_keeps_identifier_runs_and_drops_filler() {
    let tokens = repo_novelty_tokens(
        "Always run `sacct -M perceus-00` on Dori; tasks/METHODS.md has the exact command.",
    );
    assert!(tokens.contains("sacct"));
    assert!(tokens.contains("perceus-00"), "hostname stays one token");
    assert!(
        tokens.contains("tasks/methods.md"),
        "path stays one lowercased token"
    );
    assert!(!tokens.contains("dori"), "tokens under 5 chars dropped");
    assert!(!tokens.contains("always"), "stopwords dropped");
}

#[test]
fn repo_coverage_threshold_is_sixty_percent_of_takeaway_tokens() {
    let index = vec![RepoDoc {
        path: "tasks/todo.md".to_string(),
        tokens: repo_novelty_tokens("token1 token2 token3 token4 token5 token6"),
    }];

    // 10 tokens, 6 in the doc -> exactly 0.6 -> suppressed.
    let covered = "token1 token2 token3 token4 token5 token6 fresh07 fresh08 fresh09 fresh10";
    // 10 tokens, 5 in the doc -> 0.5 -> kept.
    let uncovered = "token1 token2 token3 token4 token5 fresh06 fresh07 fresh08 fresh09 fresh10";
    // Fully contained but only 6 tokens (< 8) -> gate skipped.
    let short = "token1 token2 token3 token4 token5 token6";
    let mut takeaways = vec![
        make_takeaway("covered", covered, vec![], "summary"),
        make_takeaway("uncovered", uncovered, vec![], "summary"),
        make_takeaway("short", short, vec![], "summary"),
        make_takeaway("pinned", covered, vec!["priority:9"], "summary"),
    ];

    let suppressed = suppress_repo_covered(&mut takeaways, &index);

    assert_eq!(
        suppressed.get("covered").map(String::as_str),
        Some("covered_by_repo:tasks/todo.md")
    );
    assert_eq!(suppressed.len(), 1);
    let kept: Vec<&str> = takeaways
        .iter()
        .map(|takeaway| takeaway.chunk_id.as_str())
        .collect();
    assert_eq!(kept, vec!["uncovered", "short", "pinned"]);
}

#[test]
fn repo_index_excludes_generated_memory_md_and_memd_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("tasks/.memd")).unwrap();
    fs::create_dir_all(root.join("docs/handoffs/_archive")).unwrap();
    fs::write(root.join("tasks/todo.md"), "verify consolidation ledger").unwrap();
    fs::write(
        root.join("docs/handoffs/session.md"),
        "tenant scoping notes",
    )
    .unwrap();
    fs::write(
        root.join("docs/handoffs/_archive/old.md"),
        "archived handoff",
    )
    .unwrap();
    fs::write(root.join("README.md"), "project readme overview").unwrap();
    // The previous refresh's output: if it entered the index it would
    // cover every takeaway it rendered and the next refresh would
    // suppress them all.
    fs::write(root.join("tasks/memory.md"), "generated fact library").unwrap();
    fs::write(root.join("tasks/.memd/state.md"), "internal memd state").unwrap();

    let index = build_repo_index(root, &root.join("tasks/memory.md"));

    let paths: Vec<&str> = index.iter().map(|doc| doc.path.as_str()).collect();
    assert!(paths.contains(&"tasks/todo.md"), "paths: {paths:?}");
    assert!(paths.contains(&"docs/handoffs/session.md"));
    assert!(paths.contains(&"README.md"));
    assert!(!paths.iter().any(|path| path.contains("memory.md")));
    assert!(!paths.iter().any(|path| path.contains("_archive")));
    assert!(!paths.iter().any(|path| path.contains(".memd")));
}
