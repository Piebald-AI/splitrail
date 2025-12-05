use crate::analyzer::{Analyzer, DataSource, discover_vscode_extension_sources};
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, MessageRole, Stats,
};
use crate::utils::hash_text;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::path::Path;

const ROO_CODE_EXTENSION_ID: &str = "rooveterinaryinc.roo-cline";

pub struct RooCodeAnalyzer;

impl RooCodeAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

// Roo Code-specific data structures based on the discovered format

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RooCodeUiMessage {
    Say {
        ts: i64,
        say: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        images: Vec<String>,
        #[serde(default)]
        partial: bool,
    },
    Ask {
        ts: i64,
        ask: String,
        #[serde(default)]
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RooCodeApiRequest {
    #[serde(rename = "apiProtocol")]
    api_protocol: String,
    #[serde(rename = "tokensIn")]
    tokens_in: u64,
    #[serde(rename = "tokensOut")]
    tokens_out: u64,
    #[serde(rename = "cacheWrites")]
    cache_writes: u64,
    #[serde(rename = "cacheReads")]
    cache_reads: u64,
    cost: f64,
}

// Helper function to extract project ID from Roo Code file path and hash it
fn extract_and_hash_project_id_roo_code(file_path: &Path) -> String {
    // Roo Code path format: ~/.config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/{UUID}/
    // We'll use the parent directory of tasks as the project identifier (global storage path)

    let path_components: Vec<_> = file_path.components().collect();
    for (i, component) in path_components.iter().enumerate() {
        if let std::path::Component::Normal(name) = component
            && name.to_str() == Some("tasks")
            && i > 0
            && let std::path::Component::Normal(project_id) = &path_components[i - 1]
            && let Some(project_id_str) = project_id.to_str()
        {
            return hash_text(project_id_str);
        }
    }

    // Fallback: hash the full file path
    hash_text(&file_path.to_string_lossy())
}

fn is_probably_tool_json_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with("[{")) && trimmed.contains("\"tool\"")
}

fn is_probably_shell_command(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Common shell command prefixes
    let lower = trimmed.to_lowercase();
    for prefix in [
        "git ", "cargo ", "just ", "npm ", "pnpm ", "yarn ", "python ", "pytest ", "npx ", "pip ",
        "bundle ", "rails ", "docker ", "kubectl ", "helm ", "make ", "go ", "node ", "deno ",
        "bash ", "sh ", "zsh ",
    ] {
        if lower.starts_with(prefix) {
            return true;
        }
    }

    // Pipelines or multiple commands chained
    if lower.contains(" && ") || lower.contains(" || ") || lower.contains(" | ") {
        return true;
    }

    false
}

// Helper function to extract model from environment details text
fn extract_model_from_text(text: &str) -> Option<String> {
    // Look for <model>...</model> tags in the text
    if let Some(start) = text.find("<model>")
        && let Some(end) = text[start..].find("</model>")
    {
        let model = &text[start + 7..start + end];
        return Some(model.to_string());
    }
    None
}

// Parse a single Roo Code task directory
fn parse_roo_code_task_directory(task_dir: &Path) -> Result<Vec<ConversationMessage>> {
    let project_hash = extract_and_hash_project_id_roo_code(task_dir);

    // Get the conversation hash from the task directory name (UUID)
    let conversation_hash = task_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(hash_text)
        .unwrap_or_else(|| hash_text(&task_dir.to_string_lossy()));

    // Try to extract model from api_conversation_history.json
    let mut current_model: Option<String> = None;
    let api_history_path = task_dir.join("api_conversation_history.json");
    if api_history_path.exists()
        && let Ok(mut content) = std::fs::read_to_string(&api_history_path).map(|s| s.into_bytes())
        && let Ok(history) = simd_json::from_slice::<Vec<simd_json::OwnedValue>>(&mut content)
    {
        // Look for model in user messages with environment_details (iterate forward and keep last one)
        for entry in history.iter() {
            if let Some(role) = entry.get("role").and_then(|r| r.as_str())
                && role == "user"
                && let Some(content_arr) = entry.get("content").and_then(|c| c.as_array())
            {
                for content_item in content_arr {
                    if let Some(text) = content_item.get("text").and_then(|t| t.as_str())
                        && let Some(model) = extract_model_from_text(text)
                    {
                        current_model = Some(model);
                    }
                }
            }
        }
    }

    // Read ui_messages.json
    let ui_messages_path = task_dir.join("ui_messages.json");
    if !ui_messages_path.exists() {
        return Ok(Vec::new());
    }

    let mut ui_messages_content = std::fs::read_to_string(&ui_messages_path)?.into_bytes();
    let ui_messages: Vec<RooCodeUiMessage> = simd_json::from_slice(&mut ui_messages_content)
        .context("Failed to parse ui_messages.json")?;

    let mut entries = Vec::new();
    let mut message_index = 0;
    let mut fallback_session_name: Option<String> = None;

    // Process ui_messages to extract API requests with token/cost data
    for message in ui_messages {
        match message {
            RooCodeUiMessage::Say { ts, say, text, .. } => {
                // We're interested in "api_req_started" messages which contain token/cost data
                if say == "api_req_started" && !text.is_empty() {
                    // Parse the embedded JSON in the text field
                    let mut text_bytes = text.into_bytes();
                    if let Ok(api_req) = simd_json::from_slice::<RooCodeApiRequest>(&mut text_bytes)
                    {
                        // Create a message entry for this API request
                        let date = DateTime::from_timestamp_millis(ts).unwrap_or_else(Utc::now);

                        let local_hash = format!("{}-{}", conversation_hash, message_index);
                        let global_hash = hash_text(&format!(
                            "{}:{}:{}:{}",
                            project_hash, conversation_hash, message_index, ts
                        ));

                        let stats = Stats {
                            input_tokens: api_req.tokens_in,
                            output_tokens: api_req.tokens_out,
                            cache_creation_tokens: api_req.cache_writes,
                            cache_read_tokens: api_req.cache_reads,
                            cached_tokens: api_req.cache_writes + api_req.cache_reads,
                            cost: api_req.cost,
                            tool_calls: if api_req.tokens_out > 0 { 1 } else { 0 },
                            ..Default::default()
                        };

                        entries.push(ConversationMessage {
                            application: Application::RooCode,
                            date,
                            project_hash: project_hash.clone(),
                            conversation_hash: conversation_hash.clone(),
                            local_hash: Some(local_hash),
                            global_hash,
                            model: current_model.clone(),
                            stats,
                            role: MessageRole::Assistant, // API requests are from the assistant
                            uuid: None,
                            session_name: fallback_session_name.clone(),
                        });

                        message_index += 1;
                    }
                }
            }
            RooCodeUiMessage::Ask { ts, ask, text, .. } => {
                // Track user interactions (followup questions, confirmations)
                if matches!(
                    ask.as_str(),
                    "followup" | "command" | "tool" | "completion_result"
                ) {
                    let date = DateTime::from_timestamp_millis(ts).unwrap_or_else(Utc::now);

                    let local_hash = format!("{}-user-{}", conversation_hash, message_index);
                    let global_hash = hash_text(&format!(
                        "{}:{}:user:{}:{}",
                        project_hash, conversation_hash, message_index, ts
                    ));

                    if !text.is_empty() {
                        let text_str = text;
                        if !is_probably_tool_json_text(&text_str)
                            && !is_probably_shell_command(&text_str)
                        {
                            let truncated = if text_str.chars().count() > 50 {
                                let chars: String = text_str.chars().take(50).collect();
                                format!("{}...", chars)
                            } else {
                                text_str
                            };

                            if fallback_session_name.is_none() {
                                fallback_session_name = Some(truncated);
                            }
                        }
                    }

                    entries.push(ConversationMessage {
                        application: Application::RooCode,
                        date,
                        project_hash: project_hash.clone(),
                        conversation_hash: conversation_hash.clone(),
                        local_hash: Some(local_hash),
                        global_hash,
                        model: None,
                        stats: Stats::default(), // User messages don't have token costs
                        role: MessageRole::User,
                        uuid: None,
                        session_name: fallback_session_name.clone(),
                    });

                    message_index += 1;
                }
            }
        }
    }

    Ok(entries)
}

#[async_trait]
impl Analyzer for RooCodeAnalyzer {
    fn display_name(&self) -> &'static str {
        "Roo Code"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        // VSCode forks that might have Kilo Code installed: Code, Cursor, Windsurf, VSCodium, Positron, Antigravity
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

            // Linux paths for all VSCode forks
            for fork in &vscode_gui_forks {
                patterns.push(format!("{home_str}/.config/{fork}/User/globalStorage/rooveterinaryinc.roo-cline/tasks/*/ui_messages.json"));
            }
            // Linux paths for all VSCode CLI (server) forks
            for fork in &vscode_cli_forks {
                patterns.push(format!("{home_str}/.{fork}/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks/*/ui_messages.json"));
            }

            // macOS paths for all VSCode forks
            for fork in &vscode_gui_forks {
                patterns.push(format!("{home_str}/Library/Application Support/{fork}/User/globalStorage/rooveterinaryinc.roo-cline/tasks/*/ui_messages.json"));
            }
        }

        // Windows paths for all VSCode forks
        if let Ok(appdata) = std::env::var("APPDATA") {
            for fork in &vscode_gui_forks {
                patterns.push(format!("{appdata}\\{fork}\\User\\globalStorage\\rooveterinaryinc.roo-cline\\tasks\\*\\ui_messages.json"));
            }
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        discover_vscode_extension_sources(ROO_CODE_EXTENSION_ID, "ui_messages.json", true)
    }

    async fn parse_conversations(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<Vec<ConversationMessage>> {
        // Parse all task directories in parallel
        let all_entries: Vec<ConversationMessage> = sources
            .into_par_iter()
            .flat_map(|source| match parse_roo_code_task_directory(&source.path) {
                Ok(messages) => messages,
                Err(e) => {
                    eprintln!(
                        "Failed to parse Roo Code task directory {:?}: {}",
                        source.path, e
                    );
                    Vec::new()
                }
            })
            .collect();

        // Parallel deduplicate by global hash
        Ok(crate::utils::deduplicate_by_global_hash_parallel(
            all_entries,
        ))
    }

    async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
        let sources = self.discover_data_sources()?;
        let messages = self.parse_conversations(sources).await?;
        let mut daily_stats = crate::utils::aggregate_by_date(&messages);

        // Remove any "unknown" entries
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

    fn is_available(&self) -> bool {
        self.discover_data_sources()
            .is_ok_and(|sources| !sources.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_extract_project_hash() {
        let path = PathBuf::from(
            "/home/user/.config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/9f365349-84f2-4a9a-b470-f94910583293/ui_messages.json",
        );
        let parent = path.parent().unwrap();
        let hash = extract_and_hash_project_id_roo_code(parent);
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 hex length
    }
}
