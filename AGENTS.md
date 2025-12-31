# Project Overview

Splitrail is a high-performance, cross-platform usage tracker for AI coding assistants (Claude Code, Copilot, Cline, Pi Agent, etc.). It analyzes local data files from these tools, aggregates usage statistics, and provides real-time TUI monitoring with optional cloud upload capabilities.

# Architecture

## Analyzer System

Pluggable architecture with the `Analyzer` trait. Registry in `src/analyzer.rs`, individual analyzers in `src/analyzers/`. Each analyzer discovers data sources, parses conversations, and normalizes to a common format.

## Data Flow

1. **Discovery**: Analyzers find data files using platform-specific paths (`src/analyzers/`)
2. **Parsing**: Parse JSON/JSONL into normalized messages (`src/types.rs`)
3. **Deduplication**: Hash-based dedup using global hash field
4. **Aggregation**: Group by date, compute token counts, costs, file ops (`src/utils.rs`)
5. **Display**: TUI renders daily stats + real-time updates (`src/tui.rs`, `src/watcher.rs`)

# Code Style

- Follow Rust 2024 edition conventions
- Use `anyhow::Result` for error handling
- Prefer `async/await` over raw futures
- Use `parking_lot` locks over `std::sync` for performance

# Post-Change Verification

Run after code changes:
```bash
cargo build --all-features --all-targets --quiet
cargo test --all-features --quiet
cargo clippy --all-features --quiet -- -D warnings
cargo doc --all-features --quiet
cargo fmt --all --quiet
```

# Additional Context

Read these files when working on specific areas:

- **Adding a new analyzer?** Read `.agents/NEW_ANALYZER.md`
- **Working on the MCP server?** Read `.agents/MCP.md`
- **Updating model pricing?** Read `.agents/PRICING.md`
- **Working with core types?** Read `.agents/TYPES.md`
- **Working on TUI or file watching?** Read `.agents/TUI.md`
- **Optimizing performance?** Read `.agents/PERFORMANCE.md`
