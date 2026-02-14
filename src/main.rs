use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use parking_lot::Mutex;
use std::sync::Arc;

use analyzer::AnalyzerRegistry;
use analyzers::{
    ClaudeCodeAnalyzer, ClineAnalyzer, CodexCliAnalyzer, CopilotAnalyzer, GeminiCliAnalyzer,
    KiloCliAnalyzer, KiloCodeAnalyzer, OpenCodeAnalyzer, PiAgentAnalyzer, PiebaldAnalyzer,
    QwenCodeAnalyzer, RooCodeAnalyzer,
};

mod analyzer;
mod analyzers;
mod cache;
mod config;
mod contribution_cache;
mod mcp;
mod models;
mod reqwest_simd_json;
mod tui;
mod types;
mod upload;
mod utils;
mod version_check;
mod watcher;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "splitrail")]
#[command(version)]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Output stats as JSON instead of running the TUI
    #[arg(long)]
    json: bool,

    /// Use comma-separated number formatting
    #[arg(long)]
    number_comma: bool,

    /// Use human-readable number formatting (k, m, b, t)
    #[arg(short = 'H', long)]
    number_human: bool,

    /// Locale for number formatting (en, de, fr, es, it, ja, ko, zh)
    #[arg(long)]
    locale: Option<String>,

    /// Number of decimal places for human-readable formatting
    #[arg(long)]
    decimal_places: Option<usize>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manually upload stats to Splitrail Cloud
    Upload(UploadArgs),
    /// Manage configuration
    Config(ConfigArgs),
    /// Output usage statistics as JSON
    Stats(StatsArgs),
    /// Run as an MCP (Model Context Protocol) server
    Mcp,
}

#[derive(Args)]
struct UploadArgs {
    /// Perform a full re-upload, ignoring the last upload date.
    #[arg(long, default_value_t = false)]
    full: bool,

    /// Force re-upload for a specific analyzer (e.g., "Claude Code").
    #[arg(long)]
    force_analyzer: Option<String>,

    /// Re-upload only messages with zero cost (useful for fixing pricing errors).
    #[arg(long, default_value_t = false)]
    zero_cost: bool,

    /// Show what would be uploaded without actually uploading.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Args)]
struct ConfigArgs {
    #[command(subcommand)]
    subcommand: ConfigSubcommands,
}

#[derive(Args)]
struct StatsArgs {
    /// Include raw per-message data in the JSON output
    #[arg(long, default_value_t = false)]
    include_messages: bool,

    /// Pretty-print JSON instead of a single line
    #[arg(long, default_value_t = false)]
    pretty: bool,
}

#[derive(Subcommand)]
enum ConfigSubcommands {
    /// Create default configuration file
    Init {
        #[arg(long, default_value_t = false)]
        overwrite: bool,
    },
    /// Show current configuration
    Show,
    /// Set configuration value
    Set {
        /// Configuration key (api-token, auto-upload, number-comma, number-human, locale, decimal-places)
        key: String,
        /// Configuration value
        value: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Load config file to get defaults
    let config = config::Config::load().unwrap_or(None).unwrap_or_default();

    // Create format options merging config defaults with CLI overrides
    let format_options = utils::NumberFormatOptions {
        use_comma: cli.number_comma || config.formatting.number_comma,
        use_human: cli.number_human || config.formatting.number_human,
        locale: cli.locale.unwrap_or(config.formatting.locale),
        decimal_places: cli
            .decimal_places
            .unwrap_or(config.formatting.decimal_places),
    };

    match cli.command {
        None => {
            if cli.json {
                if let Err(e) = run_stats(StatsArgs {
                    include_messages: false,
                    pretty: true,
                })
                .await
                {
                    eprintln!("Error generating JSON stats: {e:#}");
                    std::process::exit(1);
                }
            } else {
                // No subcommand - run default behavior
                run_default(format_options).await;
            }
        }
        Some(Commands::Upload(args)) => {
            match run_upload(args).await.context("Failed to run upload") {
                Ok(_) => {}
                Err(e) => {
                    tui::show_upload_error(&format!("{e:#}"));
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Config(config_args)) => {
            handle_config_subcommand(config_args).await;
        }
        Some(Commands::Stats(stats_args)) => {
            if let Err(e) = run_stats(stats_args).await {
                eprintln!("Error generating JSON stats: {e:#}");
                std::process::exit(1);
            }
        }
        Some(Commands::Mcp) => {
            if let Err(e) = mcp::run_mcp_server().await {
                eprintln!("MCP server error: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

pub fn create_analyzer_registry() -> AnalyzerRegistry {
    let mut registry = AnalyzerRegistry::new();

    // Register available analyzers
    registry.register(ClaudeCodeAnalyzer::new());
    registry.register(ClineAnalyzer::new());
    registry.register(RooCodeAnalyzer::new());
    registry.register(KiloCodeAnalyzer::new());
    registry.register(KiloCliAnalyzer::new());
    registry.register(GeminiCliAnalyzer::new());
    registry.register(QwenCodeAnalyzer::new());
    registry.register(CodexCliAnalyzer::new());
    registry.register(CopilotAnalyzer::new());
    registry.register(OpenCodeAnalyzer::new());
    registry.register(PiAgentAnalyzer::new());
    registry.register(PiebaldAnalyzer::new());

    registry
}

async fn run_default(format_options: utils::NumberFormatOptions) {
    let registry = create_analyzer_registry();

    // Create file watcher
    let file_watcher = match watcher::FileWatcher::new(&registry) {
        Ok(watcher) => watcher,
        Err(e) => {
            eprintln!("Error setting up file watcher: {e}");
            std::process::exit(1);
        }
    };

    // Create real-time stats manager using temporary rayon threadpool for parallel loading
    let mut stats_manager = {
        let pool = rayon::ThreadPoolBuilder::new()
            .build()
            .expect("Failed to create rayon threadpool");

        let result = pool.install(|| watcher::RealtimeStatsManager::new(registry));

        // Pool is dropped here, releasing threads
        match result {
            Ok(manager) => manager,
            Err(e) => {
                eprintln!("Error loading analyzer stats: {e}");
                std::process::exit(1);
            }
        }
    };

    // Release memory from parallel parsing back to OS
    release_unused_memory();

    // Create upload status for TUI
    let upload_status = Arc::new(Mutex::new(tui::UploadStatus::None));

    // Spawn background version check
    let update_status = version_check::spawn_version_check();

    // Set upload status on stats manager for real-time upload tracking
    stats_manager.set_upload_status(upload_status.clone());

    // Check if auto-upload is enabled and start background upload
    let config = config::Config::load().unwrap_or(None).unwrap_or_default();
    if config.upload.auto_upload {
        if config.is_configured() {
            // For initial auto-upload, load full stats separately (sync, no threadpool for background task)
            let registry_for_upload = create_analyzer_registry();
            let upload_status_clone = upload_status.clone();
            tokio::spawn(async move {
                if let Ok(full_stats) = registry_for_upload.load_all_stats_parallel_scoped() {
                    // Scoped threadpool already released, also release allocator memory
                    release_unused_memory();
                    upload::perform_background_upload(
                        full_stats,
                        Some(upload_status_clone),
                        Some(500),
                    )
                    .await;
                }
            });
        } else {
            // Auto-upload is enabled but configuration is incomplete
            let mut status = upload_status.lock();
            if config.is_api_token_missing() && config.is_server_url_missing() {
                *status = tui::UploadStatus::MissingConfig;
            } else if config.is_api_token_missing() {
                *status = tui::UploadStatus::MissingApiToken;
            } else if config.is_server_url_missing() {
                *status = tui::UploadStatus::MissingServerUrl;
            } else {
                // Shouldn't happen since is_configured() returned false
                *status = tui::UploadStatus::MissingConfig;
            }
        }
    }

    // Start real-time TUI with file watcher
    if let Err(e) = tui::run_tui(
        stats_manager.get_stats_receiver(),
        &format_options,
        upload_status.clone(),
        update_status,
        file_watcher,
        stats_manager,
    ) {
        eprintln!("Error displaying TUI: {e}");
    }
}

async fn run_upload(args: UploadArgs) -> Result<()> {
    let registry = create_analyzer_registry();

    // Load stats using temporary rayon threadpool for parallel parsing
    let stats = {
        let pool = rayon::ThreadPoolBuilder::new()
            .build()
            .expect("Failed to create rayon threadpool");
        pool.install(|| registry.load_all_stats_parallel())?
        // Pool is dropped here, releasing threads
    };

    // Release memory from parallel parsing back to OS
    release_unused_memory();

    // Load config file to get formatting options and upload date
    let config_file = config::Config::load().unwrap_or(None).unwrap_or_default();
    let format_options = utils::NumberFormatOptions {
        use_comma: config_file.formatting.number_comma,
        use_human: config_file.formatting.number_human,
        locale: config_file.formatting.locale,
        decimal_places: config_file.formatting.decimal_places,
    };

    match config::Config::load() {
        Ok(Some(mut config)) if config.is_configured() => {
            let messages_to_upload = if args.full {
                // --full flag: Flatten all messages from all analyzers
                stats
                    .analyzer_stats
                    .into_iter()
                    .flat_map(|s| s.messages)
                    .collect()
            } else if let Some(forced_analyzer_name) = args.force_analyzer {
                // --force-analyzer flag: Selectively filter analyzers
                let mut messages = vec![];
                for analyzer_stats in stats.analyzer_stats {
                    if analyzer_stats
                        .analyzer_name
                        .eq_ignore_ascii_case(&forced_analyzer_name)
                    {
                        // For the forced analyzer, add all its messages
                        messages.extend(analyzer_stats.messages);
                    } else {
                        // For all other analyzers, only add new messages
                        messages.extend(
                            utils::get_messages_later_than(
                                config.upload.last_date_uploaded,
                                analyzer_stats.messages,
                            )
                            .await?,
                        );
                    }
                }
                messages
            } else {
                // Default behavior: Get all messages newer than the last upload date
                let all_messages: Vec<_> = stats
                    .analyzer_stats
                    .into_iter()
                    .flat_map(|s| s.messages)
                    .collect();
                utils::get_messages_later_than(config.upload.last_date_uploaded, all_messages)
                    .await
                    .context("Failed to get messages later than last saved date")?
            };

            // Apply zero-cost filter if requested
            let messages_to_upload = if args.zero_cost {
                utils::filter_zero_cost_messages(messages_to_upload)
            } else {
                messages_to_upload
            };

            // If dry-run, show summary and exit without uploading
            if args.dry_run {
                tui::show_upload_dry_run(&messages_to_upload, &format_options);
                return Ok(());
            }

            let progress_callback = tui::create_upload_progress_callback(&format_options);
            upload::upload_message_stats(&messages_to_upload, &mut config, progress_callback)
                .await
                .context("Failed to upload messages")?;
            tui::show_upload_success(messages_to_upload.len(), &format_options);
            Ok(())
        }
        Ok(Some(_)) => {
            eprintln!("Configuration incomplete");
            upload::show_upload_help();
            std::process::exit(1);
        }
        Ok(None) => {
            eprintln!("No configuration found");
            upload::show_upload_help();
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Config error: {e:#}");
            std::process::exit(1);
        }
    }
}

async fn run_stats(args: StatsArgs) -> Result<()> {
    let registry = create_analyzer_registry();

    // Load stats using temporary rayon threadpool for parallel parsing
    let mut stats = {
        let pool = rayon::ThreadPoolBuilder::new()
            .build()
            .expect("Failed to create rayon threadpool");
        pool.install(|| registry.load_all_stats_parallel())?
        // Pool is dropped here, releasing threads
    };

    // Release memory from parallel parsing back to OS
    release_unused_memory();

    if !args.include_messages {
        for analyzer_stats in &mut stats.analyzer_stats {
            analyzer_stats.messages.clear();
        }
    }

    if args.pretty {
        let json = simd_json::to_string_pretty(&stats)?;
        println!("{json}");
    } else {
        let json = simd_json::to_string(&stats)?;
        println!("{json}");
    }

    Ok(())
}

async fn handle_config_subcommand(config_args: ConfigArgs) {
    match config_args.subcommand {
        ConfigSubcommands::Init { overwrite } => {
            if let Err(e) = config::create_default_config(overwrite) {
                eprintln!("Error creating config: {e}");
                std::process::exit(1);
            }
        }
        ConfigSubcommands::Show => {
            if let Err(e) = config::show_config() {
                eprintln!("Error showing config: {e}");
                std::process::exit(1);
            }
        }
        ConfigSubcommands::Set { key, value } => {
            if let Err(e) = config::set_config_value(&key, &value) {
                eprintln!("Error setting config: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Release unused memory back to the OS after heavy allocations.
/// Call this after heavy allocations (e.g., parsing) to reclaim memory.
#[cfg(feature = "mimalloc")]
pub fn release_unused_memory() {
    // SAFETY: mi_collect is a safe FFI call that triggers garbage collection
    // and returns unused memory to the OS. The `force` parameter (true) ensures
    // aggressive collection.
    unsafe {
        libmimalloc_sys::mi_collect(true);
    }
}

/// No-op when mimalloc is disabled.
#[cfg(not(feature = "mimalloc"))]
pub fn release_unused_memory() {}
