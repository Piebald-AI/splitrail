# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Splitrail is a comprehensive Claude Code usage analyzer written in Rust that provides detailed analytics for agentic AI coding tool usage. It features a rich TUI (Terminal User Interface), automatic data upload to the Splitrail Leaderboard, and extensive usage statistics including token counts, costs, file operations, tool usage, and productivity metrics.

## Development Commands

### Building and Testing
- `cargo check` - Check code compilation without building
- `cargo build` - Build the project in debug mode
- `cargo build --release` - Build optimized release version
- `cargo run` - Run with default behavior (show stats + optional auto-upload)
- `cargo run -- upload` - Force upload stats to leaderboard
- `cargo run -- config <subcommand>` - Manage configuration

### Available Commands
- `splitrail` - Show Claude Code stats in TUI and auto-upload if configured
- `splitrail upload` - Force upload stats to Splitrail Leaderboard
- `splitrail config init` - Create default configuration file
- `splitrail config show` - Display current configuration
- `splitrail config set <key> <value>` - Set configuration values
- `splitrail help` - Show help information

## Architecture

### Core Modules

1. **Main Module** (`src/main.rs`): Command-line interface with subcommand routing
2. **Claude Code Analyzer** (`src/claude_code.rs`): Core analysis engine for Claude Code data
3. **TUI Module** (`src/tui.rs`): Rich terminal user interface using ratatui
4. **Upload Module** (`src/upload.rs`): HTTP client for Splitrail Leaderboard integration
5. **Config Module** (`src/config.rs`): Configuration file management
6. **Types Module** (`src/types.rs`): Core data structures and enums
7. **Models Module** (`src/models.rs`): Model pricing definitions
8. **Utils Module** (`src/utils.rs`): Utility functions and helpers

### Key Data Structures

#### Core Types
- `ConversationMessage`: Represents individual AI/User messages with full analytics
- `DailyStats`: Comprehensive daily usage aggregations
- `AgenticCodingToolStats`: Top-level container for all analytics
- `ModelPricing`: Token cost definitions per model

#### Analytics Types
- `FileOperationStats`: Tracks file read/write/edit operations by type and volume
- `TodoStats`: Tracks todo list usage and task completion
- `FileCategory`: Categorizes files by type (SourceCode, Data, Documentation, etc.)

### Core Functionality

1. **Data Discovery**: Locates Claude Code data in `~/.claude/projects` directories
2. **Conversation Parsing**: Processes JSONL conversation files with full message history
3. **Deduplication**: Uses message+request ID hashing to prevent duplicate entries
4. **Cost Calculation**: Uses actual `costUSD` values or calculates from tokens using model pricing
5. **File Operation Tracking**: Monitors Read, Edit, Write, Bash, Glob, and Grep tool usage
6. **Todo Analytics**: Tracks TodoWrite/TodoRead usage and task management
7. **TUI Display**: Interactive terminal interface with multiple views and navigation
8. **Leaderboard Integration**: Secure upload to Splitrail Leaderboard with API tokens
9. **Configuration Management**: TOML-based config with auto-upload settings

### Model Support

Currently supports:
- `claude-sonnet-4-20250514` (Sonnet 4): $0.003/$0.015 per 1K input/output tokens
- `claude-opus-4-20250514` (Opus 4): $0.015/$0.075 per 1K input/output tokens
- Cache pricing for both models (creation + read costs)
- Fallback pricing for unknown models

### File Categories

Automatically categorizes files into:
- **Source Code**: .rs, .py, .js, .ts, .java, .cpp, .go, etc.
- **Data**: .json, .xml, .yaml, .csv, .sql, .db, etc.
- **Documentation**: .md, .txt, .html, .pdf, etc.
- **Media**: .png, .jpg, .mp4, .mp3, etc.
- **Config**: .config, .env, .toml, .ini, etc.
- **Other**: Everything else

### Dependencies

Core dependencies:
- `serde`/`serde_json` - JSON serialization and parsing
- `chrono`/`chrono-tz` - Timestamp handling and timezone conversion
- `ratatui` - Rich terminal user interface framework
- `crossterm` - Cross-platform terminal manipulation
- `reqwest` - HTTP client for leaderboard uploads
- `tokio` - Async runtime for HTTP operations
- `toml` - Configuration file format
- `anyhow` - Error handling and context
- `colored` - Terminal color output
- `glob` - File pattern matching
- `itertools` - Iterator utilities
- `rayon` - Parallel processing
- `dashmap` - Concurrent hash maps
- `num-format` - Number formatting
- `home` - Home directory detection
- `lazy_static` - Static data initialization

## Configuration

Configuration is stored in `~/.config/splitrail/config.toml`:

```toml
[upload]
api_token = "st_your_token_here"
auto_upload = false
```

### Configuration Commands
- `splitrail config init` - Creates default config file
- `splitrail config show` - Displays current settings
- `splitrail config set api-token <token>` - Sets API token
- `splitrail config set auto-upload <true|false>` - Enables/disables auto-upload

## Features

### Terminal User Interface
- **Daily Stats View**: Comprehensive daily breakdown with costs, tokens, and operations
- **Model Usage**: Model-specific statistics and abbreviations
- **File Operations**: Detailed file operation analytics by category
- **Navigation**: Keyboard controls for scrolling and interaction

### Leaderboard Integration
- Secure API token-based authentication
- Automatic daily stats upload when configured
- Manual upload command for on-demand sharing
- Privacy-focused: only aggregated statistics are uploaded

### Analytics Tracking
- **Token Usage**: Input, output, and cache token consumption
- **Cost Analysis**: Precise cost calculations per model
- **File Operations**: Read/write/edit operations with byte/line counts
- **Tool Usage**: Bash commands, Glob searches, Grep operations
- **Todo Management**: Task creation, completion, and productivity metrics
- **Conversation Analytics**: Message counts, tool calls, and flow analysis
