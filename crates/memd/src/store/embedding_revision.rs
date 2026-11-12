//! Which embedder revision produced a store's dense vectors.
//!
//! `<data_dir>/embedding_revision` holds the revision of the vectors in the
//! per-tenant `warm_index` directories. When it differs from
//! [`EMBEDDING_REVISION`], stored vectors and new query vectors no longer
//! come from the same function, so a writable open discards the dense
//! indexes and the store re-embeds every chunk from its segments. The
//! marker reads `<revision> rebuilding` until that pass has finished, so an
//! interrupted rebuild resumes instead of starting over.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::embeddings::EMBEDDING_REVISION;
use crate::error::Result;

const MARKER_FILE: &str = "embedding_revision";
const REBUILDING_SUFFIX: &str = " rebuilding";

fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MARKER_FILE)
}

fn write_marker(data_dir: &Path, content: &str) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    let tmp = data_dir.join(format!("{MARKER_FILE}.tmp"));
    let mut file = fs::File::create(&tmp)?;
    file.write_all(content.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&tmp, marker_path(data_dir))?;
    Ok(())
}

/// Dense index directories that hold persisted state.
fn dense_index_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(tenants) = fs::read_dir(data_dir.join("tenants")) else {
        return Vec::new();
    };
    tenants
        .flatten()
        .map(|tenant| tenant.path().join("warm_index"))
        .filter(|dir| {
            fs::read_dir(dir)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false)
        })
        .collect()
}

/// Bring the dense indexes in line with the current embedder revision.
///
/// Call on a writable open, before any tenant's dense index is loaded.
/// Returns `true` when the dense indexes must be rebuilt from segments; the
/// caller schedules that and calls [`mark_current`] when it has finished.
pub(crate) fn prepare(data_dir: &Path) -> Result<bool> {
    let current = EMBEDDING_REVISION.to_string();
    let rebuilding = format!("{current}{REBUILDING_SUFFIX}");
    let recorded = fs::read_to_string(marker_path(data_dir)).ok();
    match recorded.as_deref().map(str::trim) {
        Some(value) if value == current => return Ok(false),
        Some(value) if value == rebuilding => return Ok(true),
        _ => {}
    }

    let stale = dense_index_dirs(data_dir);
    if stale.is_empty() {
        // Nothing was embedded under another revision.
        write_marker(data_dir, &current)?;
        return Ok(false);
    }

    // Remove first, mark second: a crash in between repeats the removal.
    for dir in &stale {
        fs::remove_dir_all(dir)?;
    }
    write_marker(data_dir, &rebuilding)?;
    tracing::warn!(
        recorded = recorded.as_deref().map(str::trim).unwrap_or("none"),
        current = %current,
        tenants = stale.len(),
        "dense indexes were built by another embedder revision; re-embedding from segments"
    );
    Ok(true)
}

/// Record that every dense index now holds vectors of the current revision.
pub(crate) fn mark_current(data_dir: &Path) -> Result<()> {
    write_marker(data_dir, &EMBEDDING_REVISION.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_dense_index(data_dir: &Path, tenant: &str) -> PathBuf {
        let dir = data_dir.join("tenants").join(tenant).join("warm_index");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("embeddings.bin"), b"old vectors").unwrap();
        dir
    }

    fn marker(data_dir: &Path) -> String {
        fs::read_to_string(marker_path(data_dir))
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn fresh_store_is_marked_current_without_a_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!prepare(tmp.path()).unwrap());
        assert_eq!(marker(tmp.path()), EMBEDDING_REVISION.to_string());
        assert!(!prepare(tmp.path()).unwrap());
    }

    #[test]
    fn unmarked_store_with_vectors_is_discarded_and_rebuilt() {
        let tmp = tempfile::tempdir().unwrap();
        let index = seed_dense_index(tmp.path(), "tenant_a");

        assert!(prepare(tmp.path()).unwrap());
        assert!(
            !index.exists(),
            "vectors of the old revision must be removed"
        );
        assert!(marker(tmp.path()).ends_with("rebuilding"));
    }

    #[test]
    fn interrupted_rebuild_resumes_without_discarding_progress() {
        let tmp = tempfile::tempdir().unwrap();
        seed_dense_index(tmp.path(), "tenant_a");
        assert!(prepare(tmp.path()).unwrap());

        // Vectors written by the resumed rebuild must survive the next open.
        let rebuilt = seed_dense_index(tmp.path(), "tenant_a");
        assert!(prepare(tmp.path()).unwrap());
        assert!(rebuilt.join("embeddings.bin").exists());

        mark_current(tmp.path()).unwrap();
        assert!(!prepare(tmp.path()).unwrap());
        assert!(rebuilt.join("embeddings.bin").exists());
    }

    #[test]
    fn older_revision_marker_triggers_a_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        let index = seed_dense_index(tmp.path(), "tenant_a");
        write_marker(tmp.path(), "1").unwrap();

        assert!(prepare(tmp.path()).unwrap());
        assert!(!index.exists());
    }
}
