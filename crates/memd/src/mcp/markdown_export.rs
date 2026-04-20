//! Track G — pure markdown projection.
//!
//! `render_markdown_tree` groups a slice of `MemoryChunk`s into one
//! markdown file per `(project_id, chunk_type)` bucket and produces
//! `(path, content)` tuples ready for the caller to either return over
//! MCP (G2) or write to disk (G3 in the CLI).
//!
//! No IO is performed here.

use crate::types::MemoryChunk;
use std::collections::BTreeMap;

/// One rendered file in the export tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFile {
    /// Repo-relative POSIX path, e.g. `"by_project/p1/doc.md"` or
    /// `"no_project/doc.md"`. Always uses forward slashes so the same
    /// output is portable across platforms.
    pub path: String,
    /// Full file contents — YAML frontmatter (per-bucket header) plus
    /// one `## chunk_id` block per chunk.
    pub content: String,
}

/// Group `chunks` into `(project, chunk_type)` buckets and render each
/// bucket as a markdown document. Pure — same input always yields the
/// same output, sorted deterministically by path.
pub fn render_markdown_tree(chunks: &[MemoryChunk]) -> Vec<RenderedFile> {
    // BTreeMap ordering keeps the output stable across runs.
    let mut buckets: BTreeMap<(String, String), Vec<&MemoryChunk>> = BTreeMap::new();
    for c in chunks {
        let project_segment = c
            .project_id
            .as_option()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "no_project".to_string());
        let project_segment = sanitize_path_segment(&project_segment);
        let type_segment = c.chunk_type.to_string();
        buckets
            .entry((project_segment, type_segment))
            .or_default()
            .push(c);
    }

    // Sort buckets so project-scoped files come before the catch-all
    // `no_project` bucket — purely a UX choice; the BTreeMap iteration
    // order would put `no_project` first because 'n' < 'p'.
    let mut ordered: Vec<((String, String), Vec<&MemoryChunk>)> = buckets.into_iter().collect();
    ordered.sort_by(|a, b| {
        let a_no = a.0 .0 == "no_project";
        let b_no = b.0 .0 == "no_project";
        match (a_no, b_no) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => a.0.cmp(&b.0),
        }
    });

    let mut out = Vec::with_capacity(ordered.len());
    for ((project_segment, type_segment), mut bucket_chunks) in ordered {
        // Stable per-bucket order: oldest first.
        bucket_chunks.sort_by_key(|c| c.timestamp_created);

        let path = if project_segment == "no_project" {
            format!("no_project/{type_segment}.md")
        } else {
            format!("by_project/{project_segment}/{type_segment}.md")
        };

        let tenant_label = bucket_chunks
            .first()
            .map(|c| c.tenant_id.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let mut content = String::new();
        content.push_str("---\n");
        content.push_str(&format!("tenant: {tenant_label}\n"));
        if project_segment == "no_project" {
            content.push_str("project: null\n");
        } else {
            content.push_str(&format!("project: {project_segment}\n"));
        }
        content.push_str(&format!("chunk_type: {type_segment}\n"));
        content.push_str(&format!("count: {}\n", bucket_chunks.len()));
        content.push_str("---\n\n");

        for chunk in bucket_chunks {
            content.push_str(&format!("## {}\n\n", chunk.chunk_id));
            content.push_str(&format!(
                "- timestamp_created: {}\n",
                chunk.timestamp_created
            ));
            content.push_str(&format!("- hash: {}\n", chunk.hash));
            content.push_str(&format!("- ingestion_mode: {}\n", chunk.ingestion_mode));
            if !chunk.tags.is_empty() {
                content.push_str(&format!("- tags: {}\n", chunk.tags.join(", ")));
            }
            content.push('\n');
            content.push_str(chunk.text.trim_end());
            content.push_str("\n\n");
        }

        out.push(RenderedFile { path, content });
    }
    out
}

/// Restrict path segments to a safe-on-every-OS character set so a
/// hostile project_id can't escape the tree via `..` or absolute
/// paths. Keep alnum + `_-`. Replace anything else with `_`.
fn sanitize_path_segment(segment: &str) -> String {
    let cleaned: String = segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChunkType, MemoryChunk, ProjectId, TenantId};

    fn make(tenant: &str, project: Option<&str>, ct: ChunkType, text: &str) -> MemoryChunk {
        let chunk = MemoryChunk::new(TenantId::new(tenant).unwrap(), text, ct);
        match project {
            Some(p) => chunk.with_project(ProjectId::new(Some(p.to_string()))),
            None => chunk,
        }
    }

    #[test]
    fn render_markdown_tree_groups_by_project_and_type() {
        let chunks = vec![
            make("t", Some("p1"), ChunkType::Doc, "hello doc one"),
            make("t", Some("p1"), ChunkType::Code, "fn a() {}"),
            make("t", Some("p2"), ChunkType::Doc, "p2 doc"),
            make("t", None, ChunkType::Doc, "unscoped doc"),
        ];
        let out = render_markdown_tree(&chunks);
        let paths: Vec<&str> = out.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "by_project/p1/code.md",
                "by_project/p1/doc.md",
                "by_project/p2/doc.md",
                "no_project/doc.md",
            ]
        );
    }

    #[test]
    fn render_markdown_tree_includes_yaml_frontmatter_and_chunk_blocks() {
        let chunk = make("t", Some("p1"), ChunkType::Doc, "Hello world");
        let out = render_markdown_tree(std::slice::from_ref(&chunk));
        assert_eq!(out.len(), 1);
        let f = &out[0];
        assert!(f.content.starts_with("---\ntenant: t\nproject: p1\n"));
        assert!(f.content.contains(&format!("## {}", chunk.chunk_id)));
        assert!(f.content.contains("Hello world"));
    }

    #[test]
    fn render_markdown_tree_returns_empty_vec_for_empty_input() {
        assert!(render_markdown_tree(&[]).is_empty());
    }

    #[test]
    fn render_markdown_tree_sanitizes_hostile_project_ids() {
        let evil = make("t", Some("../../etc/passwd"), ChunkType::Doc, "x");
        let out = render_markdown_tree(std::slice::from_ref(&evil));
        let path = out[0].path.as_str();
        assert!(
            !path.contains(".."),
            "sanitized path must not contain ..: {path}"
        );
        assert!(
            !path.contains('/') || path.starts_with("by_project/"),
            "sanitized segment must not introduce extra slashes: {path}"
        );
    }
}
