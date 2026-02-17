use super::opencode_common::{OpenCodeFormatAnalyzer, OpenCodeFormatConfig};
use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::types::{Application, ConversationMessage};
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// Analyzer for [OpenCode](https://opencode.ai) — a terminal-based AI coding
/// agent.  Delegates to the shared [`OpenCodeFormatAnalyzer`] with
/// OpenCode-specific paths and identity.
pub struct OpenCodeAnalyzer(OpenCodeFormatAnalyzer);

impl OpenCodeAnalyzer {
    pub fn new() -> Self {
        Self(OpenCodeFormatAnalyzer::new(OpenCodeFormatConfig {
            display_name: "OpenCode",
            application: Application::OpenCode,
            hash_prefix: "opencode",
            storage_subdir: "opencode",
        }))
    }
}

#[async_trait]
impl Analyzer for OpenCodeAnalyzer {
    fn display_name(&self) -> &'static str {
        self.0.display_name()
    }
    fn get_data_glob_patterns(&self) -> Vec<String> {
        self.0.get_data_glob_patterns()
    }
    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        self.0.discover_data_sources()
    }
    fn is_available(&self) -> bool {
        self.0.is_available()
    }
    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        self.0.parse_source(source)
    }
    fn parse_sources_parallel_with_paths(
        &self,
        sources: &[DataSource],
    ) -> Vec<(PathBuf, Vec<ConversationMessage>)> {
        self.0.parse_sources_parallel_with_paths(sources)
    }
    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        self.0.parse_sources_parallel(sources)
    }
    fn get_stats_with_sources(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<crate::types::AgenticCodingToolStats> {
        self.0.get_stats_with_sources(sources)
    }
    fn get_watch_directories(&self) -> Vec<PathBuf> {
        self.0.get_watch_directories()
    }
    fn is_valid_data_path(&self, path: &Path) -> bool {
        self.0.is_valid_data_path(path)
    }
    fn contribution_strategy(&self) -> ContributionStrategy {
        self.0.contribution_strategy()
    }
}
