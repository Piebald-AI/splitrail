use super::opencode::parse_sqlite_messages;
use super::opencode_common::{OpenCodeFormatAnalyzer, OpenCodeFormatConfig};
use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::types::{Application, ConversationMessage};
use anyhow::Result;
use async_trait::async_trait;
use glob::glob;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Analyzer for [Kilo Code CLI](https://kilocode.ai) — a terminal-based AI
/// coding agent forked from OpenCode.
///
/// Supports two on-disk formats:
///
/// 1. **Legacy JSON files** — one JSON file per message under
///    `~/.local/share/kilo/storage/message/`, handled by the shared
///    [`OpenCodeFormatAnalyzer`].
///
/// 2. **SQLite database** — `~/.local/share/kilo/kilo.db` (and channel-specific
///    variants like `kilo-canary.db`), using the identical schema as OpenCode's
///    SQLite database. Reuses the shared SQLite helpers from [`super::opencode`].
///
/// When both sources are present, SQLite records take priority during
/// deduplication (they contain richer data — tool stats, step-finish tokens).
pub struct KiloCliAnalyzer {
    /// Delegate for legacy JSON file parsing.
    json_delegate: OpenCodeFormatAnalyzer,
}

impl KiloCliAnalyzer {
    pub fn new() -> Self {
        Self {
            json_delegate: OpenCodeFormatAnalyzer::new(OpenCodeFormatConfig {
                display_name: "Kilo CLI",
                application: Application::KiloCli,
                hash_prefix: "kilo_cli",
                storage_subdir: "kilo",
            }),
        }
    }

    /// `~/.local/share/kilo/storage/message` — legacy JSON message files.
    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".local/share/kilo/storage/message"))
    }

    /// `~/.local/share/kilo` — parent directory (for watching the DB).
    fn app_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".local/share/kilo"))
    }

    /// Discover all Kilo CLI SQLite database files.
    ///
    /// Kilo stores data in `kilo.db` for the default channel, and
    /// `kilo-{channel}.db` for other channels (e.g. `kilo-canary.db`).
    fn discover_db_files() -> Vec<PathBuf> {
        let Some(app_dir) = Self::app_dir() else {
            return Vec::new();
        };
        if !app_dir.is_dir() {
            return Vec::new();
        }

        let pattern = app_dir.join("kilo*.db");
        let pattern_str = pattern.to_string_lossy().to_string();

        glob(&pattern_str)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|p| {
                // Accept "kilo.db" and "kilo-{channel}.db", but reject
                // WAL/SHM journal files and unrelated matches.
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                name == "kilo.db" || (name.starts_with("kilo-") && name.ends_with(".db"))
            })
            .collect()
    }

    /// Check if any SQLite database file exists on disk.
    fn has_sqlite_db() -> bool {
        !Self::discover_db_files().is_empty()
    }

    /// Check if the legacy JSON message directory has any files.
    fn has_json_messages() -> bool {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|message_dir| {
                WalkDir::new(message_dir)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
            })
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "json")
            })
    }
}

#[async_trait]
impl Analyzer for KiloCliAnalyzer {
    fn display_name(&self) -> &'static str {
        "Kilo CLI"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            // Legacy JSON message files.
            patterns.push(format!(
                "{home_str}/.local/share/kilo/storage/message/*/*.json"
            ));
            // Default SQLite database.
            patterns.push(format!("{home_str}/.local/share/kilo/kilo.db"));
            // Channel-specific SQLite databases (e.g. kilo-canary.db).
            patterns.push(format!("{home_str}/.local/share/kilo/kilo-*.db"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let mut sources: Vec<DataSource> = Vec::new();

        // Discover legacy JSON message files.
        if let Some(data_dir) = Self::data_dir()
            && data_dir.is_dir()
        {
            let json_sources = WalkDir::new(data_dir)
                .min_depth(2)
                .max_depth(2)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "json")
                })
                .map(|e| DataSource {
                    path: e.into_path(),
                });
            sources.extend(json_sources);
        }

        // Discover all SQLite databases (default + channel-specific).
        for db in Self::discover_db_files() {
            sources.push(DataSource { path: db });
        }

        Ok(sources)
    }

    fn is_available(&self) -> bool {
        Self::has_sqlite_db() || Self::has_json_messages()
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        // SQLite database — return all messages at once.
        if source.path.extension().is_some_and(|ext| ext == "db") {
            return parse_sqlite_messages(&source.path, Application::KiloCli, "kilo_cli");
        }

        // Legacy JSON message file — delegate to the shared JSON parser.
        self.json_delegate.parse_source(source)
    }

    /// Load shared context once, then process all JSON files in parallel.
    /// SQLite sources are handled separately since the DB query is already fast.
    fn parse_sources_parallel_with_paths(
        &self,
        sources: &[DataSource],
    ) -> Vec<(PathBuf, Vec<ConversationMessage>)> {
        // Partition sources into JSON files and DB files.
        let (db_sources, json_sources): (Vec<_>, Vec<_>) = sources
            .iter()
            .partition(|s| s.path.extension().is_some_and(|ext| ext == "db"));

        let mut results: Vec<(PathBuf, Vec<ConversationMessage>)> = Vec::new();

        // --- SQLite sources first: parse each DB ---
        // SQLite records are richer (have tool stats, step-finish tokens, etc.)
        // so they are added first. During deduplication (which keeps the first-
        // seen entry per global_hash), SQLite wins over legacy JSON.
        for source in db_sources {
            match parse_sqlite_messages(&source.path, Application::KiloCli, "kilo_cli") {
                Ok(messages) if !messages.is_empty() => {
                    results.push((source.path.clone(), messages));
                }
                Ok(_) => {} // empty DB
                Err(e) => {
                    eprintln!(
                        "Failed to parse Kilo CLI SQLite DB {:?}: {}",
                        source.path, e
                    );
                }
            }
        }

        // --- JSON sources: delegate to the shared JSON parser ---
        if !json_sources.is_empty() {
            let json_data: Vec<DataSource> = json_sources.into_iter().cloned().collect();
            let json_results = self
                .json_delegate
                .parse_sources_parallel_with_paths(&json_data);
            results.extend(json_results);
        }

        results
    }

    /// Parse all sources and deduplicate.
    ///
    /// Deduplication is necessary because messages may exist in both the legacy
    /// JSON files and the SQLite database during the migration period.
    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        let all: Vec<ConversationMessage> = self
            .parse_sources_parallel_with_paths(sources)
            .into_iter()
            .flat_map(|(_, msgs)| msgs)
            .collect();
        crate::utils::deduplicate_by_global_hash(all)
    }

    /// Reuses `parse_sources_parallel` for the shared partition → parse → dedup
    /// pipeline, then aggregates into stats.
    fn get_stats_with_sources(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<crate::types::AgenticCodingToolStats> {
        let messages = self.parse_sources_parallel(&sources);

        let mut daily_stats = crate::utils::aggregate_by_date(&messages);
        daily_stats.retain(|date, _| date != "unknown");
        let num_conversations = daily_stats
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        Ok(crate::types::AgenticCodingToolStats {
            daily_stats,
            num_conversations,
            messages,
            analyzer_name: self.display_name().to_string(),
        })
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        // Watch the legacy JSON message directory.
        if let Some(data_dir) = Self::data_dir()
            && data_dir.is_dir()
        {
            dirs.push(data_dir);
        }

        // Watch the parent app directory for SQLite DB changes.
        if let Some(app_dir) = Self::app_dir()
            && app_dir.is_dir()
        {
            dirs.push(app_dir);
        }

        dirs
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        // Accept any Kilo CLI SQLite database file (kilo.db or kilo-{channel}.db).
        if path.is_file() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == "kilo.db" || (name.starts_with("kilo-") && name.ends_with(".db")) {
                return true;
            }
        }

        // Accept legacy JSON message files at depth 2 from the data_dir.
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
            return false;
        }
        if let Some(data_dir) = Self::data_dir()
            && let Ok(relative) = path.strip_prefix(&data_dir)
        {
            return relative.components().count() == 2;
        }
        false
    }

    /// When a SQLite database is present it contains many messages in a single
    /// file, so [`MultiSession`](ContributionStrategy::MultiSession) is the
    /// correct caching strategy. When only legacy JSON files exist, each file
    /// maps to a single message.
    fn contribution_strategy(&self) -> ContributionStrategy {
        if Self::has_sqlite_db() {
            ContributionStrategy::MultiSession
        } else {
            ContributionStrategy::SingleMessage
        }
    }
}
