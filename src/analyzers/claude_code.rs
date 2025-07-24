use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::analyzer::{Analyzer, DataSource};
use crate::models::MODEL_PRICING;
use crate::types::{
    AgenticCodingToolStats, Application, CompositionStats, ConversationMessage, FileCategory,
    FileOperationStats, GeneralStats, TodoStats,
};
use crate::upload::{estimate_lines_added, estimate_lines_deleted};
use crate::utils::ModelAbbreviations;

pub struct ClaudeCodeAnalyzer;

impl ClaudeCodeAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Analyzer for ClaudeCodeAnalyzer {
    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn get_model_abbreviations(&self) -> ModelAbbreviations {
        let mut abbrs = ModelAbbreviations::new();
        abbrs.add(
            "claude-sonnet-4-20250514".to_string(),
            "CS4".to_string(),
            "Claude Sonnet 4".to_string(),
        );
        abbrs.add(
            "claude-opus-4-20250514".to_string(),
            "CO4".to_string(),
            "Claude Opus 4".to_string(),
        );
        abbrs
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = std::env::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{home_str}/.claude/projects/*/*.jsonl"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let patterns = self.get_data_glob_patterns();
        let mut sources = Vec::new();

        for pattern in patterns {
            for entry in glob::glob(&pattern)? {
                let path = entry?;
                if path.is_file() {
                    sources.push(DataSource { path });
                }
            }
        }

        Ok(sources)
    }

    async fn parse_conversations(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<Vec<ConversationMessage>> {
        use rayon::iter::{IntoParallelIterator, ParallelIterator};

        // Parse all the files in parallel
        let all_entries: Vec<ConversationMessage> = sources
            .into_par_iter()
            .flat_map(|source| parse_jsonl_file(&source.path))
            .collect();

        // Deduplicate messages
        let mut seen_hashes = HashSet::new();
        let deduplicated_entries: Vec<ConversationMessage> = all_entries
            .into_iter()
            .filter(|entry| {
                if let ConversationMessage::AI {
                    hash: Some(hash), ..
                } = &entry
                {
                    if seen_hashes.contains(hash) {
                        false
                    } else {
                        seen_hashes.insert(hash.clone());
                        true
                    }
                } else {
                    true // Keep user messages and entries without hashes
                }
            })
            .collect();

        Ok(deduplicated_entries)
    }

    async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
        let sources = self.discover_data_sources()?;
        let messages = self.parse_conversations(sources).await?;
        let mut daily_stats = crate::utils::aggregate_by_date(&messages);

        // Remove any remaining "unknown" entries from daily_stats
        daily_stats.retain(|date, _| date != "unknown");

        let num_conversations = daily_stats
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        Ok(AgenticCodingToolStats {
            daily_stats,
            num_conversations,
            model_abbrs: self.get_model_abbreviations(),
            messages,
            analyzer_name: self.display_name().to_string(),
        })
    }

    fn is_available(&self) -> bool {
        self.discover_data_sources()
            .is_ok_and(|sources| !sources.is_empty())
    }
}

// Claude Code specific implementation functions

// Helper function to generate hash from conversation file path and timestamp
fn generate_conversation_hash(conversation_file: &str, timestamp: &str) -> String {
    let input = format!("{conversation_file}:{timestamp}");
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..8]) // Use first 8 bytes (16 hex chars) for consistency
}

// Helper function to extract project ID from Claude Code file path and hash it
fn extract_and_hash_project_id(file_path: &Path) -> String {
    // Claude Code path format: ~/.claude/projects/{PROJECT_ID}/{conversation_file}.jsonl
    // Example: "C:\Users\user\.claude\projects\D--splitrail-leaderboard\4d1b8bda-6d8c-4ee4-b480-a606953bc9c2.jsonl"
    
    if let Some(parent) = file_path.parent() {
        if let Some(project_id) = parent.file_name().and_then(|name| name.to_str()) {
            // Hash the project ID using the same algorithm as the rest of the app
            let mut hasher = Sha256::new();
            hasher.update(project_id.as_bytes());
            let result = hasher.finalize();
            return hex::encode(&result[..8]); // Use first 8 bytes (16 hex chars) for consistency
        }
    }
    
    // Fallback: hash the full file path if we can't extract project ID
    let mut hasher = Sha256::new();
    hasher.update(file_path.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..8])
}

// CLAUDE CODE JSONL FILES SCHEMA

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default, rename = "cache_creation_input_tokens")]
    cache_creation_tokens: u64,
    #[serde(default, rename = "cache_read_input_tokens")]
    cache_read_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    content: Option<Content>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContentBlock {
    r#type: String,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum Content {
    String(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolUseResult {
    #[serde(rename = "oldTodos")]
    old_todos: Option<serde_json::Value>,
    #[serde(rename = "newTodos")]
    new_todos: Option<serde_json::Value>,
    r#type: Option<String>,
    file: Option<FileInfo>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileInfo {
    #[serde(rename = "filePath")]
    file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClaudeCodeEntry {
    message: Option<Message>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    timestamp: Option<String>,
    r#type: Option<String>,
    #[serde(rename = "toolUseResult")]
    tool_use_result: Option<ToolUseResult>,
    #[serde(flatten)]
    extra_fields: HashMap<String, serde_json::Value>,
}

fn hash_cc_entry(data: &ClaudeCodeEntry) -> Option<String> {
    let message_id = data.message.as_ref().and_then(|m| m.id.clone());
    let request_id = data.request_id.clone();
    match (message_id, request_id) {
        (Some(msg_id), Some(req_id)) => Some(format!("{msg_id}:{req_id}")),
        _ => None,
    }
}

fn is_synthetic_entry(data: &ClaudeCodeEntry) -> bool {
    // Check if this is a synthetic message generated by Claude Code itself
    if let Some(message) = &data.message {
        // Check if model is synthetic
        if let Some(model) = &message.model {
            if model == "<synthetic>" || model.is_empty() {
                return true;
            }
        } else {
            // No model specified could indicate synthetic content
            return true;
        }

        // Check if content contains synthetic markers
        if let Some(Content::String(content_str)) = &message.content
            && content_str.contains("<synthetic>")
        {
            return true;
        }
    }

    false
}

fn extract_tool_stats(data: &ClaudeCodeEntry) -> (FileOperationStats, Option<TodoStats>) {
    let mut file_ops = FileOperationStats::default();
    let mut todo_stats = TodoStats::default();
    let mut has_todo_activity = false;

    if let Some(message) = &data.message
        && let Some(Content::Blocks(blocks)) = &message.content
    {
        for block in blocks {
            if block.r#type == "tool_use"
                && let Some(tool_name) = &block.name
            {
                match tool_name.as_str() {
                    "Read" => {
                        file_ops.files_read += 1;
                        if let Some(input) = &block.input {
                            if let Some(file_path) = input.get("file_path").and_then(|v| v.as_str())
                            {
                                let ext = Path::new(file_path)
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .unwrap_or("");
                                let category = FileCategory::from_extension(ext);
                                *file_ops
                                    .file_types
                                    .entry(category.as_str().to_string())
                                    .or_insert(0) += 1;
                            }
                            let lines_read =
                                input.get("limit").and_then(|v| v.as_u64()).unwrap_or(100);
                            file_ops.lines_read += lines_read;
                            file_ops.bytes_read += lines_read * 80;
                        }
                    }
                    "Edit" | "MultiEdit" => {
                        file_ops.files_edited += 1;
                        if let Some(input) = &block.input {
                            if let Some(file_path) = input.get("file_path").and_then(|v| v.as_str())
                            {
                                let ext = Path::new(file_path)
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .unwrap_or("");
                                let category = FileCategory::from_extension(ext);
                                *file_ops
                                    .file_types
                                    .entry(category.as_str().to_string())
                                    .or_insert(0) += 1;
                            }
                            let lines_edited = if tool_name == "MultiEdit" {
                                input
                                    .get("edits")
                                    .and_then(|v| v.as_array())
                                    .map(|edits| edits.len() as u64 * 5)
                                    .unwrap_or(10)
                            } else {
                                input
                                    .get("new_string")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.lines().count() as u64)
                                    .unwrap_or(5)
                            };
                            file_ops.lines_edited += lines_edited;
                            file_ops.bytes_edited += lines_edited * 80;
                        }
                    }
                    "Write" => {
                        file_ops.files_edited += 1;
                        if let Some(input) = &block.input {
                            if let Some(file_path) = input.get("file_path").and_then(|v| v.as_str())
                            {
                                let ext = Path::new(file_path)
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .unwrap_or("");
                                let category = FileCategory::from_extension(ext);
                                *file_ops
                                    .file_types
                                    .entry(category.as_str().to_string())
                                    .or_insert(0) += 1;
                            }
                            let lines_written = input
                                .get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.lines().count() as u64)
                                .unwrap_or(50);
                            file_ops.lines_added += lines_written;
                            file_ops.bytes_added += lines_written * 80;
                        }
                    }
                    "Bash" => file_ops.terminal_commands += 1,
                    "Glob" => file_ops.file_searches += 1,
                    "Grep" => file_ops.file_content_searches += 1,
                    "TodoWrite" => {
                        todo_stats.todo_writes += 1;
                        has_todo_activity = true;
                    }
                    "TodoRead" => {
                        todo_stats.todo_reads += 1;
                        has_todo_activity = true;
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(tool_result) = &data.tool_use_result
        && let (Some(old_todos), Some(new_todos)) = (&tool_result.old_todos, &tool_result.new_todos)
        && let (Ok(old_array), Ok(new_array)) = (
            serde_json::from_value::<Vec<serde_json::Value>>(old_todos.clone()),
            serde_json::from_value::<Vec<serde_json::Value>>(new_todos.clone()),
        )
    {
        if new_array.len() > old_array.len() {
            let created = (new_array.len() - old_array.len()) as u64;
            todo_stats.todos_created += created;
            has_todo_activity = true;
        }

        let old_completed = old_array
            .iter()
            .filter(|todo| todo.get("status").and_then(|s| s.as_str()) == Some("completed"))
            .count();
        let new_completed = new_array
            .iter()
            .filter(|todo| todo.get("status").and_then(|s| s.as_str()) == Some("completed"))
            .count();

        if new_completed > old_completed {
            let completed = (new_completed - old_completed) as u64;
            todo_stats.todos_completed += completed;
            has_todo_activity = true;
        }

        let old_in_progress = old_array
            .iter()
            .filter(|todo| todo.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
            .count();
        let new_in_progress = new_array
            .iter()
            .filter(|todo| todo.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
            .count();

        if new_in_progress > old_in_progress {
            let in_progress = (new_in_progress - old_in_progress) as u64;
            todo_stats.todos_in_progress += in_progress;
            has_todo_activity = true;
        }
    }

    file_ops.lines_added = estimate_lines_added(&file_ops);
    file_ops.lines_deleted = estimate_lines_deleted(&file_ops);

    (
        file_ops,
        if has_todo_activity {
            Some(todo_stats)
        } else {
            None
        },
    )
}

fn calculate_cost_from_tokens(usage: &Usage, model_name: &str) -> f64 {
    match MODEL_PRICING.get(model_name) {
        Some(pricing) => {
            usage.input_tokens as f64 * pricing.input_cost_per_token
                + usage.output_tokens as f64 * pricing.output_cost_per_token
                + usage.cache_creation_tokens as f64 * pricing.cache_creation_input_token_cost
                + usage.cache_read_tokens as f64 * pricing.cache_read_input_token_cost
        }
        None => {
            println!("WARNING: Unknown model name: {model_name}. Ignoring this model's usage.",);
            0.0
        }
    }
}

fn parse_jsonl_file(file_path: &Path) -> Vec<ConversationMessage> {
    let conversation_file = file_path.to_string_lossy().to_string();
    let project_hash = extract_and_hash_project_id(file_path);
    let mut entries = Vec::new();
    let mut has_non_summary_messages = false;
    let mut temp_messages = Vec::new();

    let file = match File::open(file_path) {
        Ok(f) => f,
        Err(_) => return entries,
    };

    let reader = BufReader::with_capacity(64 * 1024, file);

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.trim().is_empty() {
            continue;
        }

        let data = match serde_json::from_str::<ClaudeCodeEntry>(&line) {
            Ok(data) => data,
            Err(_) => {
                if let Ok(basic_json) = serde_json::from_str::<serde_json::Value>(&line) {
                    let entry_type = basic_json
                        .get("type")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let timestamp = basic_json
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let request_id = basic_json
                        .get("requestId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    ClaudeCodeEntry {
                        message: None,
                        request_id,
                        cost_usd: None,
                        timestamp,
                        r#type: entry_type,
                        tool_use_result: None,
                        extra_fields: HashMap::new(),
                    }
                } else {
                    continue;
                }
            }
        };

        // Skip synthetic messages entirely
        if is_synthetic_entry(&data) {
            continue;
        }

        // Track if we find any non-summary messages
        if data.r#type.as_deref() != Some("summary") {
            has_non_summary_messages = true;
        }

        let _hash = hash_cc_entry(&data);
        let (file_ops, todo_stats) = extract_tool_stats(&data);

        if data.message.is_none() {
            let timestamp = data.timestamp.clone().unwrap_or_else(|| "".to_string());
            temp_messages.push(ConversationMessage::User {
                timestamp: timestamp.clone(),
                application: Application::ClaudeCode,
                hash: Some(generate_conversation_hash(&conversation_file, &timestamp)),
                project_hash: project_hash.clone(),
                todo_stats,
                analyzer_specific: HashMap::new(),
            });
            continue;
        }

        let message = data.message.unwrap();
        let model_name = message.model.unwrap_or_else(|| "unknown".to_string());
        let file_types = file_ops.file_types.clone();

        let timestamp = data.timestamp.clone().unwrap_or_else(|| "".to_string());
        match message.usage {
            Some(usage) => {
                temp_messages.push(ConversationMessage::AI {
                    application: Application::ClaudeCode,
                    general_stats: GeneralStats {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_creation_tokens: usage.cache_creation_tokens,
                        cache_read_tokens: usage.cache_read_tokens,
                        cached_tokens: 0,
                        cost: match data.cost_usd {
                            Some(precalc_cost) => precalc_cost,
                            None => calculate_cost_from_tokens(&usage, &model_name),
                        },
                        tool_calls: match message.content {
                            Some(Content::Blocks(blocks)) => {
                                blocks.iter().filter(|c| c.r#type == "tool_use").count() as u32
                            }
                            _ => 0,
                        },
                    },
                    model: model_name,
                    timestamp: timestamp.clone(),
                    hash: Some(generate_conversation_hash(&conversation_file, &timestamp)),
                    project_hash: project_hash.clone(),
                    file_operations: file_ops,
                    todo_stats,
                    analyzer_specific: HashMap::new(),
                    composition_stats: CompositionStats {
                        code_lines: *file_types.get("source_code").unwrap_or(&0),
                        docs_lines: *file_types.get("documentation").unwrap_or(&0),
                        data_lines: *file_types.get("data").unwrap_or(&0),
                        media_lines: *file_types.get("media").unwrap_or(&0),
                        config_lines: *file_types.get("config").unwrap_or(&0),
                        other_lines: *file_types.get("other").unwrap_or(&0),
                    },
                });
            }
            None => {
                let timestamp = data.timestamp.clone().unwrap_or_else(|| "".to_string());
                temp_messages.push(ConversationMessage::User {
                    timestamp: timestamp.clone(),
                    application: Application::ClaudeCode,
                    hash: Some(generate_conversation_hash(&conversation_file, &timestamp)),
                    project_hash: project_hash.clone(),
                    todo_stats,
                    analyzer_specific: HashMap::new(),
                });
            }
        }
    }

    // Skip conversations that only contain summaries or have no valid timestamps
    if !has_non_summary_messages && !temp_messages.is_empty() {
        return entries; // Return empty vector for summary-only conversations
    }

    // Filter out messages with invalid timestamps and only add valid ones
    for message in temp_messages {
        let timestamp = match &message {
            ConversationMessage::AI { timestamp, .. } => timestamp,
            ConversationMessage::User { timestamp, .. } => timestamp,
        };

        // Skip messages with unknown dates
        if crate::utils::extract_date_from_timestamp(timestamp).is_none() {
            continue;
        }

        entries.push(message);
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_date_from_timestamp_valid() {
        let timestamp = "2023-12-01T10:30:00Z";
        let result = crate::utils::extract_date_from_timestamp(timestamp);
        assert!(result.is_some());
        assert!(result.unwrap().starts_with("2023-12-01"));
    }

    #[test]
    fn test_extract_date_from_timestamp_empty() {
        let timestamp = "";
        let result = crate::utils::extract_date_from_timestamp(timestamp);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_date_from_timestamp_invalid() {
        let timestamp = "invalid-timestamp";
        let result = crate::utils::extract_date_from_timestamp(timestamp);
        assert!(result.is_none());
    }

    #[test]
    fn test_summary_only_conversation_filtered() {
        // Create a temporary file with summary-only content
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("test_summary_only.jsonl");

        let jsonl_content = r#"{"type":"summary","summary":"Test Summary","leafUuid":"test-uuid"}"#;
        std::fs::write(&temp_file, jsonl_content).unwrap();

        let messages = parse_jsonl_file(&temp_file);

        // Clean up
        std::fs::remove_file(&temp_file).ok();

        // Should be empty since it only contains summaries
        assert!(messages.is_empty());
    }

    #[test]
    fn test_invalid_timestamp_filtered() {
        // Create a temporary file with invalid timestamp
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("test_invalid_timestamp.jsonl");

        let jsonl_content = r#"{"type":"ai","content":"test","timestamp":"invalid","message":{"model":"test","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        std::fs::write(&temp_file, jsonl_content).unwrap();

        let messages = parse_jsonl_file(&temp_file);

        // Clean up
        std::fs::remove_file(&temp_file).ok();

        // Should be empty since timestamp is invalid
        assert!(messages.is_empty());
    }

    #[test]
    fn test_valid_conversation_preserved() {
        // Create a temporary file with valid content
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("test_valid.jsonl");

        let jsonl_content = r#"{"type":"ai","content":"test","timestamp":"2023-12-01T10:30:00Z","message":{"model":"test","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        std::fs::write(&temp_file, jsonl_content).unwrap();

        let messages = parse_jsonl_file(&temp_file);

        // Clean up
        std::fs::remove_file(&temp_file).ok();

        // Should contain the valid message
        assert_eq!(messages.len(), 1);
    }
}
