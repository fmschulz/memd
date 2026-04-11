use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use tracing::info;

use memd::cli::{run_cli, CliCommand};
use memd::embeddings::EmbeddingModel;
use memd::store::HybridConfig;
use memd::structural::{
    CallGraphIndexer, StructuralStore, SymbolIndexer, SymbolQueryService, TraceQueryService,
};
use memd::{
    init_logging, load_config, MemoryStore, PersistentStore, PersistentStoreConfig, RerankerMode,
    TenantManager,
};

/// Run mode for memd
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    /// MCP server mode (JSON-RPC over stdio)
    Mcp,
    /// CLI mode for direct commands
    Cli,
}

/// Server transport for MCP mode.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum TransportChoice {
    /// JSON-RPC over stdio (client launches subprocess)
    Stdio,
    /// Streamable HTTP on a long-lived local daemon
    Http,
}

/// Embedding model choice
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModelChoice {
    /// all-MiniLM-L6-v2: 384-dim, fast, good quality (default)
    AllMinilm,
    /// Qwen3-Embedding-0.6B: 1024-dim, slower, best quality (not yet supported in Candle)
    Qwen3,
}

impl From<ModelChoice> for EmbeddingModel {
    fn from(choice: ModelChoice) -> Self {
        match choice {
            ModelChoice::AllMinilm => EmbeddingModel::AllMiniLmL6V2,
            ModelChoice::Qwen3 => EmbeddingModel::Qwen3Embedding0_6B,
        }
    }
}

/// Retrieval strategy for persistent mode.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum SearchVariant {
    /// Dense+sparse hybrid with feature reranker (default).
    HybridFeature,
    /// Dense+sparse hybrid with cross-encoder reranker.
    HybridCrossEncoder,
    /// Dense-only retrieval path.
    DenseOnly,
    /// BM25 baseline via hybrid search with dense_k=0.
    Bm25Only,
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

    /// Override the MCP server transport
    #[arg(long, value_enum)]
    transport: Option<TransportChoice>,

    /// Bind address for HTTP transport, e.g. 127.0.0.1:8787
    #[arg(long)]
    http_bind: Option<String>,

    /// HTTP endpoint path for streamable HTTP transport
    #[arg(long)]
    http_path: Option<String>,

    /// Use in-memory storage instead of persistent storage (for testing)
    #[arg(long, default_value = "false")]
    in_memory: bool,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Embedding model to use
    #[arg(long, value_enum, default_value = "all-minilm")]
    embedding_model: ModelChoice,

    /// Retrieval strategy variant for persistent mode
    #[arg(long, value_enum, default_value = "hybrid-feature")]
    search_variant: SearchVariant,

    /// CLI subcommand (only used in cli mode)
    #[command(subcommand)]
    command: Option<CliCommand>,
}

fn attach_structural_runtime<S: memd::store::Store>(
    server: memd::mcp::McpServer<S>,
    structural_store: Arc<StructuralStore>,
) -> memd::mcp::McpServer<S> {
    let symbol_query_service = Arc::new(SymbolQueryService::new(structural_store.clone()));
    let trace_query_service = Arc::new(TraceQueryService::new(structural_store.clone()));
    let symbol_indexer = Arc::new(SymbolIndexer::new(structural_store.clone()));
    let call_graph_indexer = Arc::new(CallGraphIndexer::new(structural_store.clone()));

    server
        .with_symbol_query_service(symbol_query_service)
        .with_trace_query_service(trace_query_service)
        .with_structural_indexers(structural_store, symbol_indexer, call_graph_indexer)
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

    // If a subcommand is provided, treat it as CLI mode even when mode flag is omitted.
    let mode = if args.command.is_some() {
        Mode::Cli
    } else {
        args.mode
    };

    // Apply server overrides after loading config and resolving data_dir.
    let mut config = config;
    if let Some(transport) = args.transport {
        config.server.transport = match transport {
            TransportChoice::Stdio => "stdio".to_string(),
            TransportChoice::Http => "http".to_string(),
        };
    }
    if let Some(bind) = args.http_bind.clone() {
        config.server.bind = bind;
    }
    if let Some(path) = args.http_path.clone() {
        config.server.path = path;
    }
    config.data_dir = data_dir.clone();
    if let Err(e) = config.validate() {
        eprintln!("error: invalid configuration: {}", e);
        std::process::exit(1);
    }

    // Initialize logging
    let log_level = if args.verbose {
        "debug"
    } else {
        &config.log_level
    };
    let log_format = match mode {
        Mode::Mcp => "json",
        Mode::Cli => "pretty",
    };
    init_logging(log_format, log_level);

    match mode {
        Mode::Mcp => {
            let server_transport = config.server.transport.clone();
            let http_bind = config.server.bind.clone();
            let http_path = config.server.path.clone();

            info!(
                version = env!("CARGO_PKG_VERSION"),
                config_path = ?args.config,
                data_dir = %data_dir.display(),
                transport = %server_transport,
                http_bind = %http_bind,
                http_path = %http_path,
                in_memory = args.in_memory,
                "memd starting"
            );

            // Run server with appropriate store type
            if args.in_memory {
                info!("using in-memory store");
                let store = Arc::new(MemoryStore::new());
                let structural_store = Arc::new(StructuralStore::in_memory().unwrap_or_else(|e| {
                    eprintln!("error: failed to create in-memory structural store: {}", e);
                    std::process::exit(1);
                }));
                match server_transport.as_str() {
                    "http" => {
                        let server = attach_structural_runtime(
                            memd::mcp::McpServer::new(config.clone(), store),
                            structural_store.clone(),
                        );
                        if let Err(e) =
                            memd::mcp::run_http_server(server, &http_bind, &http_path).await
                        {
                            eprintln!("error: HTTP MCP server error: {}", e);
                            std::process::exit(1);
                        }
                    }
                    _ => {
                        let mut server = attach_structural_runtime(
                            memd::mcp::McpServer::new(config.clone(), store),
                            structural_store,
                        );
                        if let Err(e) = server.run().await {
                            eprintln!("error: MCP server error: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            } else {
                info!(data_dir = %data_dir.display(), embedding_model = ?args.embedding_model, "using persistent store");
                let mut store_config = PersistentStoreConfig {
                    data_dir: data_dir.clone(),
                    embedding_model: args.embedding_model.into(),
                    ..Default::default()
                };
                apply_search_variant(args.search_variant, &mut store_config);
                match PersistentStore::open(store_config) {
                    Ok(store) => {
                        let metrics = store.metrics_arc();
                        let store = Arc::new(store);
                        let structural_store = Arc::new(
                            StructuralStore::open(&data_dir.join("structural.db")).unwrap_or_else(
                                |e| {
                                    eprintln!("error: failed to create structural store: {}", e);
                                    std::process::exit(1);
                                },
                            ),
                        );
                        match server_transport.as_str() {
                            "http" => {
                                let server = attach_structural_runtime(
                                    memd::mcp::McpServer::with_metrics(
                                        config.clone(),
                                        store,
                                        metrics,
                                    ),
                                    structural_store.clone(),
                                );
                                if let Err(e) =
                                    memd::mcp::run_http_server(server, &http_bind, &http_path).await
                                {
                                    eprintln!("error: HTTP MCP server error: {}", e);
                                    std::process::exit(1);
                                }
                            }
                            _ => {
                                let mut server = attach_structural_runtime(
                                    memd::mcp::McpServer::with_metrics(
                                        config.clone(),
                                        store,
                                        metrics,
                                    ),
                                    structural_store,
                                );
                                if let Err(e) = server.run().await {
                                    eprintln!("error: MCP server error: {}", e);
                                    std::process::exit(1);
                                }
                            }
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

                if !cmd.requires_store() {
                    let store = MemoryStore::new();
                    if let Err(e) = run_cli(&store, tenant_manager.as_ref(), cmd).await {
                        eprintln!("error: {}", e);
                        std::process::exit(1);
                    }
                    return;
                }

                // Run CLI with appropriate store type
                if args.in_memory {
                    info!("using in-memory store");
                    let store = MemoryStore::new();
                    if let Err(e) = run_cli(&store, tenant_manager.as_ref(), cmd).await {
                        eprintln!("error: {}", e);
                        std::process::exit(1);
                    }
                } else {
                    info!(data_dir = %data_dir.display(), embedding_model = ?args.embedding_model, "using persistent store");
                    let mut store_config = PersistentStoreConfig {
                        data_dir: data_dir.clone(),
                        embedding_model: args.embedding_model.into(),
                        ..Default::default()
                    };
                    apply_search_variant(args.search_variant, &mut store_config);
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

fn apply_search_variant(search_variant: SearchVariant, config: &mut PersistentStoreConfig) {
    match search_variant {
        SearchVariant::HybridFeature => {
            config.enable_dense_search = true;
            config.enable_hybrid_search = true;
            let mut hybrid = HybridConfig::default();
            hybrid.reranker.mode = RerankerMode::Feature;
            config.hybrid_config = Some(hybrid);
        }
        SearchVariant::HybridCrossEncoder => {
            config.enable_dense_search = true;
            config.enable_hybrid_search = true;
            let mut hybrid = HybridConfig::default();
            hybrid.reranker.mode = RerankerMode::CrossEncoder;
            config.hybrid_config = Some(hybrid);
        }
        SearchVariant::DenseOnly => {
            config.enable_dense_search = true;
            config.enable_hybrid_search = false;
            config.hybrid_config = None;
        }
        SearchVariant::Bm25Only => {
            config.enable_dense_search = true;
            config.enable_hybrid_search = true;
            config.enable_tiered_search = false;
            let mut hybrid = HybridConfig::default();
            hybrid.dense_k = 0;
            hybrid.sparse_k = 200;
            hybrid.enable_sparse = true;
            hybrid.reranker.mode = RerankerMode::Feature;
            config.hybrid_config = Some(hybrid);
        }
    }
}
