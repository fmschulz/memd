use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::types::TenantId;

use super::args::{TenantScopeConfig, TenantScopeMode};

pub(super) fn read_omf_input(path: Option<&Path>) -> Result<String> {
    let raw = match path {
        None => read_stdin_to_string()?,
        Some(p) if p.as_os_str() == std::ffi::OsStr::new("-") => read_stdin_to_string()?,
        Some(p) => std::fs::read_to_string(p).map_err(|e| {
            crate::error::MemdError::ValidationError(format!("failed to read {}: {e}", p.display()))
        })?,
    };
    if raw.trim().is_empty() {
        return Err(crate::error::MemdError::ValidationError(
            "OMF input is empty".to_string(),
        ));
    }
    Ok(raw)
}

pub(super) fn read_stdin_to_string() -> Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).map_err(|e| {
        crate::error::MemdError::ValidationError(format!("failed to read stdin: {e}"))
    })?;
    Ok(buf)
}

/// Clean + absolutize a path without requiring it (or any parent) to
/// exist. Textually resolves `.` and `..`, and prefixes
/// `std::env::current_dir()` for relative inputs. `std::Path::canonicalize`
/// is not used because it errors on non-existent paths — we need the
/// check to run *before* `memd export-markdown <outdir>` has created
/// `<outdir>`, so that a user can't slip past the containment guard
/// by pointing at a path that doesn't yet exist.
pub(super) fn normalize_absolute(p: &Path) -> PathBuf {
    use std::path::Component;
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // Walk up one component, but never past the root.
                out.pop();
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    out
}

/// Test whether `child` is the same as `parent` or a descendant of it.
///
/// Both paths must already be normalised (see `normalize_absolute`).
/// On Windows the comparison is case-insensitive to match the
/// filesystem's own semantics — `C:\Users\me\.memd` and
/// `c:\USERS\me\.MEMD` refer to the same directory, so the lexical
/// guard must refuse both (Codex G3 review MEDIUM). On Unix the
/// comparison stays case-sensitive.
pub(super) fn path_is_inside(child: &Path, parent: &Path) -> bool {
    #[cfg(windows)]
    {
        let c = child.to_string_lossy().to_lowercase();
        let p = parent.to_string_lossy().to_lowercase();
        Path::new(&c).starts_with(Path::new(&p))
    }
    #[cfg(not(windows))]
    {
        child.starts_with(parent)
    }
}

/// Refuse to follow any symlink planted inside `outdir_abs` along the
/// path to `full_target`. Closes the pre-existing-symlink escape where
/// an attacker creates `<outdir>/sub` → `/etc` before
/// `memd export-markdown` runs, so the subsequent
/// `std::fs::write(<outdir>/sub/<file>)` overwrites the attacker's
/// backing file instead of a fresh file under outdir (Item 3 from the
/// nanomem-features handoff).
///
/// Walks each already-existing component under `outdir_abs` and refuses
/// if any is a symlink. The outdir itself is NOT checked — a user may
/// legitimately point `--outdir` at a symlinked directory they own —
/// but anything *inside* outdir that predates the export must be a
/// regular file or directory, never a symlink. Non-existing segments
/// are fine; they'll be created by `create_dir_all`.
///
/// A small TOCTOU window remains between this check and the write.
/// Closing it fully on every platform would require `O_NOFOLLOW`,
/// which is Unix-only; memd's CLI is already a user-trusted surface
/// (the caller picks outdir), so narrowing the pre-planted-symlink
/// window is the practical fix.
pub(super) fn reject_if_any_symlink_inside_outdir(
    full_target: &Path,
    outdir_abs: &Path,
) -> Result<()> {
    let rel = full_target.strip_prefix(outdir_abs).map_err(|_| {
        crate::error::MemdError::ValidationError(format!(
            "internal: target {} not inside outdir {}",
            full_target.display(),
            outdir_abs.display()
        ))
    })?;
    let mut current = outdir_abs.to_path_buf();
    for segment in rel.components() {
        current.push(segment.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(crate::error::MemdError::ValidationError(format!(
                    "refusing to follow symlink inside outdir: {}",
                    current.display()
                )));
            }
            Ok(_) => continue,
            // NotFound is the expected "this component is about to be
            // created by create_dir_all" case; everything else
            // (PermissionDenied, ELOOP, transient I/O) is abnormal and
            // we fail closed rather than silently skipping the guard
            // (Codex Item 3 LOW: the helper must not fail open).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => {
                return Err(crate::error::MemdError::ValidationError(format!(
                    "cannot verify symlink status for {}: {e}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn absolutize_project_dir(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

pub(super) fn resolve_data_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        // Absolutize so a relative `--memd-data-dir ./data` passed to
        // `memd init` from CWD=X is persisted to `tenant_scope.json`
        // as an absolute path (/path/to/X/data). If we kept relative
        // values, later auto-discovery would reinterpret them against
        // the project root (the dir that contains `.memd/`), which
        // differs from the user's CWD at init time and points the
        // guard at the wrong directory (Codex Item 4 MEDIUM).
        // `normalize_absolute` is textual (no canonicalize) so the
        // path is not required to exist yet.
        return Ok(normalize_absolute(path));
    }
    let home = dirs::home_dir().ok_or_else(|| {
        crate::error::MemdError::StorageError("cannot resolve home directory".to_string())
    })?;
    Ok(home.join(".memd").join("data"))
}

/// Walk ancestors of `start` looking for `.memd/tenant_scope.json`.
///
/// Returns the `data_dir` value from the first hit, or `None` if no
/// such file exists anywhere in the walk. Relative `data_dir` values
/// are resolved against the directory that contains `.memd/` — which
/// is what `memd init` intends when a user opts into a project-local
/// data dir.
///
/// First-match-wins: once we find any `.memd/tenant_scope.json`, that
/// IS the project boundary. A malformed JSON, missing-`data_dir`, or
/// unreadable file stops the walk and returns `None` — the caller
/// falls back to `$HOME/.memd/data`, rather than silently inheriting
/// an outer project's config (Codex Item 4 MEDIUM). Silent on IO /
/// parse errors so a broken project config doesn't crash the CLI.
pub(super) fn discover_project_data_dir_from(start: &Path) -> Option<PathBuf> {
    let mut current: Option<&Path> = Some(start);
    while let Some(dir) = current {
        let scope_path = dir.join(".memd").join("tenant_scope.json");
        if scope_path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&scope_path) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(raw) = value.get("data_dir").and_then(|v| v.as_str()) {
                        let candidate = PathBuf::from(raw);
                        return Some(if candidate.is_absolute() {
                            candidate
                        } else {
                            dir.join(candidate)
                        });
                    }
                }
            }
            // Found the boundary file but couldn't extract data_dir;
            // stop here rather than fall through to an outer project.
            return None;
        }
        current = dir.parent();
    }
    None
}

/// Core resolver for `memd export-markdown`'s containment-guard
/// data_dirs. Always returns the list of paths the guard must refuse
/// the outdir against — never a single path — so auto-discovery can't
/// weaken the pre-refactor `$HOME/.memd/data` default (Codex Item 4
/// HIGH).
///
/// Priority / composition:
/// 1. If `--data-dir` is explicit, the guard checks ONLY that path.
///    This is the caller's declared intent and overrides both
///    discovery and the home default.
/// 2. Otherwise, the list includes `$HOME/.memd/data` AND any
///    `data_dir` discovered from a nearest-ancestor
///    `.memd/tenant_scope.json`. The guard refuses an outdir that is
///    inside ANY of those candidates, so an untrusted ancestor config
///    can't mask the default-install guard.
///
/// Split from `resolve_export_markdown_data_dirs` so tests can drive
/// it with an explicit `start_dir` instead of coupling to CWD.
pub(super) fn resolve_export_markdown_data_dirs_from(
    explicit: Option<&Path>,
    start_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    if let Some(path) = explicit {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(start) = start_dir {
        if let Some(discovered) = discover_project_data_dir_from(start) {
            candidates.push(discovered);
        }
    }
    let home = dirs::home_dir().ok_or_else(|| {
        crate::error::MemdError::StorageError("cannot resolve home directory".to_string())
    })?;
    let home_default = home.join(".memd").join("data");
    if !candidates.contains(&home_default) {
        candidates.push(home_default);
    }
    Ok(candidates)
}

/// Resolve the data_dir candidates for `memd export-markdown`'s
/// containment guard. See `resolve_export_markdown_data_dirs_from` for
/// priority and composition semantics. This wrapper supplies
/// `std::env::current_dir()` as the discovery start point.
pub(super) fn resolve_export_markdown_data_dirs(explicit: Option<&Path>) -> Result<Vec<PathBuf>> {
    let cwd = std::env::current_dir().ok();
    resolve_export_markdown_data_dirs_from(explicit, cwd.as_deref())
}

pub(super) fn discover_tenants(data_dir: &Path) -> Result<Vec<String>> {
    let tenants_dir = data_dir.join("tenants");
    if !tenants_dir.exists() {
        return Ok(Vec::new());
    }

    let mut tenants = Vec::new();
    for entry in std::fs::read_dir(&tenants_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if TenantId::new(&name).is_ok() {
            tenants.push(name);
        }
    }
    tenants.sort();
    tenants.dedup();
    Ok(tenants)
}

pub(super) fn normalize_allow_tenants(raw: &[String]) -> Result<Vec<String>> {
    let mut tenants = Vec::new();
    for tenant in raw {
        let trimmed = tenant.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = TenantId::new(trimmed)?.to_string();
        tenants.push(normalized);
    }
    tenants.sort();
    tenants.dedup();
    Ok(tenants)
}

pub(super) fn build_tenant_scope_config(
    primary_tenant: &str,
    scope: TenantScopeMode,
    allow_tenants: Option<&[String]>,
    data_dir: &Path,
) -> Result<TenantScopeConfig> {
    let mut config = TenantScopeConfig {
        primary_tenant: primary_tenant.to_string(),
        write_tenant: primary_tenant.to_string(),
        scope,
        allow_tenants: Vec::new(),
        read_tenants: vec![primary_tenant.to_string()],
        // Always persist data_dir — not just in scope=global — so
        // `memd export-markdown` (and any future CLI tool that needs
        // the containment guard) can auto-discover the daemon's data
        // directory from a nearest-ancestor `.memd/tenant_scope.json`
        // without forcing every caller to pass `--data-dir` explicitly.
        data_dir: Some(data_dir.display().to_string()),
    };

    match scope {
        TenantScopeMode::Local => {
            if allow_tenants.is_some() {
                return Err(crate::error::MemdError::ValidationError(
                    "--allow-tenants is only valid with --scope allowlist".to_string(),
                ));
            }
        }
        TenantScopeMode::Allowlist => {
            let Some(raw) = allow_tenants else {
                return Err(crate::error::MemdError::ValidationError(
                    "--scope allowlist requires --allow-tenants".to_string(),
                ));
            };
            let normalized = normalize_allow_tenants(raw)?;
            if normalized.is_empty() {
                return Err(crate::error::MemdError::ValidationError(
                    "--allow-tenants must include at least one valid tenant".to_string(),
                ));
            }

            let mut read_tenants = vec![primary_tenant.to_string()];
            for tenant in &normalized {
                if tenant != primary_tenant {
                    read_tenants.push(tenant.clone());
                }
            }

            config.allow_tenants = normalized;
            config.read_tenants = read_tenants;
        }
        TenantScopeMode::Global => {
            if allow_tenants.is_some() {
                return Err(crate::error::MemdError::ValidationError(
                    "--allow-tenants is not supported with --scope global".to_string(),
                ));
            }

            let mut discovered = discover_tenants(data_dir)?;
            if !discovered.iter().any(|t| t == primary_tenant) {
                discovered.push(primary_tenant.to_string());
            }
            discovered.sort();
            discovered.dedup();

            config.read_tenants = discovered;
        }
    }

    Ok(config)
}
