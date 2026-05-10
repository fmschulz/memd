//! Markdown projection helpers shared by CLI export surfaces.

use serde::{Deserialize, Serialize};

use crate::types::{MemoryChunk, TenantId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownProjectionFile {
    pub path: String,
    pub content: String,
}

pub fn filter_projection_chunks(
    chunks: Vec<MemoryChunk>,
    project_id: Option<&str>,
    max_chunks: usize,
) -> Vec<MemoryChunk> {
    chunks
        .into_iter()
        .filter(|chunk| {
            project_id
                .map(|project| chunk.project_id.as_option() == Some(project))
                .unwrap_or(true)
        })
        .take(max_chunks)
        .collect()
}

pub fn build_markdown_projection(
    chunks: &[MemoryChunk],
    tenant: &TenantId,
    project_id: Option<&str>,
) -> Vec<MarkdownProjectionFile> {
    let mut files = Vec::with_capacity(chunks.len().saturating_add(1));
    files.push(MarkdownProjectionFile {
        path: "index.md".to_string(),
        content: render_projection_index(chunks, tenant, project_id),
    });

    for chunk in chunks {
        files.push(MarkdownProjectionFile {
            path: format!("chunks/{}.md", chunk.chunk_id),
            content: render_chunk_markdown(chunk),
        });
    }

    files
}

fn render_projection_index(
    chunks: &[MemoryChunk],
    tenant: &TenantId,
    project_id: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("# memd markdown projection\n\n");
    out.push_str(&format!("- tenant_id: `{tenant}`\n"));
    if let Some(project_id) = project_id {
        out.push_str(&format!("- project_id: `{project_id}`\n"));
    }
    out.push_str(&format!("- chunk_count: `{}`\n\n", chunks.len()));
    out.push_str("## Chunks\n\n");

    for chunk in chunks {
        out.push_str(&format!(
            "- [{}](chunks/{}.md) - `{}`",
            chunk.chunk_id, chunk.chunk_id, chunk.chunk_type
        ));
        if let Some(project) = chunk.project_id.as_option() {
            out.push_str(&format!(" - `{project}`"));
        }
        out.push('\n');
    }

    out
}

fn render_chunk_markdown(chunk: &MemoryChunk) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", chunk.chunk_id));
    out.push_str(&format!("- tenant_id: `{}`\n", chunk.tenant_id));
    out.push_str(&format!("- type: `{}`\n", chunk.chunk_type));
    out.push_str(&format!("- project_id: `{}`\n", chunk.project_id));
    out.push_str(&format!(
        "- timestamp_created_ms: `{}`\n",
        chunk.timestamp_created
    ));
    if let Some(path) = &chunk.source.path {
        out.push_str(&format!("- source_path: `{path}`\n"));
    }
    if chunk.tags.is_empty() {
        out.push_str("- tags: `<none>`\n\n");
    } else {
        out.push_str(&format!("- tags: `{}`\n\n", chunk.tags.join(", ")));
    }

    out.push_str("## Text\n\n");
    for line in chunk.text.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChunkType, ProjectId};

    fn chunk(text: &str, project_id: Option<&str>) -> MemoryChunk {
        let chunk = MemoryChunk::new(TenantId::new("projection").unwrap(), text, ChunkType::Doc);
        if let Some(project_id) = project_id {
            chunk.with_project(ProjectId::from(project_id))
        } else {
            chunk
        }
    }

    #[test]
    fn filter_projection_chunks_filters_project_before_limit() {
        let chunks = vec![
            chunk("other project", Some("other")),
            chunk("first match", Some("proj_a")),
            chunk("second match", Some("proj_a")),
        ];

        let filtered = filter_projection_chunks(chunks, Some("proj_a"), 1);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].text, "first match");
    }
}
