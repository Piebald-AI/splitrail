# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Splitrail is a comprehensive agentic AI coding tool usage analyzer written in Rust that provides detailed analytics for Claude Code, Codex CLI, and Gemini CLI usage. It features a rich TUI (Terminal User Interface), automatic data upload to Splitrail Cloud, and extensive usage statistics including token counts, costs, file operations, tool usage, and productivity metrics.

## Development Commands

### Building and Testing
- `cargo check` - Check code compilation without building
- `cargo build` - Build the project in debug mode
- `cargo build --release` - Build optimized release version
- `cargo run` - Run with default behavior (show stats + optional auto-upload)
- `cargo run -- upload` - Manually upload stats to Splitrail Cloud
- `cargo run -- config <subcommand>` - Manage configuration

### Available Commands
- `splitrail` - Show stats in TUI with real-time watching and auto-upload when changes are detected (if auto-upload is enabled)
- `splitrail upload` - Manually upload stats to Splitrail Cloud
- `splitrail config init` - Create default configuration file
- `splitrail config show` - Display current configuration
- `splitrail config set <key> <value>` - Set configuration values
- `splitrail help` - Show help information

## Architecture

### Core Modules

1. **Main Module** (`src/main.rs`): Command-line interface with subcommand routing
2. **Analyzer Framework** (`src/analyzer.rs`): Trait-based analyzer architecture for multiple AI tools
3. **Claude Code Analyzer** (`src/analyzers/claude_code.rs`): Analysis engine for Claude Code data
4. **Codex CLI Analyzer** (`src/analyzers/codex_cli.rs`): Analysis engine for Codex CLI data
5. **Gemini CLI Analyzer** (`src/analyzers/gemini_cli.rs`): Analysis engine for Gemini CLI data
6. **TUI Module** (`src/tui.rs`): Rich terminal user interface using ratatui
7. **Upload Module** (`src/upload.rs`): HTTP client for Splitrail Cloud integration
8. **Config Module** (`src/config.rs`): Configuration file management
9. **Types Module** (`src/types.rs`): Core data structures and enums
10. **Models Module** (`src/models.rs`): Model pricing definitions
11. **Utils Module** (`src/utils.rs`): Utility functions and helpers

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

1. **Multi-Tool Data Discovery**:
   - Claude Code: `~/.claude/projects` directories (JSONL files)
   - Codex CLI: `~/.codex/sessions/**/*.jsonl` files
   - Gemini CLI: `~/.gemini/tmp/*/chats/*.json` directories (JSON session files)
2. **Flexible Conversation Parsing**: Processes different file formats (JSONL, JSON sessions)
3. **Advanced Deduplication**: Uses tool-specific hashing strategies to prevent duplicate entries
4. **Comprehensive Cost Calculation**: Uses actual cost values or calculates from tokens using model pricing
5. **File Operation Tracking**: Monitors tool usage across different AI coding assistants
6. **Todo Analytics**: Tracks TodoWrite/TodoRead usage and task management (Claude Code)
7. **TUI Display**: Interactive terminal interface with multiple views and navigation
8. **Splitrail Cloud Integration**: Secure upload to Splitrail Cloud with API tokens
9. **Configuration Management**: TOML-based config with auto-upload settings

### Model Support

Currently supports:
**Claude Models:**
- `claude-sonnet-4-20250514` (Sonnet 4): $0.003/$0.015 per 1K input/output tokens
- `claude-opus-4-20250514` (Opus 4): $0.015/$0.075 per 1K input/output tokens
- `claude-opus-4.1` / `claude-opus-4-1-20250805` (Opus 4.1): Same as Opus 4 pricing (aliases)
- Cache pricing for both models (creation + read costs)

**GPT Models:**
- `gpt-5`: $1.25/$10.00 per 1K input/output tokens
- `gpt-5-mini`: $0.25/$2.00 per 1K input/output tokens
- `gpt-5-nano`: $0.05/$0.40 per 1K input/output tokens
- Cache pricing supported for all GPT-5 series models

**Gemini CLI Models:**
- `gemini-2.5-pro`: $0.001/$0.003 per 1K input/output tokens
- `gemini-2.5-flash`: $0.0005/$0.0015 per 1K input/output tokens
- `gemini-1.5-pro`: Legacy model support
- `gemini-1.5-flash`: Legacy model support
- Cache read pricing supported

**Codex CLI Models:**
- `o4-mini`: $1.10/$4.40 per 1M input/output tokens (cached: $0.275 per 1M)
- `o3`: $2.00/$8.00 per 1M input/output tokens (cached: $0.50 per 1M) 
- `o3-mini`: $1.10/$4.40 per 1M input/output tokens (cached: $0.55 per 1M)
- `o3-pro`: $20.00/$80.00 per 1M input/output tokens (no caching)
- `o1`, `o1-preview`: $15.00/$60.00 per 1M input/output tokens (cached: $7.50 per 1M)
- `o1-mini`: $1.10/$4.40 per 1M input/output tokens (cached: $0.55 per 1M)
- `o1-pro`: $150.00/$600.00 per 1M input/output tokens (no caching)
- `codex-mini-latest`: $1.50/$6.00 per 1M input/output tokens (cached: $0.375 per 1M)
- `gpt-4.1`: $2.00/$8.00 per 1M input/output tokens (cached: $0.50 per 1M)
- `gpt-4.1-mini`: $0.40/$1.60 per 1M input/output tokens (cached: $0.10 per 1M)
- `gpt-4.1-nano`: $0.10/$0.40 per 1M input/output tokens (cached: $0.025 per 1M)
- `gpt-4o`: $2.50/$10.00 per 1M input/output tokens (cached: $1.25 per 1M)
- `gpt-4o-mini`: $0.15/$0.60 per 1M input/output tokens (cached: $0.075 per 1M)
- `gpt-4-turbo`: $10.00/$30.00 per 1M input/output tokens (no caching)

**Features:**
- Fallback pricing for unknown models
- Multi-dimensional token tracking (input, output, cached, thoughts, tool tokens for Gemini)

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
- `serde`/`simd-json` - SIMD-optimized JSON serialization and parsing
- `chrono`/`chrono-tz` - Timestamp handling and timezone conversion
- `ratatui` - Rich terminal user interface framework
- `crossterm` - Cross-platform terminal manipulation
- `reqwest` - HTTP client for Splitrail Cloud uploads
- `tokio` - Async runtime for HTTP operations
- `async-trait` - Async trait support for analyzer framework
- `toml` - Configuration file format
- `anyhow` - Error handling and context
- `colored` - Terminal color output
- `glob` - File pattern matching for data discovery
- `itertools` - Iterator utilities
- `rayon` - Parallel processing for file parsing
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

### Multi-Tool Support
- **Claude Code**: Full support for JSONL conversation files, TodoWrite/TodoRead tracking
- **Codex CLI**: Command-line coding agent with shell command execution, reasoning model support, and token tracking
- **Gemini CLI**: JSON session parsing with thoughts tracking and multi-dimensional tokens

### Terminal User Interface
- **Daily Stats View**: Comprehensive daily breakdown with costs, tokens, and operations
- **Model Usage**: Model-specific statistics and abbreviations across all supported tools
- **File Operations**: Detailed file operation analytics by category
- **Navigation**: Keyboard controls for scrolling and interaction

### Splitrail Cloud Integration
- Secure API token-based authentication
- Automatic daily stats upload when configured
- Manual upload command for on-demand sharing
- Privacy-focused: only aggregated statistics are uploaded to the leaderboard; per-day statistics are uploaded but are only shown to the user themselves

### Analytics Tracking
- **Token Usage**: Input, output, cache, thoughts, and tool token consumption
- **Cost Analysis**: Precise cost calculations per model and tool
- **File Operations**: Read/write/edit operations with byte/line counts
- **Tool Usage**: Tool-specific command tracking (Bash, Glob, Grep for Claude Code; shell command execution and file operations for Codex CLI; read_many_files, replace, run_shell_command for Gemini CLI)
- **Todo Management**: Task creation, completion, and productivity metrics (Claude Code)
- **Conversation Analytics**: Message counts, tool calls, and flow analysis
- **Deduplication**: Prevents duplicate entries across multiple data sources
