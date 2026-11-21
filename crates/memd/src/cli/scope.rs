use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::args::{CliCommand, ProjectScopeConfig};
use super::paths::absolutize_project_dir;
use super::session_start::PendingScope;
use crate::error::{MemdError, Result};

/// Tells whether the current user owns the entry at a path. memd trusts a
/// repository, a scope file and a scope directory only when it does, as git
/// does since CVE-2022-24765: another user's `/tmp/.git` must not make `/tmp`
/// a project. A test cannot change the owner of a file, so callers take the
/// rule as a parameter and a test passes its own.
pub(super) type Owns = dyn Fn(&Path) -> bool + Send + Sync;

/// True when the effective user of this process owns the entry at `path`. The
/// entry itself counts: a symlink is not followed. A missing entry is not owned.
#[cfg(unix)]
pub(super) fn owned_by_current_user(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    path.symlink_metadata()
        .is_ok_and(|metadata| metadata.uid() == euid)
}

/// Other platforms have no owner test: every entry counts as owned.
#[cfg(not(unix))]
pub(super) fn owned_by_current_user(_path: &Path) -> bool {
    true
}

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
    find_project_scope_with_home(start, canonical_home().as_deref(), &owned_by_current_user)
}

fn find_project_scope_with_home(
    start: &Path,
    home: Option<&Path>,
    owns: &Owns,
) -> Result<Option<ProjectScopeConfig>> {
    match locate_project_scope_with_home(start, home, owns)? {
        LocatedScope::Found { scope, .. } => Ok(Some(scope)),
        LocatedScope::Missing { .. } => Ok(None),
        LocatedScope::Invalid(invalid) => Err(invalid.error),
    }
}

/// [`find_project_scope`] that also says which scope file it found, or which
/// invalid one stopped the walk.
pub(super) fn locate_project_scope(start: &Path, owns: &Owns) -> Result<LocatedScope> {
    locate_project_scope_with_home(start, canonical_home().as_deref(), owns)
}

fn locate_project_scope_with_home(
    start: &Path,
    home: Option<&Path>,
    owns: &Owns,
) -> Result<LocatedScope> {
    let start = absolutize_project_dir(start)?;
    let repository = enclosing_repository(&start, home, owns);
    let mut not_owned = Vec::new();

    for dir in start.ancestors() {
        let own = read_scope_file_at(dir, owns, &mut not_owned);
        let main_checkout = match &own {
            Ok(None) => main_checkout_of_linked_worktree(dir),
            _ => None,
        };
        let found = match own {
            Ok(Some(found)) => Some(found),
            Ok(None) => match main_checkout
                .as_deref()
                .map(|main| read_scope_file_at(main, owns, &mut not_owned))
            {
                Some(Ok(found)) => found,
                Some(Err(invalid)) => return Ok(LocatedScope::Invalid(invalid)),
                None => None,
            },
            Err(invalid) => return Ok(LocatedScope::Invalid(invalid)),
        };
        if let Some((path, scope)) = found {
            return Ok(LocatedScope::Found { path, scope });
        }
        if home == Some(dir) || repository.as_ref().is_some_and(|r| r.repo_root == dir) {
            break;
        }
    }

    Ok(LocatedScope::Missing { not_owned })
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
/// directory at or above `dir` marks its root. The current user must own the
/// directory and its `.git` entry. An ancestor whose `.git` fails that test
/// counts as having no `.git`, and the search goes on upward. A repository
/// whose root, or main checkout, is unsafe (home or `/`), or whose main
/// checkout the current user does not own, is ignored. `home` must be
/// canonical. Reads files only and never runs `git`.
pub(super) fn enclosing_repository(
    dir: &Path,
    home: Option<&Path>,
    owns: &Owns,
) -> Option<Repository> {
    let repo_root = dir
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok() && owns_checkout(dir, owns))?;
    let repository = Repository {
        repo_root: repo_root.to_path_buf(),
        main_checkout: main_checkout_of_linked_worktree(repo_root),
    };
    let main_checkout_trusted = repository
        .main_checkout
        .as_deref()
        .is_none_or(|main| owns_checkout(main, owns));
    let unsafe_root = repository
        .root()
        .canonicalize()
        .map_or(true, |canonical| is_unsafe_scope_root(&canonical, home));
    (main_checkout_trusted && !unsafe_root).then_some(repository)
}

/// True when the current user owns the directory and its `.git` entry.
fn owns_checkout(dir: &Path, owns: &Owns) -> bool {
    owns(dir) && owns(&dir.join(".git"))
}

/// The `.memd` directory of `dir`, or its file `name`, when it exists and the
/// current user does not own it. A reader treats that entry as missing and
/// never opens it. Every reader of a scope file calls this before it reads. The
/// directory comes first, because a foreign `.memd` with mode 700 would fail the
/// read of the file inside it.
pub(super) fn foreign_scope_entry(dir: &Path, name: &str, owns: &Owns) -> Option<PathBuf> {
    let memd_dir = dir.join(".memd");
    let file = memd_dir.join(name);
    [memd_dir, file]
        .into_iter()
        .find(|entry| entry.symlink_metadata().is_ok() && !owns(entry))
}

/// Read `dir/.memd/project_scope.json`. A missing file is `None`. So is a file,
/// or a `.memd` directory, that the current user does not own (see
/// [`foreign_scope_entry`]): it is pushed to `not_owned` and never read. A file
/// that the user owns and that cannot be read or parsed is an error, because
/// silently falling through to a parent scope would route flagless writes into
/// the wrong tenant or project.
fn read_scope_file_at(
    dir: &Path,
    owns: &Owns,
    not_owned: &mut Vec<PathBuf>,
) -> std::result::Result<Option<(PathBuf, ProjectScopeConfig)>, Box<ScopeFileError>> {
    if let Some(entry) = foreign_scope_entry(dir, "project_scope.json", owns) {
        not_owned.push(entry);
        return Ok(None);
    }
    let scope_path = dir.join(".memd").join("project_scope.json");
    let text = match std::fs::read_to_string(&scope_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            let message = format!(
                "unable to read {}: {error}; fix its permissions or pass --tenant-id explicitly",
                scope_path.display()
            );
            return Err(Box::new(ScopeFileError {
                path: scope_path,
                malformed: false,
                error: MemdError::ValidationError(message),
            }));
        }
    };
    match serde_json::from_str::<ProjectScopeConfig>(&text) {
        Ok(scope) => Ok(Some((scope_path, scope))),
        Err(err) => {
            let message = format!(
                "malformed {}: {err}; repair this scope file or run memd init with explicit --tenant-id and --project-id for {}",
                scope_path.display(),
                dir.display()
            );
            Err(Box::new(ScopeFileError {
                path: scope_path,
                malformed: true,
                error: MemdError::ValidationError(message),
            }))
        }
    }
}

/// A scope file that exists but cannot be read or parsed.
#[derive(Debug)]
pub(super) struct ScopeFileError {
    pub(super) path: PathBuf,
    /// True when the file was read but is not valid scope JSON.
    pub(super) malformed: bool,
    pub(super) error: MemdError,
}

/// Where [`locate_project_scope`] found the scope that governs a directory.
#[derive(Debug)]
pub(super) enum LocatedScope {
    Found {
        path: PathBuf,
        scope: ProjectScopeConfig,
    },
    /// No scope file. `not_owned` lists the entries the walk ignored because
    /// another user owns them.
    Missing {
        not_owned: Vec<PathBuf>,
    },
    Invalid(Box<ScopeFileError>),
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
        "no --tenant-id given and no .memd/project_scope.json found upward from {}; this command needs an existing scope: the first `memd add` creates it, or pass --tenant-id",
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

/// Plan the scope of a directory that has none. The commands that write and the
/// commands that read share this plan, so both use the same ids; a writer
/// publishes the pending scope afterwards, a reader ignores it. A refusal
/// becomes an error that says why.
fn plan_scope(
    start: &Path,
    inputs: &super::session_start::AutoScopeInputs,
) -> Result<(String, Option<String>, Option<PendingScope>)> {
    match super::session_start::plan_scope_for_first_write(start, inputs)? {
        Ok(((tenant_id, project_id), pending)) => Ok((tenant_id, project_id, pending)),
        Err(refusal) => Err(MemdError::ValidationError(format!(
            "no --tenant-id given and no .memd/project_scope.json found upward from {}; no scope can be planned here: {}; pass --tenant-id",
            start.display(),
            super::session_start::refusal_reason(&refusal)
        ))),
    }
}

/// [`resolve_required`] for a command that writes. When no tenant is given and
/// no scope exists, the first write plans the scope the way `session-start`
/// would (at the repository root, else in the directory; unsafe roots,
/// directories of another user, `.memd-skip` and `MEMD_AUTO_SCOPE=0` honoured)
/// and writes under it. Nothing
/// is created here: the caller publishes the returned [`PendingScope`] once
/// the command succeeded. When planning is refused, the error says why.
fn resolve_for_write(
    start: &Path,
    tenant: Option<String>,
    project: Option<String>,
    inputs: &super::session_start::AutoScopeInputs,
) -> Result<(String, Option<String>, Option<PendingScope>)> {
    if tenant.is_none() && find_project_scope(start)?.is_none() {
        let (tenant_id, scope_project, pending) = plan_scope(start, inputs)?;
        return Ok((tenant_id, project.or(scope_project), pending));
    }
    let (tenant_id, project_id) = resolve_required(start, tenant, project)?;
    Ok((tenant_id, project_id, None))
}

/// [`resolve_required`] for a command that only reads. With no scope in reach
/// it uses the planned scope (the ids a first write would publish) and creates
/// nothing, so a read in a project without content finds nothing instead of
/// failing, and finds the rows that another checkout stored under the same
/// ids. When planning is refused, the error says why.
fn resolve_for_read(
    start: &Path,
    tenant: Option<String>,
    project: Option<String>,
    inputs: &super::session_start::AutoScopeInputs,
) -> Result<(String, Option<String>)> {
    if tenant.is_none() && find_project_scope(start)?.is_none() {
        let (tenant_id, scope_project, _unpublished) = plan_scope(start, inputs)?;
        return Ok((tenant_id, project.or(scope_project)));
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

/// Fill the tenant and project of `cmd` from the scope. A flagless first write
/// with no scope in reach returns the scope to publish after the command
/// succeeded (see [`PendingScope`]); every other command returns `None`.
pub fn resolve_command_scope(cmd: &mut CliCommand) -> Result<Option<PendingScope>> {
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
) -> Result<Option<PendingScope>> {
    let cwd = cwd.to_path_buf();
    let mut pending = None;
    match cmd {
        // The first write publishes the scope once it succeeded. Reads use the
        // planned scope and never create a file. Commands that change the
        // store but start no project need an existing scope.
        CliCommand::Add {
            tenant_id,
            project_id,
            ..
        } => {
            let (resolved_tenant, resolved_project, plan) =
                resolve_for_write(&cwd, tenant_id.take(), project_id.take(), inputs)?;
            *tenant_id = Some(resolved_tenant);
            *project_id = resolved_project;
            pending = plan;
        }
        CliCommand::Experience {
            tenant_id,
            project_id,
            command,
            ..
        } => {
            let (tenant, project) = (tenant_id.take(), project_id.take());
            let (resolved_tenant, resolved_project) = if command.first_write() {
                let (tenant, project, plan) = resolve_for_write(&cwd, tenant, project, inputs)?;
                pending = plan;
                (tenant, project)
            } else if command.reads_only() {
                resolve_for_read(&cwd, tenant, project, inputs)?
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
                resolve_for_read(&cwd, tenant_id.take(), project_id.take(), inputs)?;
            *tenant_id = Some(resolved_tenant);
            *project_id = resolved_project;
        }
        CliCommand::Get { tenant_id, .. }
        | CliCommand::Stats { tenant_id }
        | CliCommand::Export { tenant_id, .. } => {
            let (resolved_tenant, _) = resolve_for_read(&cwd, tenant_id.take(), None, inputs)?;
            *tenant_id = Some(resolved_tenant);
        }
        CliCommand::Delete { tenant_id, .. }
        | CliCommand::Outcome { tenant_id, .. }
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
    Ok(pending)
}

#[cfg(test)]
pub(super) mod tests {
    use std::os::unix::fs::PermissionsExt;

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
                "no --tenant-id given and no .memd/project_scope.json found upward from {}; this command needs an existing scope: the first `memd add` creates it, or pass --tenant-id",
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
    pub(in crate::cli) fn sibling_worktree(root: &Path) -> (PathBuf, PathBuf) {
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

    pub(in crate::cli) fn write_scope_file(dir: &Path, tenant: &str, project: &str) {
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
        assert!(
            find_project_scope_with_home(&sub, Some(&home), &owned_by_current_user)
                .unwrap()
                .is_none()
        );
        assert!(
            find_project_scope_with_home(&repo, Some(&home), &owned_by_current_user)
                .unwrap()
                .is_none()
        );
        // Outside any repository the walk still reaches home.
        let scope =
            find_project_scope_with_home(&plain, Some(&home), &owned_by_current_user).unwrap();
        assert_eq!(scope.unwrap().tenant_id, "home_tenant");
        // The repository's own scope is still found.
        write_scope_file(&repo, "repo_tenant", "repo_project");
        let scope =
            find_project_scope_with_home(&sub, Some(&home), &owned_by_current_user).unwrap();
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
        assert!(
            find_project_scope_with_home(&clone.join("sub"), None, &owned_by_current_user)
                .unwrap()
                .is_none()
        );
        assert!(
            find_project_scope_with_home(&clone, None, &owned_by_current_user)
                .unwrap()
                .is_none()
        );

        let (main, worktree) = sibling_worktree(&root.join("wt-parent"));
        std::fs::remove_file(main.join(".memd/project_scope.json")).unwrap();
        std::fs::create_dir_all(worktree.join("sub")).unwrap();
        assert!(
            find_project_scope_with_home(&worktree.join("sub"), None, &owned_by_current_user)
                .unwrap()
                .is_none()
        );
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
        let scope =
            find_project_scope_with_home(&root.join("a/link/../src"), None, &owned_by_current_user)
                .unwrap();
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
            owns: std::sync::Arc::new(owned_by_current_user),
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

        let pending = resolve_command_scope_at(&sub, &mut command, &inputs(true))
            .unwrap()
            .expect("a flagless first write plans a scope");

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
        // Resolution only plans: nothing is on disk before the write.
        assert!(!repo.join(".memd").exists());
        assert!(!sub.join(".memd").exists());

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
        // The scope is published after the row exists.
        assert!(!repo.join(".memd").exists());
        assert!(pending.publish().unwrap());
        assert!(repo.join(".memd/project_scope.json").exists());
        assert!(!sub.join(".memd").exists());
        assert!(!repo.join("review/.memd").exists());
        // A second flagless command now finds the scope and plans nothing.
        let mut again = add_command();
        let second = resolve_command_scope_at(&sub, &mut again, &inputs(true)).unwrap();
        assert!(second.is_none());
    }

    #[test]
    fn a_planned_scope_that_is_never_published_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());
        let mut command = add_command();

        // The command fails after resolution: the plan is dropped unpublished.
        drop(resolve_command_scope_at(&sub, &mut command, &inputs(true)).unwrap());

        assert_eq!(
            std::fs::read_dir(&repo).unwrap().count(),
            2,
            "only .git and review remain"
        );
        assert!(!repo.join(".memd").exists());
    }

    #[test]
    fn two_first_writes_publish_one_scope_file() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());
        let mut first = add_command();
        let mut second = add_command();
        let other = super::super::session_start::AutoScopeInputs {
            default_tenant: "other".to_string(),
            enabled: true,
            consolidate: false,
            owns: std::sync::Arc::new(owned_by_current_user),
        };
        let first_plan = resolve_command_scope_at(&sub, &mut first, &inputs(true))
            .unwrap()
            .unwrap();
        let second_plan = resolve_command_scope_at(&sub, &mut second, &other)
            .unwrap()
            .unwrap();

        assert!(first_plan.publish().unwrap());
        assert!(!second_plan.publish().unwrap(), "the loser adopts the file");

        let names: Vec<_> = std::fs::read_dir(repo.join(".memd"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["project_scope.json"]);
        let text = std::fs::read_to_string(repo.join(".memd/project_scope.json")).unwrap();
        assert!(text.contains("\"writer\""), "{text}");
    }

    fn search_command() -> CliCommand {
        CliCommand::Search {
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
        }
    }

    fn resolved_ids(command: CliCommand) -> (Option<String>, Option<String>) {
        match command {
            CliCommand::Search {
                tenant_id,
                project_id,
                ..
            } => (tenant_id, project_id),
            CliCommand::Stats { tenant_id } => (tenant_id, None),
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn flagless_search_in_an_unscoped_repository_uses_the_planned_scope_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());
        let mut command = search_command();

        let pending = resolve_command_scope_at(&sub, &mut command, &inputs(true)).unwrap();

        assert!(pending.is_none(), "a read publishes nothing");
        let (tenant, project) = resolved_ids(command);
        assert_eq!(tenant.as_deref(), Some("writer"));
        assert_eq!(project.as_deref(), Some("my_repo"));
        assert!(!repo.join(".memd").exists());
        assert!(!sub.join(".memd").exists());
    }

    #[test]
    fn reads_that_need_only_a_tenant_use_the_planned_scope_too() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());
        let mut command = CliCommand::Stats { tenant_id: None };

        resolve_command_scope_at(&sub, &mut command, &inputs(true)).unwrap();

        assert_eq!(resolved_ids(command).0.as_deref(), Some("writer"));
        assert!(!repo.join(".memd").exists());
    }

    #[test]
    fn commands_that_change_the_store_still_need_an_existing_scope() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());
        let mut command = CliCommand::Delete {
            tenant_id: None,
            chunk_id: "0".to_string(),
            warm: WarmMode::Off,
        };

        let error = resolve_command_scope_at(&sub, &mut command, &inputs(true)).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("needs an existing scope"), "{message}");
        assert!(message.contains("--tenant-id"), "{message}");
        assert!(!repo.join(".memd").exists());
    }

    #[test]
    fn skip_marker_and_disabled_auto_scope_make_a_read_fail_with_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, sub) = unscoped_repository(dir.path());

        let mut disabled = search_command();
        let error = resolve_command_scope_at(&sub, &mut disabled, &inputs(false)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("MEMD_AUTO_SCOPE=0"), "{message}");
        assert!(message.contains("--tenant-id"), "{message}");

        std::fs::write(repo.join(".memd-skip"), "").unwrap();
        let mut skipped = search_command();
        let error = resolve_command_scope_at(&sub, &mut skipped, &inputs(true)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(".memd-skip"), "{message}");
        assert!(!repo.join(".memd/project_scope.json").exists());
        assert!(!sub.join(".memd").exists());
    }

    #[test]
    fn subdirectory_and_linked_worktree_plan_the_ids_of_the_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (main, worktree) = sibling_worktree(&root);
        std::fs::remove_dir_all(main.join(".memd")).unwrap();
        let sub = main.join("review/deep");
        let worktree_sub = worktree.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&worktree_sub).unwrap();

        let mut ids = Vec::new();
        for start in [&main, &sub, &worktree, &worktree_sub] {
            let mut command = search_command();
            resolve_command_scope_at(start, &mut command, &inputs(true)).unwrap();
            ids.push(resolved_ids(command));
        }

        assert_eq!(
            ids[0],
            (Some("writer".to_string()), Some("main_repo".to_string()))
        );
        assert!(ids.iter().all(|planned| *planned == ids[0]), "{ids:?}");
        assert!(!main.join(".memd").exists());
        assert!(!worktree.join(".memd").exists());
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

        let pending = resolve_command_scope_at(&sub, &mut command, &inputs(true)).unwrap();

        assert!(pending.is_none());
        assert!(!repo.join(".memd").exists());
    }

    /// An owner rule for a test, which cannot change the owner of a file. The
    /// current user owns the paths below `root`, except the `foreign` entries,
    /// which another user owns. Everything above `root` (such as `/tmp`)
    /// belongs to someone else, so the test does not depend on the machine.
    pub(in crate::cli) fn owned_below(
        root: &Path,
        foreign: &[&Path],
    ) -> impl Fn(&Path) -> bool + Send + Sync + 'static {
        let root = root.to_path_buf();
        let foreign: Vec<PathBuf> = foreign.iter().map(|entry| entry.to_path_buf()).collect();
        move |path| path.starts_with(&root) && !foreign.iter().any(|entry| entry == path)
    }

    fn inputs_with_owner(
        owns: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> super::super::session_start::AutoScopeInputs {
        super::super::session_start::AutoScopeInputs {
            owns: std::sync::Arc::new(owns),
            ..inputs(true)
        }
    }

    /// A shared directory such as `/tmp`, with a `.git` entry, and a project
    /// below it that has no repository of its own.
    fn shared_directory_with_a_git_entry(root: &Path) -> (PathBuf, PathBuf) {
        let shared = root.join("shared");
        let project = shared.join("My-Project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(shared.join(".git")).unwrap();
        (shared, project)
    }

    #[test]
    fn the_owner_rule_trusts_entries_the_current_user_created() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();

        assert!(owned_by_current_user(dir.path()));
        assert!(owned_by_current_user(&file));
        assert!(owned_by_current_user(&link));
        assert!(!owned_by_current_user(&dir.path().join("missing")));
    }

    #[test]
    fn a_git_entry_that_another_user_owns_is_not_a_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (shared, project) = shared_directory_with_a_git_entry(&root);

        // The current user owns the directory and its `.git`: a repository.
        let repository = enclosing_repository(&project, Some(&root), &owned_by_current_user);
        assert_eq!(repository.unwrap().repo_root, shared);
        // Another user owns the `.git` entry, or the directory: no repository.
        for foreign in [shared.join(".git"), shared.clone()] {
            let owns = owned_below(&root, &[&foreign]);
            assert!(
                enclosing_repository(&project, Some(&root), &owns).is_none(),
                "{} belongs to another user",
                foreign.display()
            );
        }
    }

    #[test]
    fn a_project_below_a_foreign_git_entry_plans_its_own_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (shared, project) = shared_directory_with_a_git_entry(&root);

        // When the current user owns the `.git` entry, the shared directory
        // is the repository, so a flagless add plans the scope there, named
        // after it.
        let mut command = add_command();
        let owner = inputs_with_owner(owned_below(&root, &[]));
        let pending = resolve_command_scope_at(&project, &mut command, &owner)
            .unwrap()
            .expect("a flagless first write plans a scope");
        assert_eq!(pending.scope_dir(), shared);

        // With the `.git` entry owned by another user, the project is outside
        // a repository: the scope goes to the project, named after it.
        let mut command = add_command();
        let owner = inputs_with_owner(owned_below(&root, &[&shared.join(".git")]));
        let pending = resolve_command_scope_at(&project, &mut command, &owner)
            .unwrap()
            .expect("a flagless first write plans a scope");
        assert_eq!(pending.scope_dir(), project);
        let CliCommand::Add {
            tenant_id,
            project_id,
            ..
        } = command
        else {
            panic!("unexpected command");
        };
        assert_eq!(tenant_id.as_deref(), Some("writer"));
        assert_eq!(project_id.as_deref(), Some("my_project"));

        assert!(pending.publish().unwrap());
        assert!(project.join(".memd/project_scope.json").exists());
        assert!(!shared.join(".memd").exists());
    }

    #[test]
    fn a_foreign_git_entry_nearer_the_start_is_skipped_for_the_repository_above() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let outer = root.join("outer");
        let inner = outer.join("inner");
        let project = inner.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        std::fs::create_dir_all(inner.join(".git")).unwrap();
        write_scope_file(&outer, "outer_tenant", "outer_project");

        // The current user owns both: the nearer `.git` is the boundary.
        let nearest = enclosing_repository(&project, Some(&root), &owned_by_current_user);
        assert_eq!(nearest.unwrap().repo_root, inner);
        assert!(
            find_project_scope_with_home(&project, Some(&root), &owned_by_current_user)
                .unwrap()
                .is_none()
        );

        // Another user owns the nearer `.git`: it counts as no `.git` at all,
        // so the search goes on upward and returns the repository above.
        let owns = owned_below(&root, &[&inner.join(".git")]);
        let repository = enclosing_repository(&project, Some(&root), &owns).unwrap();
        assert_eq!(repository.repo_root, outer);
        let scope = find_project_scope_with_home(&project, Some(&root), &owns).unwrap();
        assert_eq!(scope.unwrap().tenant_id, "outer_tenant");
    }

    #[test]
    fn a_linked_worktree_whose_main_checkout_is_not_owned_has_no_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (main, worktree) = sibling_worktree(&root);

        let repository = enclosing_repository(&worktree, Some(&root), &owned_by_current_user);
        assert_eq!(repository.unwrap().main_checkout, Some(main.clone()));
        // The worktree is the current user's, the main checkout is not: the
        // repository is ignored, as for an unsafe root.
        for foreign in [main.clone(), main.join(".git")] {
            let owns = owned_below(&root, &[&foreign]);
            assert!(
                enclosing_repository(&worktree, Some(&root), &owns).is_none(),
                "{} belongs to another user",
                foreign.display()
            );
        }
        // A worktree `.git` file that another user owns is not trusted either.
        let owns = owned_below(&root, &[&worktree.join(".git")]);
        assert!(enclosing_repository(&worktree, Some(&root), &owns).is_none());
    }

    #[test]
    fn a_scope_file_that_another_user_owns_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let shared = root.join("shared");
        let project = shared.join("project");
        std::fs::create_dir_all(&project).unwrap();
        write_scope_file(&shared, "shared_tenant", "shared_project");
        let file = shared.join(".memd/project_scope.json");

        // The current user owns the file: found.
        match locate_project_scope_with_home(&project, Some(&root), &owned_by_current_user) {
            Ok(LocatedScope::Found { path, scope }) => {
                assert_eq!(path, file);
                assert_eq!(scope.tenant_id, "shared_tenant");
            }
            other => panic!("unexpected result: {other:?}"),
        }

        // Another user owns the file, or its `.memd` directory: missing, and
        // the result names the entry that was ignored. A file that cannot be
        // read makes no difference, because the owner is checked first.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        for foreign in [file.clone(), shared.join(".memd")] {
            let owns = owned_below(&root, &[&foreign]);
            let located = locate_project_scope_with_home(&project, Some(&root), &owns);
            let Ok(LocatedScope::Missing { not_owned }) = located else {
                panic!("{} belongs to another user: {located:?}", foreign.display());
            };
            assert_eq!(not_owned, [foreign]);
        }
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

        // The walk goes on past the ignored file to a scope the user owns.
        write_scope_file(&root, "own_tenant", "own_project");
        let owns = owned_below(&root, &[&file]);
        let scope = find_project_scope_with_home(&project, Some(&root), &owns).unwrap();
        assert_eq!(scope.unwrap().tenant_id, "own_tenant");
    }

    #[test]
    fn a_foreign_memd_directory_without_access_does_not_fail_the_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let shared = root.join("shared");
        let project = shared.join("project");
        std::fs::create_dir_all(&project).unwrap();
        write_scope_file(&shared, "shared_tenant", "shared_project");
        let memd = shared.join(".memd");

        // A `.memd` directory with mode 700 that another user owns would
        // make the read fail with "unable to read" if it were read.
        std::fs::set_permissions(&memd, std::fs::Permissions::from_mode(0o000)).unwrap();
        let owns = owned_below(&root, &[&memd]);
        let found = find_project_scope_with_home(&project, Some(&root), &owns);
        std::fs::set_permissions(&memd, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(found.unwrap().is_none());
    }

    #[test]
    fn flagless_commands_in_a_directory_that_another_user_owns_fail_and_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let nothing_is_owned = inputs_with_owner(|_| false);

        for mut command in [add_command(), search_command()] {
            let error =
                resolve_command_scope_at(&project, &mut command, &nothing_is_owned).unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("the directory belongs to another user"),
                "{message}"
            );
            assert!(message.contains("--tenant-id"), "{message}");
        }
        assert!(!project.join(".memd").exists());
    }
}
