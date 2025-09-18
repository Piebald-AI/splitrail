# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
