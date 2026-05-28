//! Consolidator backend selection.
//!
//! `MEMD_CONSOLIDATOR` chooses the adapter explicitly (`claude` |
//! `codex` | `auto`). In `auto` mode (the default), a populated
//! `$CODEX_*` environment selects Codex; otherwise a `claude` binary
//! on `PATH` selects Claude. If neither is available the call is a
//! hard error so consolidation never silently no-ops.

use crate::error::{MemdError, Result};

use super::claude_haiku::ClaudeHaikuConsolidator;
use super::codex_spark::CodexSparkConsolidator;
use super::{Consolidator, MockEnvConsolidator};

/// Selected consolidator backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Claude,
    Codex,
    /// Hermetic in-process backend that echoes
    /// `$MEMD_CONSOLIDATOR_MOCK_RESPONSE`. Only ever selected by an
    /// explicit `MEMD_CONSOLIDATOR=mock`; the auto path never picks
    /// it, so production cannot fall into it by accident.
    Mock,
}

/// Resolve which [`Consolidator`] to use from the process environment.
pub fn select_consolidator() -> Result<Box<dyn Consolidator>> {
    let backend = resolve_backend(
        std::env::var("MEMD_CONSOLIDATOR").ok().as_deref(),
        codex_env_present(),
        claude_on_path(),
    )?;
    Ok(match backend {
        Backend::Claude => Box::new(ClaudeHaikuConsolidator::default()),
        Backend::Codex => Box::new(CodexSparkConsolidator::default()),
        Backend::Mock => Box::new(MockEnvConsolidator),
    })
}

/// Pure selection logic, separated for unit testing.
///
/// * `explicit` — value of `MEMD_CONSOLIDATOR`, if set.
/// * `codex_env` — whether any `$CODEX_*` variable is populated.
/// * `claude_path` — whether a `claude` binary is on `PATH`.
pub fn resolve_backend(
    explicit: Option<&str>,
    codex_env: bool,
    claude_path: bool,
) -> Result<Backend> {
    match explicit
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("claude") => Ok(Backend::Claude),
        Some("codex") => Ok(Backend::Codex),
        Some("mock") => Ok(Backend::Mock),
        Some("auto") | None => {
            if codex_env {
                Ok(Backend::Codex)
            } else if claude_path {
                Ok(Backend::Claude)
            } else {
                Err(MemdError::ConfigError(
                    "no consolidator available: set MEMD_CONSOLIDATOR=claude|codex, \
                     install the `claude` CLI, or configure a Codex environment"
                        .to_string(),
                ))
            }
        }
        Some(other) => Err(MemdError::ConfigError(format!(
            "invalid MEMD_CONSOLIDATOR value `{other}`: expected claude|codex|auto"
        ))),
    }
}

/// True if any `CODEX_*` environment variable is populated.
fn codex_env_present() -> bool {
    std::env::vars().any(|(key, value)| key.starts_with("CODEX_") && !value.is_empty())
}

/// True if a `claude` executable is resolvable on `PATH`.
fn claude_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join("claude");
        candidate.is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_claude_wins() {
        assert_eq!(
            resolve_backend(Some("claude"), true, false).unwrap(),
            Backend::Claude
        );
    }

    #[test]
    fn explicit_codex_wins() {
        assert_eq!(
            resolve_backend(Some("codex"), false, true).unwrap(),
            Backend::Codex
        );
    }

    #[test]
    fn explicit_is_case_insensitive() {
        assert_eq!(
            resolve_backend(Some("  Codex  "), false, false).unwrap(),
            Backend::Codex
        );
    }

    #[test]
    fn auto_prefers_codex_when_env_present() {
        assert_eq!(
            resolve_backend(Some("auto"), true, true).unwrap(),
            Backend::Codex
        );
        assert_eq!(resolve_backend(None, true, true).unwrap(), Backend::Codex);
    }

    #[test]
    fn auto_falls_back_to_claude() {
        assert_eq!(resolve_backend(None, false, true).unwrap(), Backend::Claude);
    }

    #[test]
    fn auto_hard_errors_when_nothing_available() {
        assert!(resolve_backend(None, false, false).is_err());
    }

    #[test]
    fn invalid_value_errors() {
        assert!(resolve_backend(Some("gpt"), true, true).is_err());
    }

    #[test]
    fn explicit_mock_selects_mock() {
        assert_eq!(
            resolve_backend(Some("mock"), false, false).unwrap(),
            Backend::Mock
        );
    }

    #[test]
    fn auto_never_selects_mock() {
        // Mock is only reachable via an explicit opt-in.
        assert!(matches!(
            resolve_backend(None, true, true).unwrap(),
            Backend::Codex | Backend::Claude
        ));
    }
}
