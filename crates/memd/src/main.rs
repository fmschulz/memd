use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use tracing::info;

use memd::cli::{
    run_cli, run_warm_admin, try_run_warm_client, CliCommand, StoreAccess, WarmProcessConfig,
};
use memd::embeddings::{CandleModel, EmbeddingModel};
use memd::store::HybridConfig;
use memd::{
    configure_operation_routing, init_logging, load_config, MemoryStore, PersistentStore,
    PersistentStoreConfig, RerankerMode, TenantManager,
};

/// Embedding model choice
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModelChoice {
    /// all-MiniLM-L6-v2: 384-dim, fast, good quality (default)
    AllMinilm,
    /// Qwen3-Embedding-0.6B: 1024-dim, slower, best quality (not yet supported in Candle)
    Qwen3,
    /// bge-base-en-v1.5: 768-dim Candle BERT, stronger retrieval (CLS pooling,
    /// dense-only by default)
    BgeBase,
}

impl From<ModelChoice> for EmbeddingModel {
    fn from(choice: ModelChoice) -> Self {
        match choice {
            ModelChoice::AllMinilm => EmbeddingModel::AllMiniLmL6V2,
            ModelChoice::Qwen3 => EmbeddingModel::Qwen3Embedding0_6B,
            // EmbeddingModel is the vestigial ONNX-era selector and has no bge
            // variant; the live Candle model is chosen via CandleModel below.
            ModelChoice::BgeBase => EmbeddingModel::AllMiniLmL6V2,
        }
    }
}

impl From<ModelChoice> for CandleModel {
    fn from(choice: ModelChoice) -> Self {
        match choice {
            // Qwen3 is not supported by the Candle BERT backend; it has always
            // run MiniLM there, so preserve that behavior.
            ModelChoice::AllMinilm | ModelChoice::Qwen3 => CandleModel::MiniLm,
            ModelChoice::BgeBase => CandleModel::BgeBase,
        }
    }
}

impl ModelChoice {
    fn cli_value(self) -> &'static str {
        match self {
            ModelChoice::AllMinilm => "all-minilm",
            ModelChoice::Qwen3 => "qwen3",
            ModelChoice::BgeBase => "bge-base",
        }
    }
}

/// Resolve the default retrieval strategy for a model when `--search-variant`
/// is not given explicitly. bge-base defaults to dense-only (hybrid fusion
/// off); every other model keeps the hybrid-feature default.
fn default_search_variant(model: ModelChoice) -> SearchVariant {
    match model {
        ModelChoice::BgeBase => SearchVariant::DenseOnly,
        _ => SearchVariant::HybridFeature,
    }
}

/// Resolve the effective retrieval strategy: an explicit `--search-variant`
/// wins; otherwise fall back to the model-derived default.
fn resolve_search_variant(explicit: Option<SearchVariant>, model: ModelChoice) -> SearchVariant {
    explicit.unwrap_or_else(|| default_search_variant(model))
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
    ///
    /// Defaults to dense-only for bge-base and hybrid-feature for other
    /// models when not given explicitly.
    #[arg(long, value_enum)]
    search_variant: Option<SearchVariant>,

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

    // Resolve the retrieval strategy once, here, so an unset --search-variant
    // takes the model-derived default (dense-only for bge-base) and warm
    // workers are spawned with the same concrete variant.
    let search_variant = resolve_search_variant(args.search_variant, args.embedding_model);

    // Stamp the worker's model/variant identity onto the (hidden) WarmWorker
    // command so it reports them in the ping handshake; a client requesting a
    // different model/variant then respawns it instead of being answered by
    // the wrong embedder.
    if let CliCommand::WarmWorker {
        embedding_model,
        search_variant: worker_variant,
        ..
    } = &mut cmd
    {
        *embedding_model = Some(args.embedding_model.cli_value().to_string());
        *worker_variant = Some(search_variant.cli_value().to_string());
    }

    let warm_config = WarmProcessConfig {
        data_dir: data_dir.clone(),
        config_path: args.config.clone(),
        embedding_model: args.embedding_model.cli_value().to_string(),
        search_variant: search_variant.cli_value().to_string(),
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
            candle_model: args.embedding_model.into(),
            ..Default::default()
        };
        apply_search_variant(search_variant, &mut store_config);
        if matches!(&cmd, CliCommand::WarmWorker { .. }) {
            // A warm worker that cannot promptly become THE writer must exit
            // and let the client's ping find the winner / fall back. Long
            // retries make herd losers linger and steal the flock after
            // `memd warm stop`.
            store_config.writer_lock_timeout_cap = Some(Duration::from_millis(2_000));
            let async_indexing_env = std::env::var("MEMD_ASYNC_INDEXING").ok();
            store_config.apply_warm_worker_availability_defaults(async_indexing_env.as_deref());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_choice_maps_to_candle_model() {
        // The Candle backend is the live embedder; bge-base selects it.
        assert_eq!(
            CandleModel::from(ModelChoice::AllMinilm),
            CandleModel::MiniLm
        );
        assert_eq!(
            CandleModel::from(ModelChoice::BgeBase),
            CandleModel::BgeBase
        );
        // Qwen3 has no Candle backend and has always run MiniLM there.
        assert_eq!(CandleModel::from(ModelChoice::Qwen3), CandleModel::MiniLm);
    }

    #[test]
    fn bge_choice_has_cli_value_and_vestigial_onnx_mapping() {
        assert_eq!(ModelChoice::BgeBase.cli_value(), "bge-base");
        // EmbeddingModel is the vestigial ONNX selector; bge maps onto the
        // MiniLM id there because the live model flows via CandleModel.
        assert_eq!(
            EmbeddingModel::from(ModelChoice::BgeBase),
            EmbeddingModel::AllMiniLmL6V2
        );
    }

    #[test]
    fn default_search_variant_is_dense_only_for_bge_only() {
        assert!(matches!(
            default_search_variant(ModelChoice::BgeBase),
            SearchVariant::DenseOnly
        ));
        assert!(matches!(
            default_search_variant(ModelChoice::AllMinilm),
            SearchVariant::HybridFeature
        ));
        assert!(matches!(
            default_search_variant(ModelChoice::Qwen3),
            SearchVariant::HybridFeature
        ));
    }

    #[test]
    fn explicit_search_variant_overrides_bge_default() {
        // An explicit flag wins even against bge's dense-only default.
        assert!(matches!(
            resolve_search_variant(Some(SearchVariant::HybridFeature), ModelChoice::BgeBase),
            SearchVariant::HybridFeature
        ));
        // An unset flag falls back to the model-derived default.
        assert!(matches!(
            resolve_search_variant(None, ModelChoice::BgeBase),
            SearchVariant::DenseOnly
        ));
        assert!(matches!(
            resolve_search_variant(None, ModelChoice::AllMinilm),
            SearchVariant::HybridFeature
        ));
    }
}
