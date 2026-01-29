use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use tracing::info;

use memd::{init_logging, load_config, run_server, MemoryStore, TenantManager};
use memd::cli::{run_cli, CliCommand};

/// Run mode for memd
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    /// MCP server mode (JSON-RPC over stdio)
    Mcp,
    /// CLI mode for direct commands
    Cli,
}

/// memd - Local memory daemon for AI agents
///
/// Provides MCP server interface for memory operations.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Run mode
    #[arg(short, long, value_enum, default_value = "mcp")]
    mode: Mode,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// CLI subcommand (only used in cli mode)
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Load configuration
    let config = load_config(args.config.as_deref()).unwrap_or_else(|e| {
        eprintln!("error: failed to load configuration: {}", e);
        std::process::exit(1);
    });

    // Initialize logging
    let log_level = if args.verbose { "debug" } else { &config.log_level };
    let log_format = match args.mode {
        Mode::Mcp => "json",
        Mode::Cli => "pretty",
    };
    init_logging(log_format, log_level);

    match args.mode {
        Mode::Mcp => {
            info!(
                version = env!("CARGO_PKG_VERSION"),
                config_path = ?args.config,
                data_dir = %config.data_dir.display(),
                "memd starting"
            );
            if let Err(e) = run_server(config).await {
                eprintln!("error: MCP server error: {}", e);
                std::process::exit(1);
            }
        }
        Mode::Cli => {
            if let Some(cmd) = args.command {
                // Create store and tenant manager
                let store = Arc::new(MemoryStore::new());
                let tenant_manager = config
                    .data_dir_expanded()
                    .ok()
                    .map(TenantManager::new);

                if let Err(e) = run_cli(&*store, tenant_manager.as_ref(), cmd).await {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            } else {
                eprintln!("error: CLI mode requires a subcommand. Use --help for usage.");
                std::process::exit(1);
            }
        }
    }
}
