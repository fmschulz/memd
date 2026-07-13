use super::action::*;
use super::collect::scope_path_drift_warning;
use super::rank::*;
use super::state::*;
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct AgentUsefulnessMetrics {
    pub(super) latest_project_state_present: bool,
    pub(super) scope_present: bool,
    pub(super) git_state_present: bool,
    pub(super) git_state_present_or_not_git_repo: bool,
    pub(super) latest_work_present: bool,
    pub(super) next_action_present: bool,
    pub(super) no_open_tasks_detected: bool,
    pub(super) task_source_state: TaskSourceState,
    pub(super) scope_health_present: bool,
    pub(super) memory_degraded_warning_present: bool,
    pub(super) fragment_count: usize,
    pub(super) duplicate_cluster_count: usize,
    pub(super) boilerplate_action_count: usize,
    pub(super) unrelated_machine_items: usize,
    pub(super) source_backed_next_actions: bool,
    pub(super) answerability_passed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct MemoryMdQualityReport {
    pub(super) displayed_count: usize,
    pub(super) useful_count: usize,
    pub(super) generated_wrapper_count: usize,
    pub(super) missing_reason_count: usize,
    pub(super) missing_action_count: usize,
    pub(super) useful_ratio: f64,
}

pub(in crate::cli) async fn run_memory_md_eval<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: MemoryMdEvalOptions,
) -> Result<Value> {
    if options.gold_file.is_some() {
        return run_memory_md_gold_eval(store, tenant_manager, &options).await;
    }

    let project_dir = absolutize_project_dir(&options.project_dir)?;
    let output = options.output.clone();
    let rendered_path = if output.is_absolute() {
        output.clone()
    } else {
        project_dir.join(&output)
    };
    let refresh = refresh_memory_md_with_health(
        store,
        tenant_manager,
        MemoryMdOptions {
            tenant_id: options.tenant_id,
            project_id: options.project_id,
            project_dir,
            output,
            project_limit: options.project_limit,
            global_limit: if options.agent_usefulness { 2 } else { 0 },
            candidate_k: options.candidate_k,
            explain_output: None,
        },
    )
    .await?;
    let content = std::fs::read_to_string(&rendered_path)?;
    let top_n = options.top_n.clamp(1, 10);
    let report = evaluate_memory_md_quality(&content, top_n);
    let min_useful_ratio = options.min_useful_ratio.clamp(0.0, 1.0);

    let mut failures = Vec::new();
    if report.displayed_count == 0 {
        failures.push("no project takeaways were displayed".to_string());
    }
    if report.useful_ratio + f64::EPSILON < min_useful_ratio {
        failures.push(format!(
            "useful_ratio {:.3} below threshold {:.3}",
            report.useful_ratio, min_useful_ratio
        ));
    }
    if report.generated_wrapper_count > options.max_generated_wrappers {
        failures.push(format!(
            "generated_wrapper_count {} exceeds threshold {}",
            report.generated_wrapper_count, options.max_generated_wrappers
        ));
    }
    if report.missing_reason_count > 0 {
        failures.push(format!(
            "{} displayed items are missing reason metadata",
            report.missing_reason_count
        ));
    }
    if report.missing_action_count > 0 {
        failures.push(format!(
            "{} displayed items are missing concrete agent action guidance",
            report.missing_action_count
        ));
    }
    let agent_usefulness = if options.agent_usefulness {
        let metrics: AgentUsefulnessMetrics = serde_json::from_value(
            refresh
                .get("agent_usefulness")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .map_err(|error| {
            MemdError::ValidationError(format!("invalid agent_usefulness metrics: {error}"))
        })?;
        failures.extend(agent_usefulness_failures(&metrics));
        Some(metrics)
    } else {
        None
    };

    let payload = json!({
        "passed": failures.is_empty(),
        "output": rendered_path,
        "agent_usefulness": agent_usefulness,
        "top_n": top_n,
        "displayed_count": report.displayed_count,
        "useful_count": report.useful_count,
        "useful_ratio": report.useful_ratio,
        "generated_wrapper_count": report.generated_wrapper_count,
        "missing_reason_count": report.missing_reason_count,
        "missing_action_count": report.missing_action_count,
        "thresholds": {
            "min_useful_ratio": min_useful_ratio,
            "max_generated_wrappers": options.max_generated_wrappers,
        },
        "refresh": refresh,
        "failures": failures,
    });

    if !failures.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "memory-md quality thresholds failed: {}",
            serde_json::to_string(&payload)?
        )));
    }

    Ok(payload)
}

#[derive(Debug, Deserialize)]
pub(super) struct MemoryMdGoldFile {
    pub(super) projects: Vec<MemoryMdGoldProject>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MemoryMdGoldProject {
    pub(super) name: Option<String>,
    pub(super) project_dir: PathBuf,
    pub(super) must_contain: Option<Vec<String>>,
    pub(super) must_not_contain: Option<Vec<String>>,
    pub(super) expected_git: Option<bool>,
    pub(super) max_fragments: Option<usize>,
    pub(super) max_unrelated_machine_items: Option<usize>,
}

pub(super) async fn run_memory_md_gold_eval<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    options: &MemoryMdEvalOptions,
) -> Result<Value> {
    let gold_path = options
        .gold_file
        .as_ref()
        .expect("caller checked gold_file");
    let gold_text = fs::read_to_string(gold_path)?;
    let gold: MemoryMdGoldFile = serde_json::from_str(&gold_text).map_err(|error| {
        MemdError::ValidationError(format!("failed to parse {}: {error}", gold_path.display()))
    })?;

    let mut project_results = Vec::new();
    let mut failures = Vec::new();
    for (idx, project) in gold.projects.iter().enumerate() {
        let name = project
            .name
            .clone()
            .unwrap_or_else(|| project.project_dir.display().to_string());
        let output = std::env::temp_dir().join(format!("memd-memory-md-gold-{idx}.md"));
        let refresh = refresh_memory_md_with_health(
            store,
            tenant_manager,
            MemoryMdOptions {
                tenant_id: options.tenant_id.clone(),
                project_id: options.project_id.clone(),
                project_dir: project.project_dir.clone(),
                output: output.clone(),
                project_limit: options.project_limit,
                global_limit: 2,
                candidate_k: options.candidate_k,
                explain_output: None,
            },
        )
        .await?;
        let content = fs::read_to_string(&output)?;
        let metrics: AgentUsefulnessMetrics = serde_json::from_value(
            refresh
                .get("agent_usefulness")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .map_err(|error| {
            MemdError::ValidationError(format!("invalid agent_usefulness metrics: {error}"))
        })?;
        let mut project_failures = agent_usefulness_failures(&metrics);
        if let Some(expected_git) = project.expected_git {
            if let Some(failure) = expected_git_failure(&metrics, expected_git) {
                project_failures.push(failure);
            }
        }
        if let Some(max_fragments) = project.max_fragments {
            if metrics.fragment_count > max_fragments {
                project_failures.push(format!(
                    "fragment_count {} exceeds {}",
                    metrics.fragment_count, max_fragments
                ));
            }
        }
        if let Some(max_unrelated) = project.max_unrelated_machine_items {
            if metrics.unrelated_machine_items > max_unrelated {
                project_failures.push(format!(
                    "unrelated_machine_items {} exceeds {}",
                    metrics.unrelated_machine_items, max_unrelated
                ));
            }
        }
        for needle in project.must_contain.as_deref().unwrap_or(&[]) {
            if !content.contains(needle) {
                project_failures.push(format!("missing required text `{needle}`"));
            }
        }
        for needle in project.must_not_contain.as_deref().unwrap_or(&[]) {
            if content.contains(needle) {
                project_failures.push(format!("forbidden text present `{needle}`"));
            }
        }
        for failure in &project_failures {
            failures.push(format!("{name}: {failure}"));
        }
        project_results.push(json!({
            "name": name,
            "project_dir": project.project_dir.display().to_string(),
            "output": output,
            "agent_usefulness": metrics,
            "failures": project_failures,
        }));
    }

    let payload = json!({
        "passed": failures.is_empty(),
        "gold_file": gold_path,
        "projects": project_results,
        "failures": failures,
    });
    if !failures.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "memory-md gold-file thresholds failed: {}",
            serde_json::to_string(&payload)?
        )));
    }
    Ok(payload)
}

pub(super) fn agent_usefulness_failures(metrics: &AgentUsefulnessMetrics) -> Vec<String> {
    let mut failures = Vec::new();
    if !metrics.latest_project_state_present {
        failures.push("latest project state is missing".to_string());
    }
    if !metrics.scope_present {
        failures.push("scope is missing".to_string());
    }
    if !metrics.git_state_present_or_not_git_repo {
        failures.push("git state is missing for a git project".to_string());
    }
    if !metrics.latest_work_present {
        failures.push("latest work signal is missing".to_string());
    }
    match metrics.task_source_state {
        TaskSourceState::Missing => {
            failures.push("task source `tasks/todo.md` is missing".to_string());
        }
        TaskSourceState::ParseFailed => {
            failures.push("task source `tasks/todo.md` could not be parsed".to_string());
        }
        TaskSourceState::ParsedOpenTasks if !metrics.next_action_present => {
            failures.push("next actions are missing while open tasks exist".to_string());
        }
        TaskSourceState::ParsedNoOpenTasks if !metrics.no_open_tasks_detected => {
            failures.push("task source has no open tasks but state was not recorded".to_string());
        }
        TaskSourceState::ParsedNoOpenTasks | TaskSourceState::ParsedOpenTasks => {}
    }
    if !metrics.scope_health_present {
        failures.push("scope health is missing".to_string());
    }
    if metrics.fragment_count > 0 {
        failures.push(format!(
            "fragment_count {} exceeds threshold 0",
            metrics.fragment_count
        ));
    }
    if metrics.boilerplate_action_count > 0 {
        failures.push(format!(
            "boilerplate_action_count {} exceeds threshold 0",
            metrics.boilerplate_action_count
        ));
    }
    if metrics.unrelated_machine_items > 2 {
        failures.push(format!(
            "unrelated_machine_items {} exceeds threshold 2",
            metrics.unrelated_machine_items
        ));
    }
    if !metrics.source_backed_next_actions {
        failures.push("one or more next actions lack source path or line".to_string());
    }
    if !metrics.answerability_passed {
        failures.push("answerability_passed=false".to_string());
    }
    failures
}

pub(super) fn expected_git_failure(
    metrics: &AgentUsefulnessMetrics,
    expected_git: bool,
) -> Option<String> {
    match (expected_git, metrics.git_state_present) {
        (true, false) => Some("expected git state but git_state_present=false".to_string()),
        (false, true) => Some("expected no git state but git_state_present=true".to_string()),
        _ => None,
    }
}

pub(super) fn evaluate_agent_usefulness(
    state: &ProjectState,
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
) -> AgentUsefulnessMetrics {
    // These metrics are computed from the same structured state and filtered
    // startup items used by the renderer. They are a deterministic regression
    // gate for startup answerability, not an independent semantic judge.
    let latest_project_state_present = true;
    let scope_present = !state.tenant_id.is_empty() && !state.resolved_project_dir.is_empty();
    let git_state_present = state.git.available;
    let git_state_present_or_not_git_repo = state.git.available || state.git.not_git_repo;
    let latest_work_present =
        state.latest_task.is_some() || state.latest_handoff.is_some() || state.latest_vcs.is_some();
    let next_action_present = !state.next_actions.is_empty();
    let no_open_tasks_detected = state.task_source_state == TaskSourceState::ParsedNoOpenTasks;
    let scope_health_present = scope_health_checked(state);
    let memory_degraded_warning_present = state
        .memory
        .unreadable_active_chunks
        .map(|count| count > 0)
        .unwrap_or(false);
    let fragment_count = project_takeaways
        .iter()
        .chain(global_takeaways.iter())
        .filter(|takeaway| is_fragment_like_takeaway(takeaway))
        .count();
    let duplicate_cluster_count =
        visible_duplicate_cluster_count_union(project_takeaways, global_takeaways);
    let boilerplate_action_count = project_takeaways
        .iter()
        .chain(global_takeaways.iter())
        .filter(|takeaway| has_boilerplate_action(takeaway))
        .count();
    let unrelated_machine_items = global_takeaways
        .iter()
        .filter(|takeaway| !is_machine_wide_startup_relevant(takeaway))
        .count();
    let source_backed_next_actions = state
        .next_actions
        .iter()
        .all(|action| !action.source_path.is_empty() && action.line > 0);
    let task_source_answerable = matches!(
        state.task_source_state,
        TaskSourceState::ParsedNoOpenTasks | TaskSourceState::ParsedOpenTasks
    );
    let answerability_passed = latest_project_state_present
        && scope_present
        && git_state_present_or_not_git_repo
        && latest_work_present
        && task_source_answerable
        && (next_action_present || no_open_tasks_detected)
        && fragment_count == 0
        && boilerplate_action_count == 0;

    AgentUsefulnessMetrics {
        latest_project_state_present,
        scope_present,
        git_state_present,
        git_state_present_or_not_git_repo,
        latest_work_present,
        next_action_present,
        no_open_tasks_detected,
        task_source_state: state.task_source_state,
        scope_health_present,
        memory_degraded_warning_present,
        fragment_count,
        duplicate_cluster_count,
        boilerplate_action_count,
        unrelated_machine_items,
        source_backed_next_actions,
        answerability_passed,
    }
}

pub(super) fn scope_health_checked(state: &ProjectState) -> bool {
    if let Some(configured) = state.configured_project_dir.as_deref() {
        scope_path_drift_warning(configured, Path::new(&state.resolved_project_dir)).is_none()
            || state
                .scope_warnings
                .iter()
                .any(|warning| warning.starts_with("scope mismatch:"))
    } else {
        true
    }
}

pub(super) fn visible_duplicate_cluster_count_union(
    project_takeaways: &[Takeaway],
    global_takeaways: &[Takeaway],
) -> usize {
    duplicate_components(project_takeaways, global_takeaways)
        .into_iter()
        .filter(|component| component.len() > 1)
        .count()
}

#[derive(Debug, Clone)]
pub(super) struct DisplayedMemoryMdItem {
    pub(super) category: String,
    pub(super) text: String,
    pub(super) details: Vec<String>,
}

pub(super) fn evaluate_memory_md_quality(content: &str, top_n: usize) -> MemoryMdQualityReport {
    let items = parse_project_takeaways(content, top_n);
    let displayed_count = items.len();
    let useful_count = items
        .iter()
        .filter(|item| is_useful_display_item(item))
        .count();
    let generated_wrapper_count = items
        .iter()
        .filter(|item| is_generated_wrapper_display_item(item))
        .count();
    let missing_reason_count = items
        .iter()
        .filter(|item| !item.details.iter().any(|line| line.contains("reason: `")))
        .count();
    let missing_action_count = items
        .iter()
        .filter(|item| !has_concrete_agent_action(item))
        .count();
    let useful_ratio = if displayed_count == 0 {
        0.0
    } else {
        useful_count as f64 / displayed_count as f64
    };

    MemoryMdQualityReport {
        displayed_count,
        useful_count,
        generated_wrapper_count,
        missing_reason_count,
        missing_action_count,
        useful_ratio,
    }
}

pub(super) fn parse_project_takeaways(content: &str, top_n: usize) -> Vec<DisplayedMemoryMdItem> {
    let mut in_project = false;
    let mut category = "Other Takeaways".to_string();
    let mut items = Vec::new();
    let mut current: Option<DisplayedMemoryMdItem> = None;

    for line in content.lines() {
        if line.starts_with("## ") {
            if in_project {
                if let Some(item) = current.take() {
                    items.push(item);
                }
                break;
            }
            let heading = line.trim();
            in_project = heading == "## Project Takeaways" || heading == "## Project Fact Library";
            continue;
        }
        if !in_project {
            continue;
        }
        if let Some(heading) = line.strip_prefix("### ") {
            if let Some(item) = current.take() {
                items.push(item);
                if items.len() >= top_n {
                    break;
                }
            }
            category = heading.trim().to_string();
            continue;
        }
        if let Some(text) = ordered_item_text(line) {
            if let Some(item) = current.take() {
                items.push(item);
                if items.len() >= top_n {
                    break;
                }
            }
            current = Some(DisplayedMemoryMdItem {
                category: category.clone(),
                text: text.to_string(),
                details: Vec::new(),
            });
            continue;
        }
        if let Some(item) = current.as_mut() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                item.details.push(trimmed.to_string());
            }
        }
    }
    if items.len() < top_n {
        if let Some(item) = current {
            items.push(item);
        }
    }
    items.truncate(top_n);
    items
}

pub(super) fn ordered_item_text(line: &str) -> Option<&str> {
    let (number, rest) = line.split_once(". ")?;
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(rest.trim())
}

pub(super) fn is_useful_display_item(item: &DisplayedMemoryMdItem) -> bool {
    if is_generated_wrapper_display_item(item) {
        return false;
    }
    if !has_concrete_agent_action(item) {
        return false;
    }

    matches!(
        item.category.as_str(),
        "Decisions"
            | "Validated Fixes"
            | "Known Failures"
            | "Commands/Paths"
            | "Open Follow-ups"
            | "Evidence"
    ) || {
        let lowered = item.text.to_ascii_lowercase();
        lowered.contains("decision:")
            || lowered.contains("rationale:")
            || lowered.contains("validation:")
            || lowered.contains("validated")
            || lowered.contains("root cause")
            || lowered.contains("command:")
            || lowered.contains("path:")
            || lowered.contains("follow-up:")
            || lowered.contains("next step:")
    }
}

pub(super) fn has_concrete_agent_action(item: &DisplayedMemoryMdItem) -> bool {
    item.details.iter().any(|line| {
        let lowered = line.to_ascii_lowercase();
        let Some(action) = lowered.strip_prefix("- agent action: `") else {
            return false;
        };
        let action = action.trim_end_matches('`').trim();
        is_concrete_agent_action_text(action)
            && !action.contains("translate this takeaway into a task-specific rule")
    })
}

pub(super) fn is_generated_wrapper_display_item(item: &DisplayedMemoryMdItem) -> bool {
    let mut lowered = item.text.to_ascii_lowercase();
    for line in &item.details {
        lowered.push(' ');
        lowered.push_str(&line.to_ascii_lowercase());
    }
    lowered.contains("task digest status generated")
        || lowered.contains("task:status:generated")
        || lowered.contains("task:role:highlight_library")
        || lowered.contains("task:digest:")
        || (lowered.contains("highlight library for") && lowered.contains("ranked lessons"))
}
