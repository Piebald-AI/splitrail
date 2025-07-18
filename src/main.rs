#![feature(if_let_guard)]

use clap::{Args, Parser, Subcommand};

use analyzer::AnalyzerRegistry;
use analyzers::{ClaudeCodeAnalyzer, CodexAnalyzer};
use types::{AgenticCodingToolStats, MultiAnalyzerStats};

mod analyzer;
mod analyzers;
mod config;
mod models;
mod tui;
mod types;
mod upload;
mod utils;

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
    Init,
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
    let config = config::Config::load()
        .unwrap_or_else(|_| None)
        .unwrap_or_default();

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
        Some(Commands::Upload) => {
            run_upload(None).await;
        }
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
    
    registry
}

async fn run_default(format_options: utils::NumberFormatOptions) {
    let registry = create_analyzer_registry();
    
    // Get all available analyzers
    let available_analyzers = registry.available_analyzers();
    if available_analyzers.is_empty() {
        eprintln!("❌ No supported AI coding tools found on this system");
        eprintln!("   💡 Supported tools: Claude Code, Codex");
        std::process::exit(1);
    }

    println!("🔍 Analyzing AI coding tool usage...");

    // Get stats from all available analyzers
    let mut all_stats = Vec::new();
    for analyzer in available_analyzers {
        println!("   📊 Processing {} data...", analyzer.display_name());
        
        match analyzer.get_stats().await {
            Ok(stats) => all_stats.push(stats),
            Err(e) => {
                eprintln!("⚠️  Error analyzing {} data: {}", analyzer.display_name(), e);
                // Continue with other analyzers instead of exiting
            }
        }
    }

    if all_stats.is_empty() {
        eprintln!("❌ No data could be analyzed from any supported tools");
        std::process::exit(1);
    }

    let multi_stats = MultiAnalyzerStats {
        analyzer_stats: all_stats,
    };

    // Show TUI
    if let Err(e) = tui::run_multi_tui(&multi_stats, &format_options) {
        eprintln!("❌ Error displaying TUI: {}", e);
    }

    // For upload, use the analyzer with the most data
    if let Some(primary_stats) = multi_stats.analyzer_stats.iter().max_by_key(|s| s.num_conversations) {
        run_upload(Some(primary_stats.clone())).await;
    }
}

async fn run_upload(stats: Option<AgenticCodingToolStats>) {
    let stats = match stats {
        Some(stats) => {
            println!("🔍 Uploading {} usage...", stats.analyzer_name);
            stats
        }
        None => {
            let registry = create_analyzer_registry();
            
            // Get the primary analyzer (prioritized by data volume)
            let analyzer = match registry.get_primary_analyzer_by_volume().await {
                Some(analyzer) => analyzer,
                None => {
                    eprintln!("❌ No supported AI coding tools found on this system");
                    eprintln!("   💡 Supported tools: Claude Code, Codex");
                    std::process::exit(1);
                }
            };

            println!("🔍 Analyzing {} usage for upload...", analyzer.display_name());

            match analyzer.get_stats().await {
                Ok(stats) => stats,
                Err(e) => {
                    eprintln!("❌ Error analyzing {} data: {}", analyzer.display_name(), e);
                    std::process::exit(1);
                }
            }
        }
    };

    match config::Config::load() {
        Ok(Some(mut config)) if config.is_configured() => {
            let messages = match utils::get_messages_later_than(
                config.last_date_uploaded,
                stats.messages,
            )
            .await
            {
                Ok(messages) => messages,
                Err(e) => {
                    eprintln!("❌ Error getting messages: {}", e);
                    std::process::exit(1);
                }
            };
            if let Err(e) = upload::upload_message_stats(&messages, &mut config).await {
                eprintln!("❌ Upload failed: {:#}", e);
                eprintln!("💡 Tip: Check your configuration with 'splitrail config show'");
                std::process::exit(1);
            }
        }
        Ok(Some(_)) => {
            eprintln!("❌ Configuration incomplete");
            upload::show_upload_help();
            std::process::exit(1);
        }
        Ok(None) => {
            eprintln!("❌ No configuration found");
            upload::show_upload_help();
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("❌ Config error: {}", e);
            std::process::exit(1);
        }
    }
}

async fn handle_config_subcommand(config_args: ConfigArgs) {
    match config_args.subcommand {
        ConfigSubcommands::Init => {
            if let Err(e) = config::create_default_config() {
                eprintln!("❌ Error creating config: {}", e);
                std::process::exit(1);
            }
        }
        ConfigSubcommands::Show => {
            if let Err(e) = config::show_config() {
                eprintln!("❌ Error showing config: {}", e);
                std::process::exit(1);
            }
        }
        ConfigSubcommands::Set { key, value } => {
            if let Err(e) = config::set_config_value(&key, &value) {
                eprintln!("❌ Error setting config: {}", e);
                std::process::exit(1);
            }
        }
    }
}
