use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use jwalk::WalkDir;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::path::PathBuf;

use crate::cache::{FileCacheEntry, FileCacheKey, FileStatsCache};
use crate::types::{AgenticCodingToolStats, ConversationMessage, DailyStats};

/// Merge source DailyStats into destination aggregated map.
/// This is a key optimization - we merge pre-computed aggregates instead of re-aggregating.
fn merge_daily_stats(
    dst: &mut std::collections::BTreeMap<String, DailyStats>,
    date: &str,
    src: &DailyStats,
) {
    dst.entry(date.to_string())
        .and_modify(|existing| {
            existing.user_messages += src.user_messages;
            existing.ai_messages += src.ai_messages;
            existing.conversations += src.conversations;

            // Merge model counts
            for (model, count) in &src.models {
                *existing.models.entry(model.clone()).or_default() += count;
            }

            // Merge stats
            existing.stats.input_tokens += src.stats.input_tokens;
            existing.stats.output_tokens += src.stats.output_tokens;
            existing.stats.reasoning_tokens += src.stats.reasoning_tokens;
            existing.stats.cache_creation_tokens += src.stats.cache_creation_tokens;
            existing.stats.cache_read_tokens += src.stats.cache_read_tokens;
            existing.stats.cached_tokens += src.stats.cached_tokens;
            existing.stats.cost += src.stats.cost;
            existing.stats.tool_calls += src.stats.tool_calls;
            existing.stats.terminal_commands += src.stats.terminal_commands;
            existing.stats.file_searches += src.stats.file_searches;
            existing.stats.file_content_searches += src.stats.file_content_searches;
            existing.stats.files_read += src.stats.files_read;
            existing.stats.files_added += src.stats.files_added;
            existing.stats.files_edited += src.stats.files_edited;
            existing.stats.files_deleted += src.stats.files_deleted;
            existing.stats.lines_read += src.stats.lines_read;
            existing.stats.lines_added += src.stats.lines_added;
            existing.stats.lines_edited += src.stats.lines_edited;
            existing.stats.lines_deleted += src.stats.lines_deleted;
            existing.stats.bytes_read += src.stats.bytes_read;
            existing.stats.bytes_added += src.stats.bytes_added;
            existing.stats.bytes_edited += src.stats.bytes_edited;
            existing.stats.bytes_deleted += src.stats.bytes_deleted;
            existing.stats.todos_created += src.stats.todos_created;
            existing.stats.todos_completed += src.stats.todos_completed;
            existing.stats.todos_in_progress += src.stats.todos_in_progress;
            existing.stats.todo_writes += src.stats.todo_writes;
            existing.stats.todo_reads += src.stats.todo_reads;
            existing.stats.code_lines += src.stats.code_lines;
            existing.stats.docs_lines += src.stats.docs_lines;
            existing.stats.data_lines += src.stats.data_lines;
            existing.stats.media_lines += src.stats.media_lines;
            existing.stats.config_lines += src.stats.config_lines;
            existing.stats.other_lines += src.stats.other_lines;
        })
        .or_insert_with(|| src.clone());
}

/// VSCode GUI forks that might have extensions installed
const VSCODE_GUI_FORKS: &[&str] = &[
    "Code",
    "Code - Insiders",
    "Cursor",
    "Windsurf",
    "VSCodium",
    "Positron",
    "Antigravity",
];

/// VSCode CLI/server forks (remote development)
const VSCODE_CLI_FORKS: &[&str] = &["vscode-server", "vscode-server-insiders"];

/// Discover data sources for VSCode extension-based analyzers using jwalk.
///
/// This handles the complexity of multiple VSCode forks across different OSes:
/// - Linux GUI: `~/.config/{fork}/User/globalStorage/{extension_id}/tasks/*/`
/// - Linux CLI: `~/.{fork}/data/User/globalStorage/{extension_id}/tasks/*/`
/// - macOS: `~/Library/Application Support/{fork}/User/globalStorage/{extension_id}/tasks/*/`
/// - Windows: `%APPDATA%\{fork}\User\globalStorage\{extension_id}\tasks\*\`
///
/// # Arguments
/// * `extension_id` - The VSCode extension ID (e.g., "saoudrizwan.claude-dev")
/// * `target_filename` - The filename to search for (e.g., "ui_messages.json")
/// * `return_parent_dir` - If true, returns the parent directory instead of the file path
pub fn discover_vscode_extension_sources(
    extension_id: &str,
    target_filename: &str,
    return_parent_dir: bool,
) -> Result<Vec<DataSource>> {
    let mut sources = Vec::new();

    if let Some(home_dir) = dirs::home_dir() {
        // Collect all potential tasks directories
        let mut tasks_dirs = Vec::new();

        // Linux GUI forks: ~/.config/{fork}/User/globalStorage/{ext}/tasks
        for fork in VSCODE_GUI_FORKS {
            let tasks_dir = home_dir
                .join(".config")
                .join(fork)
                .join("User/globalStorage")
                .join(extension_id)
                .join("tasks");
            if tasks_dir.is_dir() {
                tasks_dirs.push(tasks_dir);
            }
        }

        // Linux CLI forks: ~/.{fork}/data/User/globalStorage/{ext}/tasks
        for fork in VSCODE_CLI_FORKS {
            let tasks_dir = home_dir
                .join(format!(".{fork}"))
                .join("data/User/globalStorage")
                .join(extension_id)
                .join("tasks");
            if tasks_dir.is_dir() {
                tasks_dirs.push(tasks_dir);
            }
        }

        // macOS GUI forks: ~/Library/Application Support/{fork}/User/globalStorage/{ext}/tasks
        for fork in VSCODE_GUI_FORKS {
            let tasks_dir = home_dir
                .join("Library/Application Support")
                .join(fork)
                .join("User/globalStorage")
                .join(extension_id)
                .join("tasks");
            if tasks_dir.is_dir() {
                tasks_dirs.push(tasks_dir);
            }
        }

        // Walk each tasks directory with jwalk (parallel)
        for tasks_dir in tasks_dirs {
            // Pattern: {task_id}/{target_filename}
            for entry in WalkDir::new(&tasks_dir)
                .min_depth(2)
                .max_depth(2)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().is_file()
                        && e.path()
                            .file_name()
                            .is_some_and(|name| name == target_filename)
                })
            {
                let path = if return_parent_dir {
                    entry.path().parent().map(|p| p.to_path_buf())
                } else {
                    Some(entry.path())
                };

                if let Some(p) = path {
                    sources.push(DataSource { path: p });
                }
            }
        }
    }

    // Windows GUI forks: %APPDATA%\{fork}\User\globalStorage\{ext}\tasks
    if let Ok(appdata) = std::env::var("APPDATA") {
        let appdata_path = PathBuf::from(appdata);
        for fork in VSCODE_GUI_FORKS {
            let tasks_dir = appdata_path
                .join(fork)
                .join("User\\globalStorage")
                .join(extension_id)
                .join("tasks");
            if tasks_dir.is_dir() {
                for entry in WalkDir::new(&tasks_dir)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_type().is_file()
                            && e.path()
                                .file_name()
                                .is_some_and(|name| name == target_filename)
                    })
                {
                    let path = if return_parent_dir {
                        entry.path().parent().map(|p| p.to_path_buf())
                    } else {
                        Some(entry.path())
                    };

                    if let Some(p) = path {
                        sources.push(DataSource { path: p });
                    }
                }
            }
        }
    }

    Ok(sources)
}

/// Represents a data source for an analyzer
#[derive(Debug, Clone)]
pub struct DataSource {
    pub path: PathBuf,
}

/// Main trait that all analyzers must implement
#[async_trait]
pub trait Analyzer: Send + Sync {
    /// Get the display name for this analyzer
    fn display_name(&self) -> &'static str;

    /// Get glob patterns for discovering data sources
    fn get_data_glob_patterns(&self) -> Vec<String>;

    /// Discover data sources for this analyzer
    fn discover_data_sources(&self) -> Result<Vec<DataSource>>;

    /// Parse conversations from data sources into normalized messages
    async fn parse_conversations(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<Vec<ConversationMessage>>;

    /// Get complete statistics for this analyzer
    async fn get_stats(&self) -> Result<AgenticCodingToolStats>;

    /// Check if this analyzer is available on the current system
    fn is_available(&self) -> bool;

    /// Parse a single file and return a cache entry.
    /// Default implementation returns an error - analyzers should override this
    /// to enable incremental caching.
    fn parse_single_file(&self, _source: &DataSource) -> Result<FileCacheEntry> {
        Err(anyhow::anyhow!(
            "parse_single_file not implemented for {}",
            self.display_name()
        ))
    }

    /// Whether this analyzer supports incremental caching.
    /// Default is false - analyzers should override this to return true
    /// after implementing parse_single_file.
    fn supports_caching(&self) -> bool {
        false
    }

    /// Whether this analyzer supports delta (append-only) parsing.
    /// Default is false - JSONL-based analyzers should return true.
    fn supports_delta_parsing(&self) -> bool {
        false
    }

    /// Parse a single file incrementally, using cached data if available.
    /// Default implementation falls back to full parse.
    fn parse_single_file_incremental(
        &self,
        source: &DataSource,
        _cached: Option<&FileCacheEntry>,
    ) -> Result<FileCacheEntry> {
        // Default: ignore cache, do full parse
        self.parse_single_file(source)
    }
}

/// Registry for managing multiple analyzers
pub struct AnalyzerRegistry {
    analyzers: Vec<Box<dyn Analyzer>>,
    /// Cached data sources per analyzer (display_name -> sources)
    data_source_cache: DashMap<String, Vec<DataSource>>,
    /// Per-file stats cache for incremental updates
    file_stats_cache: FileStatsCache,
}

impl Default for AnalyzerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalyzerRegistry {
    /// Create a new analyzer registry, loading any persisted cache from disk
    pub fn new() -> Self {
        // Try loading from disk, fall back to empty cache
        let file_stats_cache =
            FileStatsCache::load_from_disk().unwrap_or_else(|_| FileStatsCache::new());

        Self {
            analyzers: Vec::new(),
            data_source_cache: DashMap::new(),
            file_stats_cache,
        }
    }

    /// Get a reference to the file stats cache
    pub fn file_cache(&self) -> &FileStatsCache {
        &self.file_stats_cache
    }

    /// Persist the cache to disk (call on shutdown)
    pub fn persist_cache(&self) -> Result<()> {
        self.file_stats_cache.save_to_disk()
    }

    /// Register an analyzer
    pub fn register<A: Analyzer + 'static>(&mut self, analyzer: A) {
        self.analyzers.push(Box::new(analyzer));
    }

    /// Get or discover data sources for an analyzer (cached)
    pub fn get_cached_data_sources(&self, analyzer: &dyn Analyzer) -> Result<Vec<DataSource>> {
        let name = analyzer.display_name().to_string();

        // Check cache first
        if let Some(cached) = self.data_source_cache.get(&name) {
            return Ok(cached.clone());
        }

        // Discover and cache
        let sources = analyzer.discover_data_sources()?;
        self.data_source_cache.insert(name, sources.clone());
        Ok(sources)
    }

    /// Invalidate cache for a specific analyzer
    pub fn invalidate_cache(&self, analyzer_name: &str) {
        self.data_source_cache.remove(analyzer_name);
    }

    /// Invalidate all caches
    pub fn invalidate_all_caches(&self) {
        self.data_source_cache.clear();
    }

    /// Get available analyzers (those that are present on the system)
    /// Uses cached data sources to check availability, avoiding redundant glob scans
    pub fn available_analyzers(&self) -> Vec<&dyn Analyzer> {
        self.analyzers
            .iter()
            .filter(|a| {
                self.get_cached_data_sources(a.as_ref())
                    .is_ok_and(|sources| !sources.is_empty())
            })
            .map(|a| a.as_ref())
            .collect()
    }

    /// Get analyzer by display name
    pub fn get_analyzer_by_display_name(&self, display_name: &str) -> Option<&dyn Analyzer> {
        self.analyzers
            .iter()
            .find(|a| a.display_name() == display_name)
            .map(|a| a.as_ref())
    }

    /// Load stats using incremental caching with deduplication.
    ///
    /// Strategy:
    /// - WARM START: If files haven't changed, load cached snapshot instantly
    /// - INCREMENTAL: Load cached messages for unchanged files, parse only changed files,
    ///   then deduplicate and rebuild stats
    ///
    /// This gives both speed AND accuracy:
    /// - Unchanged files: load pre-parsed messages from per-file cache (fast)
    /// - Changed files: parse only those files (minimal work)
    /// - Always: run deduplication on combined messages (accurate)
    pub async fn load_stats_with_cache(
        &self,
        analyzer: &dyn Analyzer,
    ) -> Result<AgenticCodingToolStats> {
        use crate::analyzers::deduplicate_messages;
        use crate::cache::{compute_sources_fingerprint, load_snapshot_full, save_snapshot};
        use rayon::prelude::*;

        // If analyzer doesn't support caching, fall back to the regular method
        if !analyzer.supports_caching() {
            return analyzer.get_stats().await;
        }

        let analyzer_name = analyzer.display_name();
        let sources = self.get_cached_data_sources(analyzer)?;

        // Compute fingerprint of all source files
        let fingerprint = compute_sources_fingerprint(&sources);

        // Try to load cached snapshot (WARM START) - includes messages from cold snapshot
        if let Some(stats) = load_snapshot_full(analyzer_name, fingerprint) {
            return Ok(stats);
        }

        // INCREMENTAL COLD START: Load cached messages + parse only changed files
        let mut all_messages = Vec::new();
        let mut files_to_parse: Vec<(DataSource, Option<FileCacheEntry>)> = Vec::new();
        let supports_delta = analyzer.supports_delta_parsing();

        // Check each file against per-file cache (in parallel for speed)
        let cache_results: Vec<_> = sources
            .par_iter()
            .map(|source| {
                let cache_key = FileCacheKey::new(analyzer_name, &source.path);

                // Get cached entry (if any)
                let cached_entry = self.file_stats_cache.get_unchecked(&cache_key);

                if let Ok(current_meta) = crate::types::FileMetadata::from_path(&source.path)
                    && let Some(ref cached) = cached_entry
                {
                    // Check if file is unchanged
                    if cached.metadata.is_unchanged(&current_meta) {
                        // Exact match - use cached messages directly
                        if let Ok(messages) = self.file_stats_cache.load_messages(&cache_key) {
                            return (Some(messages), None);
                        }
                    }

                    // For delta-capable analyzers, check if we can do incremental parsing
                    if supports_delta {
                        if cached.metadata.is_append_only(&current_meta) {
                            // Append detected - pass cached entry for delta parsing
                            return (None, Some((source.clone(), cached_entry)));
                        }
                        // Truncation or other change - full reparse
                        return (None, Some((source.clone(), None)));
                    }
                }
                // Cache miss or non-delta analyzer - need to parse this file
                (None, Some((source.clone(), cached_entry)))
            })
            .collect();

        // Collect results
        for (cached_msgs, to_parse) in cache_results {
            if let Some(msgs) = cached_msgs {
                all_messages.extend(msgs);
            }
            if let Some((source, cached)) = to_parse {
                files_to_parse.push((source, cached));
            }
        }

        // Parse only the changed files in parallel
        if !files_to_parse.is_empty() {
            let new_entries: Vec<_> = files_to_parse
                .par_iter()
                .filter_map(|(source, cached)| {
                    // Use incremental parsing if analyzer supports it
                    let entry = if supports_delta {
                        analyzer
                            .parse_single_file_incremental(source, cached.as_ref())
                            .ok()
                    } else {
                        analyzer.parse_single_file(source).ok()
                    };
                    entry.map(|e| (source.path.clone(), e))
                })
                .collect();

            // Add messages from newly parsed files and update per-file cache
            for (path, entry) in new_entries {
                all_messages.extend(entry.messages.clone());
                let cache_key = FileCacheKey::new(analyzer_name, &path);
                self.file_stats_cache.insert(cache_key, entry);
            }

            // Persist per-file cache to disk
            let _ = self.file_stats_cache.save_to_disk();
        }

        // Deduplicate all messages
        let deduped_messages = deduplicate_messages(all_messages);

        // Build stats from deduplicated messages
        let mut daily_stats = crate::utils::aggregate_by_date(&deduped_messages);
        daily_stats.retain(|date, _| date != "unknown");

        let num_conversations = daily_stats
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        let stats = AgenticCodingToolStats {
            daily_stats,
            num_conversations,
            messages: deduped_messages,
            analyzer_name: analyzer_name.to_string(),
        };

        // Save snapshot for next time
        if let Err(e) = save_snapshot(analyzer_name, fingerprint, &stats) {
            eprintln!("Warning: Failed to save snapshot cache: {e}");
        }

        Ok(stats)
    }

    // Keep the old per-file cache method for reference (unused but may be useful later)
    #[allow(dead_code)]
    async fn load_stats_with_perfile_cache(
        &self,
        analyzer: &dyn Analyzer,
    ) -> Result<AgenticCodingToolStats> {
        use crate::types::DailyStats;
        use std::collections::BTreeMap;

        if !analyzer.supports_caching() {
            return analyzer.get_stats().await;
        }

        let sources = self.get_cached_data_sources(analyzer)?;
        let source_paths: Vec<_> = sources.iter().map(|s| s.path.clone()).collect();

        self.file_stats_cache
            .prune_deleted_files(analyzer.display_name(), &source_paths);

        let analyzer_name = analyzer.display_name();
        let mut aggregated_daily: BTreeMap<String, DailyStats> = BTreeMap::new();
        let mut all_messages = Vec::new();
        let mut needs_parsing = Vec::new();

        for source in &sources {
            let cache_key = FileCacheKey::new(analyzer_name, &source.path);

            if let Ok(current_meta) = crate::types::FileMetadata::from_path(&source.path)
                && !self.file_stats_cache.is_stale(&cache_key, &current_meta)
            {
                if let Some(contributions) =
                    self.file_stats_cache.get_daily_contributions(&cache_key)
                {
                    for (date, stats) in contributions {
                        merge_daily_stats(&mut aggregated_daily, &date, &stats);
                    }
                }
                continue;
            }

            needs_parsing.push(source.clone());
        }

        if !needs_parsing.is_empty() {
            let new_entries: Vec<_> = needs_parsing
                .par_iter()
                .filter_map(|source| {
                    analyzer
                        .parse_single_file(source)
                        .ok()
                        .map(|entry| (source.path.clone(), entry))
                })
                .collect();

            for (path, entry) in new_entries {
                for (date, stats) in &entry.daily_contributions {
                    merge_daily_stats(&mut aggregated_daily, date, stats);
                }
                all_messages.extend(entry.messages.clone());
                let cache_key = FileCacheKey::new(analyzer_name, &path);
                self.file_stats_cache.insert(cache_key, entry);
            }
        }

        aggregated_daily.retain(|date, _| date != "unknown");

        let num_conversations = aggregated_daily
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        Ok(AgenticCodingToolStats {
            daily_stats: aggregated_daily,
            num_conversations,
            messages: all_messages,
            analyzer_name: analyzer_name.to_string(),
        })
    }

    /// Load stats from all available analyzers, using cache where supported
    /// Loads all analyzers in PARALLEL for faster startup
    pub async fn load_all_stats(&self) -> Result<crate::types::MultiAnalyzerStats> {
        use futures::future::join_all;

        let available_analyzers = self.available_analyzers();

        // Create futures for all analyzers - they'll run concurrently
        let futures: Vec<_> = available_analyzers
            .into_iter()
            .map(|analyzer| async move {
                let name = analyzer.display_name().to_string();
                let result = if analyzer.supports_caching() {
                    self.load_stats_with_cache(analyzer).await
                } else {
                    analyzer.get_stats().await
                };
                (name, result)
            })
            .collect();

        // Run all analyzers in parallel
        let results = join_all(futures).await;

        let mut all_stats = Vec::new();
        for (name, result) in results {
            match result {
                Ok(stats) => all_stats.push(stats),
                Err(e) => {
                    eprintln!("⚠️  Error analyzing {} data: {}", name, e);
                }
            }
        }

        Ok(crate::types::MultiAnalyzerStats {
            analyzer_stats: all_stats,
        })
    }

    /// Get a mapping of data directories to analyzer names for file watching
    /// Uses cached data sources to avoid redundant glob scans
    pub fn get_directory_to_analyzer_mapping(&self) -> std::collections::HashMap<PathBuf, String> {
        let mut dir_to_analyzer = std::collections::HashMap::new();

        for analyzer in self.available_analyzers() {
            // Use cached sources instead of calling discover_data_sources() again
            if let Ok(sources) = self.get_cached_data_sources(analyzer) {
                for source in sources {
                    if let Some(parent) = source.path.parent()
                        && parent.exists()
                    {
                        dir_to_analyzer
                            .insert(parent.to_path_buf(), analyzer.display_name().to_string());
                    }
                }
            }
        }

        dir_to_analyzer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AgenticCodingToolStats, Application, ConversationMessage, MessageRole, Stats,
    };
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    struct TestAnalyzer {
        name: &'static str,
        available: bool,
        stats: Option<AgenticCodingToolStats>,
        sources: Vec<PathBuf>,
        fail_stats: bool,
    }

    #[async_trait]
    impl Analyzer for TestAnalyzer {
        fn display_name(&self) -> &'static str {
            self.name
        }

        fn get_data_glob_patterns(&self) -> Vec<String> {
            vec!["*.json".to_string()]
        }

        fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
            Ok(self
                .sources
                .iter()
                .cloned()
                .map(|path| DataSource { path })
                .collect())
        }

        async fn parse_conversations(
            &self,
            _sources: Vec<DataSource>,
        ) -> Result<Vec<ConversationMessage>> {
            Ok(Vec::new())
        }

        async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
            if self.fail_stats {
                anyhow::bail!("stats failed");
            }
            self.stats
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no stats"))
        }

        fn is_available(&self) -> bool {
            self.available
        }
    }

    fn sample_stats(name: &str) -> AgenticCodingToolStats {
        let date = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let msg = ConversationMessage {
            application: Application::ClaudeCode,
            date,
            project_hash: "proj".into(),
            conversation_hash: "conv".into(),
            local_hash: None,
            global_hash: "global".into(),
            model: Some("model".into()),
            stats: Stats {
                input_tokens: 1,
                ..Stats::default()
            },
            role: MessageRole::Assistant,
            uuid: None,
            session_name: Some("session".into()),
        };

        AgenticCodingToolStats {
            daily_stats: BTreeMap::new(),
            num_conversations: 1,
            messages: vec![msg],
            analyzer_name: name.to_string(),
        }
    }

    #[tokio::test]
    async fn registry_filters_available_analyzers_and_loads_stats() {
        let mut registry = AnalyzerRegistry::new();

        // Analyzers with non-empty sources are considered "available"
        // (availability is determined by having data sources, not by is_available())
        let analyzer_ok = TestAnalyzer {
            name: "ok",
            available: true,
            stats: Some(sample_stats("ok")),
            sources: vec![PathBuf::from("/fake/path.jsonl")],
            fail_stats: false,
        };

        // Analyzer with empty sources is "unavailable"
        let analyzer_unavailable = TestAnalyzer {
            name: "unavailable",
            available: false,
            stats: Some(sample_stats("unavailable")),
            sources: Vec::new(),
            fail_stats: false,
        };

        let analyzer_fails = TestAnalyzer {
            name: "fails",
            available: true,
            stats: None,
            sources: vec![PathBuf::from("/fake/path2.jsonl")],
            fail_stats: true,
        };

        registry.register(analyzer_ok);
        registry.register(analyzer_unavailable);
        registry.register(analyzer_fails);

        let avail = registry.available_analyzers();
        assert_eq!(avail.len(), 2); // "ok" and "fails" (have non-empty sources)

        let by_name = registry
            .get_analyzer_by_display_name("ok")
            .expect("analyzer 'ok'");
        assert_eq!(by_name.display_name(), "ok");

        let stats = registry.load_all_stats().await.expect("load stats");
        // Only the successful analyzer should contribute stats.
        assert_eq!(stats.analyzer_stats.len(), 1);
        assert_eq!(stats.analyzer_stats[0].analyzer_name, "ok");
    }

    #[tokio::test]
    async fn registry_builds_directory_mapping() {
        use std::fs;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let base = temp_dir.path().join("proj").join("chats");
        fs::create_dir_all(&base).expect("mkdirs");
        let file_path = base.join("session.json");

        let mut registry = AnalyzerRegistry::new();
        let analyzer = TestAnalyzer {
            name: "mapper",
            available: true,
            stats: Some(sample_stats("mapper")),
            sources: vec![file_path.clone()],
            fail_stats: false,
        };

        registry.register(analyzer);

        let mapping = registry.get_directory_to_analyzer_mapping();
        // Parent directory of the source should be mapped to "mapper".
        assert_eq!(mapping.get(&base).map(String::as_str), Some("mapper"));
    }

    // =========================================================================
    // MERGE_DAILY_STATS TESTS
    // =========================================================================

    fn make_test_daily_stats(
        date: &str,
        ai_msgs: u32,
        input_tokens: u64,
    ) -> crate::types::DailyStats {
        let mut models = BTreeMap::new();
        models.insert("model-a".to_string(), 2);

        crate::types::DailyStats {
            date: date.to_string(),
            user_messages: 5,
            ai_messages: ai_msgs,
            conversations: 1,
            models,
            stats: Stats {
                input_tokens,
                output_tokens: 50,
                cost: 0.01,
                tool_calls: 3,
                ..Stats::default()
            },
        }
    }

    #[test]
    fn test_merge_daily_stats_into_empty() {
        let mut dst: BTreeMap<String, crate::types::DailyStats> = BTreeMap::new();
        let src = make_test_daily_stats("2025-01-15", 10, 100);

        merge_daily_stats(&mut dst, "2025-01-15", &src);

        assert_eq!(dst.len(), 1);
        let merged = dst.get("2025-01-15").expect("should have entry");
        assert_eq!(merged.ai_messages, 10);
        assert_eq!(merged.stats.input_tokens, 100);
    }

    #[test]
    fn test_merge_daily_stats_combines_correctly() {
        let mut dst: BTreeMap<String, crate::types::DailyStats> = BTreeMap::new();

        // Insert first entry
        let src1 = make_test_daily_stats("2025-01-15", 10, 100);
        merge_daily_stats(&mut dst, "2025-01-15", &src1);

        // Merge second entry with same date
        let mut src2 = make_test_daily_stats("2025-01-15", 5, 200);
        src2.models.insert("model-b".to_string(), 3);
        merge_daily_stats(&mut dst, "2025-01-15", &src2);

        assert_eq!(dst.len(), 1);
        let merged = dst.get("2025-01-15").expect("should have entry");

        // Values should be summed
        assert_eq!(merged.ai_messages, 15); // 10 + 5
        assert_eq!(merged.user_messages, 10); // 5 + 5
        assert_eq!(merged.conversations, 2); // 1 + 1
        assert_eq!(merged.stats.input_tokens, 300); // 100 + 200
        assert_eq!(merged.stats.output_tokens, 100); // 50 + 50
        assert_eq!(merged.stats.tool_calls, 6); // 3 + 3

        // Models should be merged
        assert_eq!(merged.models.get("model-a"), Some(&4)); // 2 + 2
        assert_eq!(merged.models.get("model-b"), Some(&3)); // only in src2
    }

    // =========================================================================
    // DISCOVER_VSCODE_EXTENSION_SOURCES TESTS
    // =========================================================================

    #[test]
    fn test_discover_vscode_extension_sources_no_panic() {
        // Should handle non-existent extension gracefully
        let result = discover_vscode_extension_sources(
            "nonexistent.extension.id",
            "ui_messages.json",
            false,
        );

        // Should return Ok with empty vec, not panic
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_discover_vscode_extension_sources_return_parent_option() {
        // Both options should work without panic
        let result1 = discover_vscode_extension_sources(
            "nonexistent.ext",
            "file.json",
            false, // return file path
        );
        let result2 = discover_vscode_extension_sources(
            "nonexistent.ext",
            "file.json",
            true, // return parent dir
        );

        assert!(result1.is_ok());
        assert!(result2.is_ok());
    }
}
