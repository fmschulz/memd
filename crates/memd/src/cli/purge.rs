use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::cli::ExportFormat;
use crate::error::{MemdError, Result};
use crate::index::sparse::SparseIndex;
use crate::store::metadata::{ChunkMetadata, MetadataStore};
use crate::store::persistent::PersistentStore;
use crate::store::usage::{UsageEvent, UsageOp};
use crate::store::Store;
use crate::types::{ChunkId, ChunkStatus, MemoryChunk, MemoryTier, TenantId};

#[derive(Debug)]
pub(super) struct PurgeOptions {
    pub(super) tenant_id: String,
    pub(super) project_id: Option<String>,
    pub(super) older_than_days: u64,
    pub(super) limit: usize,
    pub(super) include_unreadable_active: bool,
    pub(super) archive: Option<PathBuf>,
    pub(super) apply: bool,
    pub(super) vacuum_metadata: bool,
    pub(super) rewrite_segments: bool,
}

#[derive(Debug)]
pub(super) struct PurgeArchiveInspectOptions {
    pub(super) archive: PathBuf,
    pub(super) expect_tenant_id: Option<String>,
    pub(super) expect_project_id: Option<String>,
    pub(super) min_records: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct PurgeArchiveInspection {
    status: &'static str,
    archive_path: String,
    archive_format: String,
    archive_sha256: String,
    file_bytes: u64,
    tenant_id: String,
    project_id: Option<String>,
    created_unix_ms: Option<i64>,
    cutoff_unix_ms: Option<i64>,
    declared_record_count: Option<usize>,
    record_count: usize,
    payload_available_count: usize,
    payload_missing_count: usize,
    records_without_recoverable_text: usize,
    estimated_recoverable_text_bytes: usize,
    reasons: BTreeMap<String, usize>,
    preview_chunk_ids: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct PurgeArchiveRecord {
    metadata: ChunkMetadata,
    payload: Option<MemoryChunk>,
    reason: &'static str,
}

#[derive(Debug)]
struct PurgeCandidate {
    metadata: ChunkMetadata,
    reason: &'static str,
}

pub(super) async fn run_purge<S: Store>(store: &S, options: PurgeOptions) -> Result<Value> {
    let tenant_id = TenantId::new(&options.tenant_id)?;
    let project_id_for_usage = options.project_id.clone();
    let Some(persistent) = store.as_persistent() else {
        return Err(MemdError::ValidationError(
            "purge requires a persistent store".to_string(),
        ));
    };

    let now_ms = now_ms() as i64;
    let older_than_days = options.older_than_days.max(1);
    let cutoff_ms = now_ms.saturating_sub((older_than_days as i64).saturating_mul(86_400_000));
    let limit = options.limit.clamp(1, 10_000);
    let hidden_candidates = persistent.metadata().list_hard_purge_candidates(
        &tenant_id,
        options.project_id.as_deref(),
        cutoff_ms,
        limit,
    )?;
    let hidden_candidate_count = hidden_candidates.len();
    let mut candidates = hidden_candidates
        .into_iter()
        .map(|metadata| PurgeCandidate {
            metadata,
            reason: "hidden_retention_candidate",
        })
        .collect::<Vec<_>>();
    let unreadable_limit = limit.saturating_sub(candidates.len());
    let unreadable_candidates = if options.include_unreadable_active && unreadable_limit > 0 {
        list_unreadable_active_candidates(
            persistent,
            &tenant_id,
            options.project_id.as_deref(),
            unreadable_limit,
        )
        .await?
    } else {
        Vec::new()
    };
    let unreadable_active_candidate_count = unreadable_candidates.len();
    candidates.extend(
        unreadable_candidates
            .into_iter()
            .map(|metadata| PurgeCandidate {
                metadata,
                reason: "unreadable_active_payload",
            }),
    );
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.metadata.chunk_id.clone())
        .collect::<Vec<_>>();
    let archive_records = build_archive_records(persistent, &tenant_id, &candidates).await?;
    let estimated_payload_bytes = archive_records
        .iter()
        .map(|record| {
            record
                .payload
                .as_ref()
                .map(|chunk| chunk.text.len())
                .or_else(|| record.metadata.canonical_text.as_ref().map(String::len))
                .unwrap_or(0)
        })
        .sum::<usize>();

    if !options.apply {
        return Ok(json!({
            "status": "dry_run",
            "tenant_id": tenant_id.to_string(),
            "project_id": options.project_id,
            "cutoff_unix_ms": cutoff_ms,
            "older_than_days": older_than_days,
            "candidate_count": candidates.len(),
            "hidden_candidate_count": hidden_candidate_count,
            "unreadable_active_candidate_count": unreadable_active_candidate_count,
            "include_unreadable_active": options.include_unreadable_active,
            "candidate_ids": preview_ids(&candidate_ids),
            "estimated_payload_bytes": estimated_payload_bytes,
            "archive_required_for_apply": !candidates.is_empty(),
            "soft_deleted_before_purge": 0,
            "hard_deleted_metadata_rows": 0,
            "sparse_pruned_chunks": 0,
            "segment_rewrite": null,
            "archive_verification": null,
            "metadata_vacuum_ran": false,
            "warnings": [],
        }));
    }

    let archive_path = if candidates.is_empty() {
        options.archive.clone()
    } else {
        Some(options.archive.clone().ok_or_else(|| {
            MemdError::ValidationError(
                "purge --apply requires --archive so hidden rows are exported before deletion"
                    .to_string(),
            )
        })?)
    };

    let mut archive_inspection = None;
    if let Some(path) = archive_path.as_ref() {
        if !candidates.is_empty() {
            write_archive(
                path,
                &tenant_id,
                options.project_id.as_deref(),
                now_ms,
                cutoff_ms,
                &archive_records,
            )?;
            archive_inspection = Some(inspect_purge_archive(PurgeArchiveInspectOptions {
                archive: path.clone(),
                expect_tenant_id: Some(tenant_id.to_string()),
                expect_project_id: options.project_id.clone(),
                min_records: Some(candidates.len()),
            })?);
        }
    }

    let mut warnings = Vec::new();
    let mut soft_deleted = 0usize;
    for candidate in &candidates {
        if candidate.metadata.status == ChunkStatus::Deleted {
            continue;
        }
        if store
            .delete(&tenant_id, &candidate.metadata.chunk_id)
            .await?
        {
            soft_deleted += 1;
        }
    }

    let mut sparse_pruned = 0usize;
    if let Some(sparse) = persistent.sparse_index() {
        for chunk_id in &candidate_ids {
            if sparse.delete(&tenant_id, chunk_id)? {
                sparse_pruned += 1;
            }
        }
        sparse.commit()?;
    }

    let compaction = match store.run_compaction(&tenant_id) {
        Ok(result) => {
            let hnsw_rebuild = result.hnsw_rebuild.as_ref().map(|rebuild| {
                json!({
                    "embeddings_processed": rebuild.embeddings_processed,
                    "embeddings_included": rebuild.embeddings_included,
                    "embeddings_excluded": rebuild.embeddings_excluded,
                    "duration_ms": rebuild.duration.as_millis(),
                })
            });
            Some(json!({
                "tombstones_processed": result.tombstones_processed,
                "expired_count": result.expired_count,
                "promoted_count": result.promoted_count,
                "hnsw_rebuilt": result.hnsw_rebuild.is_some(),
                "hnsw_rebuild": hnsw_rebuild,
                "segment_merge_ran": result.segment_merge.is_some(),
                "cache_entries_invalidated": result.cache_entries_invalidated,
                "duration_ms": result.duration.as_millis(),
            }))
        }
        Err(err) => {
            warnings.push(format!("store compaction skipped: {err}"));
            None
        }
    };

    let hard_deleted = persistent
        .metadata()
        .hard_delete_chunks(&tenant_id, &candidate_ids)?;

    let segment_rewrite = if options.rewrite_segments && hard_deleted > 0 {
        match persistent.rewrite_segments_for_tenant(&tenant_id) {
            Ok(result) => {
                warnings.extend(result.warnings.clone());
                Some(json!({
                    "segments_rewritten": result.segments_rewritten,
                    "segments_removed": result.segments_removed,
                    "chunks_moved": result.chunks_moved,
                    "bytes_before": result.bytes_before,
                    "bytes_after": result.bytes_after,
                    "bytes_reclaimed": result.bytes_reclaimed,
                }))
            }
            Err(err) => {
                warnings.push(format!("segment rewrite failed: {err}"));
                None
            }
        }
    } else {
        None
    };

    let mut metadata_vacuum_ran = false;
    if options.vacuum_metadata {
        if let Err(err) = persistent.metadata().checkpoint_wal() {
            warnings.push(format!("metadata WAL checkpoint failed: {err}"));
        }
        match persistent.metadata().vacuum() {
            Ok(()) => metadata_vacuum_ran = true,
            Err(err) => warnings.push(format!("metadata VACUUM failed: {err}")),
        }
    }

    let archive_verification = match archive_inspection.as_ref() {
        Some(report) => serde_json::to_value(report)?,
        None => Value::Null,
    };

    let payload = json!({
        "status": "completed",
        "tenant_id": tenant_id.to_string(),
        "project_id": options.project_id,
        "cutoff_unix_ms": cutoff_ms,
        "older_than_days": older_than_days,
        "candidate_count": candidates.len(),
        "hidden_candidate_count": hidden_candidate_count,
        "unreadable_active_candidate_count": unreadable_active_candidate_count,
        "include_unreadable_active": options.include_unreadable_active,
        "candidate_ids": preview_ids(&candidate_ids),
        "estimated_payload_bytes": estimated_payload_bytes,
        "archive_path": archive_path,
        "soft_deleted_before_purge": soft_deleted,
        "hard_deleted_metadata_rows": hard_deleted,
        "sparse_pruned_chunks": sparse_pruned,
        "compaction": compaction,
        "segment_rewrite": segment_rewrite,
        "archive_verification": archive_verification,
        "metadata_vacuum_ran": metadata_vacuum_ran,
        "warnings": warnings,
    });

    store.record_usage_event(UsageEvent {
        op: UsageOp::Purge,
        tenant: Some(options.tenant_id),
        project: project_id_for_usage,
        outcome: "ok".to_string(),
        chunk_count: Some(hard_deleted as i64),
        bytes: None,
        detail: None,
    });

    Ok(payload)
}

pub(super) fn inspect_purge_archive(
    options: PurgeArchiveInspectOptions,
) -> Result<PurgeArchiveInspection> {
    let bytes = std::fs::read(&options.archive).map_err(MemdError::IoError)?;
    let file_bytes = bytes.len() as u64;
    let archive_sha256 = sha256_hex(&bytes);
    let doc: Value = serde_json::from_slice(&bytes).map_err(|err| {
        MemdError::ValidationError(format!("purge archive is not valid JSON: {err}"))
    })?;

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let format = doc
        .get("archive_format")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if format != "memd_purge_archive_v1" {
        errors.push(format!(
            "unsupported archive_format `{format}`; expected `memd_purge_archive_v1`"
        ));
    }

    let tenant_id = required_string_field(&doc, "tenant_id", &mut errors);
    if let (Some(expected), Some(actual)) = (&options.expect_tenant_id, &tenant_id) {
        if expected != actual {
            errors.push(format!(
                "tenant mismatch: archive has `{actual}`, expected `{expected}`"
            ));
        }
    }

    let project_id = optional_string_field(&doc, "project_id", &mut errors);
    if let Some(expected) = &options.expect_project_id {
        match &project_id {
            Some(actual) if actual == expected => {}
            Some(actual) => errors.push(format!(
                "project mismatch: archive has `{actual}`, expected `{expected}`"
            )),
            None => errors.push(format!(
                "project mismatch: archive has no project_id, expected `{expected}`"
            )),
        }
    }

    let declared_record_count = doc
        .get("record_count")
        .and_then(Value::as_u64)
        .map(|count| count as usize);
    if doc.get("record_count").is_some() && declared_record_count.is_none() {
        errors.push("record_count must be an unsigned integer".to_string());
    }

    let records: &[Value] = match doc.get("records").and_then(Value::as_array) {
        Some(records) => records.as_slice(),
        None => {
            errors.push("records must be an array".to_string());
            &[]
        }
    };
    if let Some(declared) = declared_record_count {
        if declared != records.len() {
            errors.push(format!(
                "record_count mismatch: declared {declared}, found {} records",
                records.len()
            ));
        }
    }
    if let Some(min_records) = options.min_records {
        if records.len() < min_records {
            errors.push(format!(
                "archive has {} records, below required minimum {min_records}",
                records.len()
            ));
        }
    }

    let mut reasons = BTreeMap::new();
    let mut seen_chunk_ids = HashSet::new();
    let mut preview_chunk_ids = Vec::new();
    let mut payload_available_count = 0usize;
    let mut payload_missing_count = 0usize;
    let mut records_without_recoverable_text = 0usize;
    let mut estimated_recoverable_text_bytes = 0usize;

    for (idx, record) in records.iter().enumerate() {
        let Some(record_obj) = record.as_object() else {
            errors.push(format!("record {idx} must be an object"));
            continue;
        };

        let reason = record_obj
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if reason.is_empty() {
            errors.push(format!("record {idx} is missing reason"));
        } else {
            *reasons.entry(reason.to_string()).or_insert(0) += 1;
        }

        let Some(metadata) = record_obj.get("metadata").and_then(Value::as_object) else {
            errors.push(format!("record {idx} is missing metadata object"));
            continue;
        };

        let chunk_id = metadata
            .get("chunk_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if chunk_id.is_empty() {
            errors.push(format!("record {idx} metadata is missing chunk_id"));
        } else {
            if !seen_chunk_ids.insert(chunk_id.to_string()) {
                errors.push(format!("record {idx} duplicates chunk_id `{chunk_id}`"));
            }
            if preview_chunk_ids.len() < 50 {
                preview_chunk_ids.push(chunk_id.to_string());
            }
        }

        if let Some(actual) = metadata.get("tenant_id").and_then(Value::as_str) {
            if let Some(archive_tenant) = &tenant_id {
                if actual != archive_tenant {
                    errors.push(format!(
                        "record {idx} metadata tenant `{actual}` does not match archive tenant `{archive_tenant}`"
                    ));
                }
            }
        } else {
            errors.push(format!("record {idx} metadata is missing tenant_id"));
        }

        if let Some(archive_project) = &project_id {
            match metadata.get("project_id") {
                Some(Value::String(actual)) if actual == archive_project => {}
                Some(Value::String(actual)) => errors.push(format!(
                    "record {idx} metadata project `{actual}` does not match archive project `{archive_project}`"
                )),
                Some(Value::Null) | None => errors.push(format!(
                    "record {idx} metadata has no project_id but archive project is `{archive_project}`"
                )),
                _ => errors.push(format!("record {idx} metadata project_id must be a string or null")),
            }
        }

        let payload_available = match record_obj.get("payload_available").and_then(Value::as_bool) {
            Some(value) => value,
            None => {
                errors.push(format!("record {idx} payload_available must be boolean"));
                false
            }
        };
        let payload = record_obj.get("payload");
        let payload_present = payload.is_some_and(|value| !value.is_null());
        match (payload_available, payload_present) {
            (true, true) => {
                payload_available_count += 1;
                match payload
                    .and_then(|value| value.get("text"))
                    .and_then(Value::as_str)
                {
                    Some(text) => estimated_recoverable_text_bytes += text.len(),
                    None => errors.push(format!(
                        "record {idx} marks payload_available=true but payload.text is missing"
                    )),
                }
            }
            (false, false) => {
                payload_missing_count += 1;
                match metadata.get("canonical_text").and_then(Value::as_str) {
                    Some(text) if !text.is_empty() => {
                        estimated_recoverable_text_bytes += text.len();
                    }
                    _ => {
                        records_without_recoverable_text += 1;
                    }
                }
            }
            (true, false) => errors.push(format!(
                "record {idx} marks payload_available=true but payload is null"
            )),
            (false, true) => errors.push(format!(
                "record {idx} marks payload_available=false but payload is present"
            )),
        }
    }

    if records_without_recoverable_text > 0 {
        warnings.push(format!(
            "{records_without_recoverable_text} records have neither payload text nor metadata canonical_text"
        ));
    }

    if !errors.is_empty() {
        return Err(MemdError::ValidationError(format!(
            "purge archive verification failed: {}",
            errors.join("; ")
        )));
    }

    Ok(PurgeArchiveInspection {
        status: "verified",
        archive_path: options.archive.display().to_string(),
        archive_format: format,
        archive_sha256,
        file_bytes,
        tenant_id: tenant_id.unwrap_or_default(),
        project_id,
        created_unix_ms: doc.get("created_unix_ms").and_then(Value::as_i64),
        cutoff_unix_ms: doc.get("cutoff_unix_ms").and_then(Value::as_i64),
        declared_record_count,
        record_count: records.len(),
        payload_available_count,
        payload_missing_count,
        records_without_recoverable_text,
        estimated_recoverable_text_bytes,
        reasons,
        preview_chunk_ids,
        warnings,
    })
}

pub(super) fn render_purge_archive_inspection(
    report: &PurgeArchiveInspection,
    format: ExportFormat,
) -> Result<String> {
    match format {
        ExportFormat::Json => Ok(format!("{}\n", serde_json::to_string_pretty(report)?)),
        ExportFormat::Jsonl => Ok(format!("{}\n", serde_json::to_string(report)?)),
        ExportFormat::Markdown => Ok(render_purge_archive_markdown(report)),
    }
}

async fn list_unreadable_active_candidates(
    store: &PersistentStore,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    limit: usize,
) -> Result<Vec<ChunkMetadata>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let page_size = limit.clamp(1, 1000);
    let mut offset = 0usize;
    let mut candidates = Vec::new();
    while candidates.len() < limit {
        let rows = store
            .metadata()
            .list_for_project(tenant_id, project_id, page_size, offset)?;
        if rows.is_empty() {
            break;
        }
        let got = rows.len();
        for metadata in rows {
            if !matches!(metadata.status, ChunkStatus::Final | ChunkStatus::Draft)
                || metadata.lifecycle.tier == MemoryTier::History
            {
                continue;
            }
            if store
                .get_chunk_for_retrieval(tenant_id, &metadata.chunk_id, "purge_unreadable_active")
                .await?
                .is_none()
            {
                candidates.push(metadata);
                if candidates.len() >= limit {
                    break;
                }
            }
        }
        if got < page_size {
            break;
        }
        offset = offset.saturating_add(got);
    }
    Ok(candidates)
}

async fn build_archive_records(
    store: &PersistentStore,
    tenant_id: &TenantId,
    candidates: &[PurgeCandidate],
) -> Result<Vec<PurgeArchiveRecord>> {
    let mut records = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let metadata = &candidate.metadata;
        let payload = if metadata.status == ChunkStatus::Deleted {
            None
        } else {
            store
                .get_chunk_for_retrieval(tenant_id, &metadata.chunk_id, "purge_archive")
                .await?
        };
        records.push(PurgeArchiveRecord {
            metadata: metadata.clone(),
            payload,
            reason: candidate.reason,
        });
    }
    Ok(records)
}

fn write_archive(
    path: &PathBuf,
    tenant_id: &TenantId,
    project_id: Option<&str>,
    created_ms: i64,
    cutoff_ms: i64,
    records: &[PurgeArchiveRecord],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(MemdError::IoError)?;
    let payload = json!({
        "archive_format": "memd_purge_archive_v1",
        "tenant_id": tenant_id.to_string(),
        "project_id": project_id,
        "created_unix_ms": created_ms,
        "cutoff_unix_ms": cutoff_ms,
        "record_count": records.len(),
        "records": records.iter().map(archive_record_json).collect::<Vec<_>>(),
    });
    file.write_all(serde_json::to_string_pretty(&payload)?.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn archive_record_json(record: &PurgeArchiveRecord) -> Value {
    json!({
        "reason": record.reason,
        "metadata": metadata_json(&record.metadata),
        "payload_available": record.payload.is_some(),
        "payload": record.payload,
    })
}

fn metadata_json(metadata: &ChunkMetadata) -> Value {
    json!({
        "chunk_id": metadata.chunk_id.to_string(),
        "tenant_id": metadata.tenant_id.to_string(),
        "project_id": metadata.project_id,
        "segment_id": metadata.segment_id,
        "ordinal": metadata.ordinal,
        "chunk_type": metadata.chunk_type.to_string(),
        "status": metadata.status.to_string(),
        "timestamp_created": metadata.timestamp_created,
        "hash": metadata.hash,
        "source_uri": metadata.source_uri,
        "lifecycle": metadata.lifecycle,
        "canonical_text": metadata.canonical_text,
        "ingestion_mode": metadata.ingestion_mode.to_string(),
    })
}

fn preview_ids(ids: &[ChunkId]) -> Vec<String> {
    ids.iter().take(50).map(ToString::to_string).collect()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn required_string_field(doc: &Value, field: &str, errors: &mut Vec<String>) -> Option<String> {
    match doc.get(field) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(Value::String(_)) => {
            errors.push(format!("{field} must not be empty"));
            None
        }
        Some(_) => {
            errors.push(format!("{field} must be a string"));
            None
        }
        None => {
            errors.push(format!("{field} is required"));
            None
        }
    }
}

fn optional_string_field(doc: &Value, field: &str, errors: &mut Vec<String>) -> Option<String> {
    match doc.get(field) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(Value::String(_)) => {
            errors.push(format!("{field} must not be empty when present"));
            None
        }
        Some(Value::Null) | None => None,
        Some(_) => {
            errors.push(format!("{field} must be a string or null"));
            None
        }
    }
}

fn render_purge_archive_markdown(report: &PurgeArchiveInspection) -> String {
    let mut out = String::new();
    out.push_str("# memd purge archive\n\n");
    out.push_str(&format!("- status: `{}`\n", report.status));
    out.push_str(&format!("- archive_path: `{}`\n", report.archive_path));
    out.push_str(&format!("- archive_format: `{}`\n", report.archive_format));
    out.push_str(&format!("- archive_sha256: `{}`\n", report.archive_sha256));
    out.push_str(&format!("- file_bytes: `{}`\n", report.file_bytes));
    out.push_str(&format!("- tenant_id: `{}`\n", report.tenant_id));
    out.push_str(&format!(
        "- project_id: `{}`\n",
        report.project_id.as_deref().unwrap_or("<all>")
    ));
    out.push_str(&format!(
        "- record_count: `{}` declared=`{}`\n",
        report.record_count,
        report
            .declared_record_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "<missing>".to_string())
    ));
    out.push_str(&format!(
        "- payload_available: `{}`; payload_missing: `{}`; records_without_recoverable_text: `{}`\n",
        report.payload_available_count,
        report.payload_missing_count,
        report.records_without_recoverable_text
    ));
    out.push_str(&format!(
        "- estimated_recoverable_text_bytes: `{}`\n",
        report.estimated_recoverable_text_bytes
    ));
    if let Some(created) = report.created_unix_ms {
        out.push_str(&format!("- created_unix_ms: `{created}`\n"));
    }
    if let Some(cutoff) = report.cutoff_unix_ms {
        out.push_str(&format!("- cutoff_unix_ms: `{cutoff}`\n"));
    }

    if !report.reasons.is_empty() {
        out.push_str("\n## Reasons\n\n");
        for (reason, count) in &report.reasons {
            out.push_str(&format!("- `{reason}`: `{count}`\n"));
        }
    }

    if !report.preview_chunk_ids.is_empty() {
        out.push_str("\n## Preview Chunk IDs\n\n");
        for chunk_id in &report.preview_chunk_ids {
            out.push_str(&format!("- `{chunk_id}`\n"));
        }
    }

    if !report.warnings.is_empty() {
        out.push_str("\n## Warnings\n\n");
        for warning in &report.warnings {
            out.push_str(&format!("- {warning}\n"));
        }
    }

    out
}

#[cfg(test)]
mod archive_inspection_tests {
    use super::*;

    #[test]
    fn inspect_purge_archive_reports_valid_archive() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("purge.json");
        std::fs::write(
            &archive,
            r#"{
  "archive_format": "memd_purge_archive_v1",
  "tenant_id": "t",
  "project_id": "p",
  "created_unix_ms": 10,
  "cutoff_unix_ms": 5,
  "record_count": 2,
  "records": [
    {
      "reason": "hidden_retention_candidate",
      "metadata": {
        "chunk_id": "c1",
        "tenant_id": "t",
        "project_id": "p",
        "canonical_text": "archived hidden payload"
      },
      "payload_available": false,
      "payload": null
    },
    {
      "reason": "unreadable_active_payload",
      "metadata": {
        "chunk_id": "c2",
        "tenant_id": "t",
        "project_id": "p",
        "canonical_text": "metadata fallback"
      },
      "payload_available": true,
      "payload": {
        "text": "available payload text"
      }
    }
  ]
}"#,
        )
        .unwrap();

        let report = inspect_purge_archive(PurgeArchiveInspectOptions {
            archive: archive.clone(),
            expect_tenant_id: Some("t".to_string()),
            expect_project_id: Some("p".to_string()),
            min_records: Some(2),
        })
        .unwrap();

        assert_eq!(report.status, "verified");
        assert_eq!(report.archive_path, archive.display().to_string());
        assert_eq!(report.record_count, 2);
        assert_eq!(report.payload_available_count, 1);
        assert_eq!(report.payload_missing_count, 1);
        assert_eq!(report.records_without_recoverable_text, 0);
        assert_eq!(report.reasons["hidden_retention_candidate"], 1);
        assert_eq!(report.reasons["unreadable_active_payload"], 1);
        assert!(report.estimated_recoverable_text_bytes > 0);
        assert!(
            render_purge_archive_inspection(&report, ExportFormat::Markdown)
                .unwrap()
                .contains("archive_sha256")
        );
    }

    #[test]
    fn inspect_purge_archive_rejects_count_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("purge.json");
        std::fs::write(
            &archive,
            r#"{
  "archive_format": "memd_purge_archive_v1",
  "tenant_id": "t",
  "project_id": null,
  "record_count": 2,
  "records": []
}"#,
        )
        .unwrap();

        let err = inspect_purge_archive(PurgeArchiveInspectOptions {
            archive,
            expect_tenant_id: Some("t".to_string()),
            expect_project_id: None,
            min_records: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("record_count mismatch"));
    }

    #[test]
    fn inspect_purge_archive_rejects_payload_flag_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("purge.json");
        std::fs::write(
            &archive,
            r#"{
  "archive_format": "memd_purge_archive_v1",
  "tenant_id": "t",
  "project_id": null,
  "record_count": 1,
  "records": [
    {
      "reason": "hidden_retention_candidate",
      "metadata": {
        "chunk_id": "c1",
        "tenant_id": "t",
        "project_id": null,
        "canonical_text": "payload"
      },
      "payload_available": true,
      "payload": null
    }
  ]
}"#,
        )
        .unwrap();

        let err = inspect_purge_archive(PurgeArchiveInspectOptions {
            archive,
            expect_tenant_id: None,
            expect_project_id: None,
            min_records: None,
        })
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("payload_available=true but payload is null"));
    }
}
