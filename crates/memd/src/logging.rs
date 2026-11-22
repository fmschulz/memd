//! Logging setup for memd
//!
//! Provides human-readable logging output on stderr.

use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialize the logging system
///
/// # Arguments
/// * `level` - Log level: "trace", "debug", "info", "warn", "error"
///
/// `RUST_LOG` overrides the level. Logs are written to stderr so stdout
/// remains machine-readable for CLI commands.
pub fn init_logging(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Keep memd logs at requested level while suppressing noisy third-party info logs
        // that significantly slow benchmark runs.
        EnvFilter::new(format!("{level},tantivy=warn,hnsw_rs=warn,tokenizers=warn"))
    });

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

#[cfg(test)]
mod tests {
    // Note: We don't test init_logging directly because tracing can only be
    // initialized once per process, and the test runner uses multiple threads.
    // The logging is tested indirectly through the integration tests.
}
