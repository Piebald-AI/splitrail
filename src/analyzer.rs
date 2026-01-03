use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

use crate::contribution_cache::{
    ContributionCache, ContributionStrategy, MultiSessionContribution, PathHash,
    RemovedContribution, SessionHash, SingleMessageContribution, SingleSessionContribution,
};
use crate::types::{
    AgenticCodingToolStats, AnalyzerStatsView, ConversationMessage, SharedAnalyzerView,
};

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

fn walk_vscode_extension_tasks(extension_id: &str) -> impl Iterator<Item = WalkDir> {
    get_vscode_extension_tasks_dirs(extension_id)
        .into_iter()
        .map(|tasks_dir| WalkDir::new(tasks_dir).min_depth(2).max_depth(2))
}

/// Check if any data sources exist for a VSCode extension-based analyzer.
/// Short-circuits after finding the first match.
pub fn vscode_extension_has_sources(extension_id: &str, target_filename: &str) -> bool {
    walk_vscode_extension_tasks(extension_id)
        .flat_map(|w| w.into_iter())
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_type().is_file()
                && e.path()
                    .file_name()
                    .is_some_and(|name| name == target_filename)
        })
}

/// Discover data sources for VSCode extension-based analyzers.
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
    let sources = walk_vscode_extension_tasks(extension_id)
        .flat_map(|w| w.into_iter())
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && e.path()
                    .file_name()
                    .is_some_and(|name| name == target_filename)
        })
        .filter_map(|entry| {
            if return_parent_dir {
                entry.path().parent().map(|p| p.to_path_buf())
            } else {
                Some(entry.into_path())
            }
        })
        .map(|path| DataSource { path })
        .collect();

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

    /// Discover data sources for this analyzer (returns all sources)
    fn discover_data_sources(&self) -> Result<Vec<DataSource>>;

    /// Parse a single data source into messages.
    /// This is the core parsing logic without parallelism decisions.
    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>>;

    /// Parse multiple data sources in parallel, returning messages grouped by source path.
    ///
    /// Default: parses all sources in parallel using rayon.
    /// Override for shared context loading (e.g., OpenCode loads session data once).
    /// Must be called within a rayon threadpool context for parallelism.
    ///
    /// Note: Messages are NOT deduplicated - caller should deduplicate if needed.
    fn parse_sources_parallel_with_paths(
        &self,
        sources: &[DataSource],
    ) -> Vec<(PathBuf, Vec<ConversationMessage>)> {
        sources
            .par_iter()
            .filter_map(|source| match self.parse_source(source) {
                Ok(msgs) => Some((source.path.clone(), msgs)),
                Err(e) => {
                    eprintln!(
                        "Failed to parse {} source {:?}: {}",
                        self.display_name(),
                        source.path,
                        e
                    );
                    None
                }
            })
            .collect()
    }

    /// Parse multiple data sources in parallel and deduplicate.
    ///
    /// Default: calls `parse_sources_parallel_with_paths` and deduplicates by `global_hash`.
    /// Override for different dedup strategy (e.g., Piebald uses local_hash).
    /// Must be called within a rayon threadpool context for parallelism.
    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        let all_messages: Vec<ConversationMessage> = self
            .parse_sources_parallel_with_paths(sources)
            .into_iter()
            .flat_map(|(_, msgs)| msgs)
            .collect();
        crate::utils::deduplicate_by_global_hash(all_messages)
    }

    /// Get directories to watch for file changes.
    /// Returns the root data directories for this analyzer.
    fn get_watch_directories(&self) -> Vec<PathBuf>;

    /// Check if a path is a valid data source for this analyzer.
    /// Used by file watcher to filter events before processing.
    /// Default: returns true for files, false for directories.
    fn is_valid_data_path(&self, path: &Path) -> bool {
        path.is_file()
    }

    /// Check if this analyzer is available (has any data).
    /// Default: checks if discover_data_sources returns at least one source.
    /// Analyzers can override with optimized versions that stop after finding 1 file.
    fn is_available(&self) -> bool {
        self.discover_data_sources()
            .is_ok_and(|sources| !sources.is_empty())
    }

    /// Returns the contribution caching strategy for this analyzer.
    /// - `SingleMessage`: 1 file = 1 message (~40 bytes/file) - e.g., OpenCode
    /// - `SingleSession`: 1 file = 1 session (~72 bytes/file) - e.g., Claude Code, Cline
    /// - `MultiSession`: 1 file = many sessions (~100+ bytes/file) - e.g., Piebald
    fn contribution_strategy(&self) -> ContributionStrategy;

    /// Get stats with pre-discovered sources (avoids double discovery).
    /// Default implementation parses sources in parallel via `parse_sources_parallel()`.
    /// Override for analyzers with complex cross-file logic (e.g., claude_code).
    fn get_stats_with_sources(&self, sources: Vec<DataSource>) -> Result<AgenticCodingToolStats> {
        let messages = self.parse_sources_parallel(&sources);

        let mut daily_stats = crate::utils::aggregate_by_date(&messages);
        daily_stats.retain(|date, _| date != "unknown");
        let num_conversations = daily_stats
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        Ok(AgenticCodingToolStats {
            daily_stats,
            num_conversations,
            messages,
            analyzer_name: self.display_name().to_string(),
        })
    }

    /// Get complete statistics for this analyzer.
    /// Default: discovers sources then calls get_stats_with_sources().
    fn get_stats(&self) -> Result<AgenticCodingToolStats> {
        let sources = self.discover_data_sources()?;
        self.get_stats_with_sources(sources)
    }
}

/// Registry for managing multiple analyzers
pub struct AnalyzerRegistry {
    analyzers: Vec<Box<dyn Analyzer>>,
    /// Unified contribution cache for incremental updates.
    /// Strategy-specific storage: SingleMessage (~40B), SingleSession (~72B), MultiSession (~100+B).
    contribution_cache: ContributionCache,
    /// Cached analyzer views for incremental updates.
    /// Key: analyzer display name, Value: shared view with RwLock for in-place mutation.
    analyzer_views_cache: DashMap<String, SharedAnalyzerView>,
    /// Tracks the order in which analyzers were registered to maintain stable tab ordering.
    /// Contains display names in registration order.
    analyzer_order: parking_lot::RwLock<Vec<String>>,
    /// Tracks files that have been modified since the last upload.
    /// Used for incremental uploads - only modified files are parsed for upload.
    /// Wrapped in Arc so cloning gives a shared handle for async tasks.
    dirty_files_for_upload: Arc<DashMap<PathBuf, String>>,
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
            contribution_cache: ContributionCache::new(),
            analyzer_views_cache: DashMap::new(),
            analyzer_order: parking_lot::RwLock::new(Vec::new()),
            dirty_files_for_upload: Arc::new(DashMap::new()),
        }
    }

    /// Register an analyzer
    pub fn register<A: Analyzer + 'static>(&mut self, analyzer: A) {
        let name = analyzer.display_name().to_string();
        self.analyzers.push(Box::new(analyzer));
        // Track registration order for stable tab ordering in TUI
        self.analyzer_order.write().push(name);
    }

    /// Invalidate all caches (file contributions and analyzer views)
    pub fn invalidate_all_caches(&self) {
        self.contribution_cache.clear();
        self.analyzer_views_cache.clear();
    }

    /// Get available analyzers (fast check, no source discovery).
    /// Returns analyzers that have at least one data source on the system.
    pub fn available_analyzers(&self) -> Vec<&dyn Analyzer> {
        self.analyzers
            .iter()
            .filter(|a| a.is_available())
            .map(|a| a.as_ref())
            .collect()
    }

    /// Get available analyzers with their discovered data sources.
    /// Returns analyzers that have at least one data source on the system.
    /// Sources are discovered once and returned for callers to use directly.
    pub fn available_analyzers_with_sources(&self) -> Vec<(&dyn Analyzer, Vec<DataSource>)> {
        self.analyzers
            .iter()
            .filter_map(|a| {
                let sources = a.discover_data_sources().ok()?;
                if sources.is_empty() {
                    return None;
                }
                Some((a.as_ref(), sources))
            })
            .collect()
    }

    /// Get analyzer by display name
    pub fn get_analyzer_by_display_name(&self, display_name: &str) -> Option<&dyn Analyzer> {
        self.analyzers
            .iter()
            .find(|a| a.display_name() == display_name)
            .map(|a| a.as_ref())
    }

    /// Load stats from all available analyzers in parallel using a scoped threadpool.
    /// Creates a temporary rayon threadpool that is dropped after use, releasing memory.
    /// Use this when you need full stats but aren't already inside a rayon context.
    pub fn load_all_stats_parallel_scoped(&self) -> Result<crate::types::MultiAnalyzerStats> {
        let pool = rayon::ThreadPoolBuilder::new()
            .build()
            .expect("Failed to create rayon threadpool");
        pool.install(|| self.load_all_stats_parallel())
        // Pool is dropped here, releasing threads
    }

    /// Load stats from all available analyzers in parallel.
    /// Used for uploads - returns full stats with messages.
    /// Must be called within a rayon threadpool context for parallelism.
    pub fn load_all_stats_parallel(&self) -> Result<crate::types::MultiAnalyzerStats> {
        let available = self.available_analyzers_with_sources();

        // Parse all analyzers in parallel using rayon
        let results: Vec<_> = available
            .into_par_iter()
            .map(|(analyzer, sources)| analyzer.get_stats_with_sources(sources))
            .collect();

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

    /// Load view-only stats using rayon for parallel file reads.
    /// Called once at startup. Uses rayon threadpool for parallel I/O operations.
    /// Populates file contribution cache for true incremental updates.
    /// Must be called within a rayon threadpool context for parallelism.
    pub fn load_all_stats_views_parallel(&self) -> Result<crate::types::MultiAnalyzerStatsView> {
        // Contribution cache variants based on analyzer strategy
        enum CachedContributions {
            SingleMessage(Vec<(PathHash, SingleMessageContribution)>),
            SingleSession(Vec<(PathHash, SingleSessionContribution)>),
            MultiSession(Vec<(PathHash, MultiSessionContribution)>),
        }

        // Get available analyzers with their sources (single discovery)
        let analyzer_data: Vec<_> = self
            .available_analyzers_with_sources()
            .into_iter()
            .map(|(a, sources)| {
                let strategy = a.contribution_strategy();
                (a, a.display_name().to_string(), sources, strategy)
            })
            .collect();

        // Parse all analyzers in parallel using rayon
        let all_results: Vec<_> = analyzer_data
            .into_par_iter()
            .map(|(analyzer, name, sources, strategy)| {
                let analyzer_name_arc: Arc<str> = Arc::from(name.as_str());

                // Parse sources with path association preserved.
                let grouped = analyzer.parse_sources_parallel_with_paths(&sources);

                // Compute contributions per source based on strategy
                let (contributions, all_messages): (CachedContributions, Vec<Vec<_>>) =
                    match strategy {
                        ContributionStrategy::SingleMessage => {
                            let (contribs, msgs): (Vec<_>, Vec<_>) = grouped
                                .into_par_iter()
                                .map(|(path, msgs)| {
                                    let path_hash = PathHash::new(&path);
                                    let contribution = msgs
                                        .first()
                                        .map(SingleMessageContribution::from_message)
                                        .unwrap_or_else(|| SingleMessageContribution {
                                            stats: Default::default(),
                                            date: Default::default(),
                                            model: None,
                                            session_hash: SessionHash::default(),
                                        });
                                    ((path_hash, contribution), msgs)
                                })
                                .unzip();
                            (CachedContributions::SingleMessage(contribs), msgs)
                        }
                        ContributionStrategy::SingleSession => {
                            let (contribs, msgs): (Vec<_>, Vec<_>) = grouped
                                .into_par_iter()
                                .map(|(path, msgs)| {
                                    let path_hash = PathHash::new(&path);
                                    let contribution =
                                        SingleSessionContribution::from_messages(&msgs);
                                    ((path_hash, contribution), msgs)
                                })
                                .unzip();
                            (CachedContributions::SingleSession(contribs), msgs)
                        }
                        ContributionStrategy::MultiSession => {
                            let (contribs, msgs): (Vec<_>, Vec<_>) = grouped
                                .into_par_iter()
                                .map(|(path, msgs)| {
                                    let path_hash = PathHash::new(&path);
                                    let contribution = MultiSessionContribution::from_messages(
                                        &msgs,
                                        Arc::clone(&analyzer_name_arc),
                                    );
                                    ((path_hash, contribution), msgs)
                                })
                                .unzip();
                            (CachedContributions::MultiSession(contribs), msgs)
                        }
                    };

                let all_messages: Vec<_> = all_messages.into_iter().flatten().collect();

                // Deduplicate messages across sources
                let messages = crate::utils::deduplicate_by_global_hash(all_messages);

                // Aggregate stats
                let mut daily_stats = crate::utils::aggregate_by_date(&messages);
                daily_stats.retain(|date, _| date != "unknown");
                let num_conversations = daily_stats
                    .values()
                    .map(|stats| stats.conversations as u64)
                    .sum();

                let stats = AgenticCodingToolStats {
                    daily_stats,
                    num_conversations,
                    messages,
                    analyzer_name: name.clone(),
                };

                (
                    name,
                    contributions,
                    Ok(stats) as Result<AgenticCodingToolStats>,
                )
            })
            .collect();

        // Build views from results and cache contributions
        let mut all_views = Vec::new();
        for (name, contributions, result) in all_results {
            match result {
                Ok(stats) => {
                    // Cache file contributions based on type
                    match contributions {
                        CachedContributions::SingleMessage(contribs) => {
                            for (path_hash, contribution) in contribs {
                                self.contribution_cache
                                    .insert_single_message(path_hash, contribution);
                            }
                        }
                        CachedContributions::SingleSession(contribs) => {
                            for (path_hash, contribution) in contribs {
                                self.contribution_cache
                                    .insert_single_session(path_hash, contribution);
                            }
                        }
                        CachedContributions::MultiSession(contribs) => {
                            for (path_hash, contribution) in contribs {
                                self.contribution_cache
                                    .insert_multi_session(path_hash, contribution);
                            }
                        }
                    }
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

        // Shrink caches after bulk insertion
        self.contribution_cache.shrink_to_fit();

        Ok(crate::types::MultiAnalyzerStatsView {
            analyzer_stats: all_views,
        })
    }

    /// Reload stats for a single file change using true incremental update.
    /// O(1) update - only reparses the changed file, subtracts old contribution,
    /// adds new contribution. No cloning needed thanks to RwLock.
    /// Uses sequential parsing (no threadpool) since it's just one file.
    pub fn reload_file_incremental(
        &self,
        analyzer_name: &str,
        changed_path: &std::path::Path,
    ) -> Result<()> {
        let analyzer = self
            .get_analyzer_by_display_name(analyzer_name)
            .ok_or_else(|| anyhow::anyhow!("Analyzer not found: {}", analyzer_name))?;

        // Skip invalid paths (directories, wrong file types, etc.)
        if !analyzer.is_valid_data_path(changed_path) {
            return Ok(());
        }

        // Mark file as dirty for incremental upload (only for valid data paths)
        self.mark_file_dirty(analyzer_name, changed_path);

        // Create Arc<str> once for this update
        let analyzer_name_arc: Arc<str> = Arc::from(analyzer_name);

        // Hash the path for cache lookup (no allocation)
        let path_hash = PathHash::new(changed_path);

        // Get contribution strategy for this analyzer
        let strategy = analyzer.contribution_strategy();

        // Parse just the changed file (sequential, no threadpool needed for single file)
        let source = DataSource {
            path: changed_path.to_path_buf(),
        };
        let new_messages = match analyzer.parse_source(&source) {
            Ok(msgs) => crate::utils::deduplicate_by_global_hash(msgs),
            Err(e) => {
                eprintln!(
                    "Failed to parse {} source {:?}: {}",
                    analyzer.display_name(),
                    source.path,
                    e
                );
                Vec::new()
            }
        };

        // Get or create the cached view for this analyzer
        let shared_view = self
            .analyzer_views_cache
            .entry(analyzer_name.to_string())
            .or_insert_with(|| {
                Arc::new(parking_lot::RwLock::new(AnalyzerStatsView {
                    daily_stats: BTreeMap::new(),
                    session_aggregates: Vec::new(),
                    num_conversations: 0,
                    analyzer_name: Arc::clone(&analyzer_name_arc),
                }))
            })
            .clone();

        match strategy {
            ContributionStrategy::SingleMessage => {
                let old_contribution = self.contribution_cache.get_single_message(&path_hash);

                let new_contribution = new_messages
                    .first()
                    .map(SingleMessageContribution::from_message)
                    .unwrap_or_else(|| SingleMessageContribution {
                        stats: Default::default(),
                        date: Default::default(),
                        model: None,
                        session_hash: SessionHash::default(),
                    });

                self.contribution_cache
                    .insert_single_message(path_hash, new_contribution);

                let mut view = shared_view.write();
                if let Some(old) = old_contribution {
                    view.subtract_single_message_contribution(&old);
                }
                view.add_single_message_contribution(&new_contribution);
            }
            ContributionStrategy::SingleSession => {
                let old_contribution = self.contribution_cache.get_single_session(&path_hash);

                let new_contribution = SingleSessionContribution::from_messages(&new_messages);

                self.contribution_cache
                    .insert_single_session(path_hash, new_contribution.clone());

                let mut view = shared_view.write();
                if let Some(old) = old_contribution {
                    view.subtract_single_session_contribution(&old);
                }
                view.add_single_session_contribution(&new_contribution);
            }
            ContributionStrategy::MultiSession => {
                let old_contribution = self.contribution_cache.get_multi_session(&path_hash);

                let new_contribution = MultiSessionContribution::from_messages(
                    &new_messages,
                    Arc::clone(&analyzer_name_arc),
                );

                self.contribution_cache
                    .insert_multi_session(path_hash, new_contribution.clone());

                let mut view = shared_view.write();
                if let Some(old) = old_contribution {
                    view.subtract_multi_session_contribution(&old);
                }
                view.add_multi_session_contribution(&new_contribution);
            }
        }

        Ok(())
    }

    /// Remove a file from the cache and update the view (for file deletion events).
    /// Returns true if the file was found and removed.
    /// Also marks the file as dirty for upload if it was in the cache.
    pub fn remove_file_from_cache(&self, analyzer_name: &str, path: &std::path::Path) -> bool {
        let path_hash = PathHash::new(path);

        // Try to remove from any cache and update view accordingly
        if let Some(removed) = self.contribution_cache.remove_any(&path_hash) {
            if let Some(shared_view) = self.analyzer_views_cache.get(analyzer_name) {
                let mut view = shared_view.write();
                match removed {
                    RemovedContribution::SingleMessage(old) => {
                        view.subtract_single_message_contribution(&old);
                    }
                    RemovedContribution::SingleSession(old) => {
                        view.subtract_single_session_contribution(&old);
                    }
                    RemovedContribution::MultiSession(old) => {
                        view.subtract_multi_session_contribution(&old);
                    }
                }
            }
            true
        } else {
            false
        }
    }

    /// Check if the contribution cache is populated for an analyzer.
    pub fn has_cached_contributions(&self, analyzer_name: &str) -> bool {
        self.analyzer_views_cache.contains_key(analyzer_name)
    }

    /// Get the cached view for an analyzer.
    pub fn get_cached_view(&self, analyzer_name: &str) -> Option<SharedAnalyzerView> {
        self.analyzer_views_cache
            .get(analyzer_name)
            .map(|r| r.clone())
    }

    /// Get all cached views as a Vec, for building MultiAnalyzerStatsView.
    /// Returns SharedAnalyzerView clones (cheap Arc pointer copies).
    /// Views are returned in registration order for stable tab ordering in TUI.
    pub fn get_all_cached_views(&self) -> Vec<SharedAnalyzerView> {
        let order = self.analyzer_order.read();
        order
            .iter()
            .filter_map(|name| self.analyzer_views_cache.get(name).map(|v| v.clone()))
            .collect()
    }

    /// Update the cache with a new view for an analyzer.
    /// Used when doing a full reload (not incremental).
    pub fn update_cached_view(&self, analyzer_name: &str, view: SharedAnalyzerView) {
        self.analyzer_views_cache
            .insert(analyzer_name.to_string(), view);
    }

    /// Get a mapping of data directories to analyzer names for file watching.
    /// Uses explicit watch directories from `get_watch_directories()`.
    pub fn get_directory_to_analyzer_mapping(&self) -> std::collections::HashMap<PathBuf, String> {
        let mut dir_to_analyzer = std::collections::HashMap::new();

        for analyzer in self.available_analyzers() {
            for dir in analyzer.get_watch_directories() {
                if dir.exists() {
                    dir_to_analyzer.insert(dir, analyzer.display_name().to_string());
                }
            }
        }

        dir_to_analyzer
    }

    /// Mark a file as dirty for the next upload (file has been modified).
    pub fn mark_file_dirty(&self, analyzer_name: &str, path: &Path) {
        self.dirty_files_for_upload
            .insert(path.to_path_buf(), analyzer_name.to_string());
    }

    /// Returns a shared handle to dirty files for use in async tasks.
    pub fn dirty_files_handle(&self) -> Arc<DashMap<PathBuf, String>> {
        Arc::clone(&self.dirty_files_for_upload)
    }

    /// Check if we have any dirty files tracked.
    pub fn has_dirty_files(&self) -> bool {
        !self.dirty_files_for_upload.is_empty()
    }

    /// Load messages from dirty files for incremental upload.
    /// Returns messages filtered to only those created since last_upload_timestamp.
    /// Returns empty vec if no dirty files are tracked.
    pub fn load_messages_for_upload(
        &self,
        last_upload_timestamp: i64,
    ) -> Result<Vec<ConversationMessage>> {
        if self.dirty_files_for_upload.is_empty() {
            return Ok(Vec::new());
        }

        // Parse dirty files sequentially (typically only 1-2 files)
        let mut all_messages = Vec::with_capacity(4);
        for entry in self.dirty_files_for_upload.iter() {
            let (path, analyzer_name) = entry.pair();
            if let Some(analyzer) = self.get_analyzer_by_display_name(analyzer_name) {
                let source = DataSource { path: path.clone() };
                if let Ok(msgs) = analyzer.parse_source(&source) {
                    all_messages.extend(msgs);
                }
            }
        }
        all_messages = crate::utils::deduplicate_by_global_hash(all_messages);

        // Filter by timestamp
        let messages_later_than: Vec<_> = all_messages
            .into_iter()
            .filter(|msg| msg.date.timestamp_millis() >= last_upload_timestamp)
            .collect();

        Ok(messages_later_than)
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

        fn parse_source(&self, _source: &DataSource) -> Result<Vec<ConversationMessage>> {
            Ok(Vec::new())
        }

        fn get_stats_with_sources(
            &self,
            _sources: Vec<DataSource>,
        ) -> Result<AgenticCodingToolStats> {
            if self.fail_stats {
                anyhow::bail!("stats failed");
            }
            self.stats
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no stats"))
        }

        fn get_stats(&self) -> Result<AgenticCodingToolStats> {
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

        fn get_watch_directories(&self) -> Vec<PathBuf> {
            // Return parent directories of sources for testing
            self.sources
                .iter()
                .filter_map(|p| p.parent().map(|parent| parent.to_path_buf()))
                .collect()
        }

        fn contribution_strategy(&self) -> ContributionStrategy {
            ContributionStrategy::SingleSession
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

        let stats = registry.load_all_stats_parallel().expect("load stats");
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

        fn parse_source(&self, _source: &DataSource) -> Result<Vec<ConversationMessage>> {
            Ok(Vec::new())
        }

        fn get_stats(&self) -> Result<AgenticCodingToolStats> {
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

        fn contribution_strategy(&self) -> ContributionStrategy {
            ContributionStrategy::SingleSession
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

    /// Test that analyzer tab order remains stable across initial load and updates.
    /// Regression test for bug where DashMap iteration order caused tabs to jump.
    #[tokio::test]
    async fn test_analyzer_order_stable_across_updates() {
        let mut registry = AnalyzerRegistry::new();
        let expected_order = vec!["analyzer-a", "analyzer-b", "analyzer-c"];

        // Register analyzers in a specific order
        for name in &expected_order {
            registry.register(TestAnalyzer {
                name,
                available: true,
                stats: Some(sample_stats(name)),
                sources: vec![PathBuf::from(format!("/fake/{}.jsonl", name))],
                fail_stats: false,
            });
        }

        // Initial load should preserve registration order
        let initial_views = registry
            .load_all_stats_views_parallel()
            .expect("load_all_stats_views_parallel");
        let initial_names: Vec<String> = initial_views
            .analyzer_stats
            .iter()
            .map(|v| v.read().analyzer_name.to_string())
            .collect();
        let expected_strings: Vec<String> = expected_order.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            initial_names, expected_strings,
            "Initial load order mismatch"
        );

        // get_all_cached_views() should return same order (used by watcher updates)
        let cached_names: Vec<String> = registry
            .get_all_cached_views()
            .iter()
            .map(|v| v.read().analyzer_name.to_string())
            .collect();
        assert_eq!(
            cached_names, expected_strings,
            "Cached views order mismatch"
        );

        // Order stable after incremental file update
        let _ = registry
            .reload_file_incremental("analyzer-b", &PathBuf::from("/fake/analyzer-b.jsonl"));
        let after_update: Vec<String> = registry
            .get_all_cached_views()
            .iter()
            .map(|v| v.read().analyzer_name.to_string())
            .collect();
        assert_eq!(after_update, expected_strings, "Order changed after update");

        // Order stable after file removal
        let _ =
            registry.remove_file_from_cache("analyzer-c", &PathBuf::from("/fake/analyzer-c.jsonl"));
        let after_removal: Vec<String> = registry
            .get_all_cached_views()
            .iter()
            .map(|v| v.read().analyzer_name.to_string())
            .collect();
        assert_eq!(
            after_removal, expected_strings,
            "Order changed after removal"
        );
    }

    // =========================================================================
    // DIRTY FILE TRACKING TESTS
    // =========================================================================

    #[test]
    fn test_mark_file_dirty_and_clear() {
        let registry = AnalyzerRegistry::new();
        let path = PathBuf::from("/fake/test.json");

        assert!(!registry.has_dirty_files());

        registry.mark_file_dirty("test", &path);
        assert!(registry.has_dirty_files());

        registry.dirty_files_handle().clear();
        assert!(!registry.has_dirty_files());
    }

    #[test]
    fn test_has_dirty_files_multiple() {
        let registry = AnalyzerRegistry::new();

        registry.mark_file_dirty("test", &PathBuf::from("/a.json"));
        registry.mark_file_dirty("test", &PathBuf::from("/b.json"));
        registry.mark_file_dirty("test", &PathBuf::from("/c.json"));

        assert!(registry.has_dirty_files());

        registry.dirty_files_handle().clear();
        assert!(!registry.has_dirty_files());
    }

    #[test]
    fn test_load_messages_for_upload_empty_dirty_set_no_analyzers() {
        let registry = AnalyzerRegistry::new();

        // No analyzers, no dirty files - should return empty
        let messages = registry.load_messages_for_upload(0).expect("load");
        assert!(messages.is_empty());
    }

    #[test]
    fn test_remove_file_from_cache_marks_dirty() {
        let registry = AnalyzerRegistry::new();
        let path = PathBuf::from("/fake/test.json");

        // File not in cache - should return false and not mark dirty
        assert!(!registry.remove_file_from_cache("test", &path));
        assert!(!registry.has_dirty_files());
    }

    #[tokio::test]
    async fn test_reload_file_incremental_marks_dirty_for_valid_path() {
        use std::fs;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path().join("test.json");
        fs::write(&path, "{}").expect("write");

        let mut registry = AnalyzerRegistry::new();
        registry.register(TestAnalyzer {
            name: "test",
            available: true,
            stats: Some(sample_stats("test")),
            sources: vec![path.clone()],
            fail_stats: false,
        });

        // Load initial stats to populate cache
        let _ = registry.load_all_stats_views_parallel();

        // Reload should mark file dirty
        assert!(!registry.has_dirty_files());
        let _ = registry.reload_file_incremental("test", &path);
        assert!(registry.has_dirty_files());
    }

    #[tokio::test]
    async fn test_reload_file_incremental_skips_invalid_path() {
        use std::fs;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let valid_path = temp_dir.path().join("test.json");
        fs::write(&valid_path, "{}").expect("write");
        let invalid_path = temp_dir.path().join("subdir");
        fs::create_dir(&invalid_path).expect("mkdir");

        let mut registry = AnalyzerRegistry::new();
        registry.register(TestAnalyzer {
            name: "test",
            available: true,
            stats: Some(sample_stats("test")),
            sources: vec![valid_path],
            fail_stats: false,
        });

        // Load initial stats
        let _ = registry.load_all_stats_views_parallel();

        // Invalid path (directory) should not mark dirty
        let _ = registry.reload_file_incremental("test", &invalid_path);
        assert!(!registry.has_dirty_files());
    }
}
