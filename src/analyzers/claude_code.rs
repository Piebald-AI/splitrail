use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::analyzer::{
    Analyzer, AnalyzerCapabilities, CachingInfo, CachingType, DataFormat, DataSource,
};
use crate::models::MODEL_PRICING;
use crate::types::{
    AgenticCodingToolStats, CompositionStats, ConversationMessage, FileCategory,
    FileOperationStats, TodoStats,
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
    fn name(&self) -> &'static str {
        "claude_code"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn get_capabilities(&self) -> AnalyzerCapabilities {
        AnalyzerCapabilities {
            supports_todos: true,
            caching_type: Some(CachingType::CreationAndRead),
            supports_file_operations: true,
            supports_cost_tracking: true,
            supports_model_selection: true,
            supported_tools: vec![
                "Read".to_string(),
                "Edit".to_string(),
                "MultiEdit".to_string(),
                "Write".to_string(),
                "Bash".to_string(),
                "Glob".to_string(),
                "Grep".to_string(),
                "TodoWrite".to_string(),
                "TodoRead".to_string(),
            ],
        }
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

    fn get_data_directory_pattern(&self) -> &str {
        "~/.claude/projects/**/*.jsonl"
    }

    async fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let claude_dirs = find_claude_dirs();
        let mut sources = Vec::new();

        for claude_dir in claude_dirs {
            for entry in glob::glob(&format!("{}/**/*.jsonl", claude_dir.display()))? {
                let path = entry?;
                sources.push(DataSource {
                    path,
                    format: DataFormat::JsonL,
                    metadata: std::collections::HashMap::new(),
                });
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
                if let ConversationMessage::AI { hash, .. } = &entry {
                    if let Some(hash) = hash {
                        if seen_hashes.contains(hash) {
                            false
                        } else {
                            seen_hashes.insert(hash.clone());
                            true
                        }
                    } else {
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
        let sources = self.discover_data_sources().await?;
        let messages = self.parse_conversations(sources).await?;
        let daily_stats = crate::utils::aggregate_by_date(&messages);

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
        !find_claude_dirs().is_empty()
    }
}

// Claude Code specific implementation functions
fn find_claude_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // Try the most likely place, the home dir.
    if let Some(home_dir) = home::home_dir() {
        let claude_home_dir = home_dir.join(".claude");
        if claude_home_dir.exists() {
            dirs.push(claude_home_dir.join("projects"));
        }
    }

    // Then see if there could be one in the current directory.
    let current_dir = std::env::current_dir().unwrap();
    let claude_current_dir = current_dir.join(".claude");
    if claude_current_dir.exists() {
        dirs.push(claude_current_dir.join("projects"));
    }

    dirs
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
        (Some(msg_id), Some(req_id)) => Some(format!("{}:{}", msg_id, req_id)),
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
        if let Some(Content::String(content_str)) = &message.content {
            if content_str.contains("<synthetic>") {
                return true;
            }
        }
    }
    
    false
}

fn extract_tool_stats(data: &ClaudeCodeEntry) -> (FileOperationStats, Option<TodoStats>) {
    let mut file_ops = FileOperationStats::default();
    let mut todo_stats = TodoStats::default();
    let mut has_todo_activity = false;

    if let Some(message) = &data.message {
        if let Some(Content::Blocks(blocks)) = &message.content {
            for block in blocks {
                if block.r#type == "tool_use" {
                    if let Some(tool_name) = &block.name {
                        match tool_name.as_str() {
                            "Read" => {
                                file_ops.files_read += 1;
                                if let Some(input) = &block.input {
                                    if let Some(file_path) =
                                        input.get("file_path").and_then(|v| v.as_str())
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
                                    if let Some(file_path) =
                                        input.get("file_path").and_then(|v| v.as_str())
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
                                    if let Some(file_path) =
                                        input.get("file_path").and_then(|v| v.as_str())
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
        }
    }

    if let Some(tool_result) = &data.tool_use_result {
        if let (Some(old_todos), Some(new_todos)) = (&tool_result.old_todos, &tool_result.new_todos)
        {
            if let (Ok(old_array), Ok(new_array)) = (
                serde_json::from_value::<Vec<serde_json::Value>>(old_todos.clone()),
                serde_json::from_value::<Vec<serde_json::Value>>(new_todos.clone()),
            ) {
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
                    .filter(|todo| {
                        todo.get("status").and_then(|s| s.as_str()) == Some("in_progress")
                    })
                    .count();
                let new_in_progress = new_array
                    .iter()
                    .filter(|todo| {
                        todo.get("status").and_then(|s| s.as_str()) == Some("in_progress")
                    })
                    .count();

                if new_in_progress > old_in_progress {
                    let in_progress = (new_in_progress - old_in_progress) as u64;
                    todo_stats.todos_in_progress += in_progress;
                    has_todo_activity = true;
                }
            }
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
            println!(
                "WARNING: Unknown model name: {}. Ignoring this model's usage.",
                model_name
            );
            0.0
        }
    }
}

fn parse_jsonl_file(file_path: &Path) -> Vec<ConversationMessage> {
    let conversation_file = file_path.to_string_lossy().to_string();
    let mut entries = Vec::new();

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

        let hash = hash_cc_entry(&data);
        let (file_ops, todo_stats) = extract_tool_stats(&data);

        if data.message.is_none() {
            entries.push(ConversationMessage::User {
                timestamp: data.timestamp.unwrap_or_else(|| "".to_string()),
                conversation_file: conversation_file.clone(),
                todo_stats,
                analyzer_specific: HashMap::new(),
            });
            continue;
        }

        let message = data.message.unwrap();
        let model_name = message.model.unwrap_or_else(|| "unknown".to_string());
        let file_types = file_ops.file_types.clone();

        match message.usage {
            Some(usage) => {
                entries.push(ConversationMessage::AI {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    caching_info: if usage.cache_creation_tokens > 0 || usage.cache_read_tokens > 0
                    {
                        Some(CachingInfo::CreationAndRead {
                            cache_creation_tokens: usage.cache_creation_tokens,
                            cache_read_tokens: usage.cache_read_tokens,
                        })
                    } else {
                        None
                    },
                    cost: match data.cost_usd {
                        Some(precalc_cost) => precalc_cost,
                        None => calculate_cost_from_tokens(&usage, &model_name),
                    },
                    model: model_name,
                    timestamp: data.timestamp.unwrap_or_else(|| "".to_string()),
                    tool_calls: match message.content {
                        Some(Content::Blocks(blocks)) => {
                            blocks.iter().filter(|c| c.r#type == "tool_use").count() as u32
                        }
                        _ => 0,
                    },
                    hash,
                    conversation_file: conversation_file.clone(),
                    file_operations: file_ops,
                    todo_stats,
                    composition_stats: CompositionStats {
                        code_lines: *file_types.get("source_code").unwrap_or(&0),
                        docs_lines: *file_types.get("documentation").unwrap_or(&0),
                        data_lines: *file_types.get("data").unwrap_or(&0),
                        media_lines: *file_types.get("media").unwrap_or(&0),
                        config_lines: *file_types.get("config").unwrap_or(&0),
                        other_lines: *file_types.get("other").unwrap_or(&0),
                    },
                    analyzer_specific: HashMap::new(),
                });
            }
            None => entries.push(ConversationMessage::User {
                timestamp: data.timestamp.unwrap_or_else(|| "".to_string()),
                conversation_file: conversation_file.clone(),
                todo_stats,
                analyzer_specific: HashMap::new(),
            }),
        }
    }

    entries
}
