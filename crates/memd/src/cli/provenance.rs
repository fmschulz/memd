use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{MemdError, Result};
use crate::task_memory::ExecutionContext;

use super::args::CliCommand;
use super::call::parse_call_arguments;

const TASK_PROVENANCE_TOOLS: &[&str] = &[
    "task.start",
    "task.progress",
    "task.run_start",
    "task.run_finish",
    "task.add_evidence",
    "task.finish",
    "artifact.create",
    "artifact.review",
    "artifact.revision",
    "artifact.decision",
    "artifact.verification",
];

/// Capture the calling process's execution context.
///
/// Capture is deliberately allowlisted. It reads native identifiers and the
/// reported harness/model from known environment variables, but never opens a
/// native transcript or model configuration. The result records observed or
/// self-reported context and does not authenticate identity.
pub fn capture_execution_context(cwd: &Path) -> ExecutionContext {
    let harness = nonempty_env("AIO_HARNESS").or_else(|| {
        if nonempty_env("CODEX_THREAD_ID").is_some() {
            Some("codex".to_string())
        } else if nonempty_env("CLAUDE_SESSION_ID").is_some() {
            Some("claude".to_string())
        } else {
            None
        }
    });
    let (native_session_id, native_thread_id) = match harness.as_deref() {
        Some("codex") => (
            nonempty_env("CODEX_SESSION_ID").or_else(|| nonempty_env("AIO_NATIVE_RUNTIME_ID")),
            nonempty_env("CODEX_THREAD_ID").or_else(|| nonempty_env("AIO_NATIVE_ID")),
        ),
        Some("claude") => (
            nonempty_env("CLAUDE_SESSION_ID").or_else(|| nonempty_env("AIO_NATIVE_ID")),
            None,
        ),
        _ => (
            nonempty_env("AIO_NATIVE_RUNTIME_ID"),
            nonempty_env("AIO_NATIVE_ID"),
        ),
    };
    let (repo_root, repo_revision, repo_dirty) = capture_repository(cwd);

    ExecutionContext {
        harness,
        model: nonempty_env("AIO_MODEL"),
        native_session_id,
        native_thread_id,
        native_agent_id: None,
        native_parent_id: None,
        origin_host: command_text("hostname", &[])
            .or_else(|| nonempty_env("HOSTNAME").or_else(|| nonempty_env("COMPUTERNAME"))),
        repo_root,
        repo_revision,
        repo_dirty,
        repo_tracked_diff_sha256: None,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        observed_at_ms: observed_at_ms(),
    }
}

/// Hash HEAD plus the staged and unstaged tracked diff without retaining it.
///
/// This intentionally excludes untracked files. A check receipt that depends
/// on an untracked evidence file must hash that file explicitly.
pub fn capture_tracked_diff_sha256(repo_root: &Path, revision: &str) -> Option<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["diff", "--binary", "HEAD", "--"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut hasher = Sha256::new();
    hasher.update(revision.as_bytes());
    hasher.update([0]);
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = stdout.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    child
        .wait()
        .ok()?
        .success()
        .then(|| format!("{:x}", hasher.finalize()))
}

/// Add the current client context to one operation's arguments.
///
/// The CLI owns `provenance.execution`: replacing a supplied value prevents a
/// stale serialized context from being attributed to the current invocation.
/// Semantic identifiers supplied by the caller keep precedence.
pub fn enrich_operation_arguments(
    tool: &str,
    arguments: Value,
    context: &ExecutionContext,
) -> Result<Value> {
    enrich_operation_arguments_with_task(tool, arguments, context, nonempty_env("MEMD_TASK_ID"))
}

/// Capture once, then materialize context into a CLI command before warm/cold
/// routing. A warm worker therefore receives the caller's context and never
/// observes its own process identity on behalf of a client.
pub fn prepare_execution_context(cmd: &mut CliCommand) -> Result<()> {
    if !matches!(
        cmd,
        CliCommand::Search { .. } | CliCommand::AgentContext { .. } | CliCommand::Call { .. }
    ) {
        return Ok(());
    }
    let cwd = std::env::current_dir()?;
    let context = capture_execution_context(&cwd);
    let task_id = nonempty_env("MEMD_TASK_ID");
    prepare_execution_context_with(cmd, &context, task_id)
}

fn prepare_execution_context_with(
    cmd: &mut CliCommand,
    context: &ExecutionContext,
    task_id: Option<String>,
) -> Result<()> {
    match cmd {
        CliCommand::Search {
            task_id: command_task_id,
            thread_id,
            ..
        }
        | CliCommand::AgentContext {
            task_id: command_task_id,
            thread_id,
            ..
        } => {
            if command_task_id.is_none() {
                *command_task_id = task_id;
            }
            if thread_id.is_none() {
                *thread_id = native_conversation_id(context).map(str::to_string);
            }
        }
        CliCommand::Call {
            tool, json, input, ..
        } => {
            let arguments = parse_call_arguments(json.as_deref(), input.as_deref())?;
            let arguments =
                enrich_operation_arguments_with_task(tool, arguments, context, task_id)?;
            *json = Some(serde_json::to_string(&arguments)?);
            *input = None;
        }
        _ => {}
    }
    Ok(())
}

fn enrich_operation_arguments_with_task(
    tool: &str,
    arguments: Value,
    context: &ExecutionContext,
    task_id: Option<String>,
) -> Result<Value> {
    let mut arguments = object_or_empty(arguments)?;
    if tool == "memory.search" {
        insert_if_absent(&mut arguments, "task_id", task_id.map(Value::String));
        insert_if_absent(
            &mut arguments,
            "thread_id",
            native_conversation_id(context).map(|value| Value::String(value.to_string())),
        );
        return Ok(Value::Object(arguments));
    }

    let task_provenance = TASK_PROVENANCE_TOOLS.contains(&tool);
    let experience_provenance = matches!(tool, "experience.record" | "experience.record_check");
    if task_provenance || experience_provenance {
        insert_if_absent(
            &mut arguments,
            "session_id",
            context.native_session_id.clone().map(Value::String),
        );
        if task_provenance {
            insert_if_absent(
                &mut arguments,
                "thread_id",
                native_conversation_id(context).map(|value| Value::String(value.to_string())),
            );
        }
        insert_if_absent(
            &mut arguments,
            "agent_id",
            context.native_agent_id.clone().map(Value::String),
        );
        let provenance = arguments
            .entry("provenance".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let provenance = provenance.as_object_mut().ok_or_else(|| {
            MemdError::ValidationError("provenance must be a JSON object".to_string())
        })?;
        provenance.insert("execution".to_string(), serde_json::to_value(context)?);
    }
    Ok(Value::Object(arguments))
}

fn native_conversation_id(context: &ExecutionContext) -> Option<&str> {
    context
        .native_thread_id
        .as_deref()
        .or(context.native_session_id.as_deref())
}

fn object_or_empty(value: Value) -> Result<Map<String, Value>> {
    match value {
        Value::Object(object) => Ok(object),
        Value::Null => Ok(Map::new()),
        _ => Err(MemdError::ValidationError(
            "call arguments must be a JSON object".to_string(),
        )),
    }
}

fn insert_if_absent(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if !object.contains_key(key) {
        if let Some(value) = value {
            object.insert(key.to_string(), value);
        }
    }
}

fn capture_repository(cwd: &Path) -> (Option<String>, Option<String>, Option<bool>) {
    let Some(root) = git_text(cwd, &["rev-parse", "--show-toplevel"]) else {
        return (None, None, None);
    };
    let root_path = Path::new(&root);
    let revision = git_text(root_path, &["rev-parse", "HEAD"]);
    let dirty = git_bytes(
        root_path,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )
    .map(|output| !output.is_empty());
    (Some(root), revision, dirty)
}

fn git_text(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = git_bytes(cwd, args)?;
    nonempty_text(output)
}

fn git_bytes(cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn command_text(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| nonempty_text(output.stdout))?
}

fn nonempty_text(bytes: Vec<u8>) -> Option<String> {
    let value = String::from_utf8(bytes).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn observed_at_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context(thread_id: &str, model: Option<&str>) -> ExecutionContext {
        ExecutionContext {
            harness: Some("codex".to_string()),
            model: model.map(str::to_string),
            native_session_id: Some("session-tree".to_string()),
            native_thread_id: Some(thread_id.to_string()),
            observed_at_ms: 42,
            ..ExecutionContext::default()
        }
    }

    #[test]
    fn two_clients_keep_distinct_context_before_worker_dispatch() {
        let first = enrich_operation_arguments_with_task(
            "artifact.create",
            json!({"artifact_kind": "evidence"}),
            &context("thread-a", Some("model-a")),
            None,
        )
        .unwrap();
        let second = enrich_operation_arguments_with_task(
            "artifact.create",
            json!({"artifact_kind": "evidence"}),
            &context("thread-b", Some("model-b")),
            None,
        )
        .unwrap();

        assert_eq!(first["thread_id"], "thread-a");
        assert_eq!(first["provenance"]["execution"]["model"], "model-a");
        assert_eq!(second["thread_id"], "thread-b");
        assert_eq!(second["provenance"]["execution"]["model"], "model-b");
    }

    #[test]
    fn absent_model_stays_absent() {
        let arguments = enrich_operation_arguments_with_task(
            "task.start",
            json!({"goal": "test"}),
            &context("thread-a", None),
            None,
        )
        .unwrap();

        assert!(arguments["provenance"]["execution"].get("model").is_none());
    }

    #[test]
    fn experience_keeps_native_thread_only_in_execution_context() {
        let arguments = enrich_operation_arguments_with_task(
            "experience.record",
            json!({"event": {"kind": "problem"}}),
            &context("thread-a", Some("model-a")),
            None,
        )
        .unwrap();

        assert!(arguments.get("thread_id").is_none());
        assert_eq!(arguments["session_id"], "session-tree");
        assert_eq!(
            arguments["provenance"]["execution"]["native_thread_id"],
            "thread-a"
        );
    }

    #[test]
    fn current_client_replaces_serialized_execution_but_preserves_source_fields() {
        let arguments = enrich_operation_arguments_with_task(
            "artifact.create",
            json!({
                "artifact_kind": "evidence",
                "provenance": {
                    "uri": "file:///evidence.txt",
                    "execution": {"native_thread_id": "stale", "observed_at_ms": 1}
                }
            }),
            &context("current", None),
            None,
        )
        .unwrap();

        assert_eq!(arguments["provenance"]["uri"], "file:///evidence.txt");
        assert_eq!(
            arguments["provenance"]["execution"]["native_thread_id"],
            "current"
        );
    }

    #[test]
    fn memory_search_fills_only_missing_attribution_ids() {
        let arguments = enrich_operation_arguments_with_task(
            "memory.search",
            json!({"query": "q", "thread_id": "explicit"}),
            &context("observed", None),
            Some("task-1".to_string()),
        )
        .unwrap();

        assert_eq!(arguments["task_id"], "task-1");
        assert_eq!(arguments["thread_id"], "explicit");
        assert!(arguments.get("provenance").is_none());
    }
}
