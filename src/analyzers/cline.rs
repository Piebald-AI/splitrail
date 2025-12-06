use crate::analyzer::{Analyzer, DataSource, discover_vscode_extension_sources};
use crate::types::{AgenticCodingToolStats, Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::path::Path;

const CLINE_EXTENSION_ID: &str = "saoudrizwan.claude-dev";

pub struct ClineAnalyzer;

impl ClineAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

// Cline-specific data structures based on the discovered format

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClineFileInContext {
    path: String,
    record_state: String,
    record_source: String,
    cline_read_date: i64,
    cline_edit_date: Option<i64>,
    user_edit_date: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClineTaskMetadata {
    #[serde(default)]
    files_in_context: Vec<ClineFileInContext>,
    model_usage: Vec<ClineModelUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClineModelUsage {
    ts: i64, // Timestamp in milliseconds
    model_id: String,
    model_provider_id: String,
    mode: String, // "plan" or "act"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClineUiMessage {
    Say {
        ts: i64,
        say: String,
        #[serde(default)]
        text: String,
        #[serde(rename = "conversationHistoryIndex")]
        conversation_history_index: i32,
        #[serde(default)]
        partial: bool,
    },
    Ask {
        ts: i64,
        ask: String,
        #[serde(default)]
        text: String,
        #[serde(rename = "conversationHistoryIndex")]
        conversation_history_index: i32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClineApiRequest {
    request: String,
    #[serde(rename = "tokensIn")]
    tokens_in: u64,
    #[serde(rename = "tokensOut")]
    tokens_out: u64,
    #[serde(rename = "cacheWrites")]
    cache_writes: u64,
    #[serde(rename = "cacheReads")]
    cache_reads: u64,
    cost: f64,
    #[serde(rename = "cancelReason")]
    cancel_reason: Option<String>,
}

// Helper function to extract project ID from Cline file path and hash it
fn extract_and_hash_project_id_cline(file_path: &Path) -> String {
    // Cline path format: ~/.config/Code/User/globalStorage/saoudrizwan.claude-dev/tasks/{TIMESTAMP}/
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

// Parse a single Cline task directory
fn parse_cline_task_directory(task_dir: &Path) -> Result<Vec<ConversationMessage>> {
    let project_hash = extract_and_hash_project_id_cline(task_dir);

    // Get the conversation hash from the task directory name (timestamp)
    let conversation_hash = task_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(hash_text)
        .unwrap_or_else(|| hash_text(&task_dir.to_string_lossy()));

    // Read metadata file
    let metadata_path = task_dir.join("task_metadata.json");
    let metadata: ClineTaskMetadata = if metadata_path.exists() {
        simd_json::from_slice(&mut std::fs::read_to_string(&metadata_path)?.into_bytes())
            .context("Failed to parse task_metadata.json")?
    } else {
        // If no metadata, return empty - this task might be incomplete
        return Ok(Vec::new());
    };

    // Read ui_messages.json
    let ui_messages_path = task_dir.join("ui_messages.json");
    if !ui_messages_path.exists() {
        return Ok(Vec::new());
    }

    let mut ui_messages_content = std::fs::read_to_string(&ui_messages_path)?.into_bytes();
    let ui_messages: Vec<ClineUiMessage> = simd_json::from_slice(&mut ui_messages_content)
        .context("Failed to parse ui_messages.json")?;

    let mut entries = Vec::new();
    let mut fallback_session_name: Option<String> = None;

    // Process ui_messages to extract API requests with token/cost data
    for message in ui_messages {
        match message {
            ClineUiMessage::Say {
                ts,
                say,
                text,
                conversation_history_index,
                ..
            } => {
                // We're interested in "api_req_started" messages which contain token/cost data
                if say == "api_req_started" && !text.is_empty() {
                    // Parse the embedded JSON in the text field
                    let mut text_bytes = text.into_bytes();
                    if let Ok(api_req) = simd_json::from_slice::<ClineApiRequest>(&mut text_bytes) {
                        // Skip cancelled requests
                        if api_req.cancel_reason.is_some() {
                            continue;
                        }

                        // Determine the model from metadata based on timestamp
                        let model = metadata
                            .model_usage
                            .iter()
                            .filter(|m| m.ts <= ts)
                            .last()
                            .map(|m| m.model_id.clone());

                        // Create a message entry for this API request
                        let date = DateTime::from_timestamp_millis(ts).unwrap_or_else(Utc::now);

                        let local_hash =
                            format!("{}-{}", conversation_hash, conversation_history_index);
                        let global_hash = hash_text(&format!(
                            "{}:{}:{}:{}",
                            project_hash, conversation_hash, conversation_history_index, ts
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
                            application: Application::Cline,
                            date,
                            project_hash: project_hash.clone(),
                            conversation_hash: conversation_hash.clone(),
                            local_hash: Some(local_hash),
                            global_hash,
                            model,
                            stats,
                            role: MessageRole::Assistant, // API requests are from the assistant
                            uuid: None,
                            session_name: fallback_session_name.clone(),
                        });
                    }
                }
            }
            ClineUiMessage::Ask {
                ts,
                ask,
                text,
                conversation_history_index,
                ..
            } => {
                // Track user interactions (followup questions, confirmations)
                if matches!(ask.as_str(), "followup" | "command" | "tool") {
                    let date = DateTime::from_timestamp_millis(ts).unwrap_or_else(Utc::now);

                    let local_hash =
                        format!("{}-user-{}", conversation_hash, conversation_history_index);
                    let global_hash = hash_text(&format!(
                        "{}:{}:user:{}:{}",
                        project_hash, conversation_hash, conversation_history_index, ts
                    ));

                    if fallback_session_name.is_none() && !text.is_empty() {
                        let text_str = text;
                        if !is_probably_tool_json_text(&text_str) {
                            let truncated = if text_str.chars().count() > 50 {
                                let chars: String = text_str.chars().take(50).collect();
                                format!("{}...", chars)
                            } else {
                                text_str
                            };
                            fallback_session_name = Some(truncated);
                        }
                    }

                    entries.push(ConversationMessage {
                        application: Application::Cline,
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
                }
            }
        }
    }

    Ok(entries)
}

#[async_trait]
impl Analyzer for ClineAnalyzer {
    fn display_name(&self) -> &'static str {
        "Cline"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        // VSCode forks that might have Cline installed: Code, Cursor, Windsurf, VSCodium, Positron
        let vscode_forks = ["Code", "Cursor", "Windsurf", "VSCodium", "Positron"];

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();

            // Linux paths for all VSCode forks
            for fork in &vscode_forks {
                patterns.push(format!("{home_str}/.config/{fork}/User/globalStorage/saoudrizwan.claude-dev/tasks/*/ui_messages.json"));
            }

            // macOS paths for all VSCode forks
            for fork in &vscode_forks {
                patterns.push(format!("{home_str}/Library/Application Support/{fork}/User/globalStorage/saoudrizwan.claude-dev/tasks/*/ui_messages.json"));
            }
        }

        // Windows paths for all VSCode forks
        if let Ok(appdata) = std::env::var("APPDATA") {
            for fork in &vscode_forks {
                patterns.push(format!("{appdata}\\{fork}\\User\\globalStorage\\saoudrizwan.claude-dev\\tasks\\*\\ui_messages.json"));
            }
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        discover_vscode_extension_sources(CLINE_EXTENSION_ID, "ui_messages.json", true)
    }

    async fn parse_conversations(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<Vec<ConversationMessage>> {
        // Parse all task directories in parallel
        let all_entries: Vec<ConversationMessage> = sources
            .into_par_iter()
            .flat_map(|source| match parse_cline_task_directory(&source.path) {
                Ok(messages) => messages,
                Err(e) => {
                    eprintln!(
                        "Failed to parse Cline task directory {:?}: {}",
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
            "/home/user/.config/Code/User/globalStorage/saoudrizwan.claude-dev/tasks/1762355683581/ui_messages.json",
        );
        let parent = path.parent().unwrap();
        let hash = extract_and_hash_project_id_cline(parent);
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 hex length
    }
}
