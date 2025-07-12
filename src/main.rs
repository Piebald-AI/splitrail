#![feature(if_let_guard, let_chains)]

mod claude_code;
mod config;
mod models;
mod tui;
mod types;
mod upload;
mod utils;

use std::env;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    match args.len() {
        1 => {
            // No arguments - run normal flow with optional upload
            run_default().await;
        }
        2 => {
            // Single argument - handle subcommands
            match args[1].as_str() {
                "upload" => run_upload().await,
                "config" => config_subcommand(&args[2..]).await,
                "help" | "--help" | "-h" => show_help(),
                _ => {
                    eprintln!("Unknown command: {}", args[1]);
                    show_help();
                    std::process::exit(1);
                }
            }
        }
        _ => {
            // Multiple arguments - handle config subcommands
            if args[1] == "config" {
                config_subcommand(&args[2..]).await;
            } else {
                eprintln!("Too many arguments");
                show_help();
                std::process::exit(1);
            }
        }
    }
}

async fn run_default() {
    println!("🔍 Analyzing Claude Code usage...");

    // Get Claude Code stats
    let stats = match claude_code::get_claude_code_stats().await {
        Ok(stats) => stats,
        Err(e) => {
            eprintln!("❌ Error analyzing Claude Code data: {}", e);
            std::process::exit(1);
        }
    };

    // Show TUI
    if let Err(e) = tui::run_tui(&stats) {
        eprintln!("❌ Error displaying TUI: {}", e);
    }

    // Check if auto-upload is enabled
    match config::Config::load() {
        Ok(Some(config)) if config.upload.auto_upload && config.is_configured() => {
            println!();
            println!("📡 Auto-upload enabled, uploading stats...");

            if let Err(e) = upload::upload_daily_stats(&stats.daily_stats, &config).await {
                eprintln!("⚠️  Upload failed: {}", e);
                eprintln!("   💡 Tip: Check your configuration with 'splitrail config show'");
            }
        }
        Ok(Some(config)) if config.upload.auto_upload => {
            println!();
            println!("⚠️  Auto-upload enabled but configuration incomplete");
            upload::show_upload_help();
        }
        Ok(Some(_)) => {
            // Config exists but auto-upload disabled
            println!();
            println!("💡 Tip: Enable auto-upload with 'splitrail config set auto-upload true'");
        }
        Ok(None) => {
            // No config file
            println!();
            println!("💡 Tip: Configure splitrail to upload to the Splitrail Leaderboard:");
            upload::show_upload_help();
        }
        Err(e) => {
            eprintln!("⚠️  Config error: {}", e);
        }
    }
}

async fn run_upload() {
    println!("🔍 Analyzing Claude Code usage for upload...");

    let stats = match claude_code::get_claude_code_stats().await {
        Ok(stats) => stats,
        Err(e) => {
            eprintln!("❌ Error analyzing Claude Code data: {}", e);
            std::process::exit(1);
        }
    };

    match config::Config::load() {
        Ok(Some(config)) if config.is_configured() => {
            if let Err(e) = upload::upload_daily_stats(&stats.daily_stats, &config).await {
                eprintln!("❌ Upload failed: {}", e);
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

async fn config_subcommand(args: &[String]) {
    if args.is_empty() {
        show_config_help();
        return;
    }

    match args[0].as_str() {
        "init" => {
            if let Err(e) = config::create_default_config() {
                eprintln!("❌ Error creating config: {}", e);
                std::process::exit(1);
            }
        }
        "show" => {
            if let Err(e) = config::show_config() {
                eprintln!("❌ Error showing config: {}", e);
                std::process::exit(1);
            }
        }
        "set" => {
            if args.len() != 3 {
                eprintln!("❌ Usage: splitrail config set <key> <value>");
                eprintln!("   Keys: api-token, auto-upload");
                std::process::exit(1);
            }

            if let Err(e) = config::set_config_value(&args[1], &args[2]) {
                eprintln!("❌ Error setting config: {}", e);
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("❌ Unknown config command: {}", args[0]);
            show_config_help();
            std::process::exit(1);
        }
    }
}

fn show_help() {
    println!("🚀 Splitrail - Claude Code Usage Analytics");
    println!();
    println!("USAGE:");
    println!("    splitrail [COMMAND]");
    println!();
    println!("COMMANDS:");
    println!("    (no args)    Show Claude Code stats and auto-upload if configured");
    println!("    upload       Force upload stats to leaderboard");
    println!("    config       Manage configuration");
    println!("    help         Show this help message");
    println!();
    println!("CONFIG COMMANDS:");
    println!("    config init            Create default configuration file");
    println!("    config show            Show current configuration");
    println!("    config set <key> <val> Set configuration value");
    println!();
    println!("EXAMPLES:");
    println!("    splitrail                                    # Show stats");
    println!("    splitrail config set api-token st_abc123...");
    println!("    splitrail config set auto-upload true");
    println!("    splitrail upload                             # Manual upload");
}

fn show_config_help() {
    println!("🔧 Splitrail Configuration Commands");
    println!();
    println!("USAGE:");
    println!("    splitrail config <COMMAND>");
    println!();
    println!("COMMANDS:");
    println!("    init            Create default configuration file");
    println!("    show            Show current configuration");
    println!("    set <key> <val> Set configuration value");
    println!();
    println!("CONFIG KEYS:");
    println!("    api-token       Your API token from the leaderboard");
    println!("    auto-upload     Enable/disable automatic upload (true/false)");
}
