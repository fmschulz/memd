//! Best-effort usage-event ledger primitives.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageOp {
    Add,
    Search,
    AgentContext,
    Get,
    Delete,
    Purge,
    Consolidate,
    ImportOmf,
    Report,
}

impl UsageOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Search => "search",
            Self::AgentContext => "agent_context",
            Self::Get => "get",
            Self::Delete => "delete",
            Self::Purge => "purge",
            Self::Consolidate => "consolidate",
            Self::ImportOmf => "import_omf",
            Self::Report => "report",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub op: UsageOp,
    pub tenant: Option<String>,
    pub project: Option<String>,
    pub outcome: String,
    pub chunk_count: Option<i64>,
    pub bytes: Option<i64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UsageEventRecord {
    pub ts_unix_ms: i64,
    pub op: String,
    pub outcome: String,
    pub chunk_count: Option<i64>,
    pub bytes: Option<i64>,
    pub detail: Option<String>,
}

pub fn usage_ledger_enabled() -> bool {
    std::env::var("MEMD_USAGE_LEDGER")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            )
        })
        .unwrap_or(true)
}

pub fn usage_retention_ms() -> i64 {
    let days = std::env::var("MEMD_USAGE_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(90);
    days.saturating_mul(86_400_000).min(i64::MAX as u64) as i64
}

/// Hash a search query for privacy-preserving distinctness analytics.
/// Search queries are never stored verbatim, only this hash.
/// This hash is NOT stable across Rust releases; distinctness comparisons are
/// only meaningful among events written by the same memd build, never as a
/// persistent cross-build identifier.
pub fn query_hash_hex(query: &str) -> String {
    let mut hasher = DefaultHasher::new();
    query.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
