use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{MemdError, Result};
use crate::store::{Store, TenantManager};

use super::{cli_call_tool, read_stdin_to_string, unwrap_content_payload};

#[derive(Debug, Clone, Deserialize)]
struct BatchCallInput {
    tool: String,
    #[serde(default)]
    arguments: Option<Value>,
}

pub(super) fn read_batch_input(path: Option<&Path>) -> Result<String> {
    match path {
        None => read_stdin_to_string(),
        Some(p) if p.as_os_str() == std::ffi::OsStr::new("-") => read_stdin_to_string(),
        Some(p) => Ok(std::fs::read_to_string(p)?),
    }
}

pub(super) async fn run_batch_jsonl<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    input: &str,
    continue_on_error: bool,
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
    for (line_number, raw_line) in input.lines().enumerate() {
        let raw_line = raw_line?;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let index = processed;
        processed += 1;

        let rendered = match run_batch_jsonl(store, tenant_manager, line, continue_on_error).await {
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
