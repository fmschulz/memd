use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use memd::store::persistent::{PersistentStore, PersistentStoreConfig};
use memd::MemdError;

fn light_config(data_dir: &Path) -> PersistentStoreConfig {
    PersistentStoreConfig {
        data_dir: data_dir.to_path_buf(),
        read_only: false,
        enable_dense_search: false,
        enable_hybrid_search: false,
        enable_tiered_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    }
}

#[test]
fn writer_lock_blocks_second_process() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("store");
    std::fs::create_dir_all(&data_dir).unwrap();

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_holder_helper", "--nocapture"])
        .env("MEMD_LOCK_HOLDER_DIR", &data_dir)
        .spawn()
        .expect("spawn lock holder helper");

    let marker = data_dir.join("holder_ready");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        if Instant::now() >= deadline {
            let status = child.try_wait().expect("poll child");
            panic!("lock holder did not become ready within 30s; child status: {status:?}");
        }
        if let Some(status) = child.try_wait().expect("poll child") {
            panic!("lock holder exited before ready: {status}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // `std::env::set_var` is process-global — it leaks into every test running concurrently in this test binary. Before adding more `#[test]` fns to this file, this must be switched to per-command/per-store config injection (or the tests must be forced serial), otherwise the timeout races across tests.
    std::env::set_var("MEMD_WRITER_LOCK_TIMEOUT_MS", "500");
    let err = match PersistentStore::open(light_config(&data_dir)) {
        Ok(store) => {
            drop(store);
            let status = child.wait().expect("wait child");
            panic!("second writer unexpectedly acquired lock; child status: {status}");
        }
        Err(err) => err,
    };
    std::env::remove_var("MEMD_WRITER_LOCK_TIMEOUT_MS");

    match &err {
        MemdError::WriterLockHeld { holder, .. } => {
            assert!(
                holder.contains(&format!("pid={}", child.id())),
                "holder line should name child pid {}; holder={holder:?}",
                child.id()
            );
        }
        other => panic!("expected WriterLockHeld, got {other:?}"),
    }
    assert!(
        err.to_string().contains(&format!("pid={}", child.id())),
        "display should include holder pid: {err}"
    );

    let status = child.wait().expect("wait child");
    assert!(status.success(), "lock holder failed: {status}");
}

#[test]
fn lock_holder_helper() {
    let Some(data_dir) = std::env::var_os("MEMD_LOCK_HOLDER_DIR") else {
        return;
    };
    let data_dir = std::path::PathBuf::from(data_dir);
    let _store = PersistentStore::open(light_config(&data_dir)).expect("holder opens writer store");
    std::fs::write(data_dir.join("holder_ready"), b"ready\n").expect("write ready marker");
    std::thread::sleep(Duration::from_secs(8));
}
