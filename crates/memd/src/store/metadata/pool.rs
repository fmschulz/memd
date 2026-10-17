//! Lightweight SQLite connection pool (Phase 4.3).
//!
//! Before Phase 4.3 the metadata store held a single
//! `Mutex<Connection>` — every reader and writer serialized on the
//! same connection. With HTTP concurrency now unlocked (Phase 3.1),
//! the SQLite layer is the next serialization bottleneck.
//!
//! The standard Rust answer is `r2d2_sqlite`, but its latest release
//! pins `libsqlite3-sys ^0.37` while we depend on `rusqlite 0.38`
//! (which pulls in `libsqlite3-sys ^0.36`). That's a native-library
//! `links=sqlite3` conflict that can't be resolved without
//! coordinating a full sqlite ecosystem bump.
//!
//! This module is a minimal bespoke pool that gives us the same
//! behavior without the dependency conflict:
//!
//! * Bounded by `max_size` (default 16) to bound memory and fd use.
//! * Each connection is preconfigured with WAL / `NORMAL` /
//!   `busy_timeout = 5s` / 64MB cache — same PRAGMAs the old single
//!   connection used, applied once per connection creation. A read-only
//!   pool ([`SqliteConnectionPool::open_read_only`]) skips the WAL pragma
//!   because switching the journal mode is a write.
//! * Grow-on-demand up to `max_size`; when at cap and all idle
//!   connections are in use, `get()` blocks on a condition variable.
//! * `PooledConnection` auto-releases the underlying `Connection`
//!   on drop, returning it to the idle vec and notifying waiters.
//! * Callers can use `&Connection` (`conn.prepare(...)`) or
//!   `&mut Connection` (`conn.transaction()`) via `Deref` /
//!   `DerefMut`.
//!
//! The pool is sync — all call sites are inside synchronous
//! `MetadataStore` methods, so an async-aware pool would add no
//! value and would force runtime plumbing across the storage layer.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use rusqlite::{Connection, OpenFlags};

use crate::error::{MemdError, Result};

/// Default maximum number of connections held by the pool.
///
/// Can be overridden via `$MEMD_SQLITE_POOL_MAX`. A value of `1`
/// restores the pre-Phase-4.3 single-connection behavior.
pub const DEFAULT_POOL_MAX_SIZE: usize = 16;

/// The actual pool. Cloneable by wrapping in `Arc`. Pool state
/// (`idle` vec + `outstanding` count + `Condvar`) lives behind one
/// mutex to keep the acquire path a single critical section.
pub struct SqliteConnectionPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    db_path: PathBuf,
    open_flags: Option<OpenFlags>,
    state: Mutex<PoolState>,
    not_empty: Condvar,
    max_size: usize,
}

struct PoolState {
    idle: Vec<Connection>,
    /// Connections currently checked out to callers. `idle + outstanding`
    /// must never exceed `max_size`.
    outstanding: usize,
}

impl SqliteConnectionPool {
    /// Open a pool for the given database path. Creates one warm
    /// connection eagerly so the first caller does not pay the
    /// open+PRAGMA cost synchronously.
    pub fn open(db_path: &Path) -> Result<Self> {
        Self::open_with_max(db_path, Self::max_size_from_env())
    }

    fn max_size_from_env() -> usize {
        std::env::var("MEMD_SQLITE_POOL_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_POOL_MAX_SIZE)
    }

    /// Open a pool with an explicit cap. Exposed primarily for tests.
    pub fn open_with_max(db_path: &Path, max_size: usize) -> Result<Self> {
        Self::open_with_max_and_flags(db_path, max_size, None)
    }

    /// Open a URI-backed pool with an explicit cap. Used for shared-cache
    /// in-memory metadata databases.
    pub fn open_uri_with_max(uri: &str, max_size: usize) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_SHARED_CACHE;
        Self::open_with_max_and_flags(Path::new(uri), max_size, Some(flags))
    }

    /// Open a pool that can only read an existing database.
    ///
    /// The database opens with `SQLITE_OPEN_READ_ONLY`, so SQLite refuses
    /// every write. The journal mode stays as the writer left it and
    /// nothing creates or migrates tables.
    ///
    /// A WAL database also needs its `-shm` file. SQLite (3.22+) reads a WAL
    /// database from a read-only directory when `-wal` and `-shm` already
    /// exist, and it sees every committed frame in `-wal`. It cannot do so
    /// when both files are gone, which is the normal state of a database
    /// whose last connection closed cleanly. Only then, and only when the
    /// directory is not writable and `-wal` holds no frames, the pool opens
    /// the file with `immutable=1`. With no frames in `-wal`, the main file
    /// already holds every committed page, so no recent write is hidden.
    /// A directory that is writable never takes this path.
    ///
    /// `immutable=1` takes no locks and never re-reads the file. If a writer
    /// starts while the reader runs (for example in another mount namespace
    /// that can write the directory), the reader can see a stale snapshot or
    /// fail with a read error such as a corrupt or torn page. It never writes.
    ///
    /// SQLite keeps `-wal` and `-shm` next to the real file, so the path is
    /// resolved through symlinks first. The sidecar and directory checks then
    /// look at the files SQLite will use.
    pub fn open_read_only(db_path: &Path) -> Result<Self> {
        let max_size = Self::max_size_from_env();
        // A path that does not resolve opens, and fails, as given.
        let db_path = &std::fs::canonicalize(db_path).unwrap_or_else(|_| db_path.to_path_buf());
        let plain = Self::open_with_max_and_flags(db_path, max_size, Some(read_only_flags()));
        match plain {
            Ok(pool) => Ok(pool),
            Err(error) if immutable_open_is_safe(db_path) => {
                let uri = immutable_uri(db_path);
                Self::open_with_max_and_flags(Path::new(&uri), max_size, Some(read_only_flags()))
                    .map_err(|_| error)
            }
            Err(error) => Err(error),
        }
    }

    fn open_with_max_and_flags(
        db_path: &Path,
        max_size: usize,
        open_flags: Option<OpenFlags>,
    ) -> Result<Self> {
        assert!(max_size > 0, "SQLite pool max_size must be > 0");
        let warm = open_configured_connection(db_path, open_flags)?;
        let inner = Arc::new(PoolInner {
            db_path: db_path.to_path_buf(),
            open_flags,
            state: Mutex::new(PoolState {
                idle: vec![warm],
                outstanding: 0,
            }),
            not_empty: Condvar::new(),
            max_size,
        });
        Ok(Self { inner })
    }

    /// Acquire a connection. Blocks the calling thread when every
    /// connection is checked out and the pool is at `max_size`.
    ///
    /// Returned guard implements `Deref<Target = Connection>` (for
    /// read-style queries) and `DerefMut` (for transactions / other
    /// methods that need `&mut Connection`).
    pub fn get(&self) -> PooledConnection {
        // Fast path: grab an idle connection if any exists.
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(conn) = state.idle.pop() {
                state.outstanding += 1;
                return PooledConnection {
                    conn: Some(conn),
                    pool: Arc::clone(&self.inner),
                };
            }
            // Room to grow? Open a new connection inline.
            let total = state.idle.len() + state.outstanding;
            if total < self.inner.max_size {
                // Drop the lock during the open() syscall so other
                // threads can still acquire idle connections. Account
                // for the new connection up front so we do not race
                // past `max_size`.
                state.outstanding += 1;
                drop(state);

                match open_configured_connection(&self.inner.db_path, self.inner.open_flags) {
                    Ok(conn) => {
                        return PooledConnection {
                            conn: Some(conn),
                            pool: Arc::clone(&self.inner),
                        };
                    }
                    Err(err) => {
                        // Roll back the reservation on failure and
                        // re-raise.
                        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.outstanding -= 1;
                        self.inner.not_empty.notify_one();
                        tracing::error!(
                            error = %err,
                            path = %self.inner.db_path.display(),
                            "failed to open SQLite pool connection — panicking to surface the hard failure"
                        );
                        // In the old world this panicked via
                        // `.unwrap()`, so panic here to preserve that
                        // behavior instead of silently returning a
                        // sentinel.
                        panic!("SqliteConnectionPool: failed to open connection: {}", err);
                    }
                }
            }
            // Pool is saturated — wait for a release.
            state = self
                .inner
                .not_empty
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Current idle count. Primarily for diagnostics + tests.
    pub fn idle_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .idle
            .len()
    }

    /// Maximum configured size.
    pub fn max_size(&self) -> usize {
        self.inner.max_size
    }
}

impl Clone for SqliteConnectionPool {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// RAII guard around a checked-out connection. Returns the
/// connection to the pool on drop.
pub struct PooledConnection {
    conn: Option<Connection>,
    pool: Arc<PoolInner>,
}

impl Deref for PooledConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("PooledConnection used after drop")
    }
}

impl DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn
            .as_mut()
            .expect("PooledConnection used after drop")
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let mut state = self.pool.state.lock().unwrap_or_else(|e| e.into_inner());
            state.outstanding -= 1;
            state.idle.push(conn);
            self.pool.not_empty.notify_one();
        }
    }
}

/// Open a single SQLite connection with the PRAGMAs the metadata
/// store needs. Kept as a free function so both `open_with_max` and
/// the grow-on-demand path can call it.
fn open_configured_connection(path: &Path, flags: Option<OpenFlags>) -> Result<Connection> {
    let read_only = flags.is_some_and(|flags| flags.contains(OpenFlags::SQLITE_OPEN_READ_ONLY));
    let conn = match flags {
        Some(flags) => Connection::open_with_flags(path, flags),
        None => Connection::open(path),
    }
    .map_err(|e| MemdError::StorageError(format!("open sqlite at {}: {}", path.display(), e)))?;

    if read_only {
        // A sandbox can leave no writable temp directory, and SQLite then fails
        // a sort or index that spills with "disk I/O error". Keep temporary
        // tables and sort spills in memory. Pure reads fit.
        conn.pragma_update(None, "temp_store", "MEMORY")
            .map_err(|e| MemdError::StorageError(format!("set temp_store=MEMORY: {}", e)))?;
        // Touch the schema now: for a WAL database this is the first read, and
        // it is where SQLite reports a missing or unusable `-shm` file.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .map_err(|e| {
                MemdError::StorageError(format!("read sqlite at {}: {}", path.display(), e))
            })?;
    } else {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| MemdError::StorageError(format!("set journal_mode=WAL: {}", e)))?;
    }
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| MemdError::StorageError(format!("set synchronous=NORMAL: {}", e)))?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| MemdError::StorageError(format!("set busy_timeout=5000: {}", e)))?;
    conn.pragma_update(None, "cache_size", -64000)
        .map_err(|e| MemdError::StorageError(format!("set cache_size=-64000: {}", e)))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| MemdError::StorageError(format!("enable foreign_keys: {}", e)))?;

    Ok(conn)
}

fn read_only_flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX
}

/// True when `immutable=1` cannot hide committed data: the database
/// directory is not writable (so SQLite cannot create `-shm`) and `-wal`
/// is absent or empty. `db_path` is the resolved path of the real file.
fn immutable_open_is_safe(db_path: &Path) -> bool {
    let wal = wal_sidecar_path(db_path);
    let wal_has_frames = std::fs::metadata(&wal).is_ok_and(|metadata| metadata.len() > 0);
    let directory = db_path.parent().filter(|path| !path.as_os_str().is_empty());
    let directory_writable =
        crate::store::path_is_writable(directory.unwrap_or_else(|| Path::new(".")));
    !wal_has_frames && !directory_writable
}

fn wal_sidecar_path(db_path: &Path) -> PathBuf {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// `file:` URI for `db_path` with `immutable=1`. Percent-encodes every byte
/// except unreserved path characters, so SQLite never reads path text as URI
/// syntax.
fn immutable_uri(db_path: &Path) -> String {
    let mut uri = String::from("file:");
    for &byte in db_path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b'~') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    uri
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn pool_grows_and_returns_idle() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("test.db");

        let pool = SqliteConnectionPool::open_with_max(&db, 4).unwrap();
        // One warm connection was created eagerly, so idle == 1.
        assert_eq!(pool.idle_count(), 1);

        let c1 = pool.get();
        // After check-out, the warm connection is gone from idle.
        assert_eq!(pool.idle_count(), 0);

        let c2 = pool.get();
        let c3 = pool.get();
        // Three outstanding, still room to grow to 4.
        assert_eq!(pool.idle_count(), 0);

        // Drop one; it returns to idle.
        drop(c1);
        assert_eq!(pool.idle_count(), 1);

        drop(c2);
        drop(c3);
        assert_eq!(pool.idle_count(), 3);
    }

    fn wal_database(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1), (2);")
            .unwrap();
        conn
    }

    fn count_rows(pool: &SqliteConnectionPool) -> i64 {
        pool.get()
            .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn read_only_pool_rejects_writes_and_keeps_journal_mode() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("ro.db");
        drop(wal_database(&db));

        let pool = SqliteConnectionPool::open_read_only(&db).unwrap();
        assert_eq!(count_rows(&pool), 2);
        let conn = pool.get();
        assert!(conn.execute("INSERT INTO t VALUES (3)", []).is_err());
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn read_only_pool_keeps_temporary_storage_in_memory() {
        // A sandbox may have no writable temp dir; a spilled sort would then
        // fail with "disk I/O error". 2 is SQLite's MEMORY temp store.
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("temp.db");
        drop(wal_database(&db));

        let pool = SqliteConnectionPool::open_read_only(&db).unwrap();
        let temp_store: i64 = pool
            .get()
            .pragma_query_value(None, "temp_store", |row| row.get(0))
            .unwrap();
        assert_eq!(temp_store, 2);
    }

    #[test]
    fn read_only_pool_sees_frames_of_a_live_writer() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("live.db");
        let writer = wal_database(&db);
        writer.execute_batch("INSERT INTO t VALUES (3);").unwrap();
        assert!(std::fs::metadata(wal_sidecar_path(&db)).unwrap().len() > 0);

        let pool = SqliteConnectionPool::open_read_only(&db).unwrap();
        assert_eq!(count_rows(&pool), 3);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_pool_opens_a_closed_wal_database_in_an_unwritable_dir() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("closed");
        std::fs::create_dir(&dir).unwrap();
        let db = dir.join("closed.db");
        drop(wal_database(&db));
        assert!(
            !wal_sidecar_path(&db).exists(),
            "closing the last connection removes the WAL"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let writable = crate::store::path_is_writable(&dir);

        let pool = SqliteConnectionPool::open_read_only(&db);
        let before = std::fs::read_dir(&dir).unwrap().count();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        if writable {
            return; // root ignores the mode bits; nothing to prove here
        }
        assert_eq!(count_rows(&pool.unwrap()), 2);
        assert_eq!(before, 1, "the read must not create -wal or -shm files");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_pool_does_not_hide_wal_frames_behind_immutable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("framed");
        std::fs::create_dir(&dir).unwrap();
        let db = dir.join("framed.db");
        let writer = wal_database(&db);
        writer.execute_batch("INSERT INTO t VALUES (3);").unwrap();
        // Keep -wal and drop -shm: plain read-only SQLite cannot open this in
        // an unwritable dir, and `immutable=1` would miss the frames.
        let frames = std::fs::read(wal_sidecar_path(&db)).unwrap();
        let snapshot = tmp.path().join("snapshot");
        std::fs::create_dir(&snapshot).unwrap();
        std::fs::copy(&db, snapshot.join("framed.db")).unwrap();
        std::fs::write(snapshot.join("framed.db-wal"), frames).unwrap();
        std::fs::set_permissions(&snapshot, std::fs::Permissions::from_mode(0o500)).unwrap();
        let writable = crate::store::path_is_writable(&snapshot);

        let result = SqliteConnectionPool::open_read_only(&snapshot.join("framed.db"));
        std::fs::set_permissions(&snapshot, std::fs::Permissions::from_mode(0o700)).unwrap();
        if writable {
            return;
        }
        assert!(
            result.is_err(),
            "a database with WAL frames must fail loudly, not read stale data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_only_pool_follows_a_symlinked_database_to_its_real_sidecars() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let tmp = tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        let link_dir = tmp.path().join("link");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::create_dir(&link_dir).unwrap();
        let real = real_dir.join("real.db");
        drop(wal_database(&real));
        assert!(!wal_sidecar_path(&real).exists());
        let link = link_dir.join("metadata.db");
        symlink(&real, &link).unwrap();
        // The real directory cannot take `-shm`; the directory with the link
        // is writable, and a check that looked only there would say yes.
        std::fs::set_permissions(&real_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let writable = crate::store::path_is_writable(&real_dir);

        let pool = SqliteConnectionPool::open_read_only(&link);
        let created = std::fs::read_dir(&link_dir).unwrap().count()
            + std::fs::read_dir(&real_dir).unwrap().count();
        std::fs::set_permissions(&real_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        if writable {
            return; // root ignores the mode bits; nothing to prove here
        }
        assert_eq!(count_rows(&pool.unwrap()), 2);
        assert_eq!(created, 2, "the read must create no sidecar files");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_pool_checks_the_real_wal_of_a_symlinked_database() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let tmp = tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        let link_dir = tmp.path().join("link");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::create_dir(&link_dir).unwrap();
        let live = tmp.path().join("live.db");
        let writer = wal_database(&live);
        writer.execute_batch("INSERT INTO t VALUES (3);").unwrap();
        // Real files: database and a `-wal` with frames, no `-shm`.
        let real = real_dir.join("real.db");
        std::fs::copy(&live, &real).unwrap();
        std::fs::copy(wal_sidecar_path(&live), wal_sidecar_path(&real)).unwrap();
        let link = link_dir.join("metadata.db");
        symlink(&real, &link).unwrap();
        // Nothing is next to the link, and neither directory is writable.
        for dir in [&real_dir, &link_dir] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        let writable = crate::store::path_is_writable(&real_dir);

        let result = SqliteConnectionPool::open_read_only(&link);
        for dir in [&real_dir, &link_dir] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        if writable {
            return;
        }
        assert!(
            result.is_err(),
            "frames in the real `-wal` must fail loudly, not read a stale snapshot"
        );
    }

    #[test]
    fn immutable_uri_escapes_uri_syntax() {
        assert_eq!(
            immutable_uri(Path::new("/data/my dir/a?b#c%d.db")),
            "file:/data/my%20dir/a%3Fb%23c%25d.db?immutable=1"
        );
    }

    #[test]
    fn pool_applies_wal_pragma() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("test.db");

        let pool = SqliteConnectionPool::open_with_max(&db, 2).unwrap();
        let conn = pool.get();

        // journal_mode query returns a string for the configured value.
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    /// Codex 3.3 acceptance criterion: 16 concurrent readers + 4
    /// concurrent writers on the same tenant must not deadlock and
    /// must not surface `SQLITE_BUSY` errors. Uses a fresh table
    /// created on the pool, then fires real INSERT/SELECT traffic
    /// in parallel.
    #[test]
    fn pool_supports_mixed_concurrent_reads_and_writes() {
        use std::sync::Arc;
        use std::thread;

        let tmp = tempdir().unwrap();
        let db = tmp.path().join("stress.db");
        let pool = Arc::new(SqliteConnectionPool::open_with_max(&db, 8).unwrap());

        {
            let conn = pool.get();
            conn.execute(
                "CREATE TABLE stress (id INTEGER PRIMARY KEY, tag TEXT NOT NULL)",
                [],
            )
            .unwrap();
        }

        let mut handles = Vec::new();

        // Writers.
        for w in 0..4 {
            let pool = Arc::clone(&pool);
            handles.push(thread::spawn(move || {
                for i in 0..10 {
                    let conn = pool.get();
                    conn.execute(
                        "INSERT INTO stress (tag) VALUES (?1)",
                        rusqlite::params![format!("w{}-{}", w, i)],
                    )
                    .expect("writer insert must succeed (pool + busy_timeout)");
                }
            }));
        }

        // Readers.
        for _ in 0..16 {
            let pool = Arc::clone(&pool);
            handles.push(thread::spawn(move || {
                for _ in 0..25 {
                    let conn = pool.get();
                    let mut stmt = conn.prepare("SELECT COUNT(*) FROM stress").unwrap();
                    let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
                    assert!(count >= 0);
                }
            }));
        }

        for handle in handles {
            handle
                .join()
                .expect("concurrent pool worker must not panic");
        }

        // Verify all writes landed.
        let conn = pool.get();
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM stress", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 40, "all 4 writers x 10 inserts must commit");
    }

    #[test]
    fn pool_blocks_and_releases_when_saturated() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let tmp = tempdir().unwrap();
        let db = tmp.path().join("test.db");

        let pool = Arc::new(SqliteConnectionPool::open_with_max(&db, 1).unwrap());
        // Hold the only connection.
        let _held = pool.get();

        let (tx, rx) = mpsc::channel();
        let pool_for_thread = Arc::clone(&pool);
        let handle = thread::spawn(move || {
            let c = pool_for_thread.get();
            tx.send(()).unwrap();
            drop(c);
        });

        // The spawned thread must NOT be able to acquire while we
        // hold the connection. Give it time and confirm it's blocked.
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "second get() must block while pool is saturated"
        );

        drop(_held);
        // Now it should succeed quickly.
        rx.recv_timeout(Duration::from_millis(500))
            .expect("get() must unblock after the holder releases");
        handle.join().unwrap();
    }
}
