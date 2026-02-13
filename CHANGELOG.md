# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [3.3.3] - 2026-02-13
- fix: strip non-standard numeric format annotations from MCP JSON schemas (#114) - @mike1858
- feat: add pricing for Gemini 3 Flash and GPT-5.3-Codex (#115) - @mike1858

## [3.3.2] - 2026-02-05
- Add Opus 4.6 support (#110) - @mike1858

## [3.3.1] - 2026-01-30
- Fix #104 (#105) - @mike1858
- Add a --dry-run flag for uploading (#106) - @basekevin
- Add support for Z.AI/Zhipu AI, xAI, and Synthetic.new models (#107) - @Sewer56
- Add per-model daily stats to the JSON output (#108) - @signadou

## [3.3.0] - 2026-01-15
- Improved memory usage with incremental file-level updates and memory-efficient contribution caching (#99) - @Sewer56
- Refactored CLAUDE.md to AGENTS.md with modular skill documentation (#94) - @Sewer56
- Added support for GPT-5.2-Codex model (#101) - @mike1858

## [3.2.2] - 2025-12-27
- TUI: Display cached tokens in session view instead of redundant tool column (#88) - @Sewer56
- TUI: Filter summary totals to selected day in session view (#87) - @Sewer56
- TUI: Add 'r' hotkey to toggle reverse sort order (#84) - @Sewer56
- Fix hash collisions by using timestamp+id hash for deduplication (#90) - @mike1858
- Fix file watcher to detect new sessions in nested directories (#86) - @Sewer56
- Fix OpenCode parsing crash when messages contain boolean summary field (#83) - @Sewer56

## [3.2.1] - 2025-12-13
- Added update notifications: Splitrail now checks GitHub Releases on startup and displays a banner when a new version is available - @mike1858

## [3.2.0] - 2025-12-13
- We now fully support [<img src="https://github.com/Piebald-AI/piebald/raw/main/assets/logo.svg" width="15"> **Piebald**](https://piebald.ai/)!  Track your Piebald usage across all your favorite providers - @mike1858
- **Breaking**: Removed the disk-based caching system entirely to fix stats fluctuation bugs during file watching - @mike1858
- Added in-memory message cache for fast incremental updates during file watching - @mike1858
- Removed cache CLI commands (`cache stats`, `cache clear`, `cache rebuild`, `cache path`) - @mike1858
- Cold startup is now ~1s instead of instant, but results are guaranteed correct - @mike1858
- Added timezone support for uploads - @mike1858
- Fixed Codex CLI duplicate entries by including entry count in globalHash - @mike1858
- Added GPT-5.2, GPT-5.2-Pro, and GPT-5-Pro model pricing support - @mike1858

## [3.1.1] - 2025-12-05
- Use real GPT-5.1-Codex-Max's pricing (reported pricings should be a bit lower) - @mike1858

## [3.1.0] - 2025-12-05
- We now fully support the [Pi coding agent](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent)!  Track your historical (and current) Pi usage - @mike1858
- Splitrail Cloud: Update Next.js version in response to [critical security vulnerability](https://github.com/facebook/react/security/advisories/GHSA-fv66-9v8q-g76r) - @mike1858
- Splitrail Cloud: support Pi agent - @mike1858

## [3.0.0] - 2025-11-30
- Introducing the VS Code extension: now you can view track token counts, costs and more straight from VS Code - @mike1858
- Introducing the MCP server: let Claude access your usage history with the Splitrail MCP server - @mike1858
- Introducing JSON output mode: `--json` lets you build apps on top of Splitrail - @mike1858
- 10x the performance: Splitrail startup time has been reduced from ~2s to ~200ms - @mike1858
- We now fully support OpenCode!  Track your historical OpenCode usage - @mike1858
- Claude Opus 4.5: Anthropic's most powerful frontier model is now in Splitrail - @mike1858
- Cache manipulation: you can now manipulate Splitrail's cache: view, clear and rebuild.
- Faster hashing: we've significantly improved hashing speed by moving from SHA256 to `xxhash`.
- Fixed an issue where Gemini CLI info messages were incorrectly being parsed as conversation entries - @mike1858
- Added support for re-uploading zero-cost messages in case we didn't add a model but you used it anyway - @mike1858
- Added over 3.5 thousand lines of new tests, covering the TUI, upload system, analyzers, utilities, and file watcher - @mike1858

## [2.2.3] - 2025-11-23
- Add code-signing for Windows and macOS builds (#47) - @signadou
- Fix: Timezone issue in TUI & Add: Gemini CLI reasoning tokens (#45) - @mike1858
- Feat: Add --full and --force-analyzer flag to upload command (#46) - @mike1858

## [2.2.2] - 2025-11-21
- Correct extraneous upload requests (#43) - @mike1858

## [2.2.1] - 2025-11-20

- Fix `REQUEST_ENTITY_TOO_LARGE` uploading errors (#41) - @mike1858

## [2.2.0] - 2025-11-20

- Add support for estimated pricing; add GPT-5.1-Codex-Mini and (estimated) GPT-5.1-Codex-Max (#39) - @mike1858
- Add support for searching across days (#37) - @mike1858
- Allow `content` to be an array of content blocks or a string in CC analyzer (#38) - @mike1858
- Greatly improve performance (#36) - @mike1858
- Various fixes and improvments to Splitrail Cloud - @mike1858

## [2.1.0] - 2025-11-19

- Add support for Copilot - @mcowger
- Add better support for Roo Code - @mcowger
- Add support for per-session token + cost tracking - @mike1858
- Various fixes and improvements to Splitrail Cloud - @mike1858

## [2.0.0] - 2025-11-10

- Add support for Cline, Kilo Code, Roo Code (Cline forks) and Qwen Code (Gemini CLI fork) (#25 and #26) - @bl-ue
- Various fixes and improvements to Splitrail Cloud

## [1.2.0] - 2025-10-27

- Correct Claude Code token aggregation for split messages (#23) - @bl-ue

## [1.1.3] - 2025-10-24

- Add support for Claude Haiku 4.5 to the Claude Code analyzer (#21) - @bl-ue

## [1.1.2] - 2025-10-01

- Add support for Claude Sonnet 4.5 to the Claude Code analyzer
- Ignore the "file-history-snapshot" entry added in Claude Code 2.0.0

## [1.1.1] - 2025-09-18

- Fix Codex CLI (#16) - @bl-ue

## [1.1.0] - 2025-09-18

### Added
- Enhanced Codex CLI support with updated analyzer functionality
- Better formatting and lint compliance

## [1.0.1] - 2024-09-14

### Added
- Download link in README for binary releases

### Changed
- Rearranged README structure for better readability
- Improved documentation and project presentation

### Fixed
- Various formatting and lint error corrections

## [1.0.0] - 2025-08-09

### Added

- **Initial stable release** of Splitrail
- Real-time automatic uploading to Splitrail Cloud
- Comprehensive multi-tool support:
  - Claude Code
  - Gemini CLI
  - Codex CLI
- Rich Terminal User Interface (TUI) using ratatui
- Advanced cost calculation and token usage analytics
- File operation tracking (read/write/edit operations with byte/line counts)
- Comprehensive model support:
  - Claude models (Sonnet 4, Opus 4, Opus 4.1)
  - GPT models (GPT-5 series, GPT-4 series)
  - Gemini models (2.5-pro, 2.5-flash, legacy 1.5 series)
  - Codex CLI models (o3, o1 series, gpt-4.1 series)
- Configuration management with TOML-based config
- Splitrail Cloud integration with API token authentication
- Deduplication logic to prevent duplicate entries
- Real-time file watching capabilities
- Parallel processing for performance
