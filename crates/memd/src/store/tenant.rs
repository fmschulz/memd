//! Tenant directory management
//!
//! Handles creation and management of tenant-specific directories
//! following the storage layout defined in the implementation spec.

use std::fs;
use std::path::{Path, PathBuf};

use tracing::{debug, info};

use crate::error::{MemdError, Result};
use crate::types::TenantId;

/// Statistics about a tenant's disk usage
#[derive(Debug, Clone, Default)]
pub struct TenantDiskStats {
    /// Total bytes used by the tenant
    pub total_bytes: u64,
    /// Number of segment directories
    pub segment_count: usize,
}

/// Manages tenant directory structure
///
/// Creates and manages the per-tenant directory layout:
/// ```text
/// {data_dir}/tenants/{tenant_id}/
///   segments/    # Append-only chunk segments
///   wal/         # Write-ahead log
///   indexes/     # Sparse and dense indexes
///   cache/       # Semantic cache
/// ```
pub struct TenantManager {
    data_dir: PathBuf,
}

impl TenantManager {
    /// Create a new TenantManager with the given data directory
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// Get the path to the tenants directory
    fn tenants_dir(&self) -> PathBuf {
        self.data_dir.join("tenants")
    }

    /// Get path to a specific tenant's directory
    pub fn tenant_path(&self, tenant_id: &TenantId) -> PathBuf {
        self.tenants_dir().join(tenant_id.as_str())
    }

    /// Ensure tenant directory exists with proper structure
    ///
    /// Creates the directory and subdirectories if they don't exist.
    /// Returns the path to the tenant directory.
    pub fn ensure_tenant_dir(&self, tenant_id: &TenantId) -> Result<PathBuf> {
        let tenant_dir = self.tenant_path(tenant_id);

        debug!(
            tenant_id = %tenant_id,
            path = %tenant_dir.display(),
            "ensuring tenant directory"
        );

        // Create main tenant directory
        if !tenant_dir.exists() {
            fs::create_dir_all(&tenant_dir).map_err(|e| {
                MemdError::StorageError(format!(
                    "failed to create tenant directory {}: {}",
                    tenant_dir.display(),
                    e
                ))
            })?;

            info!(
                tenant_id = %tenant_id,
                path = %tenant_dir.display(),
                "created tenant directory"
            );
        }

        // Create subdirectories
        let subdirs = ["segments", "wal", "indexes", "cache"];
        for subdir in &subdirs {
            let path = tenant_dir.join(subdir);
            if !path.exists() {
                fs::create_dir_all(&path).map_err(|e| {
                    MemdError::StorageError(format!("failed to create {} directory: {}", subdir, e))
                })?;
                debug!(subdir = %subdir, "created subdirectory");
            }
        }

        Ok(tenant_dir)
    }

    /// List all tenant IDs that have directories
    pub fn list_tenants(&self) -> Result<Vec<TenantId>> {
        let tenants_dir = self.tenants_dir();

        if !tenants_dir.exists() {
            return Ok(Vec::new());
        }

        let mut tenants = Vec::new();

        let entries = fs::read_dir(&tenants_dir).map_err(|e| {
            MemdError::StorageError(format!("failed to read tenants directory: {}", e))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                MemdError::StorageError(format!("failed to read directory entry: {}", e))
            })?;

            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    // Validate the tenant ID
                    if let Ok(tenant_id) = TenantId::new(name) {
                        tenants.push(tenant_id);
                    }
                }
            }
        }

        Ok(tenants)
    }

    /// Get disk statistics for a tenant
    pub fn tenant_disk_stats(&self, tenant_id: &TenantId) -> Result<TenantDiskStats> {
        let tenant_dir = self.tenant_path(tenant_id);

        if !tenant_dir.exists() {
            return Ok(TenantDiskStats::default());
        }

        let total_bytes = Self::dir_size(&tenant_dir)?;
        let segment_count = Self::count_segments(&tenant_dir.join("segments"))?;

        Ok(TenantDiskStats {
            total_bytes,
            segment_count,
        })
    }

    /// Recursively calculate directory size in bytes
    fn dir_size(path: &Path) -> Result<u64> {
        let mut total = 0;

        if !path.exists() {
            return Ok(0);
        }

        if path.is_file() {
            return path
                .metadata()
                .map(|m| m.len())
                .map_err(|e| MemdError::IoError(e));
        }

        let entries = fs::read_dir(path).map_err(|e| MemdError::IoError(e))?;

        for entry in entries {
            let entry = entry.map_err(|e| MemdError::IoError(e))?;
            let path = entry.path();

            if path.is_file() {
                total += entry
                    .metadata()
                    .map(|m| m.len())
                    .map_err(|e| MemdError::IoError(e))?;
            } else if path.is_dir() {
                total += Self::dir_size(&path)?;
            }
        }

        Ok(total)
    }

    /// Count number of segment directories
    fn count_segments(segments_dir: &Path) -> Result<usize> {
        if !segments_dir.exists() {
            return Ok(0);
        }

        let count = fs::read_dir(segments_dir)
            .map_err(|e| MemdError::IoError(e))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .count();

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup() -> (TempDir, TenantManager) {
        let temp_dir = TempDir::new().unwrap();
        let manager = TenantManager::new(temp_dir.path().to_path_buf());
        (temp_dir, manager)
    }

    #[test]
    fn ensure_tenant_dir_creates_structure() {
        let (_temp_dir, manager) = setup();
        let tenant = TenantId::new("test_tenant").unwrap();

        let path = manager.ensure_tenant_dir(&tenant).unwrap();

        assert!(path.exists());
        assert!(path.join("segments").exists());
        assert!(path.join("wal").exists());
        assert!(path.join("indexes").exists());
        assert!(path.join("cache").exists());
    }

    #[test]
    fn ensure_tenant_dir_idempotent() {
        let (_temp_dir, manager) = setup();
        let tenant = TenantId::new("test_tenant").unwrap();

        // Call twice - should not fail
        let path1 = manager.ensure_tenant_dir(&tenant).unwrap();
        let path2 = manager.ensure_tenant_dir(&tenant).unwrap();

        assert_eq!(path1, path2);
    }

    #[test]
    fn tenant_path_format() {
        let (_temp_dir, manager) = setup();
        let tenant = TenantId::new("my_tenant").unwrap();

        let path = manager.tenant_path(&tenant);

        assert!(path.ends_with("tenants/my_tenant"));
    }

    #[test]
    fn list_tenants_empty() {
        let (_temp_dir, manager) = setup();

        let tenants = manager.list_tenants().unwrap();
        assert!(tenants.is_empty());
    }

    #[test]
    fn list_tenants_finds_created() {
        let (_temp_dir, manager) = setup();

        manager
            .ensure_tenant_dir(&TenantId::new("tenant_a").unwrap())
            .unwrap();
        manager
            .ensure_tenant_dir(&TenantId::new("tenant_b").unwrap())
            .unwrap();

        let tenants = manager.list_tenants().unwrap();
        assert_eq!(tenants.len(), 2);

        let names: Vec<String> = tenants.iter().map(|t| t.to_string()).collect();
        assert!(names.contains(&"tenant_a".to_string()));
        assert!(names.contains(&"tenant_b".to_string()));
    }

    #[test]
    fn disk_stats_empty_tenant() {
        let (_temp_dir, manager) = setup();
        let tenant = TenantId::new("test_tenant").unwrap();

        let stats = manager.tenant_disk_stats(&tenant).unwrap();
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.segment_count, 0);
    }

    #[test]
    fn disk_stats_with_files() {
        let (_temp_dir, manager) = setup();
        let tenant = TenantId::new("test_tenant").unwrap();

        // Create tenant directory
        let tenant_dir = manager.ensure_tenant_dir(&tenant).unwrap();

        // Create a test file
        let test_file = tenant_dir.join("cache").join("test.bin");
        let mut file = File::create(&test_file).unwrap();
        file.write_all(b"test data 123").unwrap();

        // Create a segment directory
        fs::create_dir(tenant_dir.join("segments").join("seg_000001")).unwrap();

        let stats = manager.tenant_disk_stats(&tenant).unwrap();
        assert!(stats.total_bytes > 0);
        assert_eq!(stats.segment_count, 1);
    }
}
