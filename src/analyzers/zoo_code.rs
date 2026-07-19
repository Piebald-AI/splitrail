use crate::analyzer::{
    Analyzer, DataSource, discover_vscode_extension_sources, get_vscode_extension_tasks_dirs,
    vscode_extension_has_sources,
};
use crate::analyzers::roo_code::parse_roo_format_task_directory;
use crate::contribution_cache::ContributionStrategy;
use crate::types::{Application, ConversationMessage};
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

const ZOO_CODE_EXTENSION_ID: &str = "zoocodeorganization.zoo-code";

pub struct ZooCodeAnalyzer;

impl ZooCodeAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Analyzer for ZooCodeAnalyzer {
    fn display_name(&self) -> &'static str {
        "Zoo Code"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();
        let vscode_gui_forks = [
            "Antigravity",
            "Code",
            "Code - Insiders",
            "Cursor",
            "Positron",
            "VSCodium",
            "Windsurf",
        ];
        let vscode_cli_forks = ["vscode-server-insiders", "vscode-server"];

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();

            for fork in &vscode_gui_forks {
                patterns.push(format!(
                    "{home_str}/.config/{fork}/User/globalStorage/{ZOO_CODE_EXTENSION_ID}/tasks/*/ui_messages.json"
                ));
            }
            for fork in &vscode_cli_forks {
                patterns.push(format!(
                    "{home_str}/.{fork}/data/User/globalStorage/{ZOO_CODE_EXTENSION_ID}/tasks/*/ui_messages.json"
                ));
            }
            for fork in &vscode_gui_forks {
                patterns.push(format!(
                    "{home_str}/Library/Application Support/{fork}/User/globalStorage/{ZOO_CODE_EXTENSION_ID}/tasks/*/ui_messages.json"
                ));
            }
        }

        if let Ok(appdata) = std::env::var("APPDATA") {
            for fork in &vscode_gui_forks {
                patterns.push(format!(
                    "{appdata}\\{fork}\\User\\globalStorage\\{ZOO_CODE_EXTENSION_ID}\\tasks\\*\\ui_messages.json"
                ));
            }
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        discover_vscode_extension_sources(ZOO_CODE_EXTENSION_ID, "ui_messages.json", true)
    }

    fn is_available(&self) -> bool {
        vscode_extension_has_sources(ZOO_CODE_EXTENSION_ID, "ui_messages.json")
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        parse_roo_format_task_directory(&source.path, Application::ZooCode)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        get_vscode_extension_tasks_dirs(ZOO_CODE_EXTENSION_ID)
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        path.is_file() && path.file_name().is_some_and(|n| n == "ui_messages.json")
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }
}
