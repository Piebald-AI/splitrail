# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Splitrail is a high-performance, cross-platform usage tracker for AI coding assistants (Claude Code, Copilot, Cline, etc.). It analyzes local data files from these tools, aggregates usage statistics, and provides real-time TUI monitoring with optional cloud upload capabilities.

**Key Technologies:**
- Rust (edition 2024) with async/await (Tokio)
- Memory-mapped persistent caching (rkyv, memmap2) for fast incremental parsing
- Terminal UI (ratatui + crossterm)
- MCP (Model Context Protocol) server support

## Building and Running

### Basic Commands
```bash
# Build and run (release mode recommended for performance)
cargo run --release

# Run in development mode
cargo run

# Run tests
cargo test

# Run specific test
cargo test test_name

# Run tests for a specific module
cargo test --test module_name

# Build only (no run)
cargo build --release
```

### Windows-Specific Setup
Windows requires `lld-link.exe` from LLVM for fast compilation. Install via:
```bash
winget install --id LLVM.LLVM
```
Then add `C:\Program Files\LLVM\bin\` to system PATH.

## Architecture

### Core Analyzer System

The codebase uses a **pluggable analyzer architecture** with the `Analyzer` trait as the foundation:

1. **AnalyzerRegistry** (`src/analyzer.rs`) - Central registry managing all analyzers
   - Discovers data sources across platforms (macOS, Linux, Windows)
   - Coordinates parallel loading of analyzer stats
   - Manages two-tier caching system (see below)

2. **Individual Analyzers** (`src/analyzers/`) - Platform-specific implementations
   - `claude_code.rs` - Claude Code analyzer (largest, most complex)
   - `copilot.rs` - GitHub Copilot
   - `cline.rs`, `roo_code.rs`, `kilo_code.rs` - VSCode extensions
   - `codex_cli.rs`, `gemini_cli.rs`, `qwen_code.rs`, `opencode.rs` - CLI tools

   Each analyzer:
   - Discovers data sources via glob patterns or VSCode extension paths
   - Parses conversations from JSON/JSONL files
   - Normalizes to `ConversationMessage` format
   - Implements optional incremental caching via `parse_single_file()`

### Two-Tier Caching System

**Critical for performance** - the caching system enables instant startup and incremental updates:

1. **Per-File Cache** (`src/cache/mmap_repository.rs`)
   - Memory-mapped rkyv archive for zero-copy access
   - Stores metadata + daily stats per file
   - Separate message storage (loaded lazily)
   - Detects file changes via size/mtime comparison
   - Supports delta parsing for append-only JSONL files

2. **Snapshot Cache** (`src/cache/mod.rs::load_snapshot_hot_only()`)
   - Caches final deduplicated result per analyzer
   - "Hot" snapshot: lightweight stats for TUI display
   - "Cold" snapshot: full messages for session details
   - Fingerprint-based invalidation (hashes all source file paths + metadata)

**Cache Flow:**
- **Warm start**: Fingerprint matches → load hot snapshot → instant display
- **Incremental**: Files changed → parse only changed files → merge with cached messages → rebuild stats
- **Cold start**: No cache → parse all files → save snapshot for next time

### Data Flow

1. **Discovery**: Analyzers find data files using platform-specific paths
2. **Parsing**: Parse JSON/JSONL files into `ConversationMessage` structs
3. **Deduplication**: Hash-based dedup using `global_hash` field (critical for accuracy)
4. **Aggregation**: Group messages by date, compute token counts, costs, file ops
5. **Display**: TUI renders daily stats + real-time updates via file watcher

### Key Types (`src/types.rs`)

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

### Real-Time Monitoring

**FileWatcher** (`src/watcher.rs`) provides live updates:
- Watches analyzer data directories using `notify` crate
- Invalidates cache entries on file changes
- Triggers incremental re-parsing
- Updates TUI in real-time via channels

**RealtimeStatsManager** coordinates:
- Background file watching
- Auto-upload to Splitrail Cloud (if configured)
- Stats updates to TUI via `tokio::sync::watch`

### MCP Server (`src/mcp/`)

Splitrail can run as an MCP (Model Context Protocol) server:
```bash
cargo run -- mcp
```

Provides tools for:
- `get_daily_stats` - Query usage statistics with filtering
- `get_conversation_messages` - Retrieve message details
- `get_model_breakdown` - Analyze model usage distribution

Resources:
- `splitrail://summary` - Daily summaries across all dates
- `splitrail://models` - Model usage breakdown

## Testing Strategy

### Test Organization
- **Unit tests**: Inline with source (`#[cfg(test)] mod tests`)
- **Integration tests**: `src/analyzers/tests/` for analyzer-specific parsing tests
- **Large test files**: Comprehensive tests in cache module for concurrency, persistence

### Running Tests
```bash
# All tests
cargo test

# Specific analyzer
cargo test claude_code

# Cache tests (many edge cases covered here)
cargo test cache

# Single test
cargo test test_file_metadata_is_stale
```

### Test Data
Most analyzers use real-world JSON fixtures in test modules to verify parsing logic.

## Common Development Tasks

### Adding a New Analyzer

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

### Enabling Incremental Caching for an Analyzer

1. Implement `parse_single_file()` to parse one file
2. Return `supports_caching() -> true`
3. For JSONL files, implement `parse_single_file_incremental()` and return `supports_delta_parsing() -> true`
4. Include pre-aggregated `daily_contributions` in `FileCacheEntry`

### Pricing Model Updates

Token pricing is in `src/models.rs` using compile-time `phf` maps:
- Add new model to appropriate constant (e.g., `ANTHROPIC_PRICING`)
- Format: model name → `PricePerMillion { input, output, cache_creation, cache_read }`
- Prices in USD per million tokens

## Configuration

User config stored at `~/.splitrail/config.toml`:
```toml
[upload]
api_token = "..."
server_url = "https://splitrail.dev/api"
auto_upload = false

[formatting]
number_comma = false
number_human = false
locale = "en"
decimal_places = 2
```

Cache stored at:
- `~/.splitrail/cache.meta` - Memory-mapped metadata index
- `~/.splitrail/snapshots/*.hot` - Hot snapshot cache
- `~/.splitrail/snapshots/*.cold` - Cold message cache

## Performance Considerations

1. **Parallel Loading**: Analyzers load in parallel via `futures::join_all()`
2. **Rayon for Parsing**: Use `.par_iter()` when parsing multiple files
3. **Zero-Copy Cache**: rkyv enables instant deserialization from mmap
4. **Delta Parsing**: JSONL analyzers parse only new lines since last offset
5. **Lazy Message Loading**: TUI loads messages on-demand for session view

## Code Style

- Follow Rust 2024 edition conventions
- Use `anyhow::Result` for error handling
- Prefer `async/await` over raw futures
- Use `parking_lot` locks over `std::sync` for performance
- Keep large modules like `tui.rs` self-contained (consider refactoring if adding major features)
