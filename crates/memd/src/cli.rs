//! CLI mode for direct tool invocation
//!
//! Provides command-line interface for manual testing and debugging
//! without MCP protocol overhead.

use clap::Subcommand;
use serde_json::json;
use tracing::info;

use crate::error::Result;
use crate::store::{Store, TenantManager};
use crate::types::{ChunkId, ChunkType, MemoryChunk, ProjectId, Source, TenantId};

/// CLI subcommands for memory operations
#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Add a memory chunk
    Add {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Text content of the chunk
        #[arg(long)]
        text: String,

        /// Type of chunk (code, doc, trace, decision, plan, research, message, summary, other)
        #[arg(long, value_parser = parse_chunk_type)]
        chunk_type: ChunkType,

        /// Optional project identifier
        #[arg(long)]
        project_id: Option<String>,

        /// Optional tags (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,

        /// Optional source URI
        #[arg(long)]
        source_uri: Option<String>,

        /// Optional source path
        #[arg(long)]
        source_path: Option<String>,
    },

    /// Search memory chunks
    Search {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Search query
        #[arg(long)]
        query: String,

        /// Maximum number of results
        #[arg(long, default_value = "10")]
        k: usize,
    },

    /// Get a chunk by ID
    Get {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Chunk identifier (UUID)
        #[arg(long)]
        chunk_id: String,
    },

    /// Delete a chunk (soft delete)
    Delete {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,

        /// Chunk identifier (UUID)
        #[arg(long)]
        chunk_id: String,
    },

    /// Show statistics for a tenant
    Stats {
        /// Tenant identifier
        #[arg(long)]
        tenant_id: String,
    },
}

/// Parse chunk type from string
fn parse_chunk_type(s: &str) -> std::result::Result<ChunkType, String> {
    match s.to_lowercase().as_str() {
        "code" => Ok(ChunkType::Code),
        "doc" => Ok(ChunkType::Doc),
        "trace" => Ok(ChunkType::Trace),
        "decision" => Ok(ChunkType::Decision),
        "plan" => Ok(ChunkType::Plan),
        "research" => Ok(ChunkType::Research),
        "message" => Ok(ChunkType::Message),
        "summary" => Ok(ChunkType::Summary),
        "other" => Ok(ChunkType::Other),
        _ => Err(format!(
            "invalid chunk type '{}', must be one of: code, doc, trace, decision, plan, research, message, summary, other",
            s
        )),
    }
}

/// Run a CLI command
///
/// Executes the specified command against the store and prints JSON output.
pub async fn run_cli<S: Store>(
    store: &S,
    tenant_manager: Option<&TenantManager>,
    cmd: CliCommand,
) -> Result<()> {
    match cmd {
        CliCommand::Add {
            tenant_id,
            text,
            chunk_type,
            project_id,
            tags,
            source_uri,
            source_path,
        } => {
            let tenant = TenantId::new(&tenant_id)?;

            // Ensure tenant directory exists
            if let Some(tm) = tenant_manager {
                tm.ensure_tenant_dir(&tenant)?;
            }

            let mut chunk = MemoryChunk::new(tenant, &text, chunk_type);

            if let Some(pid) = project_id {
                chunk = chunk.with_project(ProjectId::new(Some(pid)));
            }

            if let Some(t) = tags {
                chunk = chunk.with_tags(t);
            }

            if source_uri.is_some() || source_path.is_some() {
                let source = Source {
                    uri: source_uri,
                    path: source_path,
                    ..Default::default()
                };
                chunk = chunk.with_source(source);
            }

            let chunk_id = store.add(chunk).await?;
            info!(chunk_id = %chunk_id, "chunk added");

            let output = json!({
                "chunk_id": chunk_id.to_string()
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        CliCommand::Search {
            tenant_id,
            query,
            k,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let results = store.search(&tenant, &query, k).await?;

            info!(count = results.len(), "search complete");

            let output: Vec<_> = results
                .iter()
                .map(|c| {
                    json!({
                        "chunk_id": c.chunk_id.to_string(),
                        "text": c.text,
                        "chunk_type": c.chunk_type.to_string(),
                        "timestamp_created": c.timestamp_created,
                        "tags": c.tags,
                    })
                })
                .collect();

            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        CliCommand::Get {
            tenant_id,
            chunk_id,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let cid = ChunkId::parse(&chunk_id)?;
            let chunk = store.get(&tenant, &cid).await?;

            if let Some(c) = chunk {
                info!(chunk_id = %cid, "chunk found");
                println!("{}", serde_json::to_string_pretty(&c)?);
            } else {
                info!(chunk_id = %cid, "chunk not found");
                println!("null");
            }
        }

        CliCommand::Delete {
            tenant_id,
            chunk_id,
        } => {
            let tenant = TenantId::new(&tenant_id)?;
            let cid = ChunkId::parse(&chunk_id)?;
            let deleted = store.delete(&tenant, &cid).await?;

            info!(chunk_id = %cid, deleted = deleted, "delete operation");

            let output = json!({
                "deleted": deleted
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        CliCommand::Stats { tenant_id } => {
            let tenant = TenantId::new(&tenant_id)?;
            let stats = store.stats(&tenant).await?;

            info!(tenant_id = %tenant, "stats retrieved");

            let mut output = json!({
                "total_chunks": stats.total_chunks,
                "deleted_chunks": stats.deleted_chunks,
                "chunk_types": stats.chunk_types,
            });

            // Add disk stats if available
            if let Some(tm) = tenant_manager {
                if let Ok(disk_stats) = tm.tenant_disk_stats(&tenant) {
                    output["disk_stats"] = json!({
                        "total_bytes": disk_stats.total_bytes,
                        "segment_count": disk_stats.segment_count,
                    });
                }
            }

            println!("{}", serde_json::to_string_pretty(&output)?);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chunk_types() {
        assert!(matches!(parse_chunk_type("code"), Ok(ChunkType::Code)));
        assert!(matches!(parse_chunk_type("DOC"), Ok(ChunkType::Doc)));
        assert!(matches!(parse_chunk_type("Trace"), Ok(ChunkType::Trace)));
        assert!(parse_chunk_type("invalid").is_err());
    }
}
