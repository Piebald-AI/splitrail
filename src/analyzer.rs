use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;

use crate::types::{AgenticCodingToolStats, ConversationMessage};
use crate::utils::ModelAbbreviations;

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

    /// Get model abbreviations for this analyzer
    fn get_model_abbreviations(&self) -> ModelAbbreviations;

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

    /// Get the analyzer with the most data sources (prioritizes by volume)
    pub fn get_primary_analyzer_by_volume(&self) -> Option<&dyn Analyzer> {
        let mut best_analyzer: Option<&dyn Analyzer> = None;
        let mut best_count: usize = 0;

        for analyzer in self.available_analyzers() {
            if let Ok(sources) = analyzer.discover_data_sources() {
                let count = sources.len();
                if count > best_count {
                    best_count = count;
                    best_analyzer = Some(analyzer);
                }
            }
        }

        best_analyzer
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
