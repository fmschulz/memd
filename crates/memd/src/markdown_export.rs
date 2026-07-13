//! Track G — pure markdown projection.
//!
//! `render_markdown_tree` groups a slice of `MemoryChunk`s into one
//! markdown file per `(project_id, chunk_type)` bucket and produces
//! `(path, content)` tuples ready for the caller to either return over
//! MCP (G2) or write to disk (G3 in the CLI).
//!
//! No IO is performed here.

use crate::types::MemoryChunk;
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

type MarkdownBucket<'a> = ((Option<String>, String), Vec<&'a MemoryChunk>);

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
///
/// Bucket key uses the **raw** `Option<String>` project_id so distinct
/// projects whose sanitized names would collide (e.g. `"a/b"` vs
/// `"a:b"` both → `"a_b"`) stay in separate files. Path generation
/// then sanitizes for OS portability and appends a short hash
/// disambiguator when sanitization is lossy or matches a Windows
/// reserved name. Unscoped chunks (project_id = None) go into a
/// dedicated `no_project/` tree that no project_id can collide with.
pub fn render_markdown_tree(chunks: &[MemoryChunk]) -> Vec<RenderedFile> {
    let mut buckets: BTreeMap<(Option<String>, String), Vec<&MemoryChunk>> = BTreeMap::new();
    for c in chunks {
        let raw_project = c.project_id.as_option().map(|s| s.to_string());
        let type_segment = c.chunk_type.to_string();
        buckets
            .entry((raw_project, type_segment))
            .or_default()
            .push(c);
    }

    // Sort: project-scoped first, no_project last; stable within each
    // group by raw project then type.
    let mut ordered: Vec<MarkdownBucket<'_>> = buckets.into_iter().collect();
    ordered.sort_by(|a, b| match (a.0 .0.as_ref(), b.0 .0.as_ref()) {
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => a.0.cmp(&b.0),
    });

    let mut out = Vec::with_capacity(ordered.len());
    for ((raw_project, type_segment), mut bucket_chunks) in ordered {
        bucket_chunks.sort_by_key(|c| c.timestamp_created);

        let path = match raw_project.as_deref() {
            Some(raw) => {
                let safe = safe_project_segment(raw);
                format!("by_project/{safe}/{type_segment}.md")
            }
            None => format!("no_project/{type_segment}.md"),
        };

        let tenant_label = bucket_chunks
            .first()
            .map(|c| c.tenant_id.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let mut content = String::new();
        content.push_str("---\n");
        content.push_str(&format!("tenant: {tenant_label}\n"));
        match raw_project.as_deref() {
            Some(raw) => content.push_str(&format!("project: {raw}\n")),
            None => content.push_str("project: null\n"),
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

/// Build an OS-portable path segment for `raw`. Always alnum + `_-`;
/// when sanitization is lossy or the cleaned name matches a Windows
/// reserved name, append a short stable hash suffix so distinct raw
/// project_ids never collide on disk.
fn safe_project_segment(raw: &str) -> String {
    let cleaned = sanitize_path_segment(raw);
    let needs_disambiguation = cleaned != raw || is_reserved_basename(&cleaned);
    if needs_disambiguation {
        let mut h = DefaultHasher::new();
        raw.hash(&mut h);
        let suffix = format!("{:x}", h.finish());
        // Truncate to 8 hex chars — enough to make accidental
        // collisions astronomically rare while keeping paths short.
        let short = &suffix[..suffix.len().min(8)];
        format!("{cleaned}__{short}")
    } else {
        cleaned
    }
}

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

/// Windows reserves these device names regardless of extension. We
/// disambiguate any segment that would equal one (case-insensitive).
fn is_reserved_basename(s: &str) -> bool {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let upper = s.to_ascii_uppercase();
    RESERVED.iter().any(|r| *r == upper)
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
        let segments: Vec<&str> = path.split('/').collect();
        assert_eq!(segments[0], "by_project", "first segment fixed");
        assert!(segments.len() == 3, "exactly two slashes, no extra: {path}");
    }

    // Codex round-1 G1 HIGH regression: distinct raw project_ids that
    // sanitise to the same characters must NOT merge into one bucket
    // (they had previously collapsed when the bucket key was the
    // sanitised string).
    #[test]
    fn render_markdown_tree_keeps_collision_prone_project_ids_distinct() {
        let a = make("t", Some("a/b"), ChunkType::Doc, "from a/b");
        let b = make("t", Some("a:b"), ChunkType::Doc, "from a:b");
        let out = render_markdown_tree(&[a, b]);
        assert_eq!(
            out.len(),
            2,
            "raw projects 'a/b' and 'a:b' must produce two files, got {}",
            out.len()
        );
        let paths: Vec<&str> = out.iter().map(|f| f.path.as_str()).collect();
        assert_ne!(paths[0], paths[1], "paths must differ — got {paths:?}");
        // Each rendered file's frontmatter carries the *raw* project_id.
        assert!(out[0].content.contains("project: a/b") || out[0].content.contains("project: a:b"));
        assert!(out[1].content.contains("project: a/b") || out[1].content.contains("project: a:b"));
    }

    // Codex round-1 G1 HIGH regression: a real project_id of
    // "no_project" must not collapse into the unscoped sentinel
    // bucket. The bucket key is now Option<String>, so this is
    // structurally impossible — but a regression test documents the
    // contract.
    #[test]
    fn render_markdown_tree_real_no_project_id_does_not_collapse_to_unscoped() {
        let scoped = make("t", Some("no_project"), ChunkType::Doc, "scoped");
        let unscoped = make("t", None, ChunkType::Doc, "unscoped");
        let out = render_markdown_tree(&[scoped, unscoped]);
        let paths: Vec<&str> = out.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths.len(), 2, "must remain two distinct buckets");
        assert!(
            paths.iter().any(|p| p.starts_with("by_project/")),
            "scoped bucket present"
        );
        assert!(
            paths.iter().any(|p| p.starts_with("no_project/")),
            "unscoped bucket present"
        );
    }

    // Codex round-1 G1 HIGH regression: Windows reserved basenames
    // (CON, NUL, ...) must be disambiguated.
    #[test]
    fn render_markdown_tree_disambiguates_windows_reserved_basenames() {
        let con = make("t", Some("CON"), ChunkType::Doc, "x");
        let out = render_markdown_tree(std::slice::from_ref(&con));
        let path = out[0].path.as_str();
        assert!(
            path.starts_with("by_project/CON__"),
            "reserved basename must carry hash suffix, got {path}"
        );
    }
}
