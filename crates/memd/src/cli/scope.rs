use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::args::{CliCommand, ProjectScopeConfig};
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

pub(super) fn find_project_scope(start: &Path) -> Result<Option<ProjectScopeConfig>> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut current = start.to_path_buf();

    for _ in 0..64 {
        let scope_path = current.join(".memd").join("project_scope.json");
        let text = match std::fs::read_to_string(&scope_path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if home.as_deref().is_some_and(|home| current == home) {
                    break;
                }
                if !current.pop() {
                    break;
                }
                continue;
            }
            Err(error) => {
                return Err(MemdError::ValidationError(format!(
                "unable to read {}: {error}; fix its permissions or pass --tenant-id explicitly",
                scope_path.display()
            )))
            }
        };
        // A scope file that exists but cannot be read or parsed must stop the
        // walk: silently falling through to a parent scope would route
        // flagless writes into the wrong tenant/project.
        return serde_json::from_str::<ProjectScopeConfig>(&text)
            .map(Some)
            .map_err(|err| {
                MemdError::ValidationError(format!(
                    "malformed {}: {err}; re-create it with: memd session-start --project-dir {} (or pass --tenant-id explicitly)",
                    scope_path.display(),
                    current.display()
                ))
            });
    }

    Ok(None)
}

fn scope_missing_error(cwd: &Path) -> MemdError {
    MemdError::ValidationError(format!(
        "no --tenant-id given and no .memd/project_scope.json found upward from {}; run: memd session-start --project-dir . (or pass --tenant-id)",
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
    let cwd = current_dir();
    match cmd {
        CliCommand::Add {
            tenant_id,
            project_id,
            ..
        }
        | CliCommand::Search {
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
            query: "scope fallback".to_string(),
            k: 2,
            project_id: None,
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
            message.contains("malformed") && message.contains("session-start"),
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
                "no --tenant-id given and no .memd/project_scope.json found upward from {}; run: memd session-start --project-dir . (or pass --tenant-id)",
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
}
