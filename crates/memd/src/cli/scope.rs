use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::args::{CliCommand, ProjectScopeConfig};
use super::paths::absolutize_project_dir;
use crate::error::{MemdError, Result};

#[derive(Default)]
pub(super) struct OperationScopeCache {
    loaded: bool,
    scope: Option<ProjectScopeConfig>,
    error: Option<String>,
}

impl OperationScopeCache {
    pub(super) fn disabled() -> Self {
        Self {
            loaded: true,
            scope: None,
            error: None,
        }
    }
}

/// Find the project scope that governs `start`.
///
/// Walks from `start` up to `$HOME`. A directory with no scope file of its own
/// that is a linked worktree also offers the scope of its main checkout,
/// before the walk continues upward, so a worktree outside the main checkout
/// shares the repository's scope.
///
/// The walk stops at the root of the git repository that encloses `start`
/// (after the main checkout of a linked worktree): a scope above a
/// repository, such as one in the home directory, never becomes the project
/// of a repository. Outside any repository the walk runs to `$HOME`.
pub(super) fn find_project_scope(start: &Path) -> Result<Option<ProjectScopeConfig>> {
    find_project_scope_with_home(start, canonical_home().as_deref())
}

fn find_project_scope_with_home(
    start: &Path,
    home: Option<&Path>,
) -> Result<Option<ProjectScopeConfig>> {
    let start = absolutize_project_dir(start)?;
    let repository = enclosing_repository(&start, home);

    for dir in start.ancestors() {
        if let Some(scope) = read_scope_file_at(dir)? {
            return Ok(Some(scope));
        }
        if let Some(main_checkout) = main_checkout_of_linked_worktree(dir) {
            if let Some(scope) = read_scope_file_at(&main_checkout)? {
                return Ok(Some(scope));
            }
        }
        if home == Some(dir) || repository.as_ref().is_some_and(|r| r.repo_root == dir) {
            break;
        }
    }

    Ok(None)
}

/// The canonical home directory, when it resolves.
pub(super) fn canonical_home() -> Option<PathBuf> {
    dirs::home_dir().and_then(|home| home.canonicalize().ok())
}

/// True for the filesystem root and the home directory. A scope there would
/// cover every descendant. Both arguments must be canonical.
pub(super) fn is_unsafe_scope_root(canonical: &Path, home: Option<&Path>) -> bool {
    canonical.parent().is_none() || home == Some(canonical)
}

/// The git repository that encloses a directory.
#[derive(Debug)]
pub(super) struct Repository {
    /// The nearest directory at or above the directory with a `.git`.
    pub(super) repo_root: PathBuf,
    /// For a linked worktree, the main checkout that its main repository
    /// confirms.
    pub(super) main_checkout: Option<PathBuf>,
}

impl Repository {
    /// The directory that owns the repository's scope: the main checkout of a
    /// linked worktree, else the repository root.
    pub(super) fn root(&self) -> &Path {
        self.main_checkout.as_deref().unwrap_or(&self.repo_root)
    }

    /// True when a `.memd-skip` marker at the root or main checkout opts the
    /// whole repository out.
    pub(super) fn opted_out(&self) -> bool {
        [self.repo_root.as_path(), self.root()]
            .iter()
            .any(|dir| dir.join(".memd-skip").exists())
    }
}

/// The repository that encloses `dir`, if any: the nearest `.git` file or
/// directory at or above `dir` marks its root. A repository whose root, or
/// main checkout, is unsafe (home or `/`) is ignored. `home` must be
/// canonical. Reads files only and never runs `git`.
pub(super) fn enclosing_repository(dir: &Path, home: Option<&Path>) -> Option<Repository> {
    let repo_root = dir
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())?;
    let repository = Repository {
        repo_root: repo_root.to_path_buf(),
        main_checkout: main_checkout_of_linked_worktree(repo_root),
    };
    let unsafe_root = repository
        .root()
        .canonicalize()
        .map_or(true, |canonical| is_unsafe_scope_root(&canonical, home));
    (!unsafe_root).then_some(repository)
}

/// Read `dir/.memd/project_scope.json`. A missing file is `None`. A file that
/// exists but cannot be read or parsed is an error, because silently falling
/// through to a parent scope would route flagless writes into the wrong
/// tenant or project.
fn read_scope_file_at(dir: &Path) -> Result<Option<ProjectScopeConfig>> {
    let scope_path = dir.join(".memd").join("project_scope.json");
    let text = match std::fs::read_to_string(&scope_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(MemdError::ValidationError(format!(
                "unable to read {}: {error}; fix its permissions or pass --tenant-id explicitly",
                scope_path.display()
            )))
        }
    };
    serde_json::from_str::<ProjectScopeConfig>(&text)
        .map(Some)
        .map_err(|err| {
            MemdError::ValidationError(format!(
                "malformed {}: {err}; repair this scope file or run memd init with explicit --tenant-id and --project-id for {}",
                scope_path.display(),
                dir.display()
            ))
        })
}

/// The main checkout directory when `project_dir` is a linked git worktree.
///
/// A linked worktree has a `.git` file holding `gitdir: <admin dir>`. The admin
/// dir has a `commondir` file that points at the main `.git` directory, whose
/// parent is the main checkout. The admin dir also records the worktree's
/// `.git` file in its `gitdir` file. That back-pointer must resolve to this
/// directory's `.git`, so a crafted `.git` file cannot claim another
/// repository. The admin dir itself must be `<common dir>/worktrees/<name>`,
/// so an admin dir outside the repository it names is refused too. A `.git`
/// that is a symlink is refused. This reads files only and never runs `git`.
/// Plain repos, submodules (no `commondir`), bare-repo worktrees (common dir
/// not named `.git`), forged pointers, and unreadable files all return `None`.
pub(super) fn main_checkout_of_linked_worktree(project_dir: &Path) -> Option<PathBuf> {
    let dot_git = project_dir.join(".git");
    if !dot_git.symlink_metadata().ok()?.file_type().is_file() {
        return None;
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?;
    let admin_dir = project_dir.join(gitdir.trim());
    let back_pointer = std::fs::read_to_string(admin_dir.join("gitdir")).ok()?;
    let recorded = admin_dir.join(back_pointer.trim()).canonicalize().ok()?;
    if recorded != dot_git.canonicalize().ok()? {
        return None;
    }
    let commondir = std::fs::read_to_string(admin_dir.join("commondir")).ok()?;
    let common_dir = admin_dir.join(commondir.trim()).canonicalize().ok()?;
    if common_dir.file_name()? != ".git" {
        return None;
    }
    // The admin dir must live in the worktrees directory of the repository it
    // names; otherwise the claim is not the repository's own record.
    if admin_dir.canonicalize().ok()?.parent()? != common_dir.join("worktrees") {
        return None;
    }
    common_dir.parent().map(Path::to_path_buf)
}

fn scope_missing_error(cwd: &Path) -> MemdError {
    MemdError::ValidationError(format!(
        "no --tenant-id given and no .memd/project_scope.json found upward from {}; the first `memd add` creates the scope, or pass --tenant-id",
        cwd.display()
    ))
}

pub(super) fn resolve_required(
    start: &Path,
    tenant: Option<String>,
    project: Option<String>,
) -> Result<(String, Option<String>)> {
    // The scope file is consulted only when --tenant-id is absent. An explicit
    // tenant targets that tenant directly; we never borrow the scope's
    // project_id for a different (or even the same) explicitly named tenant,
    // because that silently narrows broad lookups to an unrelated project.
    if let Some(tenant_id) = tenant {
        return Ok((tenant_id, project));
    }
    let scope = find_project_scope(start)?;
    let tenant_id = scope
        .as_ref()
        .map(|scope| scope.tenant_id.clone())
        .ok_or_else(|| scope_missing_error(start))?;
    let project_id = project.or_else(|| scope.and_then(|scope| scope.project_id));
    Ok((tenant_id, project_id))
}

/// [`resolve_required`] for a command that writes. When no tenant is given and
/// no scope exists, the first write creates the scope the way `session-start`
/// would (at the repository root, else in the directory; unsafe roots,
/// `.memd-skip` and `MEMD_AUTO_SCOPE=0` honoured) and uses it. When creation
/// is refused, the usual "no scope" error follows.
fn resolve_for_write(
    start: &Path,
    tenant: Option<String>,
    project: Option<String>,
    inputs: &super::session_start::AutoScopeInputs,
) -> Result<(String, Option<String>)> {
    if tenant.is_none() && find_project_scope(start)?.is_none() {
        if let Some((tenant_id, scope_project)) =
            super::session_start::create_scope_for_first_write(start, inputs)?
        {
            return Ok((tenant_id, project.or(scope_project)));
        }
    }
    resolve_required(start, tenant, project)
}

pub(super) fn resolve_optional(
    start: &Path,
    tenant: Option<String>,
    project: Option<String>,
) -> Result<(Option<String>, Option<String>)> {
    // Same unit semantics as resolve_required: an explicit tenant suppresses
    // scope-file lookup so it never inherits an unrelated project_id.
    if let Some(tenant_id) = tenant {
        return Ok((Some(tenant_id), project));
    }
    let scope = find_project_scope(start)?;
    let tenant_id = scope.as_ref().map(|scope| scope.tenant_id.clone());
    let project_id = project.or_else(|| scope.and_then(|scope| scope.project_id));
    Ok((tenant_id, project_id))
}

pub(super) fn require_tenant(tenant: Option<String>) -> Result<String> {
    tenant.ok_or_else(|| scope_missing_error(&current_dir()))
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(super) fn apply_operation_scope(
    arguments: Value,
    cache: &mut OperationScopeCache,
) -> Result<Value> {
    apply_operation_scope_at(&current_dir(), arguments, cache)
}

pub(super) fn apply_operation_scope_at(
    start: &Path,
    arguments: Value,
    cache: &mut OperationScopeCache,
) -> Result<Value> {
    if arguments
        .as_object()
        .and_then(|object| object.get("tenant_id"))
        .is_some_and(|tenant_id| !tenant_id.is_null())
    {
        // An explicit tenant must not inherit the repository project, matching
        // the typed CLI's cross-project footgun guard.
        return Ok(arguments);
    }

    if !cache.loaded {
        cache.loaded = true;
        match find_project_scope(start) {
            Ok(scope) => cache.scope = scope,
            Err(error) => {
                cache.error = Some(match &error {
                    MemdError::ValidationError(message) => message.clone(),
                    _ => error.to_string(),
                });
                return Err(error);
            }
        }
    }
    if let Some(error) = cache.error.as_ref() {
        return Err(MemdError::ValidationError(error.clone()));
    }
    let Some(scope) = cache.scope.as_ref() else {
        return Ok(arguments);
    };

    let mut object = match arguments {
        Value::Object(object) => object,
        Value::Null => Map::new(),
        _ => {
            return Err(MemdError::ValidationError(
                "operation arguments must be a JSON object".to_string(),
            ))
        }
    };
    object.insert(
        "tenant_id".to_string(),
        Value::String(scope.tenant_id.clone()),
    );
    if !object.contains_key("project_id") {
        if let Some(project_id) = scope.project_id.as_ref() {
            object.insert("project_id".to_string(), Value::String(project_id.clone()));
        }
    }
    Ok(Value::Object(object))
}

pub fn resolve_command_scope(cmd: &mut CliCommand) -> Result<()> {
    resolve_command_scope_at(
        &current_dir(),
        cmd,
        &super::session_start::AutoScopeInputs::from_env(),
    )
}

fn resolve_command_scope_at(
    cwd: &Path,
    cmd: &mut CliCommand,
    inputs: &super::session_start::AutoScopeInputs,
) -> Result<()> {
    let cwd = cwd.to_path_buf();
    match cmd {
        // The first write creates the scope; reads never do.
        CliCommand::Add {
            tenant_id,
            project_id,
            ..
        } => {
            let (resolved_tenant, resolved_project) =
                resolve_for_write(&cwd, tenant_id.take(), project_id.take(), inputs)?;
            *tenant_id = Some(resolved_tenant);
            *project_id = resolved_project;
        }
        CliCommand::Experience {
            tenant_id,
            project_id,
            command,
            ..
        } => {
            let (tenant, project) = (tenant_id.take(), project_id.take());
            let (resolved_tenant, resolved_project) = if command.first_write() {
                resolve_for_write(&cwd, tenant, project, inputs)?
            } else {
                resolve_required(&cwd, tenant, project)?
            };
            *tenant_id = Some(resolved_tenant);
            *project_id = resolved_project;
        }
        CliCommand::Search {
            tenant_id,
            project_id,
            ..
        }
        | CliCommand::AgentContext {
            tenant_id,
            project_id,
            ..
        }
        | CliCommand::ExportMarkdown {
            tenant_id,
            project_id,
            ..
        }
        | CliCommand::ExportOmf {
            tenant_id,
            project_id,
            ..
        } => {
            let (resolved_tenant, resolved_project) =
                resolve_required(&cwd, tenant_id.take(), project_id.take())?;
            *tenant_id = Some(resolved_tenant);
            *project_id = resolved_project;
        }
        CliCommand::Get { tenant_id, .. }
        | CliCommand::Delete { tenant_id, .. }
        | CliCommand::Outcome { tenant_id, .. }
        | CliCommand::Stats { tenant_id }
        | CliCommand::Export { tenant_id, .. }
        | CliCommand::ImportOmf { tenant_id, .. } => {
            let (resolved_tenant, _) = resolve_required(&cwd, tenant_id.take(), None)?;
            *tenant_id = Some(resolved_tenant);
        }
        CliCommand::Audit {
            tenant_id,
            project_id,
            ..
        }
        | CliCommand::Report {
            tenant_id,
            project_id,
            ..
        }
        | CliCommand::ConsolidateReview {
            tenant_id,
            project_id,
            ..
        } => {
            let (resolved_tenant, resolved_project) =
                resolve_optional(&cwd, tenant_id.take(), project_id.take())?;
            *tenant_id = resolved_tenant;
            *project_id = resolved_project;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{CliQueryMode, ExportFormat, SearchReranker, WarmMode};
    use crate::types::TenantId;

    #[test]
    fn resolve_required_fills_search_scope_from_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{
              "tenant_id": "scoped_tenant",
              "project_id": "scoped_project",
              "read_tenants": ["scoped_tenant"],
              "interface": "cli",
              "cli_command": "memd",
              "agent_context_output": ".memd/context.md",
              "project_dir": "."
            }"#,
        )
        .unwrap();

        let cmd = CliCommand::Search {
            tenant_id: None,
            query: Some("scope fallback".to_string()),
            query_positional: None,
            k: 2,
            project_id: None,
            task_id: None,
            thread_id: None,
            compact: false,
            dedupe_by_source: false,
            token_budget: None,
            mode: CliQueryMode::Generic,
            no_text: false,
            include_artifact: false,
            include_superseded: false,
            format: ExportFormat::Json,
            output: None,
            reranker: SearchReranker::None,
            reranker_model: "model".to_string(),
            reranker_device: "cpu".to_string(),
            reranker_batch_size: 1,
            reranker_timeout_seconds: 1,
            reranker_python: "python3".to_string(),
            warm: WarmMode::Off,
        };

        let (tenant_id, project_id) = match cmd {
            CliCommand::Search {
                tenant_id,
                project_id,
                ..
            } => resolve_required(dir.path(), tenant_id, project_id).unwrap(),
            _ => panic!("unexpected command"),
        };
        assert_eq!(tenant_id, "scoped_tenant");
        assert_eq!(project_id.as_deref(), Some("scoped_project"));
    }

    #[test]
    fn resolve_required_explicit_tenant_does_not_borrow_scope_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{
              "tenant_id": "scoped_tenant",
              "project_id": "scoped_project",
              "read_tenants": ["scoped_tenant"],
              "interface": "cli",
              "cli_command": "memd",
              "agent_context_output": ".memd/context.md",
              "project_dir": "."
            }"#,
        )
        .unwrap();

        // Explicit tenant, no project: the scope file must not narrow the
        // lookup to its own project_id (cross-project footgun guard).
        let (tenant_id, project_id) =
            resolve_required(dir.path(), Some("other_tenant".to_string()), None).unwrap();
        assert_eq!(tenant_id, "other_tenant");
        assert_eq!(project_id, None);
    }

    #[test]
    fn malformed_scope_file_errors_instead_of_walking_up() {
        let dir = tempfile::tempdir().unwrap();
        let parent_scope = dir.path().join(".memd");
        std::fs::create_dir_all(&parent_scope).unwrap();
        std::fs::write(
            parent_scope.join("project_scope.json"),
            r#"{"tenant_id": "parent_tenant"}"#,
        )
        .unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir_all(child.join(".memd")).unwrap();
        std::fs::write(child.join(".memd/project_scope.json"), "{ not json").unwrap();

        let err = resolve_required(&child, None, None).unwrap_err();
        let MemdError::ValidationError(message) = err else {
            panic!("unexpected error variant");
        };
        assert!(
            message.contains("malformed") && message.contains("memd init"),
            "unexpected message: {message}"
        );

        // An explicit tenant bypasses scope reading entirely, so the
        // malformed file must not break flagged invocations.
        let (tenant_id, _) =
            resolve_required(&child, Some("explicit_tenant".to_string()), None).unwrap();
        assert_eq!(tenant_id, "explicit_tenant");
    }

    #[test]
    fn resolve_required_errors_with_exact_message_without_scope() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_required(dir.path(), None, None).unwrap_err();
        let MemdError::ValidationError(message) = err else {
            panic!("unexpected error variant");
        };
        assert_eq!(
            message,
            format!(
                "no --tenant-id given and no .memd/project_scope.json found upward from {}; the first `memd add` creates the scope, or pass --tenant-id",
                dir.path().display()
            )
        );
    }

    #[test]
    fn operation_arguments_inherit_scope_without_overriding_explicit_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{
              "tenant_id": "scoped_tenant",
              "project_id": "scoped_project",
              "interface": "cli",
              "cli_command": "memd",
              "agent_context_output": ".memd/context.md",
              "project_dir": "."
            }"#,
        )
        .unwrap();

        let mut cache = OperationScopeCache::default();
        let scoped = apply_operation_scope_at(
            dir.path(),
            serde_json::json!({"text": "scope me"}),
            &mut cache,
        )
        .unwrap();
        assert_eq!(scoped["tenant_id"], "scoped_tenant");
        assert_eq!(scoped["project_id"], "scoped_project");

        let mut cache = OperationScopeCache::default();
        let project_override = apply_operation_scope_at(
            dir.path(),
            serde_json::json!({"project_id": "override"}),
            &mut cache,
        )
        .unwrap();
        assert_eq!(project_override["tenant_id"], "scoped_tenant");
        assert_eq!(project_override["project_id"], "override");

        let mut cache = OperationScopeCache::default();
        let explicit_tenant = apply_operation_scope_at(
            dir.path(),
            serde_json::json!({"tenant_id": "other_tenant"}),
            &mut cache,
        )
        .unwrap();
        assert_eq!(explicit_tenant["tenant_id"], "other_tenant");
        assert!(explicit_tenant.get("project_id").is_none());

        let mut cache = OperationScopeCache::default();
        let null_tenant = apply_operation_scope_at(
            dir.path(),
            serde_json::json!({"tenant_id": null}),
            &mut cache,
        )
        .unwrap();
        assert_eq!(null_tenant["tenant_id"], "scoped_tenant");
        assert_eq!(null_tenant["project_id"], "scoped_project");
    }

    #[test]
    fn explicit_operation_tenant_bypasses_malformed_scope() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(dir.path().join(".memd/project_scope.json"), "{ not json").unwrap();

        let mut cache = OperationScopeCache::default();
        let explicit = apply_operation_scope_at(
            dir.path(),
            serde_json::json!({"tenant_id": "explicit_tenant"}),
            &mut cache,
        )
        .unwrap();
        assert_eq!(explicit["tenant_id"], "explicit_tenant");
    }

    #[test]
    fn cached_operation_scope_error_keeps_one_validation_prefix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(dir.path().join(".memd/project_scope.json"), "{ not json").unwrap();

        let mut cache = OperationScopeCache::default();
        let first =
            apply_operation_scope_at(dir.path(), serde_json::json!({"text": "first"}), &mut cache)
                .unwrap_err()
                .to_string();
        let replay = apply_operation_scope_at(
            dir.path(),
            serde_json::json!({"text": "second"}),
            &mut cache,
        )
        .unwrap_err()
        .to_string();

        assert_eq!(replay, first);
        assert_eq!(replay.matches("validation error:").count(), 1, "{replay}");
    }

    #[test]
    fn unreadable_scope_path_fails_closed_without_walking_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd/project_scope.json")).unwrap();

        let child = dir.path().join("child");
        std::fs::create_dir_all(&child).unwrap();
        let mut cache = OperationScopeCache::default();
        let error = apply_operation_scope_at(
            &child,
            serde_json::json!({"text": "do not route"}),
            &mut cache,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unable to read"), "{error}");
        assert!(
            error.to_string().contains(".memd/project_scope.json"),
            "{error}"
        );

        let mut cache = OperationScopeCache::default();
        let explicit = apply_operation_scope_at(
            &child,
            serde_json::json!({"tenant_id": "explicit_tenant"}),
            &mut cache,
        )
        .unwrap();
        assert_eq!(explicit["tenant_id"], "explicit_tenant");
    }

    /// A main checkout with a scope and a linked worktree next to it, laid out
    /// as `git worktree add ../wt` does, back-pointer included.
    fn sibling_worktree(root: &Path) -> (PathBuf, PathBuf) {
        let main = root.join("main-repo");
        let admin = main.join(".git/worktrees/wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let worktree = root.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            admin.join("gitdir"),
            format!("{}/.git\n", worktree.display()),
        )
        .unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        std::fs::create_dir_all(main.join(".memd")).unwrap();
        std::fs::write(
            main.join(".memd/project_scope.json"),
            r#"{
              "tenant_id": "main_tenant",
              "project_id": "main_project",
              "interface": "cli",
              "cli_command": "memd",
              "agent_context_output": ".memd/context.md",
              "project_dir": "."
            }"#,
        )
        .unwrap();
        (main, worktree)
    }

    #[test]
    fn resolve_required_uses_the_main_checkout_scope_in_a_linked_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let (_main, worktree) = sibling_worktree(dir.path());
        let sub = worktree.join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();

        for start in [&worktree, &sub] {
            let (tenant, project) = resolve_required(start, None, None).unwrap();
            assert_eq!(tenant, "main_tenant");
            assert_eq!(project.as_deref(), Some("main_project"));
        }
    }

    #[test]
    fn worktree_scope_file_wins_over_the_main_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let (_main, worktree) = sibling_worktree(dir.path());
        std::fs::create_dir_all(worktree.join(".memd")).unwrap();
        std::fs::write(
            worktree.join(".memd/project_scope.json"),
            r#"{"tenant_id":"wt_tenant","project_id":"wt_project","interface":"cli","cli_command":"memd","agent_context_output":".memd/context.md","project_dir":"."}"#,
        )
        .unwrap();

        let (tenant, _) = resolve_required(&worktree, None, None).unwrap();
        assert_eq!(tenant, "wt_tenant");
    }

    #[test]
    fn malformed_main_checkout_scope_stops_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        let (main, worktree) = sibling_worktree(dir.path());
        std::fs::write(main.join(".memd/project_scope.json"), b"{not json").unwrap();

        let error = resolve_required(&worktree, None, None).unwrap_err();
        assert!(error.to_string().contains("malformed"), "{error}");
    }

    #[test]
    fn independent_clone_and_forged_pointer_do_not_inherit_a_main_checkout_scope() {
        let dir = tempfile::tempdir().unwrap();
        let (main, _worktree) = sibling_worktree(dir.path());

        let clone = dir.path().join("clone");
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        assert!(resolve_required(&clone, None, None).is_err());

        let forged = dir.path().join("forged");
        std::fs::create_dir_all(&forged).unwrap();
        std::fs::write(
            forged.join(".git"),
            format!("gitdir: {}/.git/worktrees/wt\n", main.display()),
        )
        .unwrap();
        assert_eq!(main_checkout_of_linked_worktree(&forged), None);
        assert!(resolve_required(&forged, None, None).is_err());
    }

    #[test]
    fn forged_admin_directory_outside_the_victim_repository_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (victim, _worktree) = sibling_worktree(dir.path());
        // The attacker owns the admin directory: commondir names the victim's
        // `.git`, and the back-pointer names the attacker's own `.git` file.
        let attacker = dir.path().join("attacker");
        let admin = dir.path().join("attacker-admin");
        std::fs::create_dir_all(&attacker).unwrap();
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(
            admin.join("commondir"),
            format!("{}\n", victim.join(".git").display()),
        )
        .unwrap();
        std::fs::write(
            admin.join("gitdir"),
            format!("{}/.git\n", attacker.display()),
        )
        .unwrap();
        std::fs::write(
            attacker.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();

        assert_eq!(main_checkout_of_linked_worktree(&attacker), None);
        assert!(resolve_required(&attacker, None, None).is_err());
    }

    #[test]
    fn symlinked_dot_git_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (_main, worktree) = sibling_worktree(dir.path());
        assert!(main_checkout_of_linked_worktree(&worktree).is_some());
        let link_dir = dir.path().join("link-wt");
        std::fs::create_dir_all(&link_dir).unwrap();
        std::os::unix::fs::symlink(worktree.join(".git"), link_dir.join(".git")).unwrap();

        assert_eq!(main_checkout_of_linked_worktree(&link_dir), None);
    }

    fn write_scope_file(dir: &Path, tenant: &str, project: &str) {
        std::fs::create_dir_all(dir.join(".memd")).unwrap();
        std::fs::write(
            dir.join(".memd/project_scope.json"),
            format!(
                r#"{{"tenant_id":"{tenant}","project_id":"{project}","interface":"cli","cli_command":"memd","agent_context_output":".memd/context.md","project_dir":"."}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn repository_below_a_home_like_directory_does_not_inherit_its_scope() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        write_scope_file(&home, "home_tenant", "home_project");
        let repo = home.join("repo");
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let plain = home.join("notes/deep");
        std::fs::create_dir_all(&plain).unwrap();

        // Inside the repository the home scope is out of reach.
        assert!(find_project_scope_with_home(&sub, Some(&home))
            .unwrap()
            .is_none());
        assert!(find_project_scope_with_home(&repo, Some(&home))
            .unwrap()
            .is_none());
        // Outside any repository the walk still reaches home.
        let scope = find_project_scope_with_home(&plain, Some(&home)).unwrap();
        assert_eq!(scope.unwrap().tenant_id, "home_tenant");
        // The repository's own scope is still found.
        write_scope_file(&repo, "repo_tenant", "repo_project");
        let scope = find_project_scope_with_home(&sub, Some(&home)).unwrap();
        assert_eq!(scope.unwrap().tenant_id, "repo_tenant");
    }

    #[test]
    fn nested_clone_and_linked_worktree_stop_the_walk_at_their_own_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // A scope above everything.
        write_scope_file(&root, "outer_tenant", "outer");
        let clone = root.join("outer-dir/clone");
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        std::fs::create_dir_all(clone.join("sub")).unwrap();
        assert!(find_project_scope_with_home(&clone.join("sub"), None)
            .unwrap()
            .is_none());
        assert!(find_project_scope_with_home(&clone, None)
            .unwrap()
            .is_none());

        let (main, worktree) = sibling_worktree(&root.join("wt-parent"));
        std::fs::remove_file(main.join(".memd/project_scope.json")).unwrap();
        std::fs::create_dir_all(worktree.join("sub")).unwrap();
        assert!(find_project_scope_with_home(&worktree.join("sub"), None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn symlink_in_the_path_is_resolved_before_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let repo = root.join("b/repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        write_scope_file(&repo, "repo_tenant", "repo_project");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::os::unix::fs::symlink(repo.join("sub"), root.join("a/link")).unwrap();

        // `/a/link/../src` is `/b/repo/src`, not `/a/src`.
        let scope = find_project_scope_with_home(&root.join("a/link/../src"), None).unwrap();
        assert_eq!(scope.unwrap().tenant_id, "repo_tenant");
    }

    fn add_command() -> CliCommand {
        CliCommand::Add {
            tenant_id: None,
            text: "Decision: the first write creates the scope. Agent action: none.".to_string(),
            chunk_type: crate::types::ChunkType::Decision,
            project_id: None,
            tags: None,
            source_uri: None,
            source_path: None,
            warm: WarmMode::Off,
        }
    }

    fn inputs(enabled: bool) -> super::super::session_start::AutoScopeInputs {
        super::super::session_start::AutoScopeInputs {
            default_tenant: "writer".to_string(),
            enabled,
            consolidate: false,
        }
    }

    fn unscoped_repository(root: &Path) -> (PathBuf, PathBuf) {
        let repo = root.join("My-Repo");
        let sub = repo.join("review/deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        (repo, sub)
    }

    #[tokio::test]
    async fn flagless_add_in_an_unscoped_repository_creates_the_scope_and_stores_the_chunk() {
        use crate::cli::write_commands::{cli_add_rendered, CliAddRenderOptions};
        use crate::store::{MemoryStore, Store};

        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());
        let mut command = add_command();

        resolve_command_scope_at(&sub, &mut command, &inputs(true)).unwrap();

        let CliCommand::Add {
            tenant_id,
            project_id,
            text,
            chunk_type,
            ..
        } = command
        else {
            panic!("unexpected command");
        };
        assert_eq!(tenant_id.as_deref(), Some("writer"));
        assert_eq!(project_id.as_deref(), Some("my_repo"));
        assert!(repo.join(".memd/project_scope.json").exists());
        assert!(!sub.join(".memd").exists());
        assert!(!repo.join("review/.memd").exists());

        let store = MemoryStore::new();
        cli_add_rendered(
            &store,
            None,
            CliAddRenderOptions {
                tenant_id: tenant_id.unwrap(),
                text,
                chunk_type,
                project_id: project_id.clone(),
                tags: None,
                source_uri: None,
                source_path: None,
            },
        )
        .await
        .unwrap();
        let stored = store
            .list_chunks_for_project(
                &TenantId::new("writer").unwrap(),
                project_id.as_deref(),
                5,
                0,
            )
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        // A second flagless command now finds the scope and creates nothing.
        let mut again = add_command();
        resolve_command_scope_at(&sub, &mut again, &inputs(true)).unwrap();
    }

    #[test]
    fn flagless_search_in_an_unscoped_repository_fails_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());
        let mut command = CliCommand::Stats { tenant_id: None };

        let error = resolve_command_scope_at(&sub, &mut command, &inputs(true)).unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("the first `memd add` creates the scope"),
            "{message}"
        );
        assert!(message.contains("--tenant-id"), "{message}");
        assert!(!message.contains("session-start"), "{message}");
        assert!(!repo.join(".memd").exists());
        assert!(!sub.join(".memd").exists());
    }

    #[test]
    fn skip_marker_and_disabled_auto_scope_stop_the_creation_by_add() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());

        let mut disabled = add_command();
        assert!(resolve_command_scope_at(&sub, &mut disabled, &inputs(false)).is_err());
        assert!(!repo.join(".memd").exists());

        std::fs::write(repo.join(".memd-skip"), "").unwrap();
        let mut skipped = add_command();
        assert!(resolve_command_scope_at(&sub, &mut skipped, &inputs(true)).is_err());
        assert!(!repo.join(".memd/project_scope.json").exists());
        assert!(!sub.join(".memd").exists());
    }

    #[test]
    fn explicit_tenant_add_creates_no_scope() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());
        let mut command = add_command();
        if let CliCommand::Add { tenant_id, .. } = &mut command {
            *tenant_id = Some("explicit".to_string());
        }

        resolve_command_scope_at(&sub, &mut command, &inputs(true)).unwrap();

        assert!(!repo.join(".memd").exists());
    }
}
