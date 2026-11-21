use super::rank::SectionScan;
use super::state::*;
use super::*;

pub(super) fn collect_project_state(
    tenant_id: &str,
    project_id: Option<&str>,
    project_dir: &Path,
    scope: Option<&ProjectScopeConfig>,
    generated_unix_ms: u128,
    memory: MemoryState,
) -> ProjectState {
    let configured_project_dir = scope.map(|scope| scope.project_dir.clone());
    let mut scope_warnings = Vec::new();
    if let Some(configured) = configured_project_dir.as_deref() {
        if let Some(warning) = scope_path_drift_warning(configured, project_dir) {
            scope_warnings.push(warning);
        }
    }

    ProjectState {
        generated_unix_ms,
        tenant_id: tenant_id.to_string(),
        project_id: project_id.map(str::to_string),
        configured_project_dir,
        resolved_project_dir: canonical_or_lexical_path(project_dir).display().to_string(),
        scope_warnings,
        memory,
        collection_warnings: Vec::new(),
    }
}

/// Health counts and warnings from the candidate scan. No second scan runs.
/// Only the project cap warns: it cuts facts of the active project by age. The
/// machine-wide cap is the designed bound and stays in the explain output.
pub(super) fn memory_state_from_scan(
    project: &SectionScan,
    machine_wide: &SectionScan,
) -> MemoryState {
    let mut scan_warnings = Vec::new();
    if project.truncated {
        scan_warnings.push(format!(
            "project scan truncated: read the newest {} of {} visible facts (cap {}); older facts were not considered",
            project.read_rows,
            project.visible_rows,
            project.cap.unwrap_or(project.read_rows)
        ));
    }
    MemoryState {
        // Facts the scan admitted. A truncated scan did not read every row,
        // so only then the SQL count stands in.
        visible_project_facts: if project.truncated {
            project.visible_rows
        } else {
            project.admitted_rows
        },
        visible_tenant_facts: project.visible_rows + machine_wide.visible_rows,
        unreadable_active_chunks: project.unreadable_rows + machine_wide.unreadable_rows,
        scan_warnings,
    }
}

/// Bounded file read used by the repo-novelty index; returns the
/// (lossy UTF-8) text and whether the cap truncated it.
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
