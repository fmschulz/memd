use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::error::{MemdError, Result};
use crate::store::{Store, TenantManager};

use super::args::{
    CliCommand, CliQueryMode, ExportFormat, SearchRerankerOptions, WarmCommand, WarmMode,
    WarmProcessConfig,
};
use super::{
    apply_search_reranker, cli_agent_context_payload, cli_call_tool, cli_search_payload,
    parse_call_arguments, render_agent_context, render_search_payload, unwrap_content_payload,
    write_cli_log, write_rendered,
};

const WARM_WIRE_PROTOCOL: &str = "2";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WarmWireCommand {
    Search {
        tenant_id: String,
        query: String,
        k: usize,
        project_id: Option<String>,
        compact: bool,
        token_budget: Option<usize>,
        mode: CliQueryMode,
        no_text: bool,
        include_artifact: bool,
        format: ExportFormat,
        reranker: SearchRerankerOptions,
    },
    AgentContext {
        tenant_id: String,
        project_id: Option<String>,
        query: Vec<String>,
        k: usize,
        token_budget: usize,
        mode: CliQueryMode,
        no_text: bool,
        include_artifact: bool,
        format: ExportFormat,
    },
    Call {
        tool: String,
        arguments: Value,
    },
}

#[derive(Debug, Clone)]
struct WarmLocalOutputs {
    output: Option<PathBuf>,
    log_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum WarmWireRequest {
    Ping,
    Shutdown,
    Command { command: WarmWireCommand },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WarmWireResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl WarmWireResponse {
    fn ok_result(result: Value) -> Self {
        Self {
            ok: true,
            output: None,
            log_payload: None,
            result: Some(result),
            error: None,
        }
    }

    fn ok_output(output: String, log_payload: Option<Value>) -> Self {
        Self {
            ok: true,
            output: Some(output),
            log_payload,
            result: None,
            error: None,
        }
    }

    fn error(error: impl ToString) -> Self {
        Self {
            ok: false,
            output: None,
            log_payload: None,
            result: None,
            error: Some(error.to_string()),
        }
    }
}

pub fn warm_socket_path(config: &WarmProcessConfig) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(b"\0");
    hasher.update(WARM_WIRE_PROTOCOL.as_bytes());
    hasher.update(b"\0");
    hasher.update(config.data_dir.display().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(config.embedding_model.as_bytes());
    hasher.update(b"\0");
    hasher.update(config.search_variant.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let data_dir_socket = config
        .data_dir
        .join("warm")
        .join(&hex[..16])
        .join("memd.sock");
    if data_dir_socket.to_string_lossy().len() < 100 {
        data_dir_socket
    } else {
        std::env::temp_dir()
            .join("memd-warm")
            .join(&hex[..16])
            .join("memd.sock")
    }
}

fn warm_pid_path(config: &WarmProcessConfig) -> PathBuf {
    warm_socket_path(config).with_file_name("memd.pid")
}

fn warm_log_path(config: &WarmProcessConfig) -> PathBuf {
    warm_socket_path(config).with_file_name("worker.log")
}

fn warm_wire_command_from_cli(
    cmd: &CliCommand,
) -> Result<Option<(WarmWireCommand, WarmLocalOutputs)>> {
    let mapped = match cmd {
        CliCommand::Search {
            tenant_id,
            query,
            k,
            project_id,
            compact,
            token_budget,
            mode,
            no_text,
            include_artifact,
            format,
            output,
            reranker,
            reranker_model,
            reranker_device,
            reranker_batch_size,
            reranker_timeout_seconds,
            reranker_python,
            warm: _,
        } => (
            WarmWireCommand::Search {
                tenant_id: tenant_id.clone(),
                query: query.clone(),
                k: *k,
                project_id: project_id.clone(),
                compact: *compact,
                token_budget: *token_budget,
                mode: *mode,
                no_text: *no_text,
                include_artifact: *include_artifact,
                format: *format,
                reranker: SearchRerankerOptions {
                    reranker: *reranker,
                    model: reranker_model.clone(),
                    device: reranker_device.clone(),
                    batch_size: *reranker_batch_size,
                    timeout_seconds: *reranker_timeout_seconds,
                    python: reranker_python.clone(),
                },
            },
            WarmLocalOutputs {
                output: output.clone(),
                log_dir: None,
            },
        ),
        CliCommand::AgentContext {
            tenant_id,
            project_id,
            query,
            k,
            token_budget,
            mode,
            no_text,
            include_artifact,
            format,
            output,
            log_dir,
            warm: _,
        } => (
            WarmWireCommand::AgentContext {
                tenant_id: tenant_id.clone(),
                project_id: project_id.clone(),
                query: query.clone(),
                k: *k,
                token_budget: *token_budget,
                mode: *mode,
                no_text: *no_text,
                include_artifact: *include_artifact,
                format: *format,
            },
            WarmLocalOutputs {
                output: output.clone(),
                log_dir: log_dir.clone(),
            },
        ),
        CliCommand::Call {
            tool,
            json,
            input,
            output,
            warm: _,
        } => (
            WarmWireCommand::Call {
                tool: tool.clone(),
                arguments: parse_call_arguments(json.as_deref(), input.as_deref())?,
            },
            WarmLocalOutputs {
                output: output.clone(),
                log_dir: None,
            },
        ),
        _ => return Ok(None),
    };
    Ok(Some(mapped))
}

async fn execute_warm_wire_command<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    command: WarmWireCommand,
) -> Result<(String, Option<Value>)> {
    match command {
        WarmWireCommand::Search {
            tenant_id,
            query,
            k,
            project_id,
            compact,
            token_budget,
            mode,
            no_text,
            include_artifact,
            format,
            reranker,
        } => {
            let mut payload = cli_search_payload(
                store,
                tenant_id.clone(),
                project_id,
                query.clone(),
                k,
                compact,
                token_budget,
                mode,
                no_text,
                include_artifact,
            )
            .await?;
            payload = apply_search_reranker(payload, &query, &reranker)?;
            Ok((render_search_payload(&payload, format)?, None))
        }
        WarmWireCommand::AgentContext {
            tenant_id,
            project_id,
            query,
            k,
            token_budget,
            mode,
            no_text,
            include_artifact,
            format,
        } => {
            let payload = cli_agent_context_payload(
                store,
                &tenant_id,
                project_id.as_deref(),
                &query,
                k,
                token_budget,
                mode,
                no_text,
                include_artifact,
            )
            .await?;
            let rendered = render_agent_context(&payload, format)?;
            Ok((rendered, Some(payload)))
        }
        WarmWireCommand::Call { tool, arguments } => {
            let value = cli_call_tool(store, tenant_manager, &tool, arguments)
                .await
                .map_err(|e| MemdError::ProtocolError(e.to_string()))?;
            let payload = unwrap_content_payload(value.clone()).unwrap_or(value);
            Ok((serde_json::to_string_pretty(&payload)? + "\n", None))
        }
    }
}

pub async fn try_run_warm_client(config: &WarmProcessConfig, cmd: &CliCommand) -> Result<bool> {
    let Some(mode) = cmd.warm_mode() else {
        return Ok(false);
    };
    if mode == WarmMode::Off {
        return Ok(false);
    }
    let Some((wire_command, local_outputs)) = warm_wire_command_from_cli(cmd)? else {
        return Ok(false);
    };

    match warm_ping(config).await {
        Ok(_) => {}
        Err(error) => match warm_start(config).await {
            Ok(_) => {}
            Err(start_error) if mode == WarmMode::Auto => {
                warn!(
                    error = %error,
                    start_error = %start_error,
                    "warm worker unavailable; falling back to cold CLI"
                );
                return Ok(false);
            }
            Err(start_error) => {
                return Err(MemdError::ProtocolError(format!(
                    "warm worker required but unavailable: {error}; start failed: {start_error}"
                )));
            }
        },
    }

    let response = warm_request(
        &warm_socket_path(config),
        &WarmWireRequest::Command {
            command: wire_command,
        },
    )
    .await?;
    if !response.ok {
        return Err(MemdError::ProtocolError(
            response
                .error
                .unwrap_or_else(|| "warm worker command failed".to_string()),
        ));
    }
    if let Some(payload) = response.log_payload.as_ref() {
        write_cli_log(local_outputs.log_dir.as_deref(), "memd_search", payload)?;
    }
    let output = response.output.unwrap_or_default();
    write_rendered(local_outputs.output.as_deref(), &output)?;
    Ok(true)
}

pub async fn run_warm_admin(config: &WarmProcessConfig, command: WarmCommand) -> Result<()> {
    let payload = match command {
        WarmCommand::Start => warm_start(config).await?,
        WarmCommand::Status => match warm_ping(config).await {
            Ok(result) => json!({
                "status": "running",
                "socket": warm_socket_path(config),
                "result": result,
            }),
            Err(error) => json!({
                "status": "stopped",
                "socket": warm_socket_path(config),
                "error": error.to_string(),
            }),
        },
        WarmCommand::Stop => {
            match warm_request(&warm_socket_path(config), &WarmWireRequest::Shutdown).await {
                Ok(response) if response.ok => {
                    let result = response.result.unwrap_or(Value::Null);
                    let pid = result
                        .get("pid")
                        .and_then(Value::as_u64)
                        .and_then(|pid| u32::try_from(pid).ok());
                    let stopped = wait_for_warm_pid_exit(pid, Duration::from_secs(10)).await;
                    if stopped {
                        let _ = std::fs::remove_file(warm_pid_path(config));
                    }
                    json!({
                        "status": if stopped { "stopped" } else { "stopping" },
                        "socket": warm_socket_path(config),
                        "result": result,
                    })
                }
                Ok(response) => {
                    return Err(MemdError::ProtocolError(
                        response
                            .error
                            .unwrap_or_else(|| "warm worker stop failed".to_string()),
                    ));
                }
                Err(error) => json!({
                    "status": "not_running",
                    "socket": warm_socket_path(config),
                    "error": error.to_string(),
                }),
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

async fn wait_for_warm_pid_exit(pid: Option<u32>, timeout: Duration) -> bool {
    let Some(pid) = pid else {
        return true;
    };
    let deadline = Instant::now() + timeout;
    loop {
        if !warm_pid_is_running(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn warm_pid_is_running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

async fn warm_start(config: &WarmProcessConfig) -> Result<Value> {
    if let Ok(result) = warm_ping(config).await {
        return Ok(json!({
            "status": "already_running",
            "socket": warm_socket_path(config),
            "result": result,
        }));
    }

    let socket = warm_socket_path(config);
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }

    let log_path = warm_log_path(config);
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(std::env::current_exe()?);
    if let Some(config_path) = &config.config_path {
        command.arg("--config").arg(config_path);
    }
    command
        .arg("--data-dir")
        .arg(&config.data_dir)
        .arg("--embedding-model")
        .arg(&config.embedding_model)
        .arg("--search-variant")
        .arg(&config.search_variant)
        .arg("warm-worker")
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn()?;
    let pid = child.id();
    std::fs::write(warm_pid_path(config), format!("{pid}\n"))?;

    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| MemdError::ValidationError(format!("system time before epoch: {e}")))?
        .as_millis();
    for _ in 0..300 {
        match warm_ping(config).await {
            Ok(result) => {
                return Ok(json!({
                    "status": "started",
                    "pid": pid,
                    "socket": socket,
                    "log": log_path,
                    "startup_ms": SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|e| MemdError::ValidationError(format!("system time before epoch: {e}")))?
                        .as_millis()
                        .saturating_sub(start),
                    "result": result,
                }));
            }
            Err(_) => {
                if let Some(status) = child.try_wait()? {
                    return Err(MemdError::ProtocolError(format!(
                        "warm worker exited before becoming ready: {status}; see {}",
                        log_path.display()
                    )));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    Err(MemdError::ProtocolError(format!(
        "warm worker did not become ready within 30s; see {}",
        log_path.display()
    )))
}

async fn warm_ping(config: &WarmProcessConfig) -> Result<Value> {
    let response = warm_request(&warm_socket_path(config), &WarmWireRequest::Ping).await?;
    if !response.ok {
        return Err(MemdError::ProtocolError(
            response
                .error
                .unwrap_or_else(|| "warm worker ping failed".to_string()),
        ));
    }
    let result = response.result.unwrap_or(Value::Null);
    validate_warm_worker_identity(&result)?;
    Ok(result)
}

fn warm_worker_identity(socket: &Path) -> Value {
    json!({
        "pid": std::process::id(),
        "socket": socket,
        "memd_version": env!("CARGO_PKG_VERSION"),
        "warm_wire_protocol": WARM_WIRE_PROTOCOL,
    })
}

fn validate_warm_worker_identity(result: &Value) -> Result<()> {
    let worker_version = result
        .get("memd_version")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let worker_protocol = result
        .get("warm_wire_protocol")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if worker_version != env!("CARGO_PKG_VERSION") || worker_protocol != WARM_WIRE_PROTOCOL {
        return Err(MemdError::ProtocolError(format!(
            "warm worker is incompatible: worker version {worker_version}, protocol {worker_protocol}; CLI version {}, protocol {}",
            env!("CARGO_PKG_VERSION"),
            WARM_WIRE_PROTOCOL
        )));
    }
    Ok(())
}

#[cfg(unix)]
async fn warm_request(socket: &Path, request: &WarmWireRequest) -> Result<WarmWireResponse> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(socket).await?;
    let body = serde_json::to_vec(request)?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;

    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(not(unix))]
async fn warm_request(_socket: &Path, _request: &WarmWireRequest) -> Result<WarmWireResponse> {
    Err(MemdError::ProtocolError(
        "warm worker requires Unix domain sockets".to_string(),
    ))
}

#[cfg(unix)]
pub(super) async fn run_warm_worker<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    socket: &Path,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    info!(socket = %socket.display(), "memd warm worker listening");

    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await?;
        let mut shutdown = false;
        let response = match serde_json::from_slice::<WarmWireRequest>(&bytes) {
            Ok(WarmWireRequest::Ping) => WarmWireResponse::ok_result(warm_worker_identity(socket)),
            Ok(WarmWireRequest::Shutdown) => {
                shutdown = true;
                WarmWireResponse::ok_result(warm_worker_identity(socket))
            }
            Ok(WarmWireRequest::Command { command }) => {
                match execute_warm_wire_command(store, tenant_manager, command).await {
                    Ok((output, log_payload)) => WarmWireResponse::ok_output(output, log_payload),
                    Err(error) => WarmWireResponse::error(error),
                }
            }
            Err(error) => WarmWireResponse::error(format!("invalid warm request: {error}")),
        };
        let body = serde_json::to_vec(&response)?;
        stream.write_all(&body).await?;
        stream.shutdown().await?;
        if shutdown {
            break;
        }
    }

    let _ = std::fs::remove_file(socket);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_worker_identity_validation_rejects_legacy_ping_payload() {
        let legacy = json!({
            "pid": 1234,
            "socket": "/tmp/memd.sock",
        });

        let err = validate_warm_worker_identity(&legacy).unwrap_err();
        assert!(err.to_string().contains("incompatible"));
    }

    #[test]
    fn warm_worker_identity_validation_accepts_current_payload() {
        let payload = warm_worker_identity(Path::new("/tmp/memd.sock"));

        validate_warm_worker_identity(&payload).unwrap();
    }
}

#[cfg(not(unix))]
pub(super) async fn run_warm_worker<S: Store>(
    _store: &S,
    _tenant_manager: Option<&TenantManager>,
    _socket: &Path,
) -> Result<()> {
    Err(MemdError::ProtocolError(
        "warm worker requires Unix domain sockets".to_string(),
    ))
}
