use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use jwalk::WalkDir;
use std::path::PathBuf;

use crate::types::{AgenticCodingToolStats, ConversationMessage};

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
}

/// Registry for managing multiple analyzers
pub struct AnalyzerRegistry {
    analyzers: Vec<Box<dyn Analyzer>>,
    /// Cached data sources per analyzer (display_name -> sources)
    data_source_cache: DashMap<String, Vec<DataSource>>,
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

    /// Load stats from all available analyzers in parallel
    pub async fn load_all_stats(&self) -> Result<crate::types::MultiAnalyzerStats> {
        use futures::future::join_all;

        let available_analyzers = self.available_analyzers();

        // Create futures for all analyzers - they'll run concurrently
        let futures: Vec<_> = available_analyzers
            .into_iter()
            .map(|analyzer| async move {
                let name = analyzer.display_name().to_string();
                let result = analyzer.get_stats().await;
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
