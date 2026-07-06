use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::error::{MemdError, Result};
use crate::store::{RywProbeStats, Store, TenantManager};
use crate::types::ChunkType;

use super::args::{
    CliCommand, CliQueryMode, ExportFormat, ReportFormat, SearchRerankerOptions, StoreAccess,
    WarmCommand, WarmMode, WarmProcessConfig,
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
        /// Wire-compatible default: older workers/clients omit the field.
        #[serde(default)]
        dedupe_by_source: bool,
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
    /// The command failed only because the store's dense index was busy
    /// (repair/bulk insert holding its lock). Clients treat this like a
    /// momentarily unavailable worker: reads fall back to the cold path
    /// immediately instead of surfacing an error. `default` keeps the wire
    /// compatible with workers and clients that predate the field.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    busy: bool,
}

impl WarmWireResponse {
    fn ok_result(result: Value) -> Self {
        Self {
            ok: true,
            output: None,
            log_payload: None,
            result: Some(result),
            error: None,
            busy: false,
        }
    }

    fn ok_output(output: String, log_payload: Option<Value>) -> Self {
        Self {
            ok: true,
            output: Some(output),
            log_payload,
            result: None,
            error: None,
            busy: false,
        }
    }

    fn error(error: impl ToString) -> Self {
        Self {
            ok: false,
            output: None,
            log_payload: None,
            result: None,
            error: Some(error.to_string()),
            busy: false,
        }
    }

    /// Failure response for a command that hit a busy dense index.
    fn busy_error(error: impl ToString) -> Self {
        Self {
            busy: true,
            ..Self::error(error)
        }
    }

    /// Classify a command failure: busy-marked errors become busy replies.
    fn for_command_error(error: &MemdError) -> Self {
        let message = error.to_string();
        if MemdError::message_indicates_index_busy(&message) {
            Self::busy_error(message)
        } else {
            Self::error(message)
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
            dedupe_by_source,
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
                dedupe_by_source: *dedupe_by_source,
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
            dedupe_by_source,
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
                dedupe_by_source,
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

    // Don't spin up a resident worker for an ephemeral (per-test / per-run)
    // data dir under Auto: one worker per dir is the OOM vector, and the cold
    // path is correct there. `required` still honors; opt in with
    // MEMD_WARM_ALLOW_EPHEMERAL=1.
    if mode == WarmMode::Auto && is_ephemeral_data_dir(&config.data_dir) {
        debug!(
            data_dir = %config.data_dir.display(),
            "skipping auto-warm for ephemeral data dir"
        );
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

    let response = match warm_request(
        &warm_socket_path(config),
        &WarmWireRequest::Command {
            command: wire_command,
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) if mode == WarmMode::Auto && cmd.store_access() == StoreAccess::ReadOnly => {
            warn!(
                error = %error,
                "warm worker request failed; falling back to cold CLI"
            );
            return Ok(false);
        }
        Err(error) if mode == WarmMode::Auto => {
            return Err(MemdError::ProtocolError(format!(
                "warm worker request failed after dispatch for a write command; not falling back \
                 to the cold path because the command may still complete in the worker: {error}"
            )));
        }
        Err(error) => return Err(error),
    };
    // A busy reply means the worker is healthy but a repair/bulk insert
    // holds its dense index lock. Auto-mode reads fall back to the cold
    // path immediately — it opens the last persisted state read-only and
    // shares no in-process lock with the worker. Note the fallback is not
    // read-your-writes-preserving: the persisted snapshot may predate a
    // just-acked add (best-effort, matches degraded-mode semantics).
    // Writes (and required mode) surface the busy error verbatim; it
    // already advises retrying.
    if !response.ok && response.busy {
        if mode == WarmMode::Auto && cmd.store_access() == StoreAccess::ReadOnly {
            warn!("warm worker busy (index repair in flight); falling back to cold read path");
            return Ok(false);
        }
        return Err(MemdError::ProtocolError(response.error.unwrap_or_else(
            || "warm worker busy: dense index repair in flight; retry shortly".to_string(),
        )));
    }
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

    // Backstop ceiling on resident warm workers so a misconfig — notably a
    // disabled idle timeout — cannot accumulate workers until the host OOMs
    // (2026-06-16: 639 workers / 215 GiB). Best-effort: counts live workers
    // under the shared temp-fallback root + THIS data dir's root (not every
    // data dir on the host), and count→spawn is not atomic, so concurrent
    // spawns for distinct dirs may briefly overshoot. That is bounded — the
    // real runaway vector (many ephemeral / long-path dirs → the shared temp
    // root) is fully counted. On refusal the caller falls back to cold (Auto)
    // or surfaces the error (Required).
    let live = count_live_warm_workers(&config.data_dir);
    let cap = warm_max_workers_from_env();
    if live >= cap {
        warn!(
            live_workers = live,
            cap, "warm worker cap reached; refusing to spawn (cold fallback)"
        );
        return Err(MemdError::ProtocolError(format!(
            "warm worker cap reached ({live}/{cap} live under the shared temp root \
             + this data dir); refusing to spawn. Raise MEMD_WARM_MAX_WORKERS or \
             reap leaked workers (see `memd warm status`)."
        )));
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
            "repair_in_progress": stats.repair_in_progress,
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
/// one blocked on the SQLite busy_timeout) must not hang the CLI indefinitely;
/// on timeout the request fails and the command dispatcher decides whether a
/// cold fallback is safe.
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
             (check the worker log via `memd warm status`). Raise MEMD_WARM_CLIENT_TIMEOUT_MS \
             or stop it with `memd warm stop`.",
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

/// Default ceiling on concurrent warm workers, counted under the shared
/// temp-fallback socket root plus the current data dir's own socket root.
/// Each worker pins the embedding model in RAM (~400MB on CPU); this bounds
/// the runaway-accumulation vector (many ephemeral / long-path data dirs all
/// falling back to the shared temp root) so no env/config — notably a
/// disabled idle timeout — can OOM the host. See 2026-06-16: 639 workers /
/// 215 GiB.
const DEFAULT_WARM_MAX_WORKERS: usize = 16;

fn warm_max_workers_from_env() -> usize {
    parse_max_workers(std::env::var("MEMD_WARM_MAX_WORKERS").ok().as_deref())
}

/// Absent, unparseable, or `0` → default. Unlike the idle timeout, the cap
/// cannot be disabled: a disabled cap is exactly the footgun this guards.
fn parse_max_workers(value: Option<&str>) -> usize {
    value
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_WARM_MAX_WORKERS)
}

/// Count live warm workers across the roots where runaway accumulation
/// concentrates: the shared temp-fallback root (used by long-path / ephemeral
/// data dirs — the incident vector) and this data dir's own socket root.
/// Reuses the per-worker `memd.pid` files and is strictly read-only, so it
/// never races the orphan-eviction ownership check.
fn count_live_warm_workers(data_dir: &Path) -> usize {
    let temp_root = std::env::temp_dir().join("memd-warm");
    let data_root = data_dir.join("warm");
    let mut total = count_live_workers_under(&temp_root);
    if data_root != temp_root {
        total += count_live_workers_under(&data_root);
    }
    total
}

fn count_live_workers_under(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            let pid = std::fs::read_to_string(entry.path().join("memd.pid"))
                .ok()
                .and_then(|text| text.lines().next()?.trim().parse::<u32>().ok());
            pid.is_some_and(warm_pid_is_live_memd)
        })
        .count()
}

/// Stricter than `warm_pid_is_running` (bare `/proc/<pid>` existence): confirm
/// the pid is a live `memd` process, so a leaked pid file whose number was
/// recycled by an unrelated program doesn't inflate the worker count and trip
/// the cap prematurely.
fn warm_pid_is_live_memd(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|comm| comm.trim_end().starts_with("memd"))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

/// A worker owns its published socket iff the published pid file still names
/// it. After a bind+rename race the loser is orphaned (alive, holding the
/// model, unreachable); this is checked independently of the idle timeout so
/// orphans are reaped even when the idle reaper is disabled.
fn warm_worker_still_owns(socket: &Path, my_pid: u32) -> bool {
    std::fs::read_to_string(warm_pid_path_for_socket(socket))
        .ok()
        .and_then(|text| text.lines().next()?.trim().parse::<u32>().ok())
        == Some(my_pid)
}

/// Heuristic for data dirs created per test / per research run: auto-warming
/// them spawns a resident worker per dir, the 2026-06-16 OOM vector. Keyed on
/// the `pytest-of-` sandbox marker so it targets pytest-driven callers without
/// disabling warm for memd's own `tempfile` tests or users who legitimately
/// keep a store under the temp dir (those stay bounded by the worker cap).
/// `--warm required` always honors; `MEMD_WARM_ALLOW_EPHEMERAL=1` opts back in.
fn is_ephemeral_data_dir(data_dir: &Path) -> bool {
    if std::env::var_os("MEMD_WARM_ALLOW_EPHEMERAL").is_some() {
        return false;
    }
    data_dir.to_string_lossy().contains("pytest-of-")
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

    // Independent of the idle timeout: a worker whose published pid file no
    // longer names it has been replaced (bind+rename race) and must exit, so
    // orphans don't accumulate even when the idle reaper is disabled
    // (MEMD_WARM_IDLE_TIMEOUT_SECS=0). Polled in the serve loop, so it reaps an
    // *idle* orphan within ~60s; it doesn't preempt a worker stuck in a long
    // command (bounded by the client timeout), and the worker cap bounds total
    // workers regardless.
    let mut ownership_check = tokio::time::interval(Duration::from_secs(60));
    ownership_check.tick().await; // first tick is immediate; consume it

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

            // Idle reaper: an otherwise-idle worker exits after the quiet
            // period to free the model + writer flock. Disabled by
            // MEMD_WARM_IDLE_TIMEOUT_SECS=0; orphan reaping does NOT depend on
            // it (see the ownership branch below), so disabling idle cannot
            // strand orphaned workers.
            _ = warm_idle_sleep(idle_timeout, last_activity), if idle_timeout.is_some() && inflight.is_empty() => {
                info!("warm worker idle timeout reached; exiting");
                break;
            }

            _ = ownership_check.tick(), if !shutting_down => {
                if !warm_worker_still_owns(socket, pid) {
                    info!("warm worker replaced by a newer instance; exiting");
                    shutting_down = true;
                }
            }
        }
    }

    // Only remove the published socket/pid if they still name us: an orphaned
    // worker (replaced by a newer instance) must not delete the new owner's
    // files.
    if warm_worker_still_owns(socket, pid) {
        let _ = std::fs::remove_file(socket);
        let _ = std::fs::remove_file(warm_pid_path_for_socket(socket));
    }
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
                Err(error) => WarmWireResponse::for_command_error(&error),
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
    use std::sync::Mutex;
    use tempfile::tempdir;

    static WARM_TIMEOUT_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn test_warm_config(data_dir: PathBuf) -> WarmProcessConfig {
        WarmProcessConfig {
            data_dir,
            config_path: None,
            embedding_model: "all-minilm".to_string(),
            search_variant: "hybrid-feature".to_string(),
        }
    }

    fn add_command(warm: WarmMode) -> CliCommand {
        CliCommand::Add {
            tenant_id: Some("t".to_string()),
            text: "useful durable note".to_string(),
            chunk_type: ChunkType::Summary,
            project_id: None,
            tags: Some(vec!["kind:note".to_string()]),
            source_uri: None,
            source_path: None,
            warm,
        }
    }

    #[cfg(unix)]
    async fn serve_ping_then_hang_on_command(socket: PathBuf) {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();

        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            match serde_json::from_slice::<WarmWireRequest>(&bytes).unwrap() {
                WarmWireRequest::Ping => {
                    let response = WarmWireResponse::ok_result(warm_worker_identity(&socket, None));
                    assert!(write_warm_response(&mut stream, response).await);
                }
                WarmWireRequest::Command { .. } => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                other => panic!("unexpected warm request in test: {other:?}"),
            }
        }
    }

    #[cfg(unix)]
    async fn serve_ping_then_busy_on_command(socket: PathBuf) {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();

        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            match serde_json::from_slice::<WarmWireRequest>(&bytes).unwrap() {
                WarmWireRequest::Ping => {
                    let response = WarmWireResponse::ok_result(warm_worker_identity(&socket, None));
                    assert!(write_warm_response(&mut stream, response).await);
                }
                WarmWireRequest::Command { .. } => {
                    let response = WarmWireResponse::for_command_error(&MemdError::IndexBusy {
                        reason: "test repair holds the index lock".to_string(),
                    });
                    assert!(write_warm_response(&mut stream, response).await);
                }
                other => panic!("unexpected warm request in test: {other:?}"),
            }
        }
    }

    #[test]
    fn warm_wire_response_busy_field_is_wire_compatible() {
        // Responses from workers that predate the field deserialize busy=false.
        let legacy: WarmWireResponse =
            serde_json::from_str(r#"{"ok":false,"error":"boom"}"#).unwrap();
        assert!(!legacy.busy);

        // Busy replies round-trip, and non-busy errors omit the field so old
        // clients see the exact shape they always did.
        let busy_json = serde_json::to_string(&WarmWireResponse::busy_error("busy")).unwrap();
        assert!(busy_json.contains("\"busy\":true"));
        let plain_json = serde_json::to_string(&WarmWireResponse::error("boom")).unwrap();
        assert!(!plain_json.contains("busy"));
    }

    #[test]
    fn command_errors_classify_busy_by_marker() {
        let busy = WarmWireResponse::for_command_error(&MemdError::IndexBusy {
            reason: "repair in flight".to_string(),
        });
        assert!(busy.busy);
        assert!(!busy.ok);

        // The marker survives a stringly re-wrap by an intermediate layer.
        let wrapped = WarmWireResponse::for_command_error(&MemdError::ProtocolError(format!(
            "command failed: {}",
            MemdError::IndexBusy {
                reason: "repair".to_string()
            }
        )));
        assert!(wrapped.busy);

        let plain =
            WarmWireResponse::for_command_error(&MemdError::StorageError("disk full".to_string()));
        assert!(!plain.busy);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn auto_read_only_command_busy_falls_back_instantly() {
        let dir = tempdir().unwrap();
        let config = test_warm_config(dir.path().join("data"));
        let socket = warm_socket_path(&config);
        let server = tokio::spawn(serve_ping_then_busy_on_command(socket));

        let start = Instant::now();
        let result = try_run_warm_client(&config, &search_command(false, WarmMode::Auto))
            .await
            .unwrap();
        let elapsed = start.elapsed();

        server.abort();
        // Cold fallback requested (Ok(false)) without waiting out any
        // client timeout — the busy reply itself is immediate.
        assert!(!result);
        assert!(
            elapsed < Duration::from_secs(5),
            "busy fallback must not wait for timeouts, took {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn auto_write_command_busy_surfaces_clear_error() {
        let dir = tempdir().unwrap();
        let config = test_warm_config(dir.path().join("data"));
        let socket = warm_socket_path(&config);
        let server = tokio::spawn(serve_ping_then_busy_on_command(socket));

        let error = try_run_warm_client(&config, &add_command(WarmMode::Auto))
            .await
            .unwrap_err();

        server.abort();
        let message = error.to_string();
        assert!(
            MemdError::message_indicates_index_busy(&message),
            "write busy error must carry the busy marker: {message}"
        );
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
    fn parse_max_workers_defaults_and_floors() {
        assert_eq!(parse_max_workers(None), DEFAULT_WARM_MAX_WORKERS);
        // `0` must NOT disable the cap (the idle-timeout footgun).
        assert_eq!(parse_max_workers(Some("0")), DEFAULT_WARM_MAX_WORKERS);
        assert_eq!(parse_max_workers(Some("garbage")), DEFAULT_WARM_MAX_WORKERS);
        assert_eq!(parse_max_workers(Some(" 8 ")), 8);
    }

    #[test]
    fn warm_worker_still_owns_matches_published_pid() {
        let dir = tempdir().unwrap();
        let socket = dir.path().join("warm").join("memd.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        // No pid file yet → not owner.
        assert!(!warm_worker_still_owns(&socket, 1234));
        // Pid file names us → owner.
        std::fs::write(warm_pid_path_for_socket(&socket), "1234\n").unwrap();
        assert!(warm_worker_still_owns(&socket, 1234));
        // Replaced by a newer instance → no longer owner (drives orphan evict).
        std::fs::write(warm_pid_path_for_socket(&socket), "5678\n").unwrap();
        assert!(!warm_worker_still_owns(&socket, 1234));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn count_live_workers_under_counts_live_and_skips_dead() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // Live: our own pid (its /proc entry exists).
        let live_sub = root.join("live");
        std::fs::create_dir_all(&live_sub).unwrap();
        std::fs::write(
            live_sub.join("memd.pid"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        // Dead: a pid that cannot be running.
        let dead_sub = root.join("dead");
        std::fs::create_dir_all(&dead_sub).unwrap();
        std::fs::write(dead_sub.join("memd.pid"), "4000000000\n").unwrap();
        // A subdir with no pid file is ignored.
        std::fs::create_dir_all(root.join("empty")).unwrap();
        assert_eq!(count_live_workers_under(root), 1);
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
            dedupe_by_source: false,
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
    fn warm_routed_store_access_matches_timeout_retry_safety() {
        let read_only = vec![
            search_command(false, WarmMode::Auto),
            CliCommand::AgentContext {
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
            },
            CliCommand::Report {
                tenant_id: Some("t".to_string()),
                project_id: Some("p".to_string()),
                since: "24h".to_string(),
                format: ReportFormat::Json,
                strict: true,
                top: 5,
                output: None,
                warm: WarmMode::Auto,
            },
        ];

        for command in read_only {
            assert_eq!(command.store_access(), StoreAccess::ReadOnly);
        }

        let writers = vec![
            add_command(WarmMode::Auto),
            CliCommand::Delete {
                tenant_id: Some("t".to_string()),
                chunk_id: "019e6d12-c1a7-7330-8bd8-4c9cdb45bc3c".to_string(),
                warm: WarmMode::Auto,
            },
            CliCommand::ImportOmf {
                tenant_id: Some("t".to_string()),
                input: Some(PathBuf::from("input.json")),
                include_archived: true,
                fuzzy_threshold: None,
                dry_run: false,
                warm: WarmMode::Auto,
            },
            CliCommand::Purge {
                tenant_id: "t".to_string(),
                project_id: Some("p".to_string()),
                older_than_days: 30,
                limit: 100,
                include_unreadable_active: true,
                archive: None,
                apply: false,
                vacuum_metadata: false,
                rewrite_segments: false,
                warm: WarmMode::Auto,
            },
            CliCommand::Consolidate {
                tenant_id: Some("t".to_string()),
                project_id: Some("p".to_string()),
                project_dir: PathBuf::from("/tmp/project"),
                max_region: 50,
                dry_run: true,
                background: false,
                force: false,
                warm: WarmMode::Auto,
            },
            CliCommand::Call {
                tool: "memory.search".to_string(),
                json: Some("{}".to_string()),
                input: None,
                output: None,
                warm: WarmMode::Auto,
            },
            batch_command(false, WarmMode::Auto),
        ];

        for command in writers {
            assert_eq!(command.store_access(), StoreAccess::Writer);
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
                repair_in_progress: false,
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
                dedupe_by_source: false,
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
            dedupe_by_source: false,
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

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn auto_read_only_command_timeout_falls_back_to_cold_path() {
        let _guard = WARM_TIMEOUT_ENV_LOCK.lock().unwrap();
        std::env::set_var("MEMD_WARM_CLIENT_TIMEOUT_MS", "50");
        let dir = tempdir().unwrap();
        let config = test_warm_config(dir.path().join("data"));
        let socket = warm_socket_path(&config);
        let server = tokio::spawn(serve_ping_then_hang_on_command(socket));

        let result = try_run_warm_client(&config, &search_command(false, WarmMode::Auto))
            .await
            .unwrap();

        std::env::remove_var("MEMD_WARM_CLIENT_TIMEOUT_MS");
        server.abort();
        assert!(!result);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn auto_write_command_timeout_does_not_retry_cold_path() {
        let _guard = WARM_TIMEOUT_ENV_LOCK.lock().unwrap();
        std::env::set_var("MEMD_WARM_CLIENT_TIMEOUT_MS", "50");
        let dir = tempdir().unwrap();
        let config = test_warm_config(dir.path().join("data"));
        let socket = warm_socket_path(&config);
        let server = tokio::spawn(serve_ping_then_hang_on_command(socket));

        let error = try_run_warm_client(&config, &add_command(WarmMode::Auto))
            .await
            .unwrap_err();

        std::env::remove_var("MEMD_WARM_CLIENT_TIMEOUT_MS");
        server.abort();
        let message = error.to_string();
        assert!(message.contains("write command"));
        assert!(message.contains("may still complete"));
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

        let add = add_command(WarmMode::Auto);
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
