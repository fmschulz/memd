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
    let mut scope_warnings = Vec::new();
    if let Some(configured) = configured_project_dir.as_deref() {
        if let Some(warning) = scope_path_drift_warning(configured, project_dir) {
            scope_warnings.push(warning);
        }
    }

    let memory = collect_memory_state(store, tenant, project_id).await;

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
