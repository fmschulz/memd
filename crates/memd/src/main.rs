use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() {
    // Initialize tracing with JSON format by default
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .json()
        .init();

    info!("memd starting...");
}
