use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::error::{MemdError, Result};
use crate::store::{RywProbeStats, Store, TenantManager};
use crate::types::ChunkType;

use super::args::{
    CliCommand, CliQueryMode, ExportFormat, ReportFormat, SearchRerankerOptions, WarmCommand,
    WarmMode, WarmProcessConfig,
};
use super::batch::{read_batch_input, run_batch_jsonl};
use super::consolidate::{run_consolidate, ConsolidateOptions};
use super::purge::{run_purge, PurgeOptions};
use super::report::{cli_report_rendered, ReportOptions};
use super::{
    apply_search_reranker, cli_add_rendered, cli_agent_context_payload, cli_call_tool,
    cli_delete_rendered, cli_import_omf_rendered, cli_search_payload, parse_call_arguments,
    render_agent_context, render_search_payload, unwrap_content_payload, write_cli_log,
    write_rendered, CliAddRenderOptions,
};

const WARM_WIRE_PROTOCOL: &str = "3";

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
    Report {
        tenant_id: Option<String>,
        project_id: Option<String>,
        since: String,
        top: usize,
        format: ReportFormat,
    },
    Call {
        tool: String,
        arguments: Value,
    },
    Add {
        tenant_id: String,
        text: String,
        chunk_type: ChunkType,
        project_id: Option<String>,
        tags: Option<Vec<String>>,
        source_uri: Option<String>,
        source_path: Option<String>,
    },
    Delete {
        tenant_id: String,
        chunk_id: String,
    },
    ImportOmf {
        tenant_id: String,
        document_json: String,
        include_archived: bool,
        fuzzy_threshold: Option<f32>,
        dry_run: bool,
    },
    Purge {
        tenant_id: String,
        project_id: Option<String>,
        older_than_days: u64,
        limit: usize,
        include_unreadable_active: bool,
        archive: Option<PathBuf>,
        apply: bool,
        vacuum_metadata: bool,
        rewrite_segments: bool,
    },
    Consolidate {
        tenant_id: Option<String>,
        project_id: Option<String>,
        project_dir: PathBuf,
        max_region: usize,
        dry_run: bool,
        background: bool,
        force: bool,
    },
    Batch {
        jsonl_content: String,
        continue_on_error: bool,
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
    warm_socket_path_for_data_dir(&config.data_dir)
}

/// One socket per data dir, stable across binary upgrades.
/// Version/protocol/model/variant are deliberately NOT hashed so a new
/// CLI can ping and replace a worker left behind by an old binary;
/// skew is handled by the ping identity handshake.
pub(super) fn warm_socket_path_for_data_dir(data_dir: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(data_dir.display().to_string().as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let data_dir_socket = data_dir.join("warm").join(&hex[..16]).join("memd.sock");
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
    warm_pid_path_for_socket(&warm_socket_path(config))
}

fn warm_pid_path_for_socket(socket: &Path) -> PathBuf {
    socket.with_file_name("memd.pid")
}

fn warm_log_path(config: &WarmProcessConfig) -> PathBuf {
    warm_socket_path(config).with_file_name("worker.log")
}

#[cfg(unix)]
fn warm_temp_socket_path(socket: &Path, pid: u32) -> PathBuf {
    let file_name = socket
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "memd.sock".into());
    socket.with_file_name(format!("{file_name}.tmp-{pid}"))
}

#[cfg(unix)]
fn remove_stale_warm_socket_temps(socket: &Path) {
    let Some(parent) = socket.parent() else {
        return;
    };
    let Some(file_name) = socket.file_name() else {
        return;
    };
    let prefix = format!("{}.tmp-", file_name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn warm_routable(cmd: &CliCommand) -> bool {
    matches!(
        cmd,
        CliCommand::Search {
            include_superseded: false,
            ..
        } | CliCommand::AgentContext { .. }
            | CliCommand::Report { .. }
            | CliCommand::Call { .. }
            | CliCommand::Add { .. }
            | CliCommand::Delete { .. }
            | CliCommand::ImportOmf { .. }
            | CliCommand::Purge { .. }
            | CliCommand::Consolidate { .. }
            | CliCommand::Batch { stream: false, .. }
    )
}

fn describe_unroutable(cmd: &CliCommand) -> &'static str {
    match cmd {
        CliCommand::Search {
            include_superseded: true,
            ..
        } => "search --include-superseded",
        CliCommand::Batch { stream: true, .. } => "batch --stream",
        _ => "this command variant",
    }
}

fn warm_unroutable_required_error(cmd: &CliCommand) -> MemdError {
    MemdError::ProtocolError(format!(
        "{} always runs on the cold path and cannot be routed through the warm worker; re-run with --warm auto for silent local fallback or --warm off",
        describe_unroutable(cmd)
    ))
}

fn warm_client_log_name(cmd: &CliCommand) -> &'static str {
    match cmd {
        CliCommand::Search { .. } => "memd_search",
        // Cold agent-context also logs under memd_search; keep warm parity.
        CliCommand::AgentContext { .. } => "memd_search",
        CliCommand::Report { .. } => "memd_report",
        // Future warm-routed commands get a neutral name instead of
        // being silently mislabeled as search logs.
        _ => "memd_cli",
    }
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
            // The warm wire protocol does not carry this flag; a
            // provenance lookup that opts into superseded chunks runs
            // on the cold path instead.
            include_superseded: false,
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
                tenant_id: super::scope::require_tenant(tenant_id.clone())?,
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
                tenant_id: super::scope::require_tenant(tenant_id.clone())?,
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
        CliCommand::Report {
            tenant_id,
            project_id,
            since,
            format,
            strict: _,
            top,
            output,
            warm: _,
        } => (
            WarmWireCommand::Report {
                tenant_id: tenant_id.clone(),
                project_id: project_id.clone(),
                since: since.clone(),
                top: *top,
                format: *format,
            },
            WarmLocalOutputs {
                output: output.clone(),
                log_dir: None,
            },
        ),
        CliCommand::Add {
            tenant_id,
            text,
            chunk_type,
            project_id,
            tags,
            source_uri,
            source_path,
            warm: _,
        } => (
            WarmWireCommand::Add {
                tenant_id: super::scope::require_tenant(tenant_id.clone())?,
                text: text.clone(),
                chunk_type: *chunk_type,
                project_id: project_id.clone(),
                tags: tags.clone(),
                source_uri: source_uri.clone(),
                source_path: source_path.clone(),
            },
            WarmLocalOutputs {
                output: None,
                log_dir: None,
            },
        ),
        CliCommand::Delete {
            tenant_id,
            chunk_id,
            warm: _,
        } => (
            WarmWireCommand::Delete {
                tenant_id: super::scope::require_tenant(tenant_id.clone())?,
                chunk_id: chunk_id.clone(),
            },
            WarmLocalOutputs {
                output: None,
                log_dir: None,
            },
        ),
        CliCommand::ImportOmf {
            tenant_id,
            input,
            include_archived,
            fuzzy_threshold,
            dry_run,
            warm: _,
        } => (
            WarmWireCommand::ImportOmf {
                tenant_id: super::scope::require_tenant(tenant_id.clone())?,
                document_json: super::paths::read_omf_input(input.as_deref())?,
                include_archived: *include_archived,
                fuzzy_threshold: *fuzzy_threshold,
                dry_run: *dry_run,
            },
            WarmLocalOutputs {
                output: None,
                log_dir: None,
            },
        ),
        CliCommand::Purge {
            tenant_id,
            project_id,
            older_than_days,
            limit,
            include_unreadable_active,
            archive,
            apply,
            vacuum_metadata,
            rewrite_segments,
            warm: _,
        } => (
            WarmWireCommand::Purge {
                tenant_id: tenant_id.clone(),
                project_id: project_id.clone(),
                older_than_days: *older_than_days,
                limit: *limit,
                include_unreadable_active: *include_unreadable_active,
                archive: archive.as_ref().map(|path| {
                    if path.is_absolute() {
                        path.clone()
                    } else {
                        super::paths::normalize_absolute(path)
                    }
                }),
                apply: *apply,
                vacuum_metadata: *vacuum_metadata,
                rewrite_segments: *rewrite_segments,
            },
            WarmLocalOutputs {
                output: None,
                log_dir: None,
            },
        ),
        CliCommand::Consolidate {
            tenant_id,
            project_id,
            project_dir,
            max_region,
            dry_run,
            background,
            force,
            warm: _,
        } => (
            WarmWireCommand::Consolidate {
                tenant_id: tenant_id.clone(),
                project_id: project_id.clone(),
                project_dir: super::paths::absolutize_project_dir(project_dir)?,
                max_region: *max_region,
                dry_run: *dry_run,
                background: *background,
                force: *force,
            },
            WarmLocalOutputs {
                output: None,
                log_dir: None,
            },
        ),
        CliCommand::Batch {
            jsonl,
            stream: false,
            continue_on_error,
            output,
            warm: _,
        } => (
            WarmWireCommand::Batch {
                jsonl_content: read_batch_input(jsonl.as_deref())?,
                continue_on_error: *continue_on_error,
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
                false,
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
        WarmWireCommand::Report {
            tenant_id,
            project_id,
            since,
            top,
            format,
        } => {
            let (rendered, warn_count) = cli_report_rendered(
                store,
                tenant_manager,
                ReportOptions {
                    tenant_id,
                    project_id,
                    since,
                    top,
                    format,
                    served_via_worker: true,
                },
            )
            .await?;
            Ok((rendered, Some(json!({ "report_warn_count": warn_count }))))
        }
        WarmWireCommand::Add {
            tenant_id,
            text,
            chunk_type,
            project_id,
            tags,
            source_uri,
            source_path,
        } => {
            let rendered = cli_add_rendered(
                store,
                tenant_manager,
                CliAddRenderOptions {
                    tenant_id,
                    text,
                    chunk_type,
                    project_id,
                    tags,
                    source_uri,
                    source_path,
                },
            )
            .await?;
            Ok((rendered, None))
        }
        WarmWireCommand::Delete {
            tenant_id,
            chunk_id,
        } => Ok((
            cli_delete_rendered(store, &tenant_id, &chunk_id).await?,
            None,
        )),
        WarmWireCommand::ImportOmf {
            tenant_id,
            document_json,
            include_archived,
            fuzzy_threshold,
            dry_run,
        } => Ok((
            cli_import_omf_rendered(
                store,
                tenant_manager,
                &tenant_id,
                &document_json,
                include_archived,
                fuzzy_threshold,
                dry_run,
            )
            .await?,
            None,
        )),
        WarmWireCommand::Purge {
            tenant_id,
            project_id,
            older_than_days,
            limit,
            include_unreadable_active,
            archive,
            apply,
            vacuum_metadata,
            rewrite_segments,
        } => {
            let result = run_purge(
                store,
                PurgeOptions {
                    tenant_id,
                    project_id,
                    older_than_days,
                    limit,
                    include_unreadable_active,
                    archive,
                    apply,
                    vacuum_metadata,
                    rewrite_segments,
                },
            )
            .await?;
            Ok((serde_json::to_string_pretty(&result)? + "\n", None))
        }
        WarmWireCommand::Consolidate {
            tenant_id,
            project_id,
            project_dir,
            max_region,
            dry_run,
            background,
            force,
        } => {
            let result = run_consolidate(
                store,
                ConsolidateOptions {
                    tenant_id,
                    project_id,
                    project_dir,
                    max_region,
                    dry_run,
                    background,
                    force,
                },
            )
            .await?;
            Ok((serde_json::to_string_pretty(&result)? + "\n", None))
        }
        WarmWireCommand::Batch {
            jsonl_content,
            continue_on_error,
        } => Ok((
            run_batch_jsonl(store, tenant_manager, &jsonl_content, continue_on_error).await?,
            None,
        )),
    }
}

async fn ensure_warm_worker(
    config: &WarmProcessConfig,
) -> std::result::Result<(), (MemdError, MemdError)> {
    match warm_ping(config).await {
        Ok(_) => Ok(()),
        Err(error) if warm_worker_needs_replacement(&error) => {
            match replace_incompatible_warm_worker(config).await {
                Ok(_) => match warm_ping(config).await {
                    Ok(_) => Ok(()),
                    Err(retry_error) => Err((error, retry_error)),
                },
                Err(start_error) => Err((error, start_error)),
            }
        }
        Err(error) => match warm_start(config).await {
            Ok(_) => Ok(()),
            Err(start_error) => Err((error, start_error)),
        },
    }
}

pub async fn try_run_warm_client(config: &WarmProcessConfig, cmd: &CliCommand) -> Result<bool> {
    let Some(mode) = cmd.warm_mode() else {
        return Ok(false);
    };
    if mode == WarmMode::Off {
        return Ok(false);
    }
    if !warm_routable(cmd) {
        if mode == WarmMode::Required {
            return Err(warm_unroutable_required_error(cmd));
        }
        return Ok(false);
    }

    if let Err((error, start_error)) = ensure_warm_worker(config).await {
        if mode == WarmMode::Auto {
            warn!(
                error = %error,
                start_error = %start_error,
                "warm worker unavailable; falling back to cold CLI"
            );
            return Ok(false);
        }
        return Err(MemdError::ProtocolError(format!(
            "warm worker required but unavailable: {error}; start failed: {start_error}"
        )));
    }

    let Some((wire_command, local_outputs)) = warm_wire_command_from_cli(cmd)? else {
        if mode == WarmMode::Required {
            return Err(warm_unroutable_required_error(cmd));
        }
        return Ok(false);
    };

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
        write_cli_log(
            local_outputs.log_dir.as_deref(),
            warm_client_log_name(cmd),
            payload,
        )?;
    }
    let output = response.output.unwrap_or_default();
    write_rendered(local_outputs.output.as_deref(), &output)?;
    if let CliCommand::Report { strict: true, .. } = cmd {
        let warns = response
            .log_payload
            .as_ref()
            .and_then(|payload| payload.get("report_warn_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if warns > 0 {
            std::process::exit(2);
        }
    }
    Ok(true)
}

pub async fn run_warm_admin(config: &WarmProcessConfig, command: WarmCommand) -> Result<()> {
    let mut payload = match &command {
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
    match command {
        WarmCommand::Status => {
            let legacy = ping_legacy_warm_workers(&config.data_dir).await;
            if !legacy.is_empty() {
                payload["legacy_workers"] = json!(legacy);
            }
        }
        WarmCommand::Stop => {
            let legacy_stopped = stop_legacy_warm_workers(&config.data_dir).await;
            if !legacy_stopped.is_empty() {
                payload["legacy_stopped"] = json!(legacy_stopped);
            }
        }
        WarmCommand::Start => {}
    }
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

fn warm_pid_from_file(config: &WarmProcessConfig) -> Option<u32> {
    let text = std::fs::read_to_string(warm_pid_path(config)).ok()?;
    text.lines().next()?.trim().parse::<u32>().ok()
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

async fn shutdown_existing_warm_worker_best_effort(config: &WarmProcessConfig) {
    let _ = warm_request(&warm_socket_path(config), &WarmWireRequest::Shutdown).await;
    let pid = warm_pid_from_file(config);
    let exited = wait_for_warm_pid_exit(pid, Duration::from_secs(10)).await;
    // Only unlink the socket once the worker process is confirmed gone. With no
    // pid file we cannot confirm it died, so leaving the socket avoids
    // unlinking a live worker's endpoint (the kernel flock still protects the
    // data dir, and the next `warm start` rebinds the path via temp+rename).
    if pid.is_some() && exited {
        let _ = std::fs::remove_file(warm_socket_path(config));
    }
}

fn warm_worker_needs_replacement(error: &MemdError) -> bool {
    matches!(error, MemdError::IncompatibleWarmWorker { .. })
}

async fn replace_incompatible_warm_worker(config: &WarmProcessConfig) -> Result<Value> {
    shutdown_existing_warm_worker_best_effort(config).await;
    warm_start(config).await
}

/// Socket paths left behind by pre-stable-path binaries. Old versions
/// hashed version/protocol/model/variant into the socket dir name, so
/// after an upgrade an old worker may still be listening — and holding
/// the writer flock — at another `<data_dir>/warm/<hash>/memd.sock`.
fn legacy_warm_sockets(data_dir: &Path) -> Vec<PathBuf> {
    let canonical = warm_socket_path_for_data_dir(data_dir);
    let Ok(entries) = std::fs::read_dir(data_dir.join("warm")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("memd.sock"))
        .filter(|socket| socket.exists() && *socket != canonical)
        .collect()
}

/// Best-effort ping of legacy-path workers for `warm status`. No
/// identity validation — these are expected to be old versions.
async fn ping_legacy_warm_workers(data_dir: &Path) -> Vec<Value> {
    let mut workers = Vec::new();
    for socket in legacy_warm_sockets(data_dir) {
        if let Ok(response) = warm_request(&socket, &WarmWireRequest::Ping).await {
            if response.ok {
                workers.push(json!({
                    "socket": socket,
                    "result": response.result,
                }));
            }
        }
    }
    workers
}

/// Best-effort shutdown of legacy-path workers for `warm stop`, so
/// upgrading past the stable-socket change does not strand the
/// previous worker (and its writer flock).
async fn stop_legacy_warm_workers(data_dir: &Path) -> Vec<Value> {
    let mut stopped = Vec::new();
    for socket in legacy_warm_sockets(data_dir) {
        let Ok(response) = warm_request(&socket, &WarmWireRequest::Shutdown).await else {
            continue;
        };
        if !response.ok {
            continue;
        }
        let result = response.result.unwrap_or(Value::Null);
        let pid = result
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok());
        let _ = wait_for_warm_pid_exit(pid, Duration::from_secs(10)).await;
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(warm_pid_path_for_socket(&socket));
        stopped.push(json!({ "socket": socket, "pid": pid }));
    }
    stopped
}

async fn warm_start(config: &WarmProcessConfig) -> Result<Value> {
    match warm_ping(config).await {
        Ok(result) => {
            return Ok(json!({
                "status": "already_running",
                "socket": warm_socket_path(config),
                "result": result,
            }));
        }
        Err(error) if warm_worker_needs_replacement(&error) => {
            shutdown_existing_warm_worker_best_effort(config).await;
        }
        Err(_) => {}
    }

    let socket = warm_socket_path(config);
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Stale sockets are replaced atomically by the next worker bind+rename; client unlink can orphan a live worker that just bound.

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

    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| MemdError::ValidationError(format!("system time before epoch: {e}")))?
        .as_millis();
    for _ in 0..300 {
        match warm_ping(config).await {
            Ok(result) => {
                let serving_pid = result
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
                    .unwrap_or(pid);
                return Ok(json!({
                    "status": "started",
                    "pid": serving_pid,
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

/// Diagnostic ping for `memd doctor`: returns the worker's identity
/// WITHOUT validating it, so doctor can report version skew instead of
/// erroring on it.
pub(super) async fn warm_ping_identity(data_dir: &Path) -> Result<Value> {
    let socket = warm_socket_path_for_data_dir(data_dir);
    let response = warm_request(&socket, &WarmWireRequest::Ping).await?;
    if !response.ok {
        return Err(MemdError::ProtocolError(
            response
                .error
                .unwrap_or_else(|| "warm worker ping failed".to_string()),
        ));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn warm_worker_identity(socket: &Path, ryw_probe_stats: Option<RywProbeStats>) -> Value {
    let mut identity = json!({
        "pid": std::process::id(),
        "socket": socket,
        "memd_version": env!("CARGO_PKG_VERSION"),
        "warm_wire_protocol": WARM_WIRE_PROTOCOL,
    });
    if let Some(stats) = ryw_probe_stats {
        identity["ryw_probe"] = json!({
            "checks": stats.checks,
            "external_detected": stats.external_detected,
            "repairs": stats.repairs,
        });
    }
    identity
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
        return Err(MemdError::IncompatibleWarmWorker {
            worker_version: worker_version.to_string(),
            worker_protocol: worker_protocol.to_string(),
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            cli_protocol: WARM_WIRE_PROTOCOL.to_string(),
        });
    }
    Ok(())
}

#[cfg(unix)]
/// Client-side timeout for a single warm-worker request. A wedged worker (e.g.
/// one blocked on the SQLite busy_timeout) must not hang the CLI indefinitely.
/// On timeout the request FAILS and the error propagates (there is no automatic
/// cold-path fallback); the worker is often not wedged but busy repairing
/// indexes — see the timeout message below.
fn warm_client_timeout() -> Duration {
    std::env::var("MEMD_WARM_CLIENT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(30))
}

/// Defense-in-depth peer-credential check for the worker socket: only accept
/// connections from the same uid that owns the worker. The 0700 data dir and
/// 0600 socket are the primary boundary; this rejects a same-host process under
/// a different uid that somehow reached the socket. A failed credential probe
/// does not hard-fail (the perms still apply), so the check never blocks a
/// legitimate same-uid client.
#[cfg(unix)]
fn peer_uid_allowed(stream: &tokio::net::UnixStream) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let fd = stream.as_raw_fd();
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `fd` is a valid connected socket owned by `stream`;
        // getsockopt writes at most `len` bytes into `cred`.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return true;
        }
        // SAFETY: geteuid is always safe and takes no arguments.
        cred.uid == unsafe { libc::geteuid() }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SO_PEERCRED is Linux-only; the 0700/0600 perms are the boundary
        // elsewhere. Allow (the perms still apply).
        let _ = stream;
        true
    }
}

#[cfg(unix)]
async fn warm_request(socket: &Path, request: &WarmWireRequest) -> Result<WarmWireResponse> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let timeout = warm_client_timeout();
    let exchange = async {
        let mut stream = UnixStream::connect(socket).await?;
        let body = serde_json::to_vec(request)?;
        stream.write_all(&body).await?;
        stream.shutdown().await?;

        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await?;
        Ok::<WarmWireResponse, MemdError>(serde_json::from_slice(&bytes)?)
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(result) => result,
        Err(_elapsed) => Err(MemdError::ProtocolError(format!(
            "warm worker did not respond within {} ms; it may be busy repairing indexes \
             (check the worker log via `memd warm status`). Raise MEMD_WARM_CLIENT_TIMEOUT_MS, \
             retry with `--warm off`, or stop it with `memd warm stop`.",
            timeout.as_millis()
        ))),
    }
}

#[cfg(not(unix))]
async fn warm_request(_socket: &Path, _request: &WarmWireRequest) -> Result<WarmWireResponse> {
    Err(MemdError::ProtocolError(
        "warm worker requires Unix domain sockets".to_string(),
    ))
}

/// Default quiet period before an idle warm worker exits and releases
/// the writer flock.
const DEFAULT_WARM_IDLE_TIMEOUT_SECS: u64 = 1800;

fn warm_idle_timeout_from_env() -> Option<Duration> {
    parse_idle_timeout_secs(std::env::var("MEMD_WARM_IDLE_TIMEOUT_SECS").ok().as_deref())
}

/// `None` disables the timeout (value `0`); missing or unparseable
/// values fall back to the default.
fn parse_idle_timeout_secs(value: Option<&str>) -> Option<Duration> {
    let secs = value
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_WARM_IDLE_TIMEOUT_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Total future for the idle-timeout select branch: the expression is
/// evaluated even when the branch guard is false, so the disabled case
/// must be a future that never resolves.
async fn warm_idle_sleep(timeout: Option<Duration>, last_activity: Instant) {
    match timeout {
        Some(timeout) => {
            tokio::time::sleep_until(tokio::time::Instant::from_std(last_activity + timeout)).await
        }
        None => std::future::pending().await,
    }
}

#[cfg(unix)]
pub(super) async fn run_warm_worker<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    socket: &Path,
) -> Result<()> {
    use futures_util::stream::{FuturesUnordered, StreamExt};
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;
    use tokio::sync::Semaphore;

    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
        // The socket dir is private to the owning user; this also tightens
        // pre-existing dirs created by older versions with umask defaults.
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let pid = std::process::id();
    remove_stale_warm_socket_temps(socket);
    let tmp_socket = warm_temp_socket_path(socket, pid);
    let _ = std::fs::remove_file(&tmp_socket);
    let listener = UnixListener::bind(&tmp_socket)?;
    // Chmod the temp name before the atomic rename publishes it, so the
    // socket is never reachable at the published path with permissive bits.
    // rename(2) preserves the mode.
    std::fs::set_permissions(&tmp_socket, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp_socket, socket)?;
    let pid_path = warm_pid_path_for_socket(socket);
    let pid_tmp_path = socket.with_file_name(format!("memd.pid.tmp-{pid}"));
    std::fs::write(&pid_tmp_path, format!("{pid}\n"))?;
    std::fs::rename(&pid_tmp_path, &pid_path)?;
    info!(socket = %socket.display(), "memd warm worker listening");

    const MAX_INFLIGHT_CONNECTIONS: usize = 16;
    const MAX_CONCURRENT_COMMANDS: usize = 4;

    let semaphore = Semaphore::new(MAX_CONCURRENT_COMMANDS);
    // Keep connection futures in this task instead of tokio::spawn:
    // execute_warm_wire_command is currently !Send because ops/mod.rs
    // holds MutexGuards across await points.
    let mut inflight: FuturesUnordered<Pin<Box<dyn Future<Output = bool> + '_>>> =
        FuturesUnordered::new();
    let mut shutting_down = false;
    let idle_timeout = warm_idle_timeout_from_env();
    let mut last_activity = Instant::now();

    loop {
        if shutting_down && inflight.is_empty() {
            break;
        }

        tokio::select! {
            accepted = listener.accept(), if !shutting_down && inflight.len() < MAX_INFLIGHT_CONNECTIONS => {
                last_activity = Instant::now();
                match accepted {
                    Ok((stream, _)) if !peer_uid_allowed(&stream) => {
                        warn!("warm worker rejected a connection from a different uid");
                    }
                    Ok((stream, _)) => {
                        inflight.push(Box::pin(handle_warm_connection(
                            store,
                            tenant_manager,
                            socket,
                            &semaphore,
                            stream,
                        )));
                    }
                    Err(error) => {
                        warn!(error = %error, "warm worker accept failed");
                    }
                }
            }

            completed = inflight.next(), if !inflight.is_empty() => {
                last_activity = Instant::now();
                if let Some(shutdown_requested) = completed {
                    if shutdown_requested {
                        shutting_down = true;
                    }
                }
            }

            // Orphaned workers must not hold the writer flock forever:
            // exit cleanly after a quiet period so the lock releases
            // even when nobody runs `memd warm stop`.
            _ = warm_idle_sleep(idle_timeout, last_activity), if idle_timeout.is_some() && inflight.is_empty() => {
                info!("warm worker idle timeout reached; exiting");
                break;
            }
        }
    }

    let _ = std::fs::remove_file(socket);
    let _ = std::fs::remove_file(warm_pid_path_for_socket(socket));
    Ok(())
}

#[cfg(unix)]
async fn handle_warm_connection<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    socket: &Path,
    semaphore: &tokio::sync::Semaphore,
    mut stream: tokio::net::UnixStream,
) -> bool {
    use tokio::io::AsyncReadExt;

    let mut bytes = Vec::new();
    if let Err(error) = stream.read_to_end(&mut bytes).await {
        warn!(error = %error, "failed to read warm request");
        return false;
    }

    let mut shutdown = false;
    let response = match serde_json::from_slice::<WarmWireRequest>(&bytes) {
        Ok(WarmWireRequest::Ping) => {
            WarmWireResponse::ok_result(warm_worker_identity(socket, store.ryw_probe_stats()))
        }
        Ok(WarmWireRequest::Shutdown) => {
            shutdown = true;
            WarmWireResponse::ok_result(warm_worker_identity(socket, store.ryw_probe_stats()))
        }
        Ok(WarmWireRequest::Command { command }) => {
            let permit = match semaphore.acquire().await {
                Ok(permit) => permit,
                Err(error) => {
                    let _ = write_warm_response(
                        &mut stream,
                        WarmWireResponse::error(format!(
                            "warm worker command semaphore closed: {error}"
                        )),
                    )
                    .await;
                    return false;
                }
            };
            let _ = store.probe_external_mutation().await;
            let response = match execute_warm_wire_command(store, tenant_manager, command).await {
                Ok((output, log_payload)) => WarmWireResponse::ok_output(output, log_payload),
                Err(error) => WarmWireResponse::error(error),
            };
            drop(permit);
            response
        }
        Err(error) => WarmWireResponse::error(format!("invalid warm request: {error}")),
    };

    let _ = write_warm_response(&mut stream, response).await;
    shutdown
}

#[cfg(unix)]
async fn write_warm_response(
    stream: &mut tokio::net::UnixStream,
    response: WarmWireResponse,
) -> bool {
    use tokio::io::AsyncWriteExt;

    let body = match serde_json::to_vec(&response) {
        Ok(body) => body,
        Err(error) => {
            warn!(error = %error, "failed to serialize warm response");
            return false;
        }
    };
    if let Err(error) = stream.write_all(&body).await {
        warn!(error = %error, "failed to write warm response");
        return false;
    }
    if let Err(error) = stream.shutdown().await {
        warn!(error = %error, "failed to shutdown warm response stream");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::SearchReranker;
    use tempfile::tempdir;

    fn test_warm_config(data_dir: PathBuf) -> WarmProcessConfig {
        WarmProcessConfig {
            data_dir,
            config_path: None,
            embedding_model: "all-minilm".to_string(),
            search_variant: "hybrid-feature".to_string(),
        }
    }

    #[test]
    fn parse_idle_timeout_defaults_and_disables() {
        assert_eq!(
            parse_idle_timeout_secs(None),
            Some(Duration::from_secs(DEFAULT_WARM_IDLE_TIMEOUT_SECS))
        );
        assert_eq!(parse_idle_timeout_secs(Some("0")), None);
        assert_eq!(
            parse_idle_timeout_secs(Some("garbage")),
            Some(Duration::from_secs(DEFAULT_WARM_IDLE_TIMEOUT_SECS))
        );
        assert_eq!(
            parse_idle_timeout_secs(Some(" 42 ")),
            Some(Duration::from_secs(42))
        );
    }

    #[test]
    fn legacy_warm_sockets_excludes_canonical_dir() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let canonical = warm_socket_path_for_data_dir(&data_dir);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        std::fs::write(&canonical, b"").unwrap();
        let legacy = data_dir
            .join("warm")
            .join("legacyhash16char")
            .join("memd.sock");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"").unwrap();

        let found = legacy_warm_sockets(&data_dir);
        assert_eq!(found, vec![legacy]);
    }

    fn search_command(include_superseded: bool, warm: WarmMode) -> CliCommand {
        CliCommand::Search {
            tenant_id: Some("t".to_string()),
            query: "q".to_string(),
            k: 1,
            project_id: None,
            compact: false,
            token_budget: None,
            mode: CliQueryMode::Generic,
            no_text: false,
            include_artifact: false,
            include_superseded,
            format: ExportFormat::Json,
            output: None,
            reranker: SearchReranker::None,
            reranker_model: "model".to_string(),
            reranker_device: "cpu".to_string(),
            reranker_batch_size: 1,
            reranker_timeout_seconds: 1,
            reranker_python: "python3".to_string(),
            warm,
        }
    }

    fn batch_command(stream: bool, warm: WarmMode) -> CliCommand {
        CliCommand::Batch {
            jsonl: None,
            stream,
            continue_on_error: false,
            output: None,
            warm,
        }
    }

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
        let payload = warm_worker_identity(Path::new("/tmp/memd.sock"), None);

        validate_warm_worker_identity(&payload).unwrap();
    }

    #[test]
    fn warm_worker_identity_includes_probe_stats_when_available() {
        let payload = warm_worker_identity(
            Path::new("/tmp/memd.sock"),
            Some(crate::store::RywProbeStats {
                checks: 3,
                external_detected: 1,
                repairs: 1,
            }),
        );

        assert_eq!(
            payload.pointer("/ryw_probe/checks").and_then(Value::as_u64),
            Some(3)
        );
        assert_eq!(
            payload
                .pointer("/ryw_probe/external_detected")
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            payload
                .pointer("/ryw_probe/repairs")
                .and_then(Value::as_u64),
            Some(1)
        );
        validate_warm_worker_identity(&payload).unwrap();
    }

    #[test]
    fn incompatible_identity_is_typed_and_needs_replacement() {
        let payload = json!({
            "pid": 1234,
            "socket": "/tmp/memd.sock",
            "memd_version": "0.0.1",
            "warm_wire_protocol": "1",
        });

        let err = validate_warm_worker_identity(&payload).unwrap_err();
        assert!(matches!(
            &err,
            MemdError::IncompatibleWarmWorker {
                worker_version,
                worker_protocol,
                ..
            } if worker_version == "0.0.1" && worker_protocol == "1"
        ));
        assert!(warm_worker_needs_replacement(&err));
    }

    #[test]
    fn warm_worker_replacement_predicate_rejects_other_errors() {
        assert!(!warm_worker_needs_replacement(&MemdError::ProtocolError(
            "x".to_string()
        )));
        assert!(!warm_worker_needs_replacement(&MemdError::IoError(
            std::io::Error::other("x")
        )));
    }

    #[test]
    fn warm_wire_request_command_variants_round_trip_through_json() {
        let reranker = SearchRerankerOptions {
            reranker: SearchReranker::None,
            model: "model".to_string(),
            device: "cpu".to_string(),
            batch_size: 1,
            timeout_seconds: 1,
            python: "python3".to_string(),
        };
        let commands = vec![
            WarmWireCommand::Search {
                tenant_id: "t".to_string(),
                query: "q".to_string(),
                k: 3,
                project_id: Some("p".to_string()),
                compact: true,
                token_budget: Some(100),
                mode: CliQueryMode::Generic,
                no_text: false,
                include_artifact: true,
                format: ExportFormat::Json,
                reranker: reranker.clone(),
            },
            WarmWireCommand::AgentContext {
                tenant_id: "t".to_string(),
                project_id: Some("p".to_string()),
                query: vec!["q".to_string()],
                k: 2,
                token_budget: 700,
                mode: CliQueryMode::FindDecisions,
                no_text: true,
                include_artifact: false,
                format: ExportFormat::Markdown,
            },
            WarmWireCommand::Report {
                tenant_id: Some("t".to_string()),
                project_id: Some("p".to_string()),
                since: "24h".to_string(),
                top: 5,
                format: ReportFormat::Json,
            },
            WarmWireCommand::Call {
                tool: "memory.search".to_string(),
                arguments: json!({"query": "q"}),
            },
            WarmWireCommand::Add {
                tenant_id: "t".to_string(),
                text: "hello".to_string(),
                chunk_type: ChunkType::Summary,
                project_id: Some("p".to_string()),
                tags: Some(vec!["kind:note".to_string()]),
                source_uri: Some("memd://source".to_string()),
                source_path: Some("notes.md".to_string()),
            },
            WarmWireCommand::Delete {
                tenant_id: "t".to_string(),
                chunk_id: "019e6d12-c1a7-7330-8bd8-4c9cdb45bc3c".to_string(),
            },
            WarmWireCommand::ImportOmf {
                tenant_id: "t".to_string(),
                document_json: "{}".to_string(),
                include_archived: true,
                fuzzy_threshold: Some(0.8),
                dry_run: true,
            },
            WarmWireCommand::Purge {
                tenant_id: "t".to_string(),
                project_id: Some("p".to_string()),
                older_than_days: 30,
                limit: 100,
                include_unreadable_active: true,
                archive: Some(PathBuf::from("archive.json")),
                apply: false,
                vacuum_metadata: false,
                rewrite_segments: true,
            },
            WarmWireCommand::Consolidate {
                tenant_id: Some("t".to_string()),
                project_id: Some("p".to_string()),
                project_dir: PathBuf::from("/tmp/project"),
                max_region: 50,
                dry_run: true,
                background: false,
                force: true,
            },
            WarmWireCommand::Batch {
                jsonl_content: "{\"tool\":\"memory.stats\"}\n".to_string(),
                continue_on_error: true,
            },
        ];

        for command in commands {
            let request = WarmWireRequest::Command { command };
            let encoded = serde_json::to_string(&request).unwrap();
            let decoded: WarmWireRequest = serde_json::from_str(&encoded).unwrap();
            assert_eq!(
                serde_json::to_value(&decoded).unwrap(),
                serde_json::to_value(&request).unwrap()
            );
        }
    }

    #[test]
    fn warm_routable_agrees_with_wire_mapping_for_representative_commands() {
        let add = CliCommand::Add {
            tenant_id: Some("t".to_string()),
            text: "useful durable note".to_string(),
            chunk_type: ChunkType::Summary,
            project_id: None,
            tags: Some(vec!["kind:note".to_string()]),
            source_uri: None,
            source_path: None,
            warm: WarmMode::Auto,
        };
        assert_eq!(
            warm_routable(&add),
            warm_wire_command_from_cli(&add).unwrap().is_some()
        );

        let report = CliCommand::Report {
            tenant_id: Some("t".to_string()),
            project_id: Some("p".to_string()),
            since: "24h".to_string(),
            format: ReportFormat::Json,
            strict: true,
            top: 5,
            output: None,
            warm: WarmMode::Auto,
        };
        assert_eq!(
            warm_routable(&report),
            warm_wire_command_from_cli(&report).unwrap().is_some()
        );

        let batch_stream = CliCommand::Batch {
            jsonl: None,
            stream: true,
            continue_on_error: false,
            output: None,
            warm: WarmMode::Auto,
        };
        assert_eq!(
            warm_routable(&batch_stream),
            warm_wire_command_from_cli(&batch_stream).unwrap().is_some()
        );

        let superseded_search = CliCommand::Search {
            tenant_id: Some("t".to_string()),
            query: "q".to_string(),
            k: 1,
            project_id: None,
            compact: false,
            token_budget: None,
            mode: CliQueryMode::Generic,
            no_text: false,
            include_artifact: false,
            include_superseded: true,
            format: ExportFormat::Json,
            output: None,
            reranker: SearchReranker::None,
            reranker_model: "model".to_string(),
            reranker_device: "cpu".to_string(),
            reranker_batch_size: 1,
            reranker_timeout_seconds: 1,
            reranker_python: "python3".to_string(),
            warm: WarmMode::Auto,
        };
        assert_eq!(
            warm_routable(&superseded_search),
            warm_wire_command_from_cli(&superseded_search)
                .unwrap()
                .is_some()
        );

        let get = CliCommand::Get {
            tenant_id: Some("t".to_string()),
            chunk_id: "019e6d12-c1a7-7330-8bd8-4c9cdb45bc3c".to_string(),
        };
        assert!(get.warm_mode().is_none());
        assert_eq!(
            warm_routable(&get),
            warm_wire_command_from_cli(&get).unwrap().is_some()
        );

        let stdin_import = CliCommand::ImportOmf {
            tenant_id: Some("t".to_string()),
            input: None,
            include_archived: true,
            fuzzy_threshold: None,
            dry_run: true,
            warm: WarmMode::Auto,
        };
        assert!(warm_routable(&stdin_import));
    }

    #[tokio::test]
    async fn required_warm_rejects_search_include_superseded_before_worker_start() {
        let dir = tempdir().unwrap();
        let config = test_warm_config(dir.path().join("data"));
        let cmd = search_command(true, WarmMode::Required);

        let err = try_run_warm_client(&config, &cmd).await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("include-superseded"));
        assert!(message.contains("--warm"));
    }

    #[tokio::test]
    async fn required_warm_rejects_streaming_batch_before_worker_start() {
        let dir = tempdir().unwrap();
        let config = test_warm_config(dir.path().join("data"));
        let cmd = batch_command(true, WarmMode::Required);

        let err = try_run_warm_client(&config, &cmd).await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("stream"));
        assert!(message.contains("--warm"));
    }

    #[tokio::test]
    async fn auto_warm_preserves_local_fallback_for_unroutable_variants() {
        let dir = tempdir().unwrap();
        let config = test_warm_config(dir.path().join("data"));

        assert!(
            !try_run_warm_client(&config, &search_command(true, WarmMode::Auto))
                .await
                .unwrap()
        );
        assert!(
            !try_run_warm_client(&config, &batch_command(true, WarmMode::Auto))
                .await
                .unwrap()
        );
    }

    #[test]
    fn warm_client_log_name_tracks_command_kind() {
        let search = search_command(false, WarmMode::Auto);
        assert_eq!(warm_client_log_name(&search), "memd_search");

        let agent_context = CliCommand::AgentContext {
            tenant_id: Some("t".to_string()),
            project_id: None,
            query: vec!["q".to_string()],
            k: 1,
            token_budget: 100,
            mode: CliQueryMode::Generic,
            no_text: false,
            include_artifact: false,
            format: ExportFormat::Json,
            output: None,
            log_dir: None,
            warm: WarmMode::Auto,
        };
        assert_eq!(warm_client_log_name(&agent_context), "memd_search");

        let report = CliCommand::Report {
            tenant_id: Some("t".to_string()),
            project_id: Some("p".to_string()),
            since: "24h".to_string(),
            format: ReportFormat::Json,
            strict: true,
            top: 5,
            output: None,
            warm: WarmMode::Auto,
        };
        assert_eq!(warm_client_log_name(&report), "memd_report");

        let add = CliCommand::Add {
            tenant_id: Some("t".to_string()),
            text: "useful durable note".to_string(),
            chunk_type: ChunkType::Summary,
            project_id: None,
            tags: Some(vec!["kind:note".to_string()]),
            source_uri: None,
            source_path: None,
            warm: WarmMode::Auto,
        };
        assert_eq!(warm_client_log_name(&add), "memd_cli");
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
