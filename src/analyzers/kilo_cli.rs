use super::opencode_common::{OpenCodeFormatAnalyzer, OpenCodeFormatConfig};
use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::types::{Application, ConversationMessage};
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// Analyzer for [Kilo Code CLI](https://kilocode.ai) — a terminal-based AI
/// coding agent forked from OpenCode.  Delegates to the shared
/// [`OpenCodeFormatAnalyzer`] with Kilo-specific paths and identity.
pub struct KiloCliAnalyzer(OpenCodeFormatAnalyzer);

impl KiloCliAnalyzer {
    pub fn new() -> Self {
        Self(OpenCodeFormatAnalyzer::new(OpenCodeFormatConfig {
            display_name: "Kilo CLI",
            application: Application::KiloCli,
            hash_prefix: "kilo_cli",
            storage_subdir: "kilo",
        }))
    }
}

#[async_trait]
impl Analyzer for KiloCliAnalyzer {
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
