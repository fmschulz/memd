//! Cross-process writer lock for persistent stores.

use std::path::Path;
use std::time::Duration;

use crate::error::Result;

#[cfg(unix)]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::error::{MemdError, Result};

    const DEFAULT_TIMEOUT_MS: u64 = 10_000;
    const RETRY_DELAY_MS: u64 = 100;

    /// Guard that holds the writer lock file descriptor open.
    pub struct WriterLockGuard {
        file: File,
        path: PathBuf,
    }

    impl Drop for WriterLockGuard {
        fn drop(&mut self) {
            // Best effort only. Closing the file also releases the flock.
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
            tracing::debug!(path = %self.path.display(), "released memd writer lock");
        }
    }

    pub fn acquire_writer_lock(data_dir: &Path) -> Result<WriterLockGuard> {
        acquire_writer_lock_capped(data_dir, None)
    }

    pub fn acquire_writer_lock_capped(
        data_dir: &Path,
        cap: Option<Duration>,
    ) -> Result<WriterLockGuard> {
        acquire_writer_lock_with_timeout(data_dir, effective_timeout(timeout_from_env(), cap))
    }

    fn acquire_writer_lock_with_timeout(
        data_dir: &Path,
        timeout: Duration,
    ) -> Result<WriterLockGuard> {
        let lock_path = data_dir.join(".writer.lock");
        let start = std::time::Instant::now();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        loop {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                write_holder_line(&mut file)?;
                tracing::debug!(path = %lock_path.display(), "acquired memd writer lock");
                return Ok(WriterLockGuard {
                    file,
                    path: lock_path,
                });
            }

            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK)
                && err.raw_os_error() != Some(libc::EAGAIN)
            {
                return Err(MemdError::IoError(err));
            }

            if start.elapsed() >= timeout {
                let holder = read_holder_line(&mut file);
                return Err(MemdError::WriterLockHeld { lock_path, holder });
            }

            thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
        }
    }

    fn timeout_from_env() -> Duration {
        parse_timeout_ms(std::env::var("MEMD_WRITER_LOCK_TIMEOUT_MS").ok().as_deref())
    }

    fn parse_timeout_ms(value: Option<&str>) -> Duration {
        let millis = value
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|millis| *millis > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        Duration::from_millis(millis)
    }

    fn effective_timeout(env_timeout: Duration, cap: Option<Duration>) -> Duration {
        cap.map_or(env_timeout, |cap| env_timeout.min(cap))
    }

    fn write_holder_line(file: &mut File) -> Result<()> {
        let started_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(
            file,
            "pid={} started_unix_ms={}",
            std::process::id(),
            started_unix_ms
        )?;
        file.sync_all()?;
        Ok(())
    }

    fn read_holder_line(file: &mut File) -> String {
        let mut holder = String::new();
        if file.seek(SeekFrom::Start(0)).is_ok() && file.read_to_string(&mut holder).is_ok() {
            let trimmed = holder.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        "unknown".to_string()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn acquire_writes_holder_line() {
            let dir = tempfile::tempdir().unwrap();
            let _guard = acquire_writer_lock(dir.path()).unwrap();
            let text = std::fs::read_to_string(dir.path().join(".writer.lock")).unwrap();
            assert!(text.contains("pid="));
            assert!(text.contains("started_unix_ms="));
        }

        #[test]
        fn drop_releases_for_reacquire() {
            let dir = tempfile::tempdir().unwrap();
            let guard = acquire_writer_lock(dir.path()).unwrap();
            drop(guard);
            let _guard = acquire_writer_lock(dir.path()).unwrap();
        }

        #[test]
        fn timeout_env_parsing_falls_back_to_default() {
            assert_eq!(
                parse_timeout_ms(Some("not-a-number")),
                Duration::from_millis(DEFAULT_TIMEOUT_MS)
            );
            assert_eq!(
                parse_timeout_ms(Some("0")),
                Duration::from_millis(DEFAULT_TIMEOUT_MS)
            );
            assert_eq!(parse_timeout_ms(Some("42")), Duration::from_millis(42));
        }

        #[test]
        fn effective_timeout_honors_lower_bound() {
            assert_eq!(
                effective_timeout(
                    Duration::from_millis(120_000),
                    Some(Duration::from_millis(2_000))
                ),
                Duration::from_millis(2_000)
            );
            assert_eq!(
                effective_timeout(Duration::from_millis(1), Some(Duration::from_millis(2_000))),
                Duration::from_millis(1)
            );
            assert_eq!(
                effective_timeout(Duration::from_millis(42), None),
                Duration::from_millis(42)
            );
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::path::Path;

    use crate::error::Result;

    /// No-op guard on platforms without `flock`.
    pub struct WriterLockGuard;

    pub fn acquire_writer_lock(_data_dir: &Path) -> Result<WriterLockGuard> {
        tracing::warn!("memd writer locking is unsupported on this platform");
        Ok(WriterLockGuard)
    }

    pub fn acquire_writer_lock_capped(
        _data_dir: &Path,
        _cap: Option<std::time::Duration>,
    ) -> Result<WriterLockGuard> {
        acquire_writer_lock(_data_dir)
    }
}

pub use imp::WriterLockGuard;

/// Acquire the exclusive persistent-store writer lock for `data_dir`.
pub fn acquire_writer_lock(data_dir: &Path) -> Result<WriterLockGuard> {
    imp::acquire_writer_lock(data_dir)
}

/// Acquire the writer lock, capping the environment-derived timeout.
pub fn acquire_writer_lock_capped(
    data_dir: &Path,
    cap: Option<Duration>,
) -> Result<WriterLockGuard> {
    imp::acquire_writer_lock_capped(data_dir, cap)
}
