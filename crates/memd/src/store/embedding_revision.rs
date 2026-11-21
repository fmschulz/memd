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
//! cannot distinguish MiniLM from BGE-small. A command that names no model
//! uses the one this marker records ([`recorded_embedding_model`]), so an
//! existing store keeps its model and only an explicit switch changes it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::embeddings::{
    CandleEmbedder, CandleModel, Embedder, EmbeddingConfig, EMBEDDING_REVISION,
};
use crate::error::{MemdError, Result};
use crate::index::EmbeddingCache;

const MARKER_FILE: &str = "embedding_revision";
const REBUILDING_SUFFIX: &str = " rebuilding";
const MODEL_MARKER_FILE: &str = "embedding_model";

/// Every model a marker can name.
const KNOWN_MODELS: [CandleModel; 3] = [
    CandleModel::MiniLm,
    CandleModel::BgeBase,
    CandleModel::BgeSmall,
];

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

/// The model marker, if the store has one.
fn read_model_marker(data_dir: &Path) -> Result<Option<String>> {
    match fs::read_to_string(data_dir.join(MODEL_MARKER_FILE)) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Split a marker into the model identity and its state (`rebuilding`,
/// `resetting`, or empty).
fn split_model_marker(recorded: &str) -> (&str, &str) {
    let recorded = recorded.trim();
    recorded.split_once(' ').unwrap_or((recorded, ""))
}

/// The model of an unmarked dense index, from its vector width. Before model
/// markers existed, these were the only two live models. Only the validated
/// file header is read, so the cost does not grow with the index; the loader
/// validates the vectors when the index loads.
fn legacy_model(index: &Path) -> Result<Option<CandleModel>> {
    let cache_path = index.join("embeddings.bin");
    if !cache_path.exists() {
        return Ok(None);
    }
    match EmbeddingCache::read_dimension(&cache_path)? {
        384 => Ok(Some(CandleModel::MiniLm)),
        768 => Ok(Some(CandleModel::BgeBase)),
        dimension => Err(MemdError::ValidationError(format!(
            "unmarked dense index at {} has unknown embedding dimension {dimension}",
            index.display()
        ))),
    }
}

/// The model a store records, for a command that names none.
///
/// This is the `embedding_model` marker or, for an unmarked store with dense
/// vectors, the legacy width rule that [`prepare_model`] applies. `None`
/// means the store has neither: a new store, free to take the default. It only
/// reads. A marker that names a model this build does not know is an error,
/// not a reason to pick another model.
pub fn recorded_embedding_model(data_dir: &Path) -> Result<Option<CandleModel>> {
    if let Some(recorded) = read_model_marker(data_dir)? {
        let (identity, _) = split_model_marker(&recorded);
        return KNOWN_MODELS
            .into_iter()
            .find(|model| model.hf_id() == identity)
            .map(Some)
            .ok_or_else(|| {
                MemdError::ValidationError(format!(
                    "{} names the embedding model {identity:?}, which this memd does not know",
                    data_dir.join(MODEL_MARKER_FILE).display()
                ))
            });
    }
    for index in dense_index_dirs(data_dir)? {
        if let Some(model) = legacy_model(&index)? {
            return Ok(Some(model));
        }
    }
    Ok(None)
}

/// Load `model` for real: read or download its files, build it, and embed one
/// text. A switch calls this before it deletes the old dense index.
///
/// It runs on a thread of its own because a store can be opened from inside a
/// Tokio runtime, where a second `block_on` would panic.
pub(crate) fn model_loads(model: CandleModel) -> Result<()> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let embedder =
                    CandleEmbedder::with_config_and_model(EmbeddingConfig::default(), model);
                runtime
                    .block_on(embedder.embed_texts(&["memd model check"]))
                    .map(|_| ())
            })
            .join()
            .unwrap_or_else(|_| Err(MemdError::EmbeddingError("the model check panicked".into())))
    })
}

/// Check model identity before loading any dense vectors or migrating revisions.
/// Returns whether the writer must finish a segment-backed rebuild.
///
/// A switch to another model deletes the old dense indexes. `verify_model`
/// runs first and must prove that the new model loads, so a machine that
/// cannot fetch or read its files keeps its index and its marker.
pub(crate) fn prepare_model(
    data_dir: &Path,
    model: CandleModel,
    read_only: bool,
    backfill: bool,
    verify_model: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    let path = data_dir.join(MODEL_MARKER_FILE);
    let recorded = read_model_marker(data_dir)?;
    let current = model.hf_id();
    let mut stored_model = current.to_string();
    let mut resetting = false;
    if let Some(recorded) = &recorded {
        let (identity, state) = split_model_marker(recorded);
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
            let Some(legacy) = legacy_model(index)? else {
                continue;
            };
            stored_model = legacy.hf_id().to_string();
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
        verify_model().map_err(|error| {
            MemdError::EmbeddingError(format!(
                "not switching the dense index to {current}: the model failed to load ({error}); \
                 the existing index and model marker are unchanged"
            ))
        })?;
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

/// Record that some chunks have no vector because the embedding model could
/// not load. The marker then reads `<revision> rebuilding`, so the next writer
/// whose model loads resumes the segment-backed rebuild. That pass embeds only
/// the chunks that lack a vector and marks the store current when it is done.
///
/// A current or unmarked store moves to `rebuilding`; an already rebuilding
/// store stays. A marker of another revision is left alone: the next writer
/// open discards those vectors and re-embeds every chunk from the segments,
/// which covers these chunks as well.
pub(crate) fn mark_rebuilding(data_dir: &Path) -> Result<()> {
    let current = EMBEDDING_REVISION.to_string();
    let rebuilding = format!("{current}{REBUILDING_SUFFIX}");
    let recorded = fs::read_to_string(marker_path(data_dir)).ok();
    match recorded.as_deref().map(str::trim) {
        Some(value) if value == rebuilding => Ok(()),
        Some(value) if value != current => Ok(()),
        _ => write_marker(data_dir, &rebuilding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `prepare_model` for a model whose files load.
    fn prepare_loadable(
        data_dir: &Path,
        model: CandleModel,
        read_only: bool,
        backfill: bool,
    ) -> Result<bool> {
        prepare_model(data_dir, model, read_only, backfill, || Ok(()))
    }

    #[test]
    fn mark_rebuilding_moves_only_a_current_or_unmarked_store() {
        let dir = tempfile::tempdir().unwrap();
        let revision = EMBEDDING_REVISION;
        let marker = || fs::read_to_string(marker_path(dir.path())).unwrap();

        mark_rebuilding(dir.path()).unwrap();
        assert_eq!(marker().trim(), format!("{revision} rebuilding"));
        // Already rebuilding: the writer that finds it has nothing to add.
        mark_rebuilding(dir.path()).unwrap();
        assert_eq!(marker().trim(), format!("{revision} rebuilding"));
        // The next writer open resumes it; a finished rebuild is current again.
        assert!(prepare(dir.path()).unwrap());
        mark_current(dir.path()).unwrap();
        mark_rebuilding(dir.path()).unwrap();
        assert_eq!(marker().trim(), format!("{revision} rebuilding"));

        // A marker of another revision is left to the next writer open: it
        // discards the old vectors and re-embeds every chunk, which covers
        // the chunks this call would have recorded.
        write_marker(dir.path(), "0").unwrap();
        mark_rebuilding(dir.path()).unwrap();
        assert_eq!(marker().trim(), "0");
    }

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
        let error = prepare_loadable(dir.path(), CandleModel::BgeSmall, false, false).unwrap_err();
        assert!(error.to_string().contains("all-MiniLM-L6-v2"));
        assert!(error.to_string().contains("bge-small-en-v1.5"));
        assert!(error
            .to_string()
            .contains("MEMD_BACKFILL_HNSW_ON_STARTUP=1"));
        assert_eq!(fs::read(index.join("embeddings.bin")).unwrap(), before);
        assert!(!dir.path().join("embedding_model").exists());
        // Read-only opens refuse the switch even with the opt-in.
        assert!(prepare_loadable(dir.path(), CandleModel::BgeSmall, true, true).is_err());
        assert_eq!(fs::read(index.join("embeddings.bin")).unwrap(), before);
    }

    #[test]
    fn bge_small_rebuild_clears_all_tenant_caches_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let index = seed_model_vectors(dir.path(), 384);
        let second = dir.path().join("tenants/second/warm_index");
        fs::create_dir_all(&second).unwrap();
        fs::copy(index.join("embeddings.bin"), second.join("embeddings.bin")).unwrap();
        assert!(prepare_loadable(dir.path(), CandleModel::BgeSmall, false, true).unwrap());
        assert!(!index.exists());
        assert!(!second.exists());
        assert!(fs::read_to_string(dir.path().join("embedding_model"))
            .unwrap()
            .contains("bge-small-en-v1.5 rebuilding"));
        let rebuilt = seed_model_vectors(dir.path(), 384);
        // A restart continues the new model's partial cache, without another opt-in.
        assert!(prepare_loadable(dir.path(), CandleModel::BgeSmall, false, false).unwrap());
        assert!(rebuilt.join("embeddings.bin").exists());
        mark_model_current(dir.path(), CandleModel::BgeSmall).unwrap();
        assert!(!prepare_loadable(dir.path(), CandleModel::BgeSmall, false, false).unwrap());
        // Cached vectors must never be reused in the reverse direction either.
        assert!(prepare_loadable(dir.path(), CandleModel::MiniLm, false, false).is_err());
    }

    #[test]
    fn bge_small_model_marker_infers_existing_models_on_writer_open_only() {
        for (dimension, model) in [(384, CandleModel::MiniLm), (768, CandleModel::BgeBase)] {
            let dir = tempfile::tempdir().unwrap();
            let index = seed_model_vectors(dir.path(), dimension);
            assert!(!prepare_loadable(dir.path(), model, true, false).unwrap());
            assert!(!dir.path().join("embedding_model").exists());
            assert!(!prepare_loadable(dir.path(), model, false, false).unwrap());
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
        assert!(prepare_loadable(dir.path(), CandleModel::BgeSmall, true, false).is_err());
        assert!(old.exists());
        assert!(prepare_loadable(dir.path(), CandleModel::BgeSmall, false, false).unwrap());
        assert!(!old.exists());
    }

    #[test]
    fn bge_small_cannot_mark_a_store_when_tenant_scan_fails() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("tenants"), b"not a tenant directory").unwrap();
        assert!(prepare_loadable(dir.path(), CandleModel::BgeSmall, false, true).is_err());
        assert!(!dir.path().join(MODEL_MARKER_FILE).exists());
    }

    /// A store that records `model` and holds one dense index of `dimension`.
    fn seed_marked_store(data_dir: &Path, model: CandleModel, dimension: usize) -> PathBuf {
        write_named_marker(data_dir, MODEL_MARKER_FILE, model.hf_id()).unwrap();
        seed_model_vectors(data_dir, dimension)
    }

    fn model_marker(data_dir: &Path) -> String {
        fs::read_to_string(data_dir.join(MODEL_MARKER_FILE)).unwrap()
    }

    fn never_loaded() -> Result<()> {
        panic!("the model must not load on this path")
    }

    #[test]
    fn new_store_records_no_model_and_the_lookup_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(recorded_embedding_model(dir.path()).unwrap(), None);
        // A tenant without dense vectors does not count as a recorded model.
        fs::create_dir_all(dir.path().join("tenants/test/segments")).unwrap();
        assert_eq!(recorded_embedding_model(dir.path()).unwrap(), None);
        assert!(!dir.path().join(MODEL_MARKER_FILE).exists());
    }

    #[test]
    fn marked_store_resolves_its_model_and_opens_untouched() {
        for model in [CandleModel::MiniLm, CandleModel::BgeSmall] {
            let dir = tempfile::tempdir().unwrap();
            let index = seed_marked_store(dir.path(), model, 384);
            let vectors = fs::read(index.join("embeddings.bin")).unwrap();
            let marker = model_marker(dir.path());

            let resolved = recorded_embedding_model(dir.path()).unwrap().unwrap();
            assert_eq!(resolved, model);
            // Same open a flagless command makes: writable, no opt-in, no load.
            assert!(!prepare_model(dir.path(), resolved, false, false, never_loaded).unwrap());

            assert_eq!(model_marker(dir.path()), marker);
            assert_eq!(fs::read(index.join("embeddings.bin")).unwrap(), vectors);
        }
    }

    #[test]
    fn marker_state_suffix_does_not_change_the_resolved_model() {
        for state in ["rebuilding", "resetting"] {
            let dir = tempfile::tempdir().unwrap();
            write_named_marker(
                dir.path(),
                MODEL_MARKER_FILE,
                &format!("{} {state}", CandleModel::BgeSmall.hf_id()),
            )
            .unwrap();
            assert_eq!(
                recorded_embedding_model(dir.path()).unwrap(),
                Some(CandleModel::BgeSmall)
            );
        }
    }

    #[test]
    fn unknown_marker_identity_is_an_error_not_a_default() {
        let dir = tempfile::tempdir().unwrap();
        write_named_marker(dir.path(), MODEL_MARKER_FILE, "acme/future-embedder-v9").unwrap();
        let error = recorded_embedding_model(dir.path()).unwrap_err();
        assert!(error.to_string().contains("acme/future-embedder-v9"));
    }

    #[test]
    fn unmarked_store_resolves_its_model_from_the_vector_width() {
        for (dimension, model) in [(384, CandleModel::MiniLm), (768, CandleModel::BgeBase)] {
            let dir = tempfile::tempdir().unwrap();
            let index = seed_model_vectors(dir.path(), dimension);
            let vectors = fs::read(index.join("embeddings.bin")).unwrap();

            assert_eq!(recorded_embedding_model(dir.path()).unwrap(), Some(model));

            // A lookup is a read: it records nothing and leaves the vectors alone.
            assert!(!dir.path().join(MODEL_MARKER_FILE).exists());
            assert_eq!(fs::read(index.join("embeddings.bin")).unwrap(), vectors);
        }
        let dir = tempfile::tempdir().unwrap();
        seed_model_vectors(dir.path(), 100);
        assert!(recorded_embedding_model(dir.path()).is_err());
    }

    /// Bytes of the fixed `embeddings.bin` header: magic, version, dimension,
    /// count, next id and the header CRC.
    const CACHE_HEADER_LEN: u64 = 24;

    fn cache_file(index: &Path) -> PathBuf {
        index.join("embeddings.bin")
    }

    /// The model is read from the header, so a body that fails the data CRC
    /// does not stop the discovery. The index loader validates the body.
    #[test]
    fn unmarked_store_resolves_its_model_from_the_header_when_the_body_is_damaged() {
        for damage in ["truncated", "corrupt"] {
            for (dimension, model) in [(384, CandleModel::MiniLm), (768, CandleModel::BgeBase)] {
                let dir = tempfile::tempdir().unwrap();
                let index = seed_model_vectors(dir.path(), dimension);
                let path = cache_file(&index);
                if damage == "truncated" {
                    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
                    file.set_len(CACHE_HEADER_LEN + 3).unwrap();
                } else {
                    let mut bytes = fs::read(&path).unwrap();
                    let last = bytes.len() - 1;
                    bytes[last] ^= 0xFF;
                    fs::write(&path, bytes).unwrap();
                }
                let damaged = fs::read(&path).unwrap();

                assert_eq!(
                    recorded_embedding_model(dir.path()).unwrap(),
                    Some(model),
                    "{damage} body, {dimension}-d"
                );
                // A writer open applies the same rule, so it marks the store.
                assert!(!prepare_loadable(dir.path(), model, false, false).unwrap());
                assert_eq!(model_marker(dir.path()).trim(), model.hf_id());
                assert_eq!(
                    fs::read(&path).unwrap(),
                    damaged,
                    "the cache is not touched"
                );
            }
        }
    }

    #[test]
    fn unmarked_store_with_a_bad_cache_header_is_an_error() {
        for fault in ["magic", "version", "header crc", "short"] {
            let dir = tempfile::tempdir().unwrap();
            let index = seed_model_vectors(dir.path(), 384);
            let path = cache_file(&index);
            let mut bytes = fs::read(&path).unwrap();
            match fault {
                "magic" => bytes[0] ^= 0xFF,
                "version" => bytes[4] ^= 0xFF,
                "header crc" => bytes[8] ^= 0xFF,
                _ => bytes.truncate(10),
            }
            fs::write(&path, bytes).unwrap();

            assert!(
                recorded_embedding_model(dir.path()).is_err(),
                "{fault} fault must not resolve a model"
            );
            assert!(prepare_loadable(dir.path(), CandleModel::MiniLm, false, false).is_err());
            assert!(!dir.path().join(MODEL_MARKER_FILE).exists());
        }
    }

    #[test]
    fn explicit_model_that_differs_from_the_marker_fails_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let index = seed_marked_store(dir.path(), CandleModel::MiniLm, 384);
        let vectors = fs::read(index.join("embeddings.bin")).unwrap();
        let marker = model_marker(dir.path());

        let error = prepare_model(
            dir.path(),
            CandleModel::BgeSmall,
            false,
            false,
            never_loaded,
        )
        .unwrap_err();
        assert!(error.to_string().contains("all-MiniLM-L6-v2"));
        assert!(error.to_string().contains("bge-small-en-v1.5"));
        // A read-only open refuses the switch even with the opt-in.
        assert!(
            prepare_model(dir.path(), CandleModel::BgeSmall, true, true, never_loaded).is_err()
        );

        assert_eq!(model_marker(dir.path()), marker);
        assert_eq!(fs::read(index.join("embeddings.bin")).unwrap(), vectors);
    }

    #[test]
    fn switch_loads_the_new_model_before_it_deletes_the_old_index() {
        let dir = tempfile::tempdir().unwrap();
        let index = seed_marked_store(dir.path(), CandleModel::MiniLm, 384);
        let vectors = fs::read(index.join("embeddings.bin")).unwrap();
        let marker = model_marker(dir.path());

        // The new model's files are missing: the switch stops with a clear
        // error and the store keeps its model, marker and index.
        let error = prepare_model(dir.path(), CandleModel::BgeSmall, false, true, || {
            Err(MemdError::EmbeddingError("no network".to_string()))
        })
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("no network"), "{message}");
        assert!(message.contains("bge-small-en-v1.5"), "{message}");
        assert_eq!(model_marker(dir.path()), marker);
        assert_eq!(fs::read(index.join("embeddings.bin")).unwrap(), vectors);
        assert_eq!(
            recorded_embedding_model(dir.path()).unwrap(),
            Some(CandleModel::MiniLm)
        );

        // The files load: the switch deletes the old index and starts the rebuild.
        let loaded = std::cell::Cell::new(false);
        let rebuild = prepare_model(dir.path(), CandleModel::BgeSmall, false, true, || {
            assert!(
                index.exists(),
                "the old index must still exist at load time"
            );
            loaded.set(true);
            Ok(())
        })
        .unwrap();
        assert!(rebuild);
        assert!(loaded.get());
        assert!(!index.exists());
        assert!(model_marker(dir.path()).contains("bge-small-en-v1.5 rebuilding"));
    }

    #[test]
    fn after_a_switch_a_flagless_open_resolves_the_new_model() {
        let dir = tempfile::tempdir().unwrap();
        seed_marked_store(dir.path(), CandleModel::MiniLm, 384);
        assert!(prepare_model(dir.path(), CandleModel::BgeSmall, false, true, || Ok(())).unwrap());
        // Mid-rebuild a restart continues with the new model, no opt-in.
        let resolved = recorded_embedding_model(dir.path()).unwrap().unwrap();
        assert_eq!(resolved, CandleModel::BgeSmall);
        assert!(prepare_model(dir.path(), resolved, false, false, never_loaded).unwrap());

        let rebuilt = seed_model_vectors(dir.path(), 384);
        mark_model_current(dir.path(), CandleModel::BgeSmall).unwrap();
        let resolved = recorded_embedding_model(dir.path()).unwrap().unwrap();
        assert_eq!(resolved, CandleModel::BgeSmall);
        assert!(!prepare_model(dir.path(), resolved, false, false, never_loaded).unwrap());
        assert!(rebuilt.join("embeddings.bin").exists());
        assert_eq!(
            model_marker(dir.path()).trim(),
            CandleModel::BgeSmall.hf_id()
        );
    }

    #[test]
    fn resuming_an_interrupted_reset_loads_the_model_before_deleting_more() {
        let dir = tempfile::tempdir().unwrap();
        let old = seed_model_vectors(dir.path(), 384);
        write_named_marker(
            dir.path(),
            MODEL_MARKER_FILE,
            &format!("{} resetting", CandleModel::BgeSmall.hf_id()),
        )
        .unwrap();
        let outcome = prepare_model(dir.path(), CandleModel::BgeSmall, false, false, || {
            Err(MemdError::EmbeddingError("no network".to_string()))
        });
        assert!(outcome.is_err());
        assert!(old.exists());
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
