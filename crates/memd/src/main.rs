use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use tracing::info;

use memd::cli::{run_cli, CliCommand};
use memd::{
    init_logging, load_config, MemoryStore, PersistentStore, PersistentStoreConfig, TenantManager,
};

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

    /// Data directory for persistent storage
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Use in-memory storage instead of persistent storage (for testing)
    #[arg(long, default_value = "false")]
    in_memory: bool,

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

    // Determine data directory: CLI arg > config > default
    let data_dir = args
        .data_dir
        .clone()
        .or_else(|| config.data_dir_expanded().ok())
        .unwrap_or_else(|| PathBuf::from("data"));

    // Initialize logging
    let log_level = if args.verbose {
        "debug"
    } else {
        &config.log_level
    };
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
                data_dir = %data_dir.display(),
                in_memory = args.in_memory,
                "memd starting"
            );

            // Run server with appropriate store type
            if args.in_memory {
                info!("using in-memory store");
                let store = Arc::new(MemoryStore::new());
                let mut server = memd::mcp::McpServer::new(config, store);
                if let Err(e) = server.run().await {
                    eprintln!("error: MCP server error: {}", e);
                    std::process::exit(1);
                }
            } else {
                info!(data_dir = %data_dir.display(), "using persistent store");
                let store_config = PersistentStoreConfig {
                    data_dir: data_dir.clone(),
                    ..Default::default()
                };
                match PersistentStore::open(store_config) {
                    Ok(store) => {
                        let store = Arc::new(store);
                        let mut server = memd::mcp::McpServer::new(config, store);
                        if let Err(e) = server.run().await {
                            eprintln!("error: MCP server error: {}", e);
                            std::process::exit(1);
                        }
                    }
                    Err(e) => {
                        eprintln!("error: failed to create persistent store: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
        Mode::Cli => {
            if let Some(cmd) = args.command {
                // Create tenant manager
                let tenant_manager = Some(TenantManager::new(data_dir.clone()));

                // Run CLI with appropriate store type
                if args.in_memory {
                    info!("using in-memory store");
                    let store = MemoryStore::new();
                    if let Err(e) = run_cli(&store, tenant_manager.as_ref(), cmd).await {
                        eprintln!("error: {}", e);
                        std::process::exit(1);
                    }
                } else {
                    info!(data_dir = %data_dir.display(), "using persistent store");
                    let store_config = PersistentStoreConfig {
                        data_dir: data_dir.clone(),
                        ..Default::default()
                    };
                    match PersistentStore::open(store_config) {
                        Ok(store) => {
                            if let Err(e) = run_cli(&store, tenant_manager.as_ref(), cmd).await {
                                eprintln!("error: {}", e);
                                std::process::exit(1);
                            }
                        }
                        Err(e) => {
                            eprintln!("error: failed to create persistent store: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            } else {
                eprintln!("error: CLI mode requires a subcommand. Use --help for usage.");
                std::process::exit(1);
            }
        }
    }
}
