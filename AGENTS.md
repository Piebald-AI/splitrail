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

# Code Style

- Follow Rust 2024 edition conventions
- Use `anyhow::Result` for error handling
- Prefer `async/await` over raw futures
- Use `parking_lot` locks over `std::sync` for performance

# Post-Change Verification

Run after code changes:
```bash
cargo build --release --quiet
cargo test --quiet
cargo clippy --quiet -- -D warnings
cargo fmt --check
```

# Additional Context

Read these files when working on specific areas:

- **Adding a new analyzer?** Read `.agents/NEW_ANALYZER.md`
- **Working on tests?** Read `.agents/TESTING.md`
- **Working on the MCP server?** Read `.agents/MCP.md`
- **Updating model pricing?** Read `.agents/PRICING.md`
- **Working with core types?** Read `.agents/TYPES.md`
- **Working on TUI or file watching?** Read `.agents/TUI.md`
- **Optimizing performance?** Read `.agents/PERFORMANCE.md`
