//! Explicit checks run in the client, with bounded output collection.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{MemdError, Result};
use crate::experience::{
    validate_check_target, CheckReceipt, SourceFingerprint, SourceFingerprintCoverage,
};
use crate::store::{PersistentStore, PersistentStoreConfig};
use crate::types::{ProjectId, TenantId};

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_check(
    data_dir: &Path,
    tenant: &str,
    project: Option<&str>,
    attempt_id: String,
    evidence: Vec<PathBuf>,
    timeout_seconds: u64,
    argv: Vec<String>,
) -> Result<CheckReceipt> {
    let tenant_id = TenantId::new(tenant)?;
    // Read-only metadata access coexists with the worker's writer lock. Validate
    // the target before a user-supplied program can have side effects.
    let store = PersistentStore::open(PersistentStoreConfig {
        data_dir: data_dir.to_path_buf(),
        read_only: true,
        enable_dense_search: false,
        enable_hybrid_search: false,
        backfill_hnsw_on_startup: false,
        backfill_canonical_text_on_startup: false,
        ..Default::default()
    })?;
    validate_check_target(
        &store,
        &tenant_id,
        &ProjectId::from(project.map(str::to_string)),
        &attempt_id,
    )
    .await?;
    drop(store);

    let cwd = std::env::current_dir()?;
    let mut execution = super::capture_execution_context(&cwd);
    execution.repo_tracked_diff_sha256 = execution
        .repo_root
        .as_deref()
        .zip(execution.repo_revision.as_deref())
        .and_then(|(root, revision)| super::capture_tracked_diff_sha256(Path::new(root), revision));
    let evidence = evidence
        .iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()?;
    let source_before = fingerprint_source(&cwd, &evidence)?;
    let program = argv
        .first()
        .ok_or_else(|| MemdError::ValidationError("check requires a program after --".into()))?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(&argv[1..])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let started_at_ms = now_ms();
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut child = command.spawn()?;
    #[cfg(unix)]
    let process_group =
        CheckProcessGroup(child.id().expect("new child has a process ID") as libc::pid_t);
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let completion = tokio::time::timeout(Duration::from_secs(timeout_seconds), async {
        tokio::try_join!(child.wait(), hash_stream(stdout), hash_stream(stderr))
    });
    let interrupted = async {
        #[cfg(unix)]
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await
    };
    let result = tokio::select! {
        result = completion => result,
        result = interrupted => {
            result?;
            return Err(MemdError::ValidationError("check interrupted before a complete receipt was available".into()));
        }
    };
    // End the command's process group on every normal exit too. A verifier
    // cannot leave a background child changing evidence after it returns.
    #[cfg(unix)]
    drop(process_group);
    let (timed_out, exit_code, stdout_sha256, stderr_sha256) = match result {
        Ok(result) => {
            let (status, stdout, stderr) = result?;
            (false, status.code(), Some(stdout), Some(stderr))
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            // Dropping the timed-out future closes both pipes. Partial output
            // hashes are unknown, not hashes of a fabricated empty stream.
            (true, None, None, None)
        }
    };
    let finished_at_ms = now_ms();
    let source_after = fingerprint_source(&cwd, &evidence).ok().flatten();
    Ok(CheckReceipt {
        attempt_id,
        cwd: cwd.to_string_lossy().into_owned(),
        argv,
        execution,
        started_at_ms,
        finished_at_ms,
        timed_out,
        exit_code,
        stdout_sha256,
        stderr_sha256,
        source_before,
        source_after,
    })
}

/// Own the verifier's process group so cancellation and I/O errors clean up
/// descendants as well as the direct child owned by Tokio.
#[cfg(unix)]
struct CheckProcessGroup(libc::pid_t);

#[cfg(unix)]
impl Drop for CheckProcessGroup {
    fn drop(&mut self) {
        // SAFETY: spawn used process_group(0), so this positive child PID is
        // the verifier's private PGID. killpg receives no pointers.
        unsafe { libc::killpg(self.0, libc::SIGKILL) };
    }
}

async fn hash_stream(mut reader: impl AsyncRead + Unpin) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(format!("{:x}", hasher.finalize()));
        }
        hasher.update(&buffer[..count]);
    }
}

fn fingerprint_source(cwd: &Path, evidence: &[PathBuf]) -> Result<Option<SourceFingerprint>> {
    if evidence.is_empty() {
        let context = super::capture_execution_context(cwd);
        return Ok(context
            .repo_root
            .as_deref()
            .zip(context.repo_revision.as_deref())
            .and_then(|(root, revision)| {
                super::capture_tracked_diff_sha256(Path::new(root), revision)
            })
            .map(|sha256| SourceFingerprint {
                sha256,
                coverage: SourceFingerprintCoverage::TrackedRepository,
            }));
    }
    let mut paths = evidence.to_vec();
    paths.sort();
    paths.dedup();
    let mut hasher = Sha256::new();
    for path in &paths {
        let mut file = std::fs::File::open(path)?;
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update([0]);
        let mut file_hasher = Sha256::new();
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            file_hasher.update(&buffer[..count]);
        }
        hasher.update(file_hasher.finalize());
    }
    Ok(Some(SourceFingerprint {
        sha256: format!("{:x}", hasher.finalize()),
        coverage: SourceFingerprintCoverage::ExplicitPaths {
            paths: paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        },
    }))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
