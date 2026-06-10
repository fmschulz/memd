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
//!   connection used, applied once per connection creation.
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
        let max_size = std::env::var("MEMD_SQLITE_POOL_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_POOL_MAX_SIZE);
        Self::open_with_max(db_path, max_size)
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
    let conn = match flags {
        Some(flags) => Connection::open_with_flags(path, flags),
        None => Connection::open(path),
    }
    .map_err(|e| MemdError::StorageError(format!("open sqlite at {}: {}", path.display(), e)))?;

    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| MemdError::StorageError(format!("set journal_mode=WAL: {}", e)))?;
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
