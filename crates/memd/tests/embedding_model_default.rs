//! `--embedding-model` is optional: a run uses the flag when given, else the
//! model the store records, else `bge-small` for a new store.
//!
//! Every command here runs with an empty `HOME`, so none of them can read or
//! download a model file. They open the store and stop before any embedding.
//! Each test asserts its own outcome: the exit status, the output or error
//! message, and the marker. An unrelated failure fails the test.

use std::path::Path;
use std::process::{Command, Output};

const MINILM: &str = "sentence-transformers/all-MiniLM-L6-v2";
const BGE_SMALL: &str = "BAAI/bge-small-en-v1.5";
const BGE_BASE: &str = "BAAI/bge-base-en-v1.5";
const CHUNK_ID: &str = "00000000-0000-0000-0000-000000000001";

/// A writer command that opens the store and runs: the chunk does not exist,
/// so it succeeds with `"deleted": false`. It never embeds text.
fn open_writer(data_dir: &Path, home: &Path, model_flag: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memd"));
    command.env("HOME", home).env("XDG_CACHE_HOME", home);
    command.env_remove("MEMD_BACKFILL_HNSW_ON_STARTUP");
    command.arg("--data-dir").arg(data_dir);
    if let Some(model) = model_flag {
        command.args(["--embedding-model", model]);
    }
    command
        .args(["delete", "--tenant-id", "t", "--chunk-id", CHUNK_ID])
        .args(["--warm", "off"])
        .output()
        .expect("run memd")
}

fn marker(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(data_dir.join("embedding_model"))
        .ok()
        .map(|text| text.trim().to_string())
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn store_marked(model: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("embedding_model"), format!("{model}\n")).unwrap();
    (data, home)
}

/// The command opened the store and ran: exit 0, and the delete reports that
/// no chunk matched.
fn assert_ran(output: &Output) {
    assert!(output.status.success(), "stderr: {}", stderr(output));
    assert!(
        stdout(output).contains("\"deleted\": false"),
        "stdout: {}",
        stdout(output)
    );
    assert!(
        stderr(output).trim().is_empty(),
        "stderr: {}",
        stderr(output)
    );
}

/// The command stopped at the model check: exit 1, the mismatch message that
/// names both models, and no output from the command itself.
fn assert_refused(output: &Output, store_model: &str, active_model: &str) {
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(output));
    let message = stderr(output);
    assert!(
        message.contains(&format!(
            "was built with embedding model {store_model}, but the active model is {active_model}."
        )),
        "{message}"
    );
    assert!(
        stdout(output).trim().is_empty(),
        "stdout: {}",
        stdout(output)
    );
}

/// A tenant index with one vector of `dimension`, as a store from before the
/// model marker holds it. The embedder revision is current, so the separate
/// revision rebuild leaves the index alone.
fn write_dense_index(data_dir: &Path, dimension: usize) -> (std::path::PathBuf, Vec<u8>) {
    std::fs::write(
        data_dir.join("embedding_revision"),
        format!("{}\n", memd::embeddings::EMBEDDING_REVISION),
    )
    .unwrap();
    let index = data_dir.join("tenants/t/warm_index");
    std::fs::create_dir_all(&index).unwrap();
    let mut cache = memd::index::EmbeddingCache::new(dimension);
    cache.insert(0, &vec![0.25; dimension]).unwrap();
    let file = index.join("embeddings.bin");
    cache.save_to(&file).unwrap();
    let bytes = std::fs::read(&file).unwrap();
    (file, bytes)
}

#[test]
fn new_store_without_a_flag_records_bge_small() {
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    assert_ran(&open_writer(data.path(), home.path(), None));
    assert_eq!(marker(data.path()).as_deref(), Some(BGE_SMALL));
}

#[test]
fn new_store_with_a_flag_records_that_model() {
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    assert_ran(&open_writer(data.path(), home.path(), Some("all-minilm")));
    assert_eq!(marker(data.path()).as_deref(), Some(MINILM));
}

#[test]
fn minilm_store_without_a_flag_keeps_minilm() {
    let (data, home) = store_marked(MINILM);
    assert_ran(&open_writer(data.path(), home.path(), None));
    assert_eq!(marker(data.path()).as_deref(), Some(MINILM));
}

#[test]
fn bge_small_store_without_a_flag_keeps_bge_small() {
    let (data, home) = store_marked(BGE_SMALL);
    assert_ran(&open_writer(data.path(), home.path(), None));
    assert_eq!(marker(data.path()).as_deref(), Some(BGE_SMALL));
}

#[test]
fn unmarked_store_without_a_flag_keeps_the_model_of_its_vector_width() {
    for (dimension, model) in [(384, MINILM), (768, BGE_BASE)] {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let (file, vectors) = write_dense_index(data.path(), dimension);
        assert_ran(&open_writer(data.path(), home.path(), None));
        assert_eq!(marker(data.path()).as_deref(), Some(model), "{dimension}-d");
        assert_eq!(std::fs::read(file).unwrap(), vectors, "{dimension}-d");
    }
}

#[test]
fn explicit_flag_that_differs_from_the_store_still_fails() {
    let (data, home) = store_marked(MINILM);
    assert_refused(
        &open_writer(data.path(), home.path(), Some("bge-small")),
        MINILM,
        BGE_SMALL,
    );
    assert_eq!(marker(data.path()).as_deref(), Some(MINILM));
}

/// The switch must load the new model before it deletes the old dense index.
/// The model cache sits below a regular file, so no model file can be read or
/// fetched, with or without a network.
#[test]
fn failed_switch_keeps_the_old_marker_and_dense_index() {
    let (data, home) = store_marked(MINILM);
    let (file, vectors) = write_dense_index(data.path(), 384);
    let blocker = home.path().join("not-a-directory");
    std::fs::write(&blocker, b"x").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_memd"))
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", blocker.join("cache"))
        .env("MEMD_BACKFILL_HNSW_ON_STARTUP", "1")
        .arg("--data-dir")
        .arg(data.path())
        .args(["--embedding-model", "bge-small"])
        .args(["delete", "--tenant-id", "t", "--chunk-id", CHUNK_ID])
        .args(["--warm", "off"])
        .output()
        .expect("run memd");
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let message = stderr(&output);
    assert!(
        message.contains(
            "failed to create persistent store: embedding error: not switching the dense index \
             to BAAI/bge-small-en-v1.5: the model failed to load"
        ),
        "{message}"
    );
    assert!(
        message.contains("the existing index and model marker are unchanged"),
        "{message}"
    );
    assert!(
        stdout(&output).trim().is_empty(),
        "stdout: {}",
        stdout(&output)
    );
    assert_eq!(marker(data.path()).as_deref(), Some(MINILM));
    assert_eq!(std::fs::read(file).unwrap(), vectors);
}

#[test]
fn help_names_no_fixed_default_for_the_embedding_model() {
    let output = Command::new(env!("CARGO_BIN_EXE_memd"))
        .arg("--help")
        .output()
        .expect("run memd --help");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let help = stdout(&output);
    let (_, rest) = help
        .split_once("--embedding-model")
        .expect("help lists --embedding-model");
    let (section, _) = rest
        .split_once("--search-variant")
        .expect("help lists --search-variant after it");
    assert!(!section.contains("[default"), "{section}");
    let section = section.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        section.contains(
            "Without this flag a store keeps the model it records, and a new store uses bge-small."
        ),
        "{section}"
    );
}
