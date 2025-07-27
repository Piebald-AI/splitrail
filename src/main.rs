#![feature(if_let_guard)]

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use analyzer::AnalyzerRegistry;
use analyzers::{ClaudeCodeAnalyzer, CodexAnalyzer, GeminiAnalyzer};

use crate::types::MultiAnalyzerStats;

mod analyzer;
mod analyzers;
mod config;
mod models;
mod tui;
mod types;
mod upload;
mod utils;
mod watcher;

#[derive(Parser)]
#[command(name = "splitrail")]
#[command(version)]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

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
    /// Force upload stats to the Splitrail Leaderboard
    Upload,
    /// Manage configuration
    Config(ConfigArgs),
}

#[derive(Args)]
struct ConfigArgs {
    #[command(subcommand)]
    subcommand: ConfigSubcommands,
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
            // No subcommand - run default behavior
            run_default(format_options).await;
        }
        Some(Commands::Upload) => match run_upload().await.context("Failed to run upload") {
            Ok(_) => {}
            Err(e) => {
                tui::show_upload_error(&format!("{e:#}"));
                std::process::exit(1);
            }
        },
        Some(Commands::Config(config_args)) => {
            handle_config_subcommand(config_args).await;
        }
    }
}

fn create_analyzer_registry() -> AnalyzerRegistry {
    let mut registry = AnalyzerRegistry::new();

    // Register available analyzers
    registry.register(ClaudeCodeAnalyzer::new());
    registry.register(CodexAnalyzer::new());
    registry.register(GeminiAnalyzer::new());

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

    // Create real-time stats manager
    let stats_manager = match watcher::RealtimeStatsManager::new(registry).await {
        Ok(manager) => manager,
        Err(e) => {
            eprintln!("Error loading analyzer stats: {e}");
            std::process::exit(1);
        }
    };

    // Get the initial stats to check if we have data
    let initial_stats = stats_manager.get_stats_receiver().borrow().clone();

    // Create upload status for TUI
    let upload_status = Arc::new(Mutex::new(tui::UploadStatus::None));

    // Check if auto-upload is enabled and start background upload
    let config = config::Config::load().unwrap_or(None).unwrap_or_default();
    if config.upload.auto_upload {
        if config.is_configured() {
            let upload_status_clone = upload_status.clone();
            tokio::spawn(async move {
                run_background_upload(initial_stats, upload_status_clone).await;
            });
        } else {
            // Auto-upload is enabled but configuration is incomplete
            if let Ok(mut status) = upload_status.lock() {
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
    }

    // Start real-time TUI with file watcher
    if let Err(e) = tui::run_tui(
        stats_manager.get_stats_receiver(),
        &format_options,
        upload_status.clone(),
        file_watcher,
        stats_manager,
    ) {
        eprintln!("Error displaying TUI: {e}");
    }
}

async fn run_background_upload(
    initial_stats: MultiAnalyzerStats,
    upload_status: Arc<Mutex<tui::UploadStatus>>,
) {
    // Helper to set status
    fn set_status(status: &Arc<Mutex<tui::UploadStatus>>, value: tui::UploadStatus) {
        if let Ok(mut s) = status.lock() {
            *s = value;
        }
    }

    // Don't set initial upload status - let it get set by the first progress callback
    tokio::time::sleep(Duration::from_millis(500)).await;

    let upload_result = async {
        let config = config::Config::load().ok().flatten()?;
        if !config.is_configured() {
            return None;
        }
        let mut messages = vec![];
        for analyzer_stats in initial_stats.analyzer_stats {
            messages.extend(analyzer_stats.messages);
        }
        let mut config = config;
        let messages = utils::get_messages_later_than(config.upload.last_date_uploaded, messages)
            .await
            .ok()?;
        Some(
            upload::upload_message_stats(&messages, &mut config, |current, total| {
                // Preserve the current dots value when updating
                if let Ok(mut status) = upload_status.lock() {
                    match &*status {
                        tui::UploadStatus::Uploading { dots, .. } => {
                            *status = tui::UploadStatus::Uploading {
                                current,
                                total,
                                dots: *dots,
                            };
                        }
                        _ => {
                            *status = tui::UploadStatus::Uploading {
                                current,
                                total,
                                dots: 0,
                            };
                        }
                    }
                }
            })
            .await,
        )
    }
    .await;

    match upload_result {
        Some(Ok(_)) => {
            set_status(&upload_status, tui::UploadStatus::Uploaded);
            // Only hide success messages after 3 seconds
            tokio::time::sleep(Duration::from_secs(3)).await;
            set_status(&upload_status, tui::UploadStatus::None);
        }
        Some(Err(e)) => {
            // Keep error messages visible permanently (don't auto-hide)
            set_status(&upload_status, tui::UploadStatus::Failed(format!("{e:#}")));
        }
        None => return, // Config not available or not configured - skip upload
    }
}

async fn run_upload() -> Result<()> {
    let registry = create_analyzer_registry();
    let stats = registry.load_all_stats().await?;
    let mut messages = vec![];
    for analyzer_stats in stats.analyzer_stats {
        messages.extend(analyzer_stats.messages);
    }
    
    // Load config file to get formatting options
    let config_file = config::Config::load().unwrap_or(None).unwrap_or_default();
    let format_options = utils::NumberFormatOptions {
        use_comma: config_file.formatting.number_comma,
        use_human: config_file.formatting.number_human,
        locale: config_file.formatting.locale,
        decimal_places: config_file.formatting.decimal_places,
    };
    
    match config::Config::load() {
        Ok(Some(mut config)) if config.is_configured() => {
            let messages =
                utils::get_messages_later_than(config.upload.last_date_uploaded, messages)
                    .await
                    .context("Failed to get messages later than last saved date")?;
            let progress_callback = tui::create_upload_progress_callback(&format_options);
            upload::upload_message_stats(&messages, &mut config, progress_callback)
            .await
            .context("Failed to upload messages")?;
            tui::show_upload_success(messages.len(), &format_options);
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
