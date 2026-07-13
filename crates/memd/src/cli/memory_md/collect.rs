use super::state::*;
use super::*;

pub(super) async fn collect_project_state<S: Store>(
    store: &S,
    tenant: &TenantId,
    tenant_id: &str,
    project_id: Option<&str>,
    project_dir: &Path,
    scope: Option<&ProjectScopeConfig>,
    generated_unix_ms: u128,
) -> ProjectState {
    let configured_project_dir = scope.map(|scope| scope.project_dir.clone());
    let mut collection_warnings = Vec::new();
    let mut scope_warnings = Vec::new();
    if let Some(configured) = configured_project_dir.as_deref() {
        if let Some(warning) = scope_path_drift_warning(configured, project_dir) {
            scope_warnings.push(warning);
        }
    }

    let git = collect_git_state(project_dir, "git");
    let latest_vcs = git
        .available
        .then(|| collect_latest_git_commit(project_dir, "git"))
        .flatten();
    let task_scan = collect_task_state(project_dir);
    collection_warnings.extend(task_scan.warnings);
    let handoff_scan = collect_handoff_state(project_dir);
    collection_warnings.extend(handoff_scan.warnings);
    let memory = collect_memory_state(store, tenant, project_id).await;

    ProjectState {
        generated_unix_ms,
        tenant_id: tenant_id.to_string(),
        project_id: project_id.map(str::to_string),
        configured_project_dir,
        resolved_project_dir: canonical_or_lexical_path(project_dir).display().to_string(),
        scope_warnings,
        git,
        latest_task: task_scan.latest_task,
        latest_handoff: handoff_scan.latest_handoff,
        latest_vcs,
        next_actions: task_scan.next_actions,
        task_source_state: task_scan.source_state,
        memory,
        collection_warnings,
    }
}

#[derive(Debug, Default)]
pub(super) struct TaskScan {
    pub(super) latest_task: Option<StateSignal>,
    pub(super) next_actions: Vec<NextAction>,
    pub(super) warnings: Vec<String>,
    pub(super) source_state: TaskSourceState,
}

#[derive(Debug, Default)]
pub(super) struct HandoffScan {
    pub(super) latest_handoff: Option<StateSignal>,
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct Heading {
    pub(super) level: usize,
    pub(super) title: String,
    pub(super) line: usize,
}

pub(super) fn collect_task_state(project_dir: &Path) -> TaskScan {
    let path = project_dir.join("tasks/todo.md");
    if !path.exists() {
        return TaskScan::default();
    }
    let relative = relative_path(project_dir, &path);
    let mut warnings = Vec::new();
    let mut source_state = TaskSourceState::ParsedNoOpenTasks;
    let text = match read_text_capped(&path, PROJECT_STATE_FILE_CAP_BYTES) {
        Ok((text, truncated)) => {
            if truncated {
                warnings.push(format!(
                    "{} was truncated to {} bytes while collecting project state",
                    relative, PROJECT_STATE_FILE_CAP_BYTES
                ));
                source_state = TaskSourceState::ParseFailed;
            }
            text
        }
        Err(error) => {
            return TaskScan {
                warnings: vec![format!("could not read {relative}: {error}")],
                source_state: TaskSourceState::ParseFailed,
                ..TaskScan::default()
            };
        }
    };

    let mut stack: Vec<Heading> = Vec::new();
    let mut headings = Vec::new();
    let mut next_actions = Vec::new();

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if let Some(heading) = parse_heading(line, line_no) {
            while stack
                .last()
                .map(|existing| existing.level >= heading.level)
                .unwrap_or(false)
            {
                stack.pop();
            }
            stack.push(heading.clone());
            headings.push(heading);
            continue;
        }

        if let Some(action) = parse_next_action(line) {
            let heading = stack.last().map(|heading| heading.title.clone());
            next_actions.push(NextAction {
                source_path: relative.clone(),
                line: line_no,
                heading,
                text: action,
            });
        }
    }

    let latest_task = if let Some(first_action) = next_actions.first() {
        Some(StateSignal {
            source_path: first_action.source_path.clone(),
            line: Some(first_action.line),
            heading: first_action.heading.clone(),
            text: format!("open action: {}", first_action.text),
        })
    } else if let Some(heading) = headings
        .iter()
        .rev()
        .find(|heading| active_heading(&heading.title))
    {
        Some(StateSignal {
            source_path: relative,
            line: Some(heading.line),
            heading: Some(heading.title.clone()),
            text: "latest active section".to_string(),
        })
    } else {
        headings
            .iter()
            .rev()
            .find(|heading| completed_or_dated_heading(&heading.title))
            .map(|heading| StateSignal {
                source_path: relative,
                line: Some(heading.line),
                heading: Some(heading.title.clone()),
                text: "latest completed or dated section".to_string(),
            })
    };

    if source_state != TaskSourceState::ParseFailed && !next_actions.is_empty() {
        source_state = TaskSourceState::ParsedOpenTasks;
    }
    TaskScan {
        latest_task,
        next_actions,
        warnings,
        source_state,
    }
}

pub(super) fn collect_handoff_state(project_dir: &Path) -> HandoffScan {
    let dir = project_dir.join("docs/handoffs");
    if !dir.exists() {
        return HandoffScan::default();
    }
    let mut warnings = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            return HandoffScan {
                warnings: vec![format!(
                    "could not read {}: {error}",
                    relative_path(project_dir, &dir)
                )],
                ..HandoffScan::default()
            };
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            warnings.push("could not read one docs/handoffs entry".to_string());
            continue;
        };
        let path = entry.path();
        if path
            .components()
            .any(|component| component.as_os_str() == "_archive")
        {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        candidates.push((modified, path));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let Some((_, path)) = candidates.into_iter().next() else {
        return HandoffScan {
            warnings,
            ..HandoffScan::default()
        };
    };
    let relative = relative_path(project_dir, &path);
    let text = match read_text_capped(&path, HANDOFF_FILE_CAP_BYTES) {
        Ok((text, truncated)) => {
            if truncated {
                warnings.push(format!(
                    "{} was truncated to {} bytes while collecting handoff state",
                    relative, HANDOFF_FILE_CAP_BYTES
                ));
            }
            text
        }
        Err(error) => {
            warnings.push(format!("could not read {relative}: {error}"));
            return HandoffScan {
                warnings,
                ..HandoffScan::default()
            };
        }
    };

    let mut title = None;
    let mut status_lines = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if title.is_none() {
            title = parse_heading(line, idx + 1).map(|heading| (idx + 1, heading.title));
        }
        let trimmed = line.trim();
        let lowered = trimmed.to_ascii_lowercase();
        if lowered.starts_with("status:")
            || lowered.starts_with("- status:")
            || lowered.starts_with("next:")
            || lowered.starts_with("next step:")
            || lowered.starts_with("follow-up:")
        {
            status_lines.push(trimmed.trim_start_matches("- ").to_string());
        }
        if status_lines.len() >= 2 {
            break;
        }
    }

    let (line, title_text) = title.unwrap_or((1, relative.clone()));
    let signal_text = if status_lines.is_empty() {
        "latest handoff".to_string()
    } else {
        status_lines.join(" | ")
    };
    HandoffScan {
        latest_handoff: Some(StateSignal {
            source_path: relative,
            line: Some(line),
            heading: Some(title_text),
            text: signal_text,
        }),
        warnings,
    }
}

pub(super) async fn collect_memory_state<S: Store>(
    store: &S,
    tenant: &TenantId,
    project_id: Option<&str>,
) -> MemoryState {
    let snapshot = match store.health_snapshot(tenant, project_id, 0).await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return MemoryState::default(),
        Err(error) => {
            return MemoryState {
                scan_warning: Some(format!("memory health scan failed: {error}")),
                ..MemoryState::default()
            };
        }
    };
    let metadata_active = snapshot.counts.active_chunks;
    let mut readable = 0usize;
    let mut warning = None;
    let mut offset = 0usize;
    let scan_limit = metadata_active.min(READABLE_SCAN_MAX_METADATA_ROWS);
    while offset < scan_limit {
        let limit = READABLE_SCAN_PAGE_SIZE.min(scan_limit.saturating_sub(offset));
        match store
            .list_chunks_for_project(tenant, project_id, limit, offset)
            .await
        {
            Ok(chunks) => readable = readable.saturating_add(chunks.len()),
            Err(error) => {
                warning = Some(format!(
                    "readable memory scan failed at offset {offset}: {error}"
                ));
                break;
            }
        }
        offset = offset.saturating_add(limit);
    }
    if metadata_active > scan_limit && warning.is_none() {
        warning = Some(format!(
            "readable memory scan partial: checked {scan_limit} of {metadata_active} active chunks; unreadable count may be understated"
        ));
    }
    let unreadable = scan_limit.saturating_sub(readable);
    MemoryState {
        metadata_active_chunks: Some(metadata_active),
        readable_active_chunks: Some(readable),
        unreadable_active_chunks: Some(unreadable),
        scan_warning: warning,
    }
}

pub(super) fn collect_git_state(project_dir: &Path, git_binary: &str) -> GitState {
    let mut command = Command::new(git_binary);
    command
        .arg("-C")
        .arg(project_dir)
        .args(["status", "--short", "--branch"]);
    let output = match run_command_with_timeout(command, GIT_STATUS_TIMEOUT) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return GitState {
                available: false,
                not_git_repo: false,
                branch: None,
                clean: None,
                changed_entries: 0,
                summary: "git unavailable: executable not found".to_string(),
                warning: Some("git unavailable: executable not found".to_string()),
            };
        }
        Err(error) => {
            return GitState {
                available: false,
                not_git_repo: false,
                branch: None,
                clean: None,
                changed_entries: 0,
                summary: format!("git unavailable: {error}"),
                warning: Some(format!("git unavailable: {error}")),
            };
        }
    };

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.timed_out {
        return GitState {
            available: false,
            not_git_repo: false,
            branch: None,
            clean: None,
            changed_entries: 0,
            summary: "git unavailable: status timed out".to_string(),
            warning: Some("git unavailable: status timed out".to_string()),
        };
    }
    if !output.status_success {
        let not_git_repo = stderr.contains("not a git repository");
        let reason = if not_git_repo {
            "not a git repository".to_string()
        } else if stderr.is_empty() {
            "git status failed".to_string()
        } else {
            stderr
        };
        return GitState {
            available: false,
            not_git_repo,
            branch: None,
            clean: None,
            changed_entries: 0,
            summary: format!("git unavailable: {reason}"),
            warning: Some(format!("git unavailable: {reason}")),
        };
    }

    let mut lines = stdout.lines();
    let branch = lines
        .next()
        .and_then(|line| line.strip_prefix("## "))
        .map(|line| line.split("...").next().unwrap_or(line).trim().to_string())
        .filter(|branch| !branch.is_empty());
    let changed_entries = lines.filter(|line| !line.trim().is_empty()).count();
    let clean = changed_entries == 0;
    let branch_label = branch.as_deref().unwrap_or("<unknown>");
    let summary = if clean {
        format!("branch `{branch_label}`; clean")
    } else {
        format!("branch `{branch_label}`; dirty ({changed_entries} changed entries)")
    };
    GitState {
        available: true,
        not_git_repo: false,
        branch,
        clean: Some(clean),
        changed_entries,
        summary,
        warning: None,
    }
}

pub(super) fn collect_latest_git_commit(
    project_dir: &Path,
    git_binary: &str,
) -> Option<StateSignal> {
    let mut command = Command::new(git_binary);
    command.arg("-C").arg(project_dir).args([
        "log",
        "-1",
        "--date=short",
        "--pretty=format:%h %cd %s",
    ]);
    let output = run_command_with_timeout(command, GIT_STATUS_TIMEOUT).ok()?;
    if output.timed_out || !output.status_success {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(StateSignal {
        source_path: ".git".to_string(),
        line: None,
        heading: Some("latest commit".to_string()),
        text,
    })
}

pub(super) struct TimedCommandOutput {
    pub(super) status_success: bool,
    pub(super) timed_out: bool,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

pub(super) fn run_command_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> std::io::Result<TimedCommandOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = thread::spawn(move || read_child_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_child_pipe(stderr));
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let stdout = join_child_pipe(stdout_reader)?;
            let stderr = join_child_pipe(stderr_reader)?;
            return Ok(TimedCommandOutput {
                status_success: status.success(),
                timed_out: false,
                stdout,
                stderr,
            });
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let stdout = join_child_pipe(stdout_reader)?;
            let stderr = join_child_pipe(stderr_reader)?;
            return Ok(TimedCommandOutput {
                status_success: false,
                timed_out: true,
                stdout,
                stderr,
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn read_child_pipe(mut pipe: Option<impl Read>) -> io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    if let Some(pipe) = pipe.as_mut() {
        pipe.read_to_end(&mut buffer)?;
    }
    Ok(buffer)
}

pub(super) fn join_child_pipe(
    handle: thread::JoinHandle<io::Result<Vec<u8>>>,
) -> io::Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| io::Error::other("command output reader panicked"))?
}

pub(super) fn parse_heading(line: &str, line_no: usize) -> Option<Heading> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || !trimmed[level..].starts_with(' ') {
        return None;
    }
    let title = trimmed[level..].trim().to_string();
    (!title.is_empty()).then_some(Heading {
        level,
        title,
        line: line_no,
    })
}

pub(super) fn parse_next_action(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with("- [x]")
        || trimmed.starts_with("- [X]")
        || trimmed.starts_with("* [x]")
        || trimmed.starts_with("* [X]")
    {
        return None;
    }
    let body = trimmed
        .strip_prefix("- [ ]")
        .or_else(|| trimmed.strip_prefix("* [ ]"))
        .map(str::trim)
        .or_else(|| strip_bullet(trimmed));
    let candidate = body.unwrap_or(trimmed).trim();
    let lowered = candidate.to_ascii_lowercase();
    let explicit = lowered.starts_with("next step:")
        || lowered.starts_with("follow-up:")
        || lowered.starts_with("followup:")
        || lowered.starts_with("todo:")
        || lowered.starts_with("todo ")
        || lowered.starts_with("pending:")
        || lowered.starts_with("pending ");
    if trimmed.starts_with("- [ ]") || trimmed.starts_with("* [ ]") || explicit {
        Some(candidate.trim_end_matches('.').trim().to_string()).filter(|s| !s.is_empty())
    } else {
        None
    }
}

pub(super) fn strip_bullet(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .map(str::trim)
}

pub(super) fn active_heading(title: &str) -> bool {
    let lowered = title.to_ascii_lowercase();
    lowered.contains("in progress")
        || lowered.contains("todo")
        || lowered.contains("pending")
        || lowered.contains("open")
}

pub(super) fn completed_or_dated_heading(title: &str) -> bool {
    let lowered = title.to_ascii_lowercase();
    lowered.contains("done")
        || lowered.contains("complete")
        || lowered.contains("completed")
        || lowered.contains("202")
}

pub(super) fn read_text_capped(path: &Path, cap: u64) -> std::io::Result<(String, bool)> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > cap;
    if truncated {
        bytes.truncate(cap as usize);
    }
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

pub(super) fn scope_path_drift_warning(
    configured: &str,
    resolved_project_dir: &Path,
) -> Option<String> {
    let configured_path = PathBuf::from(configured);
    let configured_abs = if configured_path.is_absolute() {
        configured_path
    } else {
        resolved_project_dir.join(configured_path)
    };
    let configured_norm = canonical_or_lexical_path(&configured_abs);
    let resolved_norm = canonical_or_lexical_path(resolved_project_dir);
    (configured_norm != resolved_norm).then(|| {
        format!(
            "scope mismatch: configured project_dir `{}` differs from resolved project_dir `{}`",
            configured_norm.display(),
            resolved_norm.display()
        )
    })
}

pub(super) fn canonical_or_lexical_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize_path(path))
}

pub(super) fn lexical_normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(Path::new("/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

pub(super) fn relative_path(project_dir: &Path, path: &Path) -> String {
    path.strip_prefix(project_dir)
        .unwrap_or(path)
        .display()
        .to_string()
}
