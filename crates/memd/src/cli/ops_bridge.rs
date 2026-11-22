use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::maintenance::DreamParams;
use crate::ops::error::OperationError as McpError;
use crate::ops::{
    handle_artifact_create, handle_artifact_find_decisions, handle_artifact_find_evidence,
    handle_artifact_find_failures, handle_artifact_find_highlights, handle_artifact_get,
    handle_artifact_list_thread, handle_artifact_search, handle_artifact_verify,
    handle_context_brief_project, handle_context_find_relevant_context,
    handle_context_get_files_for_subsystem, handle_context_get_hot_context,
    handle_context_list_subsystems, handle_context_suggest_agent, handle_memory_add,
    handle_memory_add_batch, handle_memory_compact, handle_memory_consolidate_episode,
    handle_memory_delete, handle_memory_dream, handle_memory_export_markdown,
    handle_memory_export_omf, handle_memory_feedback, handle_memory_find_near_duplicates,
    handle_memory_get, handle_memory_health, handle_memory_import_omf,
    handle_memory_preview_omf_import, handle_memory_record_outcome, handle_memory_search,
    handle_memory_set_expiry, handle_memory_stats, handle_memory_supersede,
    handle_task_add_evidence, handle_task_finish, handle_task_get, handle_task_progress,
    handle_task_resume, handle_task_run_finish, handle_task_run_start, handle_task_search,
    handle_task_start, AddBatchParams, AddParams, ArtifactCreateParams, ArtifactGetParams,
    ArtifactLibraryParams, ArtifactListThreadParams, ArtifactVerifyParams, CompactParams,
    ConsolidateEpisodeParams, ContextFindRelevantContextParams, ContextGetFilesForSubsystemParams,
    ContextGetHotContextParams, ContextListSubsystemsParams, ContextSuggestAgentParams,
    DeleteParams, ExportMarkdownParams, ExportOmfParams, FeedbackParams, FindNearDuplicatesParams,
    GetParams, HealthParams, ImportOmfParams, PreviewOmfImportParams, ProjectBriefParams,
    RecordOutcomeParams, SearchParams, SetExpiryParams, StatsParams, SupersedeParams,
    TaskAddEvidenceParams, TaskFinishParams, TaskGetParams, TaskProgressParams, TaskResumeParams,
    TaskRunFinishParams, TaskRunStartParams, TaskSearchParams, TaskStartParams,
};
use crate::store::{Store, TenantManager};

fn parse_tool_params<T: DeserializeOwned>(
    tool: &str,
    arguments: Value,
) -> std::result::Result<T, McpError> {
    serde_json::from_value(arguments)
        .map_err(|e| McpError::InvalidParams(format!("invalid {tool} params: {e}")))
}

pub(super) async fn cli_call_tool<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    tool: &str,
    arguments: Value,
) -> std::result::Result<Value, McpError> {
    if tool.starts_with("experience.") {
        return super::experience::call(store, tool, arguments)
            .await
            .map_err(|error| McpError::ToolError(error.to_string()));
    }

    match tool {
        "memory.search" => {
            let params: SearchParams = parse_tool_params(tool, arguments)?;
            handle_memory_search(store, params).await
        }
        "memory.add" => {
            let params: AddParams = parse_tool_params(tool, arguments)?;
            handle_memory_add(store, tenant_manager, params).await
        }
        "memory.add_batch" => {
            let params: AddBatchParams = parse_tool_params(tool, arguments)?;
            handle_memory_add_batch(store, tenant_manager, params).await
        }
        "task.start" => {
            let params: TaskStartParams = parse_tool_params(tool, arguments)?;
            handle_task_start(store, tenant_manager, params).await
        }
        "task.progress" => {
            let params: TaskProgressParams = parse_tool_params(tool, arguments)?;
            handle_task_progress(store, tenant_manager, params).await
        }
        "task.run_start" => {
            let params: TaskRunStartParams = parse_tool_params(tool, arguments)?;
            handle_task_run_start(store, tenant_manager, params).await
        }
        "task.run_finish" => {
            let params: TaskRunFinishParams = parse_tool_params(tool, arguments)?;
            handle_task_run_finish(store, tenant_manager, params).await
        }
        "task.add_evidence" => {
            let params: TaskAddEvidenceParams = parse_tool_params(tool, arguments)?;
            handle_task_add_evidence(store, tenant_manager, params).await
        }
        "task.finish" => {
            let params: TaskFinishParams = parse_tool_params(tool, arguments)?;
            handle_task_finish(store, tenant_manager, params).await
        }
        "task.get" => {
            let params: TaskGetParams = parse_tool_params(tool, arguments)?;
            handle_task_get(store, params).await
        }
        "task.search" => {
            let params: TaskSearchParams = parse_tool_params(tool, arguments)?;
            handle_task_search(store, params).await
        }
        "task.resume" => {
            let params: TaskResumeParams = parse_tool_params(tool, arguments)?;
            handle_task_resume(store, params).await
        }
        "artifact.create" => {
            let params: ArtifactCreateParams = parse_tool_params(tool, arguments)?;
            handle_artifact_create(store, tenant_manager, params).await
        }
        "artifact.review" | "artifact.revision" | "artifact.decision" | "artifact.verification" => {
            let kind = match tool {
                "artifact.review" => "review",
                "artifact.revision" => "revision",
                "artifact.decision" => "decision",
                "artifact.verification" => "verification",
                _ => unreachable!(),
            };
            let mut arguments = arguments;
            if let Some(obj) = arguments.as_object_mut() {
                if let Some(existing) = obj.get("artifact_kind") {
                    if existing.as_str() != Some(kind) {
                        return Err(McpError::InvalidParams(format!(
                            "{tool} forbids an overriding artifact_kind; got {existing}"
                        )));
                    }
                }
                obj.insert("artifact_kind".to_string(), Value::String(kind.to_string()));
            }
            let params: ArtifactCreateParams = parse_tool_params(tool, arguments)?;
            handle_artifact_create(store, tenant_manager, params).await
        }
        "artifact.get" => {
            let params: ArtifactGetParams = parse_tool_params(tool, arguments)?;
            handle_artifact_get(store, params).await
        }
        "artifact.search" => {
            let params: TaskSearchParams = parse_tool_params(tool, arguments)?;
            handle_artifact_search(store, params).await
        }
        "artifact.find_related" | "artifact.verify" => {
            let params: ArtifactVerifyParams = parse_tool_params(tool, arguments)?;
            handle_artifact_verify(store, params).await
        }
        "artifact.find_failures" => {
            let params: ArtifactLibraryParams = parse_tool_params(tool, arguments)?;
            handle_artifact_find_failures(store, params).await
        }
        "artifact.find_decisions" => {
            let params: ArtifactLibraryParams = parse_tool_params(tool, arguments)?;
            handle_artifact_find_decisions(store, params).await
        }
        "artifact.find_evidence" => {
            let params: ArtifactLibraryParams = parse_tool_params(tool, arguments)?;
            handle_artifact_find_evidence(store, params).await
        }
        "artifact.find_highlights" => {
            let params: ArtifactLibraryParams = parse_tool_params(tool, arguments)?;
            handle_artifact_find_highlights(store, params).await
        }
        "artifact.list_thread" => {
            let params: ArtifactListThreadParams = parse_tool_params(tool, arguments)?;
            handle_artifact_list_thread(store, params).await
        }
        "memory.get" => {
            let params: GetParams = parse_tool_params(tool, arguments)?;
            handle_memory_get(store, params).await
        }
        "memory.delete" => {
            let params: DeleteParams = parse_tool_params(tool, arguments)?;
            handle_memory_delete(store, params).await
        }
        "memory.feedback" => {
            let params: FeedbackParams = parse_tool_params(tool, arguments)?;
            handle_memory_feedback(store, params).await
        }
        "memory.record_outcome" => {
            let params: RecordOutcomeParams = parse_tool_params(tool, arguments)?;
            handle_memory_record_outcome(store, params).await
        }
        "memory.stats" => {
            let params: StatsParams = parse_tool_params(tool, arguments)?;
            handle_memory_stats(store, tenant_manager, params).await
        }

        "memory.health" => {
            let params: HealthParams = parse_tool_params(tool, arguments)?;
            handle_memory_health(store, params).await
        }
        "memory.compact" => {
            let params: CompactParams = parse_tool_params(tool, arguments)?;
            handle_memory_compact(store, params).await
        }
        "memory.dream" => {
            let params: DreamParams = parse_tool_params(tool, arguments)?;
            handle_memory_dream(store, tenant_manager, params).await
        }
        "memory.supersede" => {
            let params: SupersedeParams = parse_tool_params(tool, arguments)?;
            handle_memory_supersede(store, tenant_manager, params).await
        }
        "memory.set_expiry" => {
            let params: SetExpiryParams = parse_tool_params(tool, arguments)?;
            handle_memory_set_expiry(store, tenant_manager, params).await
        }
        "memory.find_near_duplicates" => {
            let params: FindNearDuplicatesParams = parse_tool_params(tool, arguments)?;
            handle_memory_find_near_duplicates(store, params).await
        }
        "memory.export_markdown" => {
            let params: ExportMarkdownParams = parse_tool_params(tool, arguments)?;
            handle_memory_export_markdown(store, params).await
        }
        "memory.export_omf" => {
            let params: ExportOmfParams = parse_tool_params(tool, arguments)?;
            handle_memory_export_omf(store, params).await
        }
        "memory.preview_omf_import" => {
            let params: PreviewOmfImportParams = parse_tool_params(tool, arguments)?;
            handle_memory_preview_omf_import(store, params).await
        }
        "memory.import_omf" => {
            let params: ImportOmfParams = parse_tool_params(tool, arguments)?;
            handle_memory_import_omf(store, tenant_manager, params).await
        }
        "memory.consolidate_episode" => {
            let params: ConsolidateEpisodeParams = parse_tool_params(tool, arguments)?;
            handle_memory_consolidate_episode(store, params).await
        }
        "context.list_subsystems" => {
            let params: ContextListSubsystemsParams = parse_tool_params(tool, arguments)?;
            handle_context_list_subsystems(store, params).await
        }
        "context.get_files_for_subsystem" => {
            let params: ContextGetFilesForSubsystemParams = parse_tool_params(tool, arguments)?;
            handle_context_get_files_for_subsystem(store, params).await
        }
        "context.find_relevant_context" => {
            let params: ContextFindRelevantContextParams = parse_tool_params(tool, arguments)?;
            handle_context_find_relevant_context(store, params).await
        }
        "context.brief_project" => {
            let params: ProjectBriefParams = parse_tool_params(tool, arguments)?;
            handle_context_brief_project(store, params).await
        }
        "context.suggest_agent" => {
            let params: ContextSuggestAgentParams = parse_tool_params(tool, arguments)?;
            handle_context_suggest_agent(store, params).await
        }
        "context.get_hot_context" => {
            let params: ContextGetHotContextParams = parse_tool_params(tool, arguments)?;
            handle_context_get_hot_context(store, params).await
        }
        _ => Err(McpError::MethodNotFound(format!("unknown tool '{tool}'"))),
    }
}
