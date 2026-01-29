//! Structured logging setup for memd
//!
//! Provides JSON or pretty-formatted logging output.
//! In MCP mode, logs go to stderr to avoid interfering with JSON-RPC on stdout.

use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialize the logging system
///
/// # Arguments
/// * `format` - Output format: "json" for structured JSON, "pretty" for human-readable
/// * `level` - Log level: "trace", "debug", "info", "warn", "error"
///
/// Logs are written to stderr so they don't interfere with MCP protocol
/// messages on stdout.
pub fn init_logging(format: &str, level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    match format {
        "json" => {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    fmt::layer()
                        .json()
                        .with_writer(std::io::stderr)
                        .with_target(true)
                        .with_file(false)
                        .with_line_number(false),
                )
                .init();
        }
        "pretty" | _ => {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    fmt::layer()
                        .pretty()
                        .with_writer(std::io::stderr)
                        .with_target(true),
                )
                .init();
        }
    }
}

#[cfg(test)]
mod tests {
    // Note: We don't test init_logging directly because tracing can only be
    // initialized once per process, and the test runner uses multiple threads.
    // The logging is tested indirectly through the integration tests.
}
