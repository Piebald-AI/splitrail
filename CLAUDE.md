# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Splitrail is a Claude usage analyzer written in Rust that parses Claude data files and provides detailed usage statistics including token counts, costs, and daily breakdowns. The tool analyzes JSONL files from Claude's data directories and presents comprehensive usage reports with color-coded output.

## Development Commands

### Building and Testing
- `cargo check` - Check code compilation without building
- `cargo build` - Build the project
- `cargo run` - Run the usage analyzer

### Code Quality
- The project compiles with warnings about unused fields in `ProcessedEntry` struct (fields: `message_id`, `request_id`, `has_cost_usd`)

## Architecture

### Core Components

1. **Main Module** (`src/main.rs`): Simple entry point that calls the analyzer
2. **Usage Analyzer** (`src/claude_usage_analyzer.rs`): Contains all core functionality

### Key Data Structures

- `ProcessedEntry`: Represents a processed Claude API usage entry with tokens, cost, and metadata
- `DailyStats`: Aggregates usage statistics by date
- `ClaudeEntry`: Raw entry structure from JSONL files
- `Usage`: Token usage information from API calls

### Core Functionality

1. **Data Discovery**: Finds Claude data directories in `~/.claude/projects` and current directory's `.claude/projects`
2. **JSONL Parsing**: Reads and parses Claude usage data from JSONL files
3. **Deduplication**: Uses message ID + request ID hash to prevent duplicate entries
4. **Cost Calculation**: Either uses pre-calculated `costUSD` or calculates from tokens using model pricing
5. **Time Zone Handling**: Converts UTC timestamps to America/New_York timezone
6. **Daily Aggregation**: Groups usage by date with comprehensive statistics
7. **Formatted Output**: Color-coded table display with aligned columns

### Model Support

Currently supports:
- `claude-sonnet-4-20250514` (Sonnet 4)
- `claude-opus-4-20250514` (Opus 4)
- Fallback pricing for unknown models

### Dependencies

Key dependencies include:
- `serde`/`serde_json` for JSON parsing
- `chrono`/`chrono-tz` for timestamp handling
- `colored` for terminal output formatting
- `tokio` for async operations
- `anyhow` for error handling
- `glob` for file pattern matching
- `itertools` for iterator utilities

## File Structure

- Python version (`claude_usage_simple.py`) exists as an alternative implementation with similar functionality but different output format
- Both versions implement the same core logic: deduplication, cost calculation, and usage analysis