//! Configuration module for memd
//!
//! Handles loading configuration from TOML files with fallback to defaults.
//! Supports path expansion (~/) and XDG config directory conventions.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MemdError, Result};

/// Main configuration for memd
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directory for tenant data storage
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// Claude consolidation model alias or identifier.
    #[serde(default = "default_consolidation_model")]
    pub consolidation_model: String,

    /// Compatibility routing configuration.
    #[serde(default)]
    pub server: ServerConfig,
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("~/.memd/data")
}

fn default_consolidation_model() -> String {
    crate::consolidate::claude_haiku::DEFAULT_MODEL.to_string()
}

/// Compatibility routing configuration.
///
/// The TOML table name remains `[server]` for existing config files, but the
/// executable no longer exposes a network or stdio server mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Opt-in to the legacy cross-tenant `project_id` fallback.
    ///
    /// When true, any retrieval that carries a `project_id` widens its
    /// search across all tenants on the daemon that contain that project
    /// string. This is a compatibility shim for older fragmented history
    /// — it silently leaks data across tenant boundaries and should stay
    /// off unless you are deliberately consolidating writes that were
    /// misrouted. `tenant_id` is a logical partition only and not an
    /// authentication boundary; put HTTP behind a reverse proxy with real
    /// auth if you need multi-user isolation.
    #[serde(default)]
    pub allow_cross_tenant_project_fallback: bool,
    /// Explicit project aliases for intentional cross-scope retrieval.
    ///
    /// Each rule says: when the requested `(tenant_id, project_id)` matches,
    /// also search the listed alias scopes. An alias may point at another
    /// tenant and/or another project ID. Unlike the legacy fallback, this does
    /// not widen to every tenant that happens to share the project name.
    #[serde(default)]
    pub project_aliases: Vec<ProjectAliasConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectAliasConfig {
    pub tenant_id: String,
    pub project_id: String,
    #[serde(default)]
    pub aliases: Vec<ProjectAliasScopeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectAliasScopeConfig {
    pub tenant_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            consolidation_model: default_consolidation_model(),
            server: ServerConfig::default(),
        }
    }
}

impl Config {
    /// Get the data directory with tilde expansion applied
    pub fn data_dir_expanded(&self) -> Result<PathBuf> {
        expand_tilde(&self.data_dir)
    }

    /// Validate the configuration values
    pub fn validate(&self) -> Result<()> {
        if self.consolidation_model.trim().is_empty() {
            return Err(MemdError::ConfigError(
                "consolidation_model must not be empty".into(),
            ));
        }

        for rule in &self.server.project_aliases {
            if rule.tenant_id.trim().is_empty() || rule.project_id.trim().is_empty() {
                return Err(MemdError::ConfigError(
                    "server.project_aliases entries require tenant_id and project_id".to_string(),
                ));
            }
            for alias in &rule.aliases {
                if alias.tenant_id.trim().is_empty() {
                    return Err(MemdError::ConfigError(
                        "server.project_aliases aliases require tenant_id".to_string(),
                    ));
                }
                if alias
                    .project_id
                    .as_deref()
                    .is_some_and(|project_id| project_id.trim().is_empty())
                {
                    return Err(MemdError::ConfigError(
                        "server.project_aliases aliases cannot use an empty project_id".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }
}

/// Expand tilde (~) in a path to the user's home directory
fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let path_str = path.to_string_lossy();

    if let Some(relative) = path_str.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| MemdError::ConfigError("HOME environment variable not set".to_string()))?;
        Ok(PathBuf::from(home).join(relative))
    } else if path_str == "~" {
        let home = std::env::var("HOME")
            .map_err(|_| MemdError::ConfigError("HOME environment variable not set".to_string()))?;
        Ok(PathBuf::from(home))
    } else {
        Ok(path.to_path_buf())
    }
}

/// Load configuration from a file path or default locations
///
/// Search order:
/// 1. Explicit path if provided
/// 2. XDG config: ~/.config/memd/config.toml
/// 3. Fall back to defaults
pub fn load_config(path: Option<&Path>) -> Result<Config> {
    // If explicit path provided, load from there
    if let Some(p) = path {
        return load_from_file(p);
    }

    // Check XDG config location
    if let Ok(home) = std::env::var("HOME") {
        let xdg_config = PathBuf::from(&home)
            .join(".config")
            .join("memd")
            .join("config.toml");

        if xdg_config.exists() {
            return load_from_file(&xdg_config);
        }
    }

    // Fall back to defaults
    Ok(Config::default())
}

/// Load configuration from a specific file
fn load_from_file(path: &Path) -> Result<Config> {
    let content = fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content)?;

    // Validate after loading
    config.validate()?;

    Ok(config)
}

/// Load configuration from a TOML string (useful for testing)
pub fn load_from_str(content: &str) -> Result<Config> {
    let config: Config = toml::from_str(content)?;
    config.validate()?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn load_from_toml_string() {
        let toml = r#"
            data_dir = "/custom/path"

            [server]
            allow_cross_tenant_project_fallback = true
        "#;

        let config = load_from_str(toml).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/custom/path"));
        assert!(config.server.allow_cross_tenant_project_fallback);
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let toml = r#"
            data_dir = "/custom/path"
        "#;

        let config = load_from_str(toml).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/custom/path"));
        // Other fields should use defaults
        assert_eq!(config.consolidation_model, default_consolidation_model());
        assert!(!config.server.allow_cross_tenant_project_fallback);
    }

    #[test]
    fn empty_toml_uses_all_defaults() {
        let config = load_from_str("").unwrap();
        assert_eq!(config.consolidation_model, default_consolidation_model());
        assert_eq!(config.data_dir, PathBuf::from("~/.memd/data"));
    }

    #[test]
    fn expand_tilde_works() {
        std::env::set_var("HOME", "/home/testuser");
        let path = PathBuf::from("~/.memd/data");
        let expanded = expand_tilde(&path).unwrap();
        assert_eq!(expanded, PathBuf::from("/home/testuser/.memd/data"));
    }

    #[test]
    fn expand_tilde_absolute_path_unchanged() {
        let path = PathBuf::from("/absolute/path");
        let expanded = expand_tilde(&path).unwrap();
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn removed_log_keys_are_ignored() {
        let toml = r#"
            log_level = "invalid"
            log_format = "xml"
        "#;

        let config = load_from_str(toml).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("~/.memd/data"));
    }

    #[test]
    fn config_serializes_to_toml() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("data_dir"));
        assert!(!toml_str.contains("log_level"));
    }

    #[test]
    fn load_config_returns_default_when_no_file() {
        // When no path provided and no XDG config exists, should return defaults
        let config = load_config(None).unwrap();
        assert_eq!(config.consolidation_model, default_consolidation_model());
    }

    #[test]
    fn legacy_transport_keys_are_ignored() {
        let toml = r#"
            [server]
            transport = "http"
            bind = "127.0.0.1:8787"
            path = "/legacy"
        "#;

        let config = load_from_str(toml).unwrap();
        assert!(!config.server.allow_cross_tenant_project_fallback);
    }

    #[test]
    fn project_aliases_load_from_toml() {
        let toml = r#"
            [server]

            [[server.project_aliases]]
            tenant_id = "memd"
            project_id = "memd"

            [[server.project_aliases.aliases]]
            tenant_id = "default"
            reason = "historical writes"
        "#;

        let config = load_from_str(toml).unwrap();
        assert_eq!(config.server.project_aliases.len(), 1);
        assert_eq!(config.server.project_aliases[0].aliases.len(), 1);
        assert_eq!(
            config.server.project_aliases[0].aliases[0]
                .reason
                .as_deref(),
            Some("historical writes")
        );
    }

    #[test]
    fn project_aliases_allow_project_renames() {
        let toml = r#"
            [server]

            [[server.project_aliases]]
            tenant_id = "memd"
            project_id = "memd"

            [[server.project_aliases.aliases]]
            tenant_id = "default"
            project_id = "old_memd"
        "#;

        let config = load_from_str(toml).unwrap();
        assert_eq!(
            config.server.project_aliases[0].aliases[0]
                .project_id
                .as_deref(),
            Some("old_memd")
        );
    }

    #[test]
    fn legacy_server_path_is_ignored() {
        let toml = r#"
            [server]
            path = "legacy"
        "#;

        let config = load_from_str(toml).unwrap();
        assert!(config.server.project_aliases.is_empty());
    }
}
