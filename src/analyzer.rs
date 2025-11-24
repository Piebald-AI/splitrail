use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;

use crate::types::{AgenticCodingToolStats, ConversationMessage};

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
#[derive(Default)]
pub struct AnalyzerRegistry {
    analyzers: Vec<Box<dyn Analyzer>>,
}

impl AnalyzerRegistry {
    /// Create a new analyzer registry
    pub fn new() -> Self {
        Self {
            analyzers: Vec::new(),
        }
    }

    /// Register an analyzer
    pub fn register<A: Analyzer + 'static>(&mut self, analyzer: A) {
        self.analyzers.push(Box::new(analyzer));
    }

    /// Get available analyzers (those that are present on the system)
    pub fn available_analyzers(&self) -> Vec<&dyn Analyzer> {
        self.analyzers
            .iter()
            .filter(|a| a.is_available())
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

    /// Load stats from all available analyzers
    pub async fn load_all_stats(&self) -> Result<crate::types::MultiAnalyzerStats> {
        let available_analyzers = self.available_analyzers();
        let mut all_stats = Vec::new();

        for analyzer in available_analyzers {
            match analyzer.get_stats().await {
                Ok(stats) => all_stats.push(stats),
                Err(e) => {
                    eprintln!(
                        "⚠️  Error analyzing {} data: {}",
                        analyzer.display_name(),
                        e
                    );
                }
            }
        }

        Ok(crate::types::MultiAnalyzerStats {
            analyzer_stats: all_stats,
        })
    }

    /// Get a mapping of data directories to analyzer names for file watching
    pub fn get_directory_to_analyzer_mapping(&self) -> std::collections::HashMap<PathBuf, String> {
        let mut dir_to_analyzer = std::collections::HashMap::new();

        for analyzer in self.available_analyzers() {
            if let Ok(sources) = analyzer.discover_data_sources() {
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
    use crate::types::{AgenticCodingToolStats, ConversationMessage, MessageRole, Stats, Application};
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

        let analyzer_ok = TestAnalyzer {
            name: "ok",
            available: true,
            stats: Some(sample_stats("ok")),
            sources: Vec::new(),
            fail_stats: false,
        };

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
            sources: Vec::new(),
            fail_stats: true,
        };

        registry.register(analyzer_ok);
        registry.register(analyzer_unavailable);
        registry.register(analyzer_fails);

        let avail = registry.available_analyzers();
        assert_eq!(avail.len(), 2); // "ok" and "fails"

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
        assert_eq!(
            mapping.get(&base).map(String::as_str),
            Some("mapper")
        );
    }
}
