// Configuration module - placeholder for Task 3
// This file will be fully implemented in Task 3

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Main configuration for memd
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directory for tenant data storage
    pub data_dir: PathBuf,
    /// Logging level: trace, debug, info, warn, error
    pub log_level: String,
    /// Log format: json (structured) or pretty (human-readable)
    pub log_format: String,
    /// Server configuration
    pub server: ServerConfig,
}

/// Server-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Transport type: stdio for now
    pub transport: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("~/.memd/data"),
            log_level: "info".to_string(),
            log_format: "json".to_string(),
            server: ServerConfig::default(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            transport: "stdio".to_string(),
        }
    }
}

/// Load configuration from a file path or default locations
pub fn load_config(_path: Option<&std::path::Path>) -> Result<Config> {
    // Placeholder - full implementation in Task 3
    Ok(Config::default())
}
