//! Client-side experience commands. Verification commands never run in the worker.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};

use super::CliCommand;
use crate::error::{MemdError, Result};
use crate::experience::{self, CheckReceipt, ExperienceBundleV1, ExperienceContext, RecordEvent};
use crate::store::Store;
use crate::types::{ProjectId, TenantId};

/// Record, retrieve, verify, and transfer operational experience.
#[derive(Debug, Clone, Subcommand)]
pub enum ExperienceCommand {
    /// Record a problem, attempt, conditional lesson, or correction from JSON.
    Record {
        /// JSON object containing an `event` and optional observation time.
        #[arg(long, conflicts_with = "input", required_unless_present = "input")]
        json: Option<String>,
        /// File containing the event JSON object.
        #[arg(long)]
        input: Option<PathBuf>,
    },
    /// Read a problem and its attempts, checks, and current lessons.
    Get { problem_id: String },
    /// Find current lessons whose conditions match the target environment.
    Find {
        query: String,
        /// Target machine; defaults to the calling machine.
        #[arg(long)]
        machine: Option<String>,
        #[arg(long)]
        tool: Option<String>,
        #[arg(long)]
        version: Option<String>,
        /// Also consider lessons explicitly shared with other projects.
        #[arg(long)]
        include_shared: bool,
        #[arg(short, long, default_value_t = 5)]
        limit: usize,
    },
    /// Execute an explicit check locally and store a receipt for one attempt.
    Check {
        attempt_id: String,
        /// Files whose contents are hashed before and after the check.
        #[arg(long)]
        evidence: Vec<PathBuf>,
        /// Command deadline, including output collection.
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
        /// Program and arguments to execute, after `--`. No implicit shell.
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<String>,
    },
    /// Export complete canonical cases, preserving IDs and provenance.
    Export {
        #[arg(required = true, num_args = 1..)]
        problem_ids: Vec<String>,
    },
    /// Import a canonical experience bundle without executing its commands.
    Import { input: PathBuf },
}

/// Convert experience commands to the existing local operation transport.
///
/// Called before warm routing so a check runs in the requesting process.
pub async fn prepare_experience_command(
    cmd: CliCommand,
    data_dir: &Path,
    in_memory: bool,
) -> Result<CliCommand> {
    let CliCommand::Experience {
        tenant_id,
        project_id,
        command,
        output,
        warm,
    } = cmd
    else {
        return Ok(cmd);
    };
    let tenant_id = super::scope::require_tenant(tenant_id)?;
    let (tool, mut arguments) = match command {
        ExperienceCommand::Record { json, input } => (
            "experience.record",
            super::call::parse_call_arguments(json.as_deref(), input.as_deref())?,
        ),
        ExperienceCommand::Get { problem_id } => {
            ("experience.get", json!({"problem_id": problem_id}))
        }
        ExperienceCommand::Find {
            query,
            machine,
            tool,
            version,
            include_shared,
            limit,
        } => {
            let context = super::capture_execution_context(&std::env::current_dir()?);
            (
                "experience.find",
                json!({
                    "symptom": query,
                    "machine": machine.or(context.origin_host),
                    "tool": tool,
                    "version": version,
                    "include_shared": include_shared,
                    "limit": limit,
                }),
            )
        }
        ExperienceCommand::Check {
            attempt_id,
            evidence,
            timeout_seconds,
            command,
        } => {
            if in_memory {
                return Err(MemdError::ValidationError(
                    "experience check requires a persistent attempt record".into(),
                ));
            }
            let receipt = super::experience_check::execute_check(
                data_dir,
                &tenant_id,
                project_id.as_deref(),
                attempt_id,
                evidence,
                timeout_seconds,
                command,
            )
            .await?;
            ("experience.record_check", json!({"receipt": receipt}))
        }
        ExperienceCommand::Export { problem_ids } => {
            ("experience.export", json!({"problem_ids": problem_ids}))
        }
        ExperienceCommand::Import { input } => (
            "experience.import",
            json!({"bundle": serde_json::from_str::<Value>(&std::fs::read_to_string(input)?)?}),
        ),
    };
    let object = arguments.as_object_mut().ok_or_else(|| {
        MemdError::ValidationError("experience arguments must be a JSON object".into())
    })?;
    object.insert("tenant_id".into(), json!(tenant_id));
    object.insert("project_id".into(), json!(project_id));
    Ok(CliCommand::Call {
        tool: tool.into(),
        json: Some(serde_json::to_string(&arguments)?),
        input: None,
        output,
        warm,
    })
}

#[derive(Deserialize)]
struct RecordRequest {
    #[serde(flatten)]
    context: ExperienceContext,
    event: RecordEvent,
}

#[derive(Deserialize)]
struct CheckRequest {
    #[serde(flatten)]
    context: ExperienceContext,
    receipt: CheckReceipt,
}

#[derive(Deserialize)]
struct CaseRequest {
    tenant_id: TenantId,
    #[serde(default)]
    project_id: ProjectId,
    problem_id: String,
}

#[derive(Deserialize)]
struct ExportRequest {
    tenant_id: TenantId,
    #[serde(default)]
    project_id: ProjectId,
    problem_ids: Vec<String>,
}

#[derive(Deserialize)]
struct ImportRequest {
    tenant_id: TenantId,
    #[serde(default)]
    project_id: ProjectId,
    bundle: ExperienceBundleV1,
}

pub(super) async fn call<S: Store>(store: &S, tool: &str, arguments: Value) -> Result<Value> {
    let payload = match tool {
        "experience.record" => {
            let request: RecordRequest = serde_json::from_value(arguments)?;
            serde_json::to_value(experience::record(store, request.context, request.event).await?)?
        }
        "experience.record_check" => {
            let request: CheckRequest = serde_json::from_value(arguments)?;
            serde_json::to_value(
                experience::record_check(store, request.context, request.receipt).await?,
            )?
        }
        "experience.get" => {
            let request: CaseRequest = serde_json::from_value(arguments)?;
            let case =
                experience::resolve_case(store, &request.tenant_id, &request.problem_id).await?;
            require_project(&request.project_id, &case.problem.project_id)?;
            serde_json::to_value(case)?
        }
        "experience.find" => serde_json::to_value(
            experience::retrieve_lessons(store, serde_json::from_value(arguments)?).await?,
        )?,
        "experience.export" => {
            let request: ExportRequest = serde_json::from_value(arguments)?;
            let bundle =
                experience::export_bundle(store, &request.tenant_id, &request.problem_ids).await?;
            for artifact in &bundle.artifacts {
                require_project(&request.project_id, &artifact.project_id)?;
            }
            serde_json::to_value(bundle)?
        }
        "experience.import" => {
            let request: ImportRequest = serde_json::from_value(arguments)?;
            for artifact in &request.bundle.artifacts {
                require_project(&request.project_id, &artifact.project_id)?;
            }
            serde_json::to_value(
                experience::import_bundle(store, &request.tenant_id, request.bundle).await?,
            )?
        }
        _ => {
            return Err(MemdError::ValidationError(format!(
                "unknown experience operation: {tool}"
            )))
        }
    };
    Ok(json!({"content": [{"type": "text", "text": serde_json::to_string(&payload)?}]}))
}

fn require_project(requested: &ProjectId, actual: &ProjectId) -> Result<()> {
    if requested.as_option().is_some() && requested != actual {
        return Err(MemdError::ValidationError(
            "experience belongs to another project".into(),
        ));
    }
    Ok(())
}
