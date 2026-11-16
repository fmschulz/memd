//! Which embedder revision produced a store's dense vectors.
//!
//! `<data_dir>/embedding_revision` holds the revision of the vectors in the
//! per-tenant `warm_index` directories. When it differs from
//! [`EMBEDDING_REVISION`], stored vectors and new query vectors no longer
//! come from the same function, so a writable open discards the dense
//! indexes and the store re-embeds every chunk from its segments. The
//! marker reads `<revision> rebuilding` until that pass has finished, so an
//! interrupted rebuild resumes instead of starting over.
//! `embedding_model` separately records the model identity; dimensions alone
//! cannot distinguish MiniLM from BGE-small.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::embeddings::{CandleModel, EMBEDDING_REVISION};
use crate::error::{MemdError, Result};
use crate::index::EmbeddingCache;

const MARKER_FILE: &str = "embedding_revision";
const REBUILDING_SUFFIX: &str = " rebuilding";
const MODEL_MARKER_FILE: &str = "embedding_model";

fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MARKER_FILE)
}

fn write_marker(data_dir: &Path, content: &str) -> Result<()> {
    write_named_marker(data_dir, MARKER_FILE, content)
}

fn write_named_marker(data_dir: &Path, name: &str, content: &str) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    let tmp = data_dir.join(format!("{name}.tmp"));
    let mut file = fs::File::create(&tmp)?;
    file.write_all(content.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&tmp, data_dir.join(name))?;
    #[cfg(unix)]
    fs::File::open(data_dir)?.sync_all()?;
    Ok(())
}

/// Check model identity before loading any dense vectors or migrating revisions.
/// Returns whether the writer must finish a segment-backed rebuild.
pub(crate) fn prepare_model(
    data_dir: &Path,
    model: CandleModel,
    read_only: bool,
    backfill: bool,
) -> Result<bool> {
    let path = data_dir.join(MODEL_MARKER_FILE);
    let recorded = match fs::read_to_string(&path) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let current = model.hf_id();
    let mut stored_model = current.to_string();
    let mut resetting = false;
    if let Some(recorded) = &recorded {
        let (identity, state) = recorded
            .trim()
            .split_once(' ')
            .unwrap_or((recorded.trim(), ""));
        stored_model = identity.to_string();
        resetting = state == "resetting";
        if identity == current && !resetting {
            return Ok(state == "rebuilding");
        }
    }
    let indexes = dense_index_dirs(data_dir)?;
    if recorded.is_none() {
        // Before model markers existed, these were the only two live models.
        // Inspect every tenant so even a legacy mixed store cannot pass.
        for index in &indexes {
            let cache_path = index.join("embeddings.bin");
            if !cache_path.exists() {
                continue;
            }
            let cache = EmbeddingCache::load_from(&cache_path)?;
            let legacy_model = match cache.dimension() {
                384 => CandleModel::MiniLm,
                768 => CandleModel::BgeBase,
                dimension => {
                    return Err(MemdError::ValidationError(format!(
                        "unmarked dense index at {} has unknown embedding dimension {dimension}",
                        index.display()
                    )))
                }
            };
            stored_model = legacy_model.hf_id().to_string();
            if stored_model != current {
                break;
            }
        }
    }
    if stored_model != current || resetting {
        if read_only || (!backfill && !(resetting && stored_model == current)) {
            return Err(MemdError::DenseIndexEmbeddingModelMismatch {
                path,
                store_model: if resetting {
                    format!("{stored_model} (reset incomplete)")
                } else {
                    stored_model
                },
                active_model: current.to_string(),
            });
        }
        // Persist reset intent first. A crash during deletion repeats the whole
        // reset; only after every old tenant cache is gone can new vectors load.
        write_named_marker(data_dir, MODEL_MARKER_FILE, &format!("{current} resetting"))?;
        for index in indexes {
            fs::remove_dir_all(&index)?;
            #[cfg(unix)]
            fs::File::open(index.parent().unwrap())?.sync_all()?;
        }
        write_named_marker(
            data_dir,
            MODEL_MARKER_FILE,
            &format!("{current}{REBUILDING_SUFFIX}"),
        )?;
        return Ok(true);
    }
    if !read_only {
        mark_model_current(data_dir, model)?;
    }
    Ok(false)
}

/// Called only after the rebuilt dense indexes have been saved.
pub(crate) fn mark_model_current(data_dir: &Path, model: CandleModel) -> Result<()> {
    write_named_marker(data_dir, MODEL_MARKER_FILE, model.hf_id())
}

/// Dense index directories that hold persisted state.
fn dense_index_dirs(data_dir: &Path) -> Result<Vec<PathBuf>> {
    let tenants = match fs::read_dir(data_dir.join("tenants")) {
        Ok(tenants) => tenants,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut indexes = Vec::new();
    for tenant in tenants {
        let index = tenant?.path().join("warm_index");
        match fs::read_dir(&index) {
            Ok(mut entries) => {
                if let Some(entry) = entries.next() {
                    entry?;
                    indexes.push(index);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(indexes)
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

    let stale = dense_index_dirs(data_dir)?;
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

    fn seed_model_vectors(data_dir: &Path, dimension: usize) -> PathBuf {
        let index = data_dir.join("tenants/test/warm_index");
        fs::create_dir_all(&index).unwrap();
        let mut cache = crate::index::embedding_cache::EmbeddingCache::new(dimension);
        cache.insert(0, &vec![0.25; dimension]).unwrap();
        cache.save_to(&index.join("embeddings.bin")).unwrap();
        index
    }

    #[test]
    fn bge_small_rejects_same_dimension_model_and_preserves_cache() {
        let dir = tempfile::tempdir().unwrap();
        let index = seed_model_vectors(dir.path(), 384);
        let before = fs::read(index.join("embeddings.bin")).unwrap();
        let error = prepare_model(dir.path(), CandleModel::BgeSmall, false, false).unwrap_err();
        assert!(error.to_string().contains("all-MiniLM-L6-v2"));
        assert!(error.to_string().contains("bge-small-en-v1.5"));
        assert!(error
            .to_string()
            .contains("MEMD_BACKFILL_HNSW_ON_STARTUP=1"));
        assert_eq!(fs::read(index.join("embeddings.bin")).unwrap(), before);
        assert!(!dir.path().join("embedding_model").exists());
        // Read-only opens refuse the switch even with the opt-in.
        assert!(prepare_model(dir.path(), CandleModel::BgeSmall, true, true).is_err());
        assert_eq!(fs::read(index.join("embeddings.bin")).unwrap(), before);
    }

    #[test]
    fn bge_small_rebuild_clears_all_tenant_caches_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let index = seed_model_vectors(dir.path(), 384);
        let second = dir.path().join("tenants/second/warm_index");
        fs::create_dir_all(&second).unwrap();
        fs::copy(index.join("embeddings.bin"), second.join("embeddings.bin")).unwrap();
        assert!(prepare_model(dir.path(), CandleModel::BgeSmall, false, true).unwrap());
        assert!(!index.exists());
        assert!(!second.exists());
        assert!(fs::read_to_string(dir.path().join("embedding_model"))
            .unwrap()
            .contains("bge-small-en-v1.5 rebuilding"));
        let rebuilt = seed_model_vectors(dir.path(), 384);
        // A restart continues the new model's partial cache, without another opt-in.
        assert!(prepare_model(dir.path(), CandleModel::BgeSmall, false, false).unwrap());
        assert!(rebuilt.join("embeddings.bin").exists());
        mark_model_current(dir.path(), CandleModel::BgeSmall).unwrap();
        assert!(!prepare_model(dir.path(), CandleModel::BgeSmall, false, false).unwrap());
        // Cached vectors must never be reused in the reverse direction either.
        assert!(prepare_model(dir.path(), CandleModel::MiniLm, false, false).is_err());
    }

    #[test]
    fn bge_small_model_marker_infers_existing_models_on_writer_open_only() {
        for (dimension, model) in [(384, CandleModel::MiniLm), (768, CandleModel::BgeBase)] {
            let dir = tempfile::tempdir().unwrap();
            let index = seed_model_vectors(dir.path(), dimension);
            assert!(!prepare_model(dir.path(), model, true, false).unwrap());
            assert!(!dir.path().join("embedding_model").exists());
            assert!(!prepare_model(dir.path(), model, false, false).unwrap());
            assert_eq!(
                fs::read_to_string(dir.path().join("embedding_model"))
                    .unwrap()
                    .trim(),
                model.hf_id()
            );
            assert!(index.join("embeddings.bin").exists());
        }
    }

    #[test]
    fn bge_small_interrupted_reset_cannot_load_surviving_old_cache() {
        let dir = tempfile::tempdir().unwrap();
        let old = seed_model_vectors(dir.path(), 384);
        write_named_marker(
            dir.path(),
            MODEL_MARKER_FILE,
            &format!("{} resetting", CandleModel::BgeSmall.hf_id()),
        )
        .unwrap();
        assert!(prepare_model(dir.path(), CandleModel::BgeSmall, true, false).is_err());
        assert!(old.exists());
        assert!(prepare_model(dir.path(), CandleModel::BgeSmall, false, false).unwrap());
        assert!(!old.exists());
    }

    #[test]
    fn bge_small_cannot_mark_a_store_when_tenant_scan_fails() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("tenants"), b"not a tenant directory").unwrap();
        assert!(prepare_model(dir.path(), CandleModel::BgeSmall, false, true).is_err());
        assert!(!dir.path().join(MODEL_MARKER_FILE).exists());
    }

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
