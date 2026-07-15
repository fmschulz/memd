//! Shared direct and warm-worker implementations for mutating CLI commands.

use serde_json::json;
use tracing::info;

use crate::error::{MemdError, Result};
use crate::store::usage::{UsageEvent, UsageOp};
use crate::store::{Store, TenantManager};
use crate::types::{ChunkId, ChunkType, MemoryChunk, ProjectId, Source, TenantId};

#[derive(Debug, Clone)]
pub(super) struct CliAddRenderOptions {
    pub(super) tenant_id: String,
    pub(super) text: String,
    pub(super) chunk_type: ChunkType,
    pub(super) project_id: Option<String>,
    pub(super) tags: Option<Vec<String>>,
    pub(super) source_uri: Option<String>,
    pub(super) source_path: Option<String>,
}

pub(super) async fn cli_add_rendered<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    opts: CliAddRenderOptions,
) -> Result<String> {
    let tenant = TenantId::new(&opts.tenant_id)?;
    ProjectId::validate_opt(opts.project_id.as_deref())?;

    if let Some(tm) = tenant_manager {
        tm.ensure_tenant_dir(&tenant)?;
    }

    let project_id_for_usage = opts.project_id.clone();
    let mut chunk = MemoryChunk::new(tenant, &opts.text, opts.chunk_type);
    if let Some(pid) = opts.project_id {
        chunk = chunk.with_project(ProjectId::new(Some(pid)));
    }

    let effective_tags = opts.tags.unwrap_or_default();
    let mut prepared =
        crate::write_service::prepare_write(crate::write_service::PrepareWriteRequest {
            chunk_type: opts.chunk_type,
            text: &opts.text,
            tags: &effective_tags,
            ingestion_mode: crate::types::IngestionMode::Document,
            expires_at_ms: None,
            review_after_ms: None,
        });
    if prepared.is_rejected() {
        store.record_usage_event(UsageEvent {
            op: UsageOp::Add,
            tenant: Some(chunk.tenant_id.to_string()),
            project: project_id_for_usage.clone(),
            outcome: format!("rejected:{}", prepared.outcome.reason),
            chunk_count: Some(0),
            bytes: Some(opts.text.len() as i64),
            detail: None,
        });
        return Err(MemdError::ValidationError(format!(
            "memory.add rejected by quality gate: {}",
            prepared.outcome.reason
        )));
    }
    if store.as_persistent().is_none() {
        prepared.strip_optional_retention_defaults();
    }
    chunk = prepared.apply_to_chunk(chunk);

    if opts.source_uri.is_some() || opts.source_path.is_some() {
        chunk = chunk.with_source(Source {
            uri: opts.source_uri,
            path: opts.source_path,
            ..Default::default()
        });
    }

    store.record_usage_event(UsageEvent {
        op: UsageOp::Add,
        tenant: Some(chunk.tenant_id.to_string()),
        project: project_id_for_usage,
        outcome: prepared.usage_outcome().to_string(),
        chunk_count: Some(1),
        bytes: Some(opts.text.len() as i64),
        detail: None,
    });

    let lifecycle_delta = prepared.lifecycle_delta();
    let (chunk_id, stored_chunk_ids) = if lifecycle_delta.is_empty() {
        store.add_with_stored_ids(chunk).await?
    } else {
        let persistent = store.as_persistent().ok_or_else(|| {
            MemdError::StorageError(
                "prepared write retention requires a persistent store".to_string(),
            )
        })?;
        persistent
            .add_chunk_with_lifecycle_and_stored_ids(chunk, lifecycle_delta)
            .await?
    };
    info!(chunk_id = %chunk_id, "chunk added");

    let output = json!({
        "chunk_id": chunk_id.to_string(),
        "stored_chunk_ids": stored_chunk_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "admission_decision": prepared.decision(),
        "admission_reason": prepared.outcome.reason,
        "admission_warning": prepared.outcome.warning,
        "lifecycle_tier": prepared.lifecycle_tier_name(),
        "expires_at_ms": prepared.retention.expires_at_ms,
        "review_after_ms": prepared.retention.review_after_ms,
    });
    Ok(serde_json::to_string_pretty(&output)? + "\n")
}

pub(super) async fn cli_delete_rendered<S: Store>(
    store: &S,
    tenant_id: &str,
    chunk_id: &str,
) -> Result<String> {
    let tenant = TenantId::new(tenant_id)?;
    let cid = ChunkId::parse(chunk_id)?;
    let deleted = store.delete(&tenant, &cid).await?;

    info!(chunk_id = %cid, deleted = deleted, "delete operation");
    Ok(serde_json::to_string_pretty(&json!({ "deleted": deleted }))? + "\n")
}

pub(super) async fn cli_import_omf_rendered<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    tenant_id: &str,
    raw_document: &str,
    include_archived: bool,
    fuzzy_threshold: Option<f32>,
    dry_run: bool,
) -> Result<String> {
    let tenant = TenantId::new(tenant_id)?;
    let doc: crate::omf::OmfDocument = serde_json::from_str(raw_document).map_err(|e| {
        MemdError::ValidationError(format!("input is not a valid OMF 1.0 document: {e}"))
    })?;

    let ps = store.as_persistent().ok_or_else(|| {
        MemdError::StorageError("import-omf requires a persistent store".to_string())
    })?;
    let opts = crate::omf::import::ImportOptions {
        include_archived,
        fuzzy_threshold,
    };

    if dry_run {
        let preview = crate::omf::import::preview_omf_import(ps, &tenant, &doc, opts).await?;
        let output = json!({
            "tenant_id": tenant.to_string(),
            "dry_run": true,
            "total": preview.total,
            "to_import": preview.to_import,
            "duplicates": preview.duplicates,
            "filtered": preview.filtered,
            "unscoped": preview.unscoped,
            "by_project": preview.by_project,
        });
        Ok(serde_json::to_string_pretty(&output)? + "\n")
    } else {
        if let Some(tm) = tenant_manager {
            tm.ensure_tenant_dir(&tenant)?;
        }
        let result = crate::omf::import::import_omf(ps, &tenant, &doc, opts).await?;
        let output = json!({
            "tenant_id": tenant.to_string(),
            "dry_run": false,
            "total": result.total,
            "imported": result.imported,
            "duplicates": result.duplicates,
            "skipped": result.skipped,
        });
        Ok(serde_json::to_string_pretty(&output)? + "\n")
    }
}
