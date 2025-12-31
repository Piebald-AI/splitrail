# Project Overview

Splitrail is a high-performance, cross-platform usage tracker for AI coding assistants (Claude Code, Copilot, Cline, Pi Agent, etc.). It analyzes local data files from these tools, aggregates usage statistics, and provides real-time TUI monitoring with optional cloud upload capabilities.

# Architecture

## Core Analyzer System

The codebase uses a **pluggable analyzer architecture** with the `Analyzer` trait as the foundation:

1. **AnalyzerRegistry** (`src/analyzer.rs`) - Central registry managing all analyzers
   - Discovers data sources across platforms (macOS, Linux, Windows)
   - Coordinates parallel loading of analyzer stats

2. **Individual Analyzers** (`src/analyzers/`) - Platform-specific implementations
   - `claude_code.rs` - Claude Code analyzer (largest, most complex)
   - `copilot.rs` - GitHub Copilot
   - `cline.rs`, `roo_code.rs`, `kilo_code.rs` - VSCode extensions
   - `codex_cli.rs`, `gemini_cli.rs`, `qwen_code.rs`, `opencode.rs`, `pi_agent.rs` - CLI tools

   Each analyzer:
   - Discovers data sources via glob patterns or VSCode extension paths
   - Parses conversations from JSON/JSONL files
   - Normalizes to `ConversationMessage` format

## Data Flow

1. **Discovery**: Analyzers find data files using platform-specific paths
2. **Parsing**: Parse JSON/JSONL files into `ConversationMessage` structs
3. **Deduplication**: Hash-based dedup using `global_hash` field (critical for accuracy)
4. **Aggregation**: Group messages by date, compute token counts, costs, file ops
5. **Display**: TUI renders daily stats + real-time updates via file watcher

## Key Types (`src/types.rs`)

- **ConversationMessage**: Normalized message format across all analyzers
  - Contains tokens, costs, file operations, tool usage stats
  - Includes hashes for deduplication (`local_hash`, `global_hash`)

- **Stats**: Comprehensive usage metrics
  - Token counts (input, output, reasoning, cache tokens)
  - File operations (reads, edits, deletes with line/byte counts)
  - Todo tracking (created, completed, in_progress)
  - File categorization (code, docs, data, media, config)

- **DailyStats**: Pre-aggregated stats per date
  - Message counts, conversation counts, model breakdown
  - Embedded `Stats` struct with all metrics

## Real-Time Monitoring

**FileWatcher** (`src/watcher.rs`) provides live updates:
- Watches analyzer data directories using `notify` crate
- Triggers incremental re-parsing on file changes
- Updates TUI in real-time via channels

**RealtimeStatsManager** coordinates:
- Background file watching
- Auto-upload to Splitrail Cloud (if configured)
- Stats updates to TUI via `tokio::sync::watch`

## MCP Server (`src/mcp/`)

Splitrail can run as an MCP (Model Context Protocol) server:
```bash
cargo run -- mcp
```

Provides tools for:
- `get_daily_stats` - Query usage statistics with filtering
- `get_model_usage` - Analyze model usage distribution
- `get_cost_breakdown` - Get cost breakdown over a date range
- `get_file_operations` - Get file operation statistics
- `compare_tools` - Compare usage across different AI coding tools
- `list_analyzers` - List available analyzers

Resources:
- `splitrail://summary` - Daily summaries across all dates
- `splitrail://models` - Model usage breakdown

# Testing Strategy

## Test Organization
- **Unit tests**: Inline with source (`#[cfg(test)] mod tests`)
- **Integration tests**: `src/analyzers/tests/` for analyzer-specific parsing tests

## Test Data
Most analyzers use real-world JSON fixtures in test modules to verify parsing logic.

# Common Development Tasks

## Adding a New Analyzer

1. Create new file in `src/analyzers/your_analyzer.rs`
2. Implement the `Analyzer` trait:
   ```rust
   #[async_trait]
   impl Analyzer for YourAnalyzer {
       fn display_name(&self) -> &'static str { "Your Tool" }
       fn discover_data_sources(&self) -> Result<Vec<DataSource>> { ... }
       async fn parse_conversations(&self, sources: Vec<DataSource>) -> Result<Vec<ConversationMessage>> { ... }
       // ... other required methods
   }
   ```
3. For VSCode extensions, use `discover_vscode_extension_sources()` helper
4. Register in `src/main.rs::create_analyzer_registry()`
5. Add to `Application` enum in `src/types.rs`

## Pricing Model Updates

Token pricing is in `src/models.rs` using compile-time `phf` maps:
- Add new model to appropriate constant (e.g., `ANTHROPIC_PRICING`)
- Format: model name -> `PricePerMillion { input, output, cache_creation, cache_read }`
- Prices in USD per million tokens

# Configuration

User config stored at `~/.splitrail.toml`:
```toml
[server]
url = "https://splitrail.dev"
api_token = "..."

[upload]
auto_upload = false
upload_today_only = false

[formatting]
number_comma = false
number_human = false
locale = "en"
decimal_places = 2
```

# Performance Considerations

1. **Parallel Loading**: Analyzers load in parallel via `futures::join_all()`
2. **Rayon for Parsing**: Use `.par_iter()` when parsing multiple files
3. **Lazy Message Loading**: TUI loads messages on-demand for session view

# Code Style

- Follow Rust 2024 edition conventions
- Use `anyhow::Result` for error handling
- Prefer `async/await` over raw futures
- Use `parking_lot` locks over `std::sync` for performance
- Keep large modules like `tui.rs` self-contained (consider refactoring if adding major features)

# Post-Change Verification

Run after code changes:
```bash
cargo build --release --quiet
cargo test --quiet
cargo clippy --quiet -- -D warnings
cargo fmt --check
```
