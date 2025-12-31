use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use futures::future::join_all;
use jwalk::WalkDir;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::types::{
    AgenticCodingToolStats, AnalyzerStatsView, ConversationMessage, FileContribution,
};
use crate::utils::hash_text;

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

/// Get all tasks directories for a VSCode extension across all forks and platforms.
///
/// This is the single source of truth for VSCode extension data locations:
/// - Linux GUI: `~/.config/{fork}/User/globalStorage/{extension_id}/tasks/`
/// - Linux CLI: `~/.{fork}/data/User/globalStorage/{extension_id}/tasks/`
/// - macOS: `~/Library/Application Support/{fork}/User/globalStorage/{extension_id}/tasks/`
/// - Windows: `%APPDATA%\{fork}\User\globalStorage\{extension_id}\tasks\`
pub fn get_vscode_extension_tasks_dirs(extension_id: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home_dir) = dirs::home_dir() {
        // Linux GUI forks: ~/.config/{fork}/User/globalStorage/{ext}/tasks
        for fork in VSCODE_GUI_FORKS {
            let tasks_dir = home_dir
                .join(".config")
                .join(fork)
                .join("User/globalStorage")
                .join(extension_id)
                .join("tasks");
            if tasks_dir.is_dir() {
                dirs.push(tasks_dir);
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
                dirs.push(tasks_dir);
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
                dirs.push(tasks_dir);
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
                dirs.push(tasks_dir);
            }
        }
    }

    dirs
}

/// Discover data sources for VSCode extension-based analyzers using jwalk.
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

    for tasks_dir in get_vscode_extension_tasks_dirs(extension_id) {
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

    /// Get directories to watch for file changes.
    ///
    /// Returns the root data directories for this analyzer. The file watcher will
    /// recursively watch these directories for new, modified, or deleted files.
    ///
    /// This is important for analyzers with nested directory structures (e.g.,
    /// `sessions/{id}/file.json`) where new subdirectories need to be detected.
    /// Without this, only existing subdirectories would be watched, missing new
    /// sessions/projects/tasks.
    ///
    /// Default implementation returns empty vec, which falls back to watching
    /// parent directories of discovered data sources (legacy behavior).
    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Get lightweight view for TUI (default: compute full stats, convert to view).
    /// Individual analyzers can override for efficiency if they can avoid loading messages.
    async fn get_stats_view(&self) -> Result<crate::types::AnalyzerStatsView> {
        self.get_stats().await.map(|s| s.into_view())
    }
}

/// Registry for managing multiple analyzers
pub struct AnalyzerRegistry {
    analyzers: Vec<Box<dyn Analyzer>>,
    /// Cached data sources per analyzer (display_name -> sources)
    data_source_cache: DashMap<String, Vec<DataSource>>,
    /// Per-file contribution cache for true incremental updates.
    /// Key: file path, Value: pre-computed aggregate contribution from that file.
    /// Much smaller than storing raw messages (~1KB vs ~100KB per file).
    file_contribution_cache: DashMap<PathBuf, FileContribution>,
    /// Cached analyzer views for incremental updates.
    /// Key: analyzer display name, Value: current aggregated view.
    analyzer_views_cache: DashMap<String, AnalyzerStatsView>,
}

impl Default for AnalyzerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalyzerRegistry {
    /// Create a new analyzer registry
    pub fn new() -> Self {
        Self {
            analyzers: Vec::new(),
            data_source_cache: DashMap::new(),
            file_contribution_cache: DashMap::new(),
            analyzer_views_cache: DashMap::new(),
        }
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
        self.file_contribution_cache.clear();
        self.analyzer_views_cache.clear();
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

    /// Load stats from all available analyzers in parallel.
    /// Used for uploads - returns full stats with messages.
    pub async fn load_all_stats(&self) -> Result<crate::types::MultiAnalyzerStats> {
        let available_analyzers = self.available_analyzers();

        // Create futures for all analyzers - they'll run concurrently
        let futures: Vec<_> = available_analyzers
            .into_iter()
            .map(|analyzer| async move { analyzer.get_stats().await })
            .collect();

        // Run all analyzers in parallel
        let results = join_all(futures).await;

        let mut all_stats = Vec::new();
        for result in results {
            match result {
                Ok(stats) => {
                    all_stats.push(stats);
                }
                Err(e) => {
                    eprintln!("⚠️  Error analyzing data: {}", e);
                }
            }
        }

        Ok(crate::types::MultiAnalyzerStats {
            analyzer_stats: all_stats,
        })
    }

    /// Load view-only stats using a temporary thread pool. Ran once at startup.
    /// The pool is dropped after loading, releasing all thread-local memory.
    /// Populates file contribution cache for true incremental updates.
    pub fn load_all_stats_views_parallel(
        &self,
        num_threads: usize,
    ) -> Result<crate::types::MultiAnalyzerStatsView> {
        // Create the temporary pool
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to create thread pool: {}", e))?;

        // Collect analyzer info
        let available_analyzers = self.available_analyzers();
        let analyzer_data: Vec<_> = available_analyzers
            .iter()
            .map(|a| {
                let name = a.display_name().to_string();
                let sources = self.get_cached_data_sources(*a).unwrap_or_default();
                (name, sources)
            })
            .collect();

        // Run all analyzer parsing inside the temp pool
        // All into_par_iter() calls will use this pool
        let all_stats: Vec<Result<AgenticCodingToolStats>> = pool.install(|| {
            // Create a runtime for async operations inside the pool
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create runtime");

            available_analyzers
                .into_iter()
                .map(|analyzer| rt.block_on(analyzer.get_stats()))
                .collect()
        });

        // Pool is dropped here, releasing all thread memory
        drop(pool);

        // Build views from results
        let mut all_views = Vec::new();
        for ((name, sources), result) in analyzer_data.into_iter().zip(all_stats.into_iter()) {
            match result {
                Ok(stats) => {
                    // Populate file contribution cache for incremental updates
                    self.populate_file_contribution_cache(&name, &sources, &stats.messages);
                    // Convert to view (drops messages)
                    let view = stats.into_view();
                    // Cache the view for incremental updates
                    self.analyzer_views_cache.insert(name, view.clone());
                    all_views.push(view);
                }
                Err(e) => {
                    eprintln!("⚠️  Error analyzing {} data: {}", name, e);
                }
            }
        }

        Ok(crate::types::MultiAnalyzerStatsView {
            analyzer_stats: all_views,
        })
    }

    /// Populate the file contribution cache from parsed messages.
    /// Groups messages by their source file, computes per-file aggregates.
    fn populate_file_contribution_cache(
        &self,
        analyzer_name: &str,
        sources: &[DataSource],
        messages: &[ConversationMessage],
    ) {
        // Create a map of conversation_hash -> file_path
        let hash_to_path: HashMap<String, PathBuf> = sources
            .iter()
            .map(|s| (hash_text(&s.path.to_string_lossy()), s.path.clone()))
            .collect();

        // Group messages by their source file
        let mut file_messages: HashMap<PathBuf, Vec<ConversationMessage>> = HashMap::new();
        for msg in messages {
            if let Some(path) = hash_to_path.get(&msg.conversation_hash) {
                file_messages
                    .entry(path.clone())
                    .or_default()
                    .push(msg.clone());
            }
        }

        // Compute and cache contribution for each file
        for (path, msgs) in file_messages {
            let contribution = FileContribution::from_messages(&msgs, analyzer_name);
            self.file_contribution_cache.insert(path, contribution);
        }
    }

    /// Reload stats for a single file change using true incremental update.
    /// O(1) update - only reparses the changed file, subtracts old contribution,
    /// adds new contribution. No full reload needed.
    pub async fn reload_file_incremental(
        &self,
        analyzer_name: &str,
        changed_path: &std::path::Path,
    ) -> Result<AnalyzerStatsView> {
        let analyzer = self
            .get_analyzer_by_display_name(analyzer_name)
            .ok_or_else(|| anyhow::anyhow!("Analyzer not found: {}", analyzer_name))?;

        // Get the old contribution (if any)
        let old_contribution = self
            .file_contribution_cache
            .get(changed_path)
            .map(|r| r.clone());

        // Parse just the changed file
        let source = DataSource {
            path: changed_path.to_path_buf(),
        };
        let new_messages = analyzer.parse_conversations(vec![source]).await?;

        // Compute new contribution
        let new_contribution = FileContribution::from_messages(&new_messages, analyzer_name);

        // Update the contribution cache
        self.file_contribution_cache
            .insert(changed_path.to_path_buf(), new_contribution.clone());

        // Get or create the cached view for this analyzer
        let mut view = self
            .analyzer_views_cache
            .get(analyzer_name)
            .map(|r| r.clone())
            .unwrap_or_else(|| AnalyzerStatsView {
                daily_stats: BTreeMap::new(),
                session_aggregates: Vec::new(),
                num_conversations: 0,
                analyzer_name: analyzer_name.to_string(),
            });

        // Subtract old contribution (if any)
        if let Some(old) = old_contribution {
            view.subtract_contribution(&old);
        }

        // Add new contribution
        view.add_contribution(&new_contribution);

        // Update the view cache
        self.analyzer_views_cache
            .insert(analyzer_name.to_string(), view.clone());

        Ok(view)
    }

    /// Remove a file from the cache and update the view (for file deletion events).
    /// Returns the updated view.
    pub fn remove_file_from_cache(
        &self,
        analyzer_name: &str,
        path: &std::path::Path,
    ) -> Option<AnalyzerStatsView> {
        // Get the old contribution
        let old_contribution = self.file_contribution_cache.remove(path);

        if let Some((_, old)) = old_contribution {
            // Update the cached view
            if let Some(mut view) = self.analyzer_views_cache.get_mut(analyzer_name) {
                view.subtract_contribution(&old);
                return Some(view.clone());
            }
        }

        self.analyzer_views_cache
            .get(analyzer_name)
            .map(|r| r.clone())
    }

    /// Check if the contribution cache is populated for an analyzer.
    pub fn has_cached_contributions(&self, analyzer_name: &str) -> bool {
        self.analyzer_views_cache.contains_key(analyzer_name)
    }

    /// Get the cached view for an analyzer.
    pub fn get_cached_view(&self, analyzer_name: &str) -> Option<AnalyzerStatsView> {
        self.analyzer_views_cache
            .get(analyzer_name)
            .map(|r| r.clone())
    }

    /// Get a mapping of data directories to analyzer names for file watching.
    ///
    /// Prefers explicit watch directories from `get_watch_directories()` when available,
    /// which allows detecting new subdirectories (sessions/projects/tasks).
    /// Falls back to parent directories of data sources for backward compatibility.
    pub fn get_directory_to_analyzer_mapping(&self) -> std::collections::HashMap<PathBuf, String> {
        let mut dir_to_analyzer = std::collections::HashMap::new();

        for analyzer in self.available_analyzers() {
            let watch_dirs = analyzer.get_watch_directories();

            if !watch_dirs.is_empty() {
                // Use explicit watch directories (preferred - catches new subdirectories)
                for dir in watch_dirs {
                    if dir.exists() {
                        dir_to_analyzer.insert(dir, analyzer.display_name().to_string());
                    }
                }
            } else {
                // Fallback: derive from data sources (legacy behavior)
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

    /// Test analyzer that overrides get_watch_directories() to return a custom root dir.
    /// This simulates analyzers with nested subdirectory structures.
    struct TestAnalyzerWithWatchDirs {
        name: &'static str,
        sources: Vec<PathBuf>,
        watch_dirs: Vec<PathBuf>,
    }

    #[async_trait]
    impl Analyzer for TestAnalyzerWithWatchDirs {
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
            Ok(AgenticCodingToolStats {
                daily_stats: BTreeMap::new(),
                num_conversations: 0,
                messages: Vec::new(),
                analyzer_name: self.name.to_string(),
            })
        }

        fn is_available(&self) -> bool {
            true
        }

        fn get_watch_directories(&self) -> Vec<PathBuf> {
            self.watch_dirs.clone()
        }
    }

    #[tokio::test]
    async fn registry_uses_explicit_watch_directories() {
        use std::fs;

        // Simulate nested structure like OpenCode: message/{session}/{file}.json
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let message_dir = temp_dir.path().join("message");
        let session1_dir = message_dir.join("session1");
        let session2_dir = message_dir.join("session2");
        fs::create_dir_all(&session1_dir).expect("mkdirs session1");
        fs::create_dir_all(&session2_dir).expect("mkdirs session2");

        let file1 = session1_dir.join("msg1.json");
        let file2 = session2_dir.join("msg2.json");

        let mut registry = AnalyzerRegistry::new();
        let analyzer = TestAnalyzerWithWatchDirs {
            name: "nested",
            sources: vec![file1.clone(), file2.clone()],
            watch_dirs: vec![message_dir.clone()],
        };

        registry.register(analyzer);

        let mapping = registry.get_directory_to_analyzer_mapping();

        // With explicit watch directories, only the root message_dir should be watched.
        // NOT the individual session directories.
        assert_eq!(
            mapping.get(&message_dir).map(String::as_str),
            Some("nested"),
            "Should watch the explicit root directory"
        );
        assert!(
            !mapping.contains_key(&session1_dir),
            "Should NOT watch individual session directories when watch_dirs is set"
        );
        assert!(
            !mapping.contains_key(&session2_dir),
            "Should NOT watch individual session directories when watch_dirs is set"
        );
    }

    #[tokio::test]
    async fn registry_falls_back_to_parent_dirs_when_no_watch_dirs() {
        use std::fs;

        // Without explicit watch_dirs, should fall back to parent directories
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let base = temp_dir.path().join("data").join("files");
        fs::create_dir_all(&base).expect("mkdirs");
        let file_path = base.join("data.json");

        let mut registry = AnalyzerRegistry::new();
        let analyzer = TestAnalyzerWithWatchDirs {
            name: "fallback",
            sources: vec![file_path.clone()],
            watch_dirs: vec![], // Empty = use legacy behavior
        };

        registry.register(analyzer);

        let mapping = registry.get_directory_to_analyzer_mapping();

        // Should fall back to parent directory of the source file
        assert_eq!(
            mapping.get(&base).map(String::as_str),
            Some("fallback"),
            "Should fall back to watching parent directory when watch_dirs is empty"
        );
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
