//! `--embedding-model` is optional: a run uses the flag when given, else the
//! model the store records, else `bge-small` for a new store.
//!
//! Every command here runs with an empty `HOME`, so none of them can read or
//! download a model file. They open the store and stop before any embedding.

use std::path::Path;
use std::process::{Command, Output};

const MINILM: &str = "sentence-transformers/all-MiniLM-L6-v2";
const BGE_SMALL: &str = "BAAI/bge-small-en-v1.5";
const MISMATCH: &str = "was built with embedding model";

/// A writer command that opens the store, then fails because the chunk does
/// not exist. It never embeds text.
fn open_writer(data_dir: &Path, home: &Path, model_flag: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memd"));
    command.env("HOME", home).env("XDG_CACHE_HOME", home);
    command.env_remove("MEMD_BACKFILL_HNSW_ON_STARTUP");
    command.arg("--data-dir").arg(data_dir);
    if let Some(model) = model_flag {
        command.args(["--embedding-model", model]);
    }
    command
        .args([
            "delete",
            "--tenant-id",
            "t",
            "--chunk-id",
            "00000000-0000-0000-0000-000000000001",
            "--warm",
            "off",
        ])
        .output()
        .expect("run memd")
}

fn marker(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(data_dir.join("embedding_model"))
        .ok()
        .map(|text| text.trim().to_string())
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

#[test]
fn new_store_without_a_flag_records_bge_small() {
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = open_writer(data.path(), home.path(), None);
    assert!(!stderr(&output).contains(MISMATCH), "{}", stderr(&output));
    assert_eq!(marker(data.path()).as_deref(), Some(BGE_SMALL));
}

#[test]
fn new_store_with_a_flag_records_that_model() {
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    open_writer(data.path(), home.path(), Some("all-minilm"));
    assert_eq!(marker(data.path()).as_deref(), Some(MINILM));
}

#[test]
fn minilm_store_without_a_flag_keeps_minilm() {
    let (data, home) = store_marked(MINILM);
    let output = open_writer(data.path(), home.path(), None);
    assert!(!stderr(&output).contains(MISMATCH), "{}", stderr(&output));
    assert_eq!(marker(data.path()).as_deref(), Some(MINILM));
}

#[test]
fn bge_small_store_without_a_flag_keeps_bge_small() {
    let (data, home) = store_marked(BGE_SMALL);
    let output = open_writer(data.path(), home.path(), None);
    assert!(!stderr(&output).contains(MISMATCH), "{}", stderr(&output));
    assert_eq!(marker(data.path()).as_deref(), Some(BGE_SMALL));
}

#[test]
fn explicit_flag_that_differs_from_the_store_still_fails() {
    let (data, home) = store_marked(MINILM);
    let output = open_writer(data.path(), home.path(), Some("bge-small"));
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains(MISMATCH), "{message}");
    assert!(message.contains(BGE_SMALL), "{message}");
    assert_eq!(marker(data.path()).as_deref(), Some(MINILM));
}

/// The switch must load the new model before it deletes the old dense index.
/// The model cache sits below a regular file, so no model file can be read or
/// fetched, with or without a network.
#[test]
fn failed_switch_keeps_the_old_marker_and_dense_index() {
    let (data, home) = store_marked(MINILM);
    let index = data.path().join("tenants/t/warm_index");
    std::fs::create_dir_all(&index).unwrap();
    let mut cache = memd::index::EmbeddingCache::new(384);
    cache.insert(0, &[0.25; 384]).unwrap();
    cache.save_to(&index.join("embeddings.bin")).unwrap();
    let vectors = std::fs::read(index.join("embeddings.bin")).unwrap();
    let blocker = home.path().join("not-a-directory");
    std::fs::write(&blocker, b"x").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_memd"))
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", blocker.join("cache"))
        .env("MEMD_BACKFILL_HNSW_ON_STARTUP", "1")
        .arg("--data-dir")
        .arg(data.path())
        .args(["--embedding-model", "bge-small"])
        .args(["delete", "--tenant-id", "t", "--warm", "off"])
        .args(["--chunk-id", "00000000-0000-0000-0000-000000000001"])
        .output()
        .expect("run memd");
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains("not switching the dense index"),
        "{message}"
    );
    assert_eq!(marker(data.path()).as_deref(), Some(MINILM));
    assert_eq!(
        std::fs::read(index.join("embeddings.bin")).unwrap(),
        vectors
    );
}

#[test]
fn help_names_no_fixed_default_for_the_embedding_model() {
    let output = Command::new(env!("CARGO_BIN_EXE_memd"))
        .arg("--help")
        .output()
        .expect("run memd --help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    let (_, rest) = help
        .split_once("--embedding-model")
        .expect("help lists --embedding-model");
    let (section, _) = rest
        .split_once("--search-variant")
        .expect("help lists --search-variant after it");
    assert!(!section.contains("[default"), "{section}");
    assert!(section.contains("bge-small"), "{section}");
    assert!(section.contains("store"), "{section}");
}
