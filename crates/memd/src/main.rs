use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use tracing::info;

use memd::cli::{
    run_cli, run_warm_admin, try_run_warm_client, CliCommand, StoreAccess, WarmProcessConfig,
};
use memd::embeddings::EmbeddingModel;
use memd::store::HybridConfig;
use memd::{
    configure_operation_routing, init_logging, load_config, MemoryStore, PersistentStore,
    PersistentStoreConfig, RerankerMode, TenantManager,
};

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

impl ModelChoice {
    fn cli_value(self) -> &'static str {
        match self {
            ModelChoice::AllMinilm => "all-minilm",
            ModelChoice::Qwen3 => "qwen3",
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

impl SearchVariant {
    fn cli_value(self) -> &'static str {
        match self {
            SearchVariant::HybridFeature => "hybrid-feature",
            SearchVariant::HybridCrossEncoder => "hybrid-cross-encoder",
            SearchVariant::DenseOnly => "dense-only",
            SearchVariant::Bm25Only => "bm25-only",
        }
    }
}

/// memd - Local memory CLI for AI agents
///
/// Provides executable memory operations for skill-driven agent workflows.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Data directory for persistent storage
    #[arg(long)]
    data_dir: Option<PathBuf>,

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

    /// CLI subcommand
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

    let mut config = config;
    config.data_dir = data_dir.clone();
    if let Err(e) = config.validate() {
        eprintln!("error: invalid configuration: {}", e);
        std::process::exit(1);
    }
    configure_operation_routing(
        config.server.allow_cross_tenant_project_fallback,
        config.server.project_aliases.clone(),
    );

    let Some(mut cmd) = args.command else {
        eprintln!("error: memd requires a CLI subcommand. Use --help for usage.");
        std::process::exit(1);
    };

    // Initialize logging
    let is_warm_worker = matches!(&cmd, CliCommand::WarmWorker { .. });
    let log_level = if args.verbose || is_warm_worker {
        "info"
    } else {
        "warn"
    };
    init_logging("pretty", log_level);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config_path = ?args.config,
        data_dir = %data_dir.display(),
        in_memory = args.in_memory,
        "memd CLI starting"
    );

    if let Err(e) = memd::cli::resolve_command_scope(&mut cmd) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }

    let warm_config = WarmProcessConfig {
        data_dir: data_dir.clone(),
        config_path: args.config.clone(),
        embedding_model: args.embedding_model.cli_value().to_string(),
        search_variant: args.search_variant.cli_value().to_string(),
    };

    if let CliCommand::Warm { command } = &cmd {
        if args.in_memory {
            eprintln!("error: warm workers require persistent storage");
            std::process::exit(1);
        }
        if let Err(e) = run_warm_admin(&warm_config, command.clone()).await {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    if matches!(&cmd, CliCommand::WarmWorker { .. }) && args.in_memory {
        eprintln!("error: warm-worker requires persistent storage");
        std::process::exit(1);
    }

    if let Some(mode) = cmd.warm_mode() {
        if args.in_memory {
            if mode == memd::cli::WarmMode::Required {
                eprintln!("error: --warm required is not supported with --in-memory");
                std::process::exit(1);
            }
        } else if mode != memd::cli::WarmMode::Off {
            match try_run_warm_client(&warm_config, &cmd).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }

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
            read_only: cmd.store_access() == StoreAccess::ReadOnly,
            embedding_model: args.embedding_model.into(),
            ..Default::default()
        };
        apply_search_variant(args.search_variant, &mut store_config);
        if matches!(&cmd, CliCommand::WarmWorker { .. }) {
            // A warm worker that cannot promptly become THE writer must exit
            // and let the client's ping find the winner / fall back. Long
            // retries make herd losers linger and steal the flock after
            // `memd warm stop`.
            store_config.writer_lock_timeout_cap = Some(Duration::from_millis(2_000));
        }
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
