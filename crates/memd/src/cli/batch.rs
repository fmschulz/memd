use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{MemdError, Result};
use crate::store::{Store, TenantManager};

use super::scope::{apply_operation_scope, apply_operation_scope_at, OperationScopeCache};
use super::{cli_call_tool, read_stdin_to_string, unwrap_content_payload};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BatchCallInput {
    tool: String,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(
        default,
        rename = "__memd_scope_error",
        skip_serializing_if = "Option::is_none"
    )]
    scope_error: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

pub(super) fn read_batch_input(path: Option<&Path>) -> Result<String> {
    match path {
        None => read_stdin_to_string(),
        Some(p) if p.as_os_str() == std::ffi::OsStr::new("-") => read_stdin_to_string(),
        Some(p) => Ok(std::fs::read_to_string(p)?),
    }
}

/// Resolve repository scope before a non-streaming batch is sent to a warm
/// worker, whose process may have been started from a different repository.
pub(super) fn scope_batch_jsonl(input: &str, continue_on_error: bool) -> Result<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    scope_batch_jsonl_at(input, &cwd, continue_on_error)
}

fn scope_batch_jsonl_at(input: &str, start: &Path, continue_on_error: bool) -> Result<String> {
    let mut out = String::new();
    let mut scope_cache = OperationScopeCache::default();
    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            out.push('\n');
            continue;
        }

        let rendered = match serde_json::from_str::<BatchCallInput>(line) {
            Ok(mut request) => {
                if request.scope_error.is_some() {
                    return Err(MemdError::ValidationError(
                        "reserved batch field __memd_scope_error is only valid on the warm wire"
                            .to_string(),
                    ));
                }
                let arguments = request.arguments.take().unwrap_or_else(|| json!({}));
                if arguments.is_object() || arguments.is_null() {
                    request.arguments = Some(
                        match apply_operation_scope_at(start, arguments.clone(), &mut scope_cache) {
                            Ok(arguments) => arguments,
                            Err(error) if !continue_on_error => return Err(error),
                            Err(error) => {
                                request.scope_error = Some(error.to_string());
                                arguments
                            }
                        },
                    );
                    serde_json::to_string(&request)?
                } else {
                    raw_line.to_string()
                }
            }
            Err(_) => raw_line.to_string(),
        };
        out.push_str(&rendered);
        out.push('\n');
    }
    Ok(out)
}

pub(super) async fn run_batch_jsonl<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    input: &str,
    continue_on_error: bool,
) -> Result<String> {
    let mut scope_cache = OperationScopeCache::default();
    run_batch_jsonl_with_scope_cache(
        store,
        tenant_manager,
        input,
        continue_on_error,
        &mut scope_cache,
        false,
    )
    .await
}

pub(super) async fn run_pre_scoped_batch_jsonl<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    input: &str,
    continue_on_error: bool,
) -> Result<String> {
    let mut scope_cache = OperationScopeCache::disabled();
    run_batch_jsonl_with_scope_cache(
        store,
        tenant_manager,
        input,
        continue_on_error,
        &mut scope_cache,
        true,
    )
    .await
}

async fn run_batch_jsonl_with_scope_cache<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    input: &str,
    continue_on_error: bool,
    scope_cache: &mut OperationScopeCache,
    pre_scoped: bool,
) -> Result<String> {
    let mut out = String::new();
    let mut processed = 0usize;

    for (line_number, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let index = processed;
        processed += 1;

        let request = match serde_json::from_str::<BatchCallInput>(line) {
            Ok(request) => request,
            Err(error) => {
                if !continue_on_error {
                    return Err(MemdError::ValidationError(format!(
                        "invalid JSONL request on line {}: {error}",
                        line_number + 1
                    )));
                }
                let row = json!({
                    "ok": false,
                    "index": index,
                    "line": line_number + 1,
                    "error": format!("invalid JSONL request: {error}"),
                });
                out.push_str(&serde_json::to_string(&row)?);
                out.push('\n');
                continue;
            }
        };

        if let Some(scope_error) = request.scope_error.as_ref() {
            if !pre_scoped {
                return Err(MemdError::ValidationError(
                    "reserved batch field __memd_scope_error is only valid on the warm wire"
                        .to_string(),
                ));
            }
            if !continue_on_error {
                return Err(MemdError::ValidationError(scope_error.clone()));
            }
            let row = json!({
                "ok": false,
                "index": index,
                "line": line_number + 1,
                "tool": request.tool,
                "error": scope_error,
            });
            out.push_str(&serde_json::to_string(&row)?);
            out.push('\n');
            continue;
        }

        let arguments = request.arguments.unwrap_or_else(|| json!({}));
        if !(arguments.is_object() || arguments.is_null()) {
            let message = "batch arguments must be a JSON object".to_string();
            if !continue_on_error {
                return Err(MemdError::ValidationError(message));
            }
            let row = json!({
                "ok": false,
                "index": index,
                "line": line_number + 1,
                "tool": request.tool,
                "error": message,
            });
            out.push_str(&serde_json::to_string(&row)?);
            out.push('\n');
            continue;
        }
        let arguments = match apply_operation_scope(arguments, scope_cache) {
            Ok(arguments) => arguments,
            Err(error) if !continue_on_error => return Err(error),
            Err(error) => {
                let row = json!({
                    "ok": false,
                    "index": index,
                    "line": line_number + 1,
                    "tool": request.tool,
                    "error": error.to_string(),
                });
                out.push_str(&serde_json::to_string(&row)?);
                out.push('\n');
                continue;
            }
        };

        let started = std::time::Instant::now();
        match cli_call_tool(store, tenant_manager, &request.tool, arguments).await {
            Ok(value) => {
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                let payload = unwrap_content_payload(value.clone()).unwrap_or(value);
                let row = json!({
                    "ok": true,
                    "index": index,
                    "line": line_number + 1,
                    "tool": request.tool,
                    "elapsed_ms": elapsed_ms,
                    "result": payload,
                });
                out.push_str(&serde_json::to_string(&row)?);
                out.push('\n');
            }
            Err(error) => {
                if !continue_on_error {
                    return Err(MemdError::ProtocolError(error.to_string()));
                }
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                let row = json!({
                    "ok": false,
                    "index": index,
                    "line": line_number + 1,
                    "tool": request.tool,
                    "elapsed_ms": elapsed_ms,
                    "error": error.to_string(),
                });
                out.push_str(&serde_json::to_string(&row)?);
                out.push('\n');
            }
        }
    }

    Ok(out)
}

pub(super) async fn stream_batch_jsonl<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    input_path: Option<&Path>,
    output_path: Option<&Path>,
    continue_on_error: bool,
) -> Result<()> {
    use std::io::{BufRead, BufReader, BufWriter, Write};

    let input: Box<dyn BufRead> = match input_path {
        None => Box::new(BufReader::new(std::io::stdin())),
        Some(p) if p.as_os_str() == std::ffi::OsStr::new("-") => {
            Box::new(BufReader::new(std::io::stdin()))
        }
        Some(p) => Box::new(BufReader::new(std::fs::File::open(p)?)),
    };
    let mut output: Box<dyn Write> = match output_path {
        None => Box::new(BufWriter::new(std::io::stdout())),
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Box::new(BufWriter::new(std::fs::File::create(path)?))
        }
    };

    let mut processed = 0usize;
    let mut scope_cache = OperationScopeCache::default();
    for (line_number, raw_line) in input.lines().enumerate() {
        let raw_line = raw_line?;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let index = processed;
        processed += 1;

        let rendered = match run_batch_jsonl_with_scope_cache(
            store,
            tenant_manager,
            line,
            continue_on_error,
            &mut scope_cache,
            false,
        )
        .await
        {
            Ok(rendered) => {
                let mut row: Value = serde_json::from_str(rendered.trim())?;
                if let Some(obj) = row.as_object_mut() {
                    obj.insert("index".to_string(), json!(index));
                    obj.insert("line".to_string(), json!(line_number + 1));
                }
                serde_json::to_string(&row)? + "\n"
            }
            Err(error) if continue_on_error => {
                let row = json!({
                    "ok": false,
                    "index": index,
                    "line": line_number + 1,
                    "error": error.to_string(),
                });
                serde_json::to_string(&row)? + "\n"
            }
            Err(error) => return Err(error),
        };

        output.write_all(rendered.as_bytes())?;
        output.flush()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_batch_payload_inherits_scope_before_routing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(
            dir.path().join(".memd/project_scope.json"),
            r#"{
              "tenant_id": "batch_tenant",
              "project_id": "batch_project",
              "interface": "cli",
              "cli_command": "memd",
              "agent_context_output": ".memd/context.md",
              "project_dir": "."
            }"#,
        )
        .unwrap();
        let input = concat!(
            "{\"tool\":\"memory.add\",\"arguments\":{\"text\":\"scoped\"},\"id\":\"correlation\"}\n",
            "{\"tool\":\"memory.search\",\"arguments\":{\"tenant_id\":\"explicit\",\"query\":\"q\"}}\n",
            "not-json\n"
        );

        let output = scope_batch_jsonl_at(input, dir.path(), false).unwrap();
        let lines = output.lines().collect::<Vec<_>>();
        let scoped: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(scoped["arguments"]["tenant_id"], "batch_tenant");
        assert_eq!(scoped["arguments"]["project_id"], "batch_project");
        assert_eq!(scoped["id"], "correlation");
        let explicit: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(explicit["arguments"]["tenant_id"], "explicit");
        assert!(explicit["arguments"].get("project_id").is_none());
        assert_eq!(lines[2], "not-json");
    }

    #[test]
    fn warm_batch_continue_on_error_encodes_scope_failure_per_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".memd")).unwrap();
        std::fs::write(dir.path().join(".memd/project_scope.json"), "{not json").unwrap();
        let input = concat!(
            "{\"tool\":\"memory.add\",\"arguments\":{\"tenant_id\":\"explicit\",\"text\":\"ok\"}}\n",
            "{\"tool\":\"memory.add\",\"arguments\":{\"text\":\"scope fails\"}}\n",
            "{\"tool\":\"memory.search\",\"arguments\":{\"tenant_id\":\"explicit\",\"query\":\"ok\"}}\n"
        );

        let output = scope_batch_jsonl_at(input, dir.path(), true).unwrap();
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert!(first.get("__memd_scope_error").is_none());
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert!(second["__memd_scope_error"]
            .as_str()
            .unwrap()
            .contains("malformed"));
        let third: Value = serde_json::from_str(lines[2]).unwrap();
        assert!(third.get("__memd_scope_error").is_none());
    }

    #[test]
    fn warm_batch_rejects_client_supplied_scope_error_field() {
        let dir = tempfile::tempdir().unwrap();
        let input = concat!(
            "{\"tool\":\"memory.add\",\"arguments\":{\"tenant_id\":\"explicit\",\"text\":\"do not run\"},",
            "\"__memd_scope_error\":\"spoofed\"}\n"
        );

        let error = scope_batch_jsonl_at(input, dir.path(), true).unwrap_err();
        assert!(
            error.to_string().contains("reserved batch field"),
            "{error}"
        );
    }
}
