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
    PersistentStoreConfig, RerankerConfig, RerankerMode, TenantManager,
};

/// Embedding model choice
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModelChoice {
    /// all-MiniLM-L6-v2: 384-dim, fast, good quality
    AllMinilm,
    /// Qwen3-Embedding-0.6B: 1024-dim, slower, best quality (not yet supported in Candle)
    Qwen3,
    /// bge-base-en-v1.5: 768-dim Candle BERT, stronger retrieval (CLS pooling,
    /// dense-only by default)
    BgeBase,
    /// bge-small-en-v1.5: 384-dim Candle BERT, CLS pooling
    BgeSmall,
}

impl From<ModelChoice> for EmbeddingModel {
    fn from(choice: ModelChoice) -> Self {
        match choice {
            ModelChoice::AllMinilm => EmbeddingModel::AllMiniLmL6V2,
            ModelChoice::Qwen3 => EmbeddingModel::Qwen3Embedding0_6B,
            // EmbeddingModel is the vestigial ONNX-era selector and has no bge
            // variant; the live Candle model is chosen via CandleModel below.
            ModelChoice::BgeBase | ModelChoice::BgeSmall => EmbeddingModel::AllMiniLmL6V2,
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
            ModelChoice::BgeSmall => CandleModel::BgeSmall,
        }
    }
}

impl From<CandleModel> for ModelChoice {
    fn from(model: CandleModel) -> Self {
        match model {
            CandleModel::MiniLm => ModelChoice::AllMinilm,
            CandleModel::BgeBase => ModelChoice::BgeBase,
            CandleModel::BgeSmall => ModelChoice::BgeSmall,
        }
    }
}

/// The model of one run: the explicit flag, else the model the store records,
/// else `bge-small` for a new store (no recorded model, no dense vectors).
fn resolve_model_choice(
    explicit: Option<ModelChoice>,
    recorded: Option<CandleModel>,
) -> ModelChoice {
    explicit
        .or(recorded.map(ModelChoice::from))
        .unwrap_or(ModelChoice::BgeSmall)
}

impl ModelChoice {
    fn cli_value(self) -> &'static str {
        match self {
            ModelChoice::AllMinilm => "all-minilm",
            ModelChoice::Qwen3 => "qwen3",
            ModelChoice::BgeBase => "bge-base",
            ModelChoice::BgeSmall => "bge-small",
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
    /// Pure BM25 baseline with dense retrieval and feature bonuses disabled.
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
    ///
    /// Without this flag a store keeps the model it records, and a new store
    /// uses bge-small. A model that differs from the store's is an error;
    /// switching needs MEMD_BACKFILL_HNSW_ON_STARTUP=1 on a writer.
    #[arg(long, value_enum)]
    embedding_model: Option<ModelChoice>,

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
    // Pin the effective model in the process environment so detached children
    // and warm workers inherit the same explicit config-file selection.
    if std::env::var("MEMD_CONSOLIDATION_MODEL").map_or(true, |value| value.trim().is_empty()) {
        std::env::set_var("MEMD_CONSOLIDATION_MODEL", &config.consolidation_model);
    }
    configure_operation_routing(
        config.server.allow_cross_tenant_project_fallback,
        config.server.project_aliases.clone(),
    );

    let Some(mut cmd) = args.command else {
        eprintln!("error: memd requires a CLI subcommand. Use --help for usage.");
        std::process::exit(1);
    };
    cmd.normalize_positional_aliases();

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

    // `experience note --emit` validates a file and needs no scope or store.
    match memd::cli::emit_case_if_requested(&cmd) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }

    // A flagless first write resolves to a scope that is only planned; it is
    // published after the command succeeded, so a failed command leaves no
    // `.memd` directory behind.
    let pending_scope = match memd::cli::resolve_command_scope(&mut cmd) {
        Ok(pending) => pending,
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    };

    cmd = match memd::cli::prepare_experience_command(cmd, &data_dir, args.in_memory).await {
        Ok(cmd) => cmd,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = memd::cli::prepare_execution_context(&mut cmd) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }

    // Resolve the model once, here: the flag, else what the store records, else
    // bge-small for a new store. A flagless client and a flagless warm worker
    // read the same store, so both get the same model. The store is read only
    // when no flag names a model.
    let recorded_model = match args.embedding_model {
        Some(_) => None,
        None if args.in_memory => None,
        None => memd::store::recorded_embedding_model(&data_dir).unwrap_or_else(|e| {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }),
    };
    let embedding_model = resolve_model_choice(args.embedding_model, recorded_model);

    // Resolve the retrieval strategy once, here, so an unset --search-variant
    // takes the model-derived default (dense-only for bge-base) and warm
    // workers are spawned with the same concrete variant.
    let search_variant = resolve_search_variant(args.search_variant, embedding_model);

    // Stamp the worker's model/variant identity onto the (hidden) WarmWorker
    // command so it reports them in the ping handshake; a client requesting a
    // different model/variant then respawns it instead of being answered by
    // the wrong embedder.
    if let CliCommand::WarmWorker {
        embedding_model: worker_model,
        search_variant: worker_variant,
        ..
    } = &mut cmd
    {
        *worker_model = Some(embedding_model.cli_value().to_string());
        *worker_variant = Some(search_variant.cli_value().to_string());
    }

    let warm_config = WarmProcessConfig {
        data_dir: data_dir.clone(),
        config_path: args.config.clone(),
        embedding_model: embedding_model.cli_value().to_string(),
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
                Ok(true) => {
                    publish_pending_scope(pending_scope);
                    return;
                }
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
        publish_pending_scope(pending_scope);
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
        info!(data_dir = %data_dir.display(), embedding_model = ?embedding_model, "using persistent store");
        let mut store_config = PersistentStoreConfig {
            data_dir: data_dir.clone(),
            read_only: cmd.store_access() == StoreAccess::ReadOnly,
            candle_model: embedding_model.into(),
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
    // An in-memory run keeps no fact, so it leaves no scope either.
    publish_pending_scope(pending_scope.filter(|_| !args.in_memory));
}

/// Publish the scope that a first write planned, now that the write
/// succeeded. A failure here does not undo the stored fact: it is reported and
/// the next first write tries again.
fn publish_pending_scope(pending: Option<memd::cli::PendingScope>) {
    if let Some(pending) = pending {
        if let Err(error) = pending.publish() {
            eprintln!("warning: stored the fact but could not create the project scope: {error}");
        }
    }
}

fn apply_search_variant(search_variant: SearchVariant, config: &mut PersistentStoreConfig) {
    match search_variant {
        SearchVariant::HybridFeature => {
            config.enable_dense_search = true;
            config.enable_hybrid_search = true;
            let hybrid = HybridConfig {
                reranker: RerankerConfig {
                    mode: RerankerMode::Feature,
                    ..RerankerConfig::default()
                },
                ..HybridConfig::default()
            };
            config.hybrid_config = Some(hybrid);
        }
        SearchVariant::HybridCrossEncoder => {
            config.enable_dense_search = true;
            config.enable_hybrid_search = true;
            let hybrid = HybridConfig {
                reranker: RerankerConfig {
                    mode: RerankerMode::CrossEncoder,
                    ..RerankerConfig::default()
                },
                ..HybridConfig::default()
            };
            config.hybrid_config = Some(hybrid);
        }
        SearchVariant::DenseOnly => {
            config.enable_dense_search = true;
            config.enable_hybrid_search = false;
            config.hybrid_config = None;
        }
        SearchVariant::Bm25Only => {
            config.enable_dense_search = false;
            config.enable_hybrid_search = true;
            let hybrid = HybridConfig {
                dense_k: 0,
                enable_sparse: true,
                enable_rerank: false,
                reranker: RerankerConfig {
                    mode: RerankerMode::Feature,
                    rrf_weight: 1.0,
                    recency_weight: 0.0,
                    project_weight: 0.0,
                    type_weight: 0.0,
                    query_text_weight: 0.0,
                    cross_encoder_weight: 0.0,
                    ..RerankerConfig::default()
                },
                ..HybridConfig::default()
            };
            config.hybrid_config = Some(hybrid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bge_small_cli_and_variant_resolution() {
        let args = Args::try_parse_from([
            "memd",
            "--embedding-model",
            "bge-small",
            "stats",
            "--tenant-id",
            "test",
        ])
        .unwrap();
        let model = args.embedding_model.unwrap();
        assert_eq!(model.cli_value(), "bge-small");
        assert_eq!(CandleModel::from(model).hf_id(), "BAAI/bge-small-en-v1.5");
        assert!(matches!(
            resolve_search_variant(None, model),
            SearchVariant::HybridFeature
        ));
        assert!(matches!(
            resolve_search_variant(Some(SearchVariant::DenseOnly), model),
            SearchVariant::DenseOnly
        ));
        // No flag means no choice yet: the store decides.
        assert_eq!(
            Args::try_parse_from(["memd", "stats", "--tenant-id", "test"])
                .unwrap()
                .embedding_model,
            None
        );
    }

    #[test]
    fn model_resolves_from_flag_then_store_then_bge_small() {
        // A new store records nothing and takes bge-small, with its variant.
        let new_store = resolve_model_choice(None, None);
        assert_eq!(new_store, ModelChoice::BgeSmall);
        assert!(matches!(
            resolve_search_variant(None, new_store),
            SearchVariant::HybridFeature
        ));
        // An existing store keeps the model it records.
        for (recorded, expected) in [
            (CandleModel::MiniLm, ModelChoice::AllMinilm),
            (CandleModel::BgeSmall, ModelChoice::BgeSmall),
            (CandleModel::BgeBase, ModelChoice::BgeBase),
        ] {
            assert_eq!(resolve_model_choice(None, Some(recorded)), expected);
        }
        // The flag wins, so a mismatch with the store still reaches the check.
        assert_eq!(
            resolve_model_choice(Some(ModelChoice::BgeSmall), Some(CandleModel::MiniLm)),
            ModelChoice::BgeSmall
        );
        assert_eq!(
            resolve_model_choice(Some(ModelChoice::AllMinilm), None),
            ModelChoice::AllMinilm
        );
    }

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

    #[test]
    fn bm25_only_disables_non_lexical_ranking_features() {
        let mut config = PersistentStoreConfig::default();
        apply_search_variant(SearchVariant::Bm25Only, &mut config);

        let hybrid = config.hybrid_config.expect("bm25 hybrid configuration");
        assert_eq!(hybrid.dense_k, 0);
        assert!(hybrid.enable_sparse);
        assert!(!hybrid.enable_rerank);
        assert!(!config.enable_dense_search);
        assert_eq!(hybrid.reranker.rrf_weight, 1.0);
        assert_eq!(hybrid.reranker.recency_weight, 0.0);
        assert_eq!(hybrid.reranker.project_weight, 0.0);
        assert_eq!(hybrid.reranker.type_weight, 0.0);
        assert_eq!(hybrid.reranker.query_text_weight, 0.0);
        assert_eq!(hybrid.reranker.cross_encoder_weight, 0.0);
    }
}
