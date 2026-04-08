use super::copilot::{
    COPILOT_CLI_STATE_DIRS, copilot_cli_session_dirs, is_copilot_cli_session_file,
    parse_copilot_cli_session_file,
};
use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::types::ConversationMessage;
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct CopilotCliAnalyzer;

impl CopilotCliAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Analyzer for CopilotCliAnalyzer {
    fn display_name(&self) -> &'static str {
        "GitHub Copilot CLI"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            for dir_name in COPILOT_CLI_STATE_DIRS {
                patterns.push(format!("{home_str}/.copilot/{dir_name}/*.jsonl"));
                patterns.push(format!("{home_str}/.copilot/{dir_name}/*/events.jsonl"));
            }
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = copilot_cli_session_dirs()
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).min_depth(1).max_depth(2).into_iter())
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_type().is_file() && is_copilot_cli_session_file(entry.path())
            })
            .map(|entry| DataSource {
                path: entry.into_path(),
            })
            .collect();

        Ok(sources)
    }

    fn is_available(&self) -> bool {
        copilot_cli_session_dirs()
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).min_depth(1).max_depth(2).into_iter())
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_type().is_file() && is_copilot_cli_session_file(entry.path()))
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        parse_copilot_cli_session_file(&source.path)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        copilot_cli_session_dirs()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        is_copilot_cli_session_file(path)
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }
}
