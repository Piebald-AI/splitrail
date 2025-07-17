use anyhow::{Context, Result};
use chrono::DateTime;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::analyzer::CachingInfo;
use crate::models::MODEL_PRICING;
use crate::types::{
    AgenticCodingToolStats, ConversationMessage, FileCategory, FileOperationStats, TodoStats,
};
use crate::utils::ModelAbbreviations;

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
    // Handle additional fields that might cause parsing failures
    #[serde(flatten)]
    extra: std::collections::HashMap<String, serde_json::Value>,
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
    // Handle additional fields that might cause parsing failures
    #[serde(flatten)]
    extra_fields: std::collections::HashMap<String, serde_json::Value>,
}

// END OF SCHEMA

/// Finds `.claude` dirs, which would contain the JSONL files for Claude Code conversations.
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

    // Return whatever we could find.
    dirs
}

/// Combines various properties about a Claude Code JSONL entry (message ID & request ID) to form a
/// hash so that we can deduplicate entries later.
fn hash_cc_entry(data: &ClaudeCodeEntry) -> Option<String> {
    let message_id = data.message.as_ref().and_then(|m| m.id.clone());
    let request_id = data.request_id.clone();

    match (message_id, request_id) {
        (Some(msg_id), Some(req_id)) => Some(format!("{}:{}", msg_id, req_id)),
        _ => None,
    }
}

/// Extracts tool usage statistics from a Claude Code JSONL entry with enhanced validation.
fn extract_tool_stats(data: &ClaudeCodeEntry) -> (FileOperationStats, TodoStats) {
    let mut file_ops = FileOperationStats::default();
    let mut todo_stats = TodoStats::default();

    // Check tool usage in message content - this is the primary source
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
                                        let ext = std::path::Path::new(file_path)
                                            .extension()
                                            .and_then(|e| e.to_str())
                                            .unwrap_or("");
                                        let category = FileCategory::from_extension(ext);
                                        *file_ops
                                            .file_types
                                            .entry(category.as_str().to_string())
                                            .or_insert(0) += 1;
                                    }
                                    // Extract line count from limit parameter or default to estimated lines
                                    let lines_read =
                                        input.get("limit").and_then(|v| v.as_u64()).unwrap_or(100); // Default estimate for full file reads
                                    file_ops.lines_read += lines_read;
                                    // Estimate bytes read (average ~80 chars per line)
                                    file_ops.bytes_read += lines_read * 80;
                                }
                            }
                            "Edit" | "MultiEdit" => {
                                file_ops.files_edited += 1;
                                if let Some(input) = &block.input {
                                    if let Some(file_path) =
                                        input.get("file_path").and_then(|v| v.as_str())
                                    {
                                        let ext = std::path::Path::new(file_path)
                                            .extension()
                                            .and_then(|e| e.to_str())
                                            .unwrap_or("");
                                        let category = FileCategory::from_extension(ext);
                                        *file_ops
                                            .file_types
                                            .entry(category.as_str().to_string())
                                            .or_insert(0) += 1;
                                    }
                                    // Estimate lines edited based on edit content
                                    let lines_edited = if tool_name == "MultiEdit" {
                                        input
                                            .get("edits")
                                            .and_then(|v| v.as_array())
                                            .map(|edits| edits.len() as u64 * 5) // Estimate 5 lines per edit
                                            .unwrap_or(10)
                                    } else {
                                        input
                                            .get("new_string")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.lines().count() as u64)
                                            .unwrap_or(5) // Default estimate
                                    };
                                    file_ops.lines_edited += lines_edited;
                                    // Estimate bytes edited (average ~80 chars per line)
                                    file_ops.bytes_edited += lines_edited * 80;
                                }
                            }
                            "Write" => {
                                file_ops.files_written += 1;
                                if let Some(input) = &block.input {
                                    if let Some(file_path) =
                                        input.get("file_path").and_then(|v| v.as_str())
                                    {
                                        let ext = std::path::Path::new(file_path)
                                            .extension()
                                            .and_then(|e| e.to_str())
                                            .unwrap_or("");
                                        let category = FileCategory::from_extension(ext);
                                        *file_ops
                                            .file_types
                                            .entry(category.as_str().to_string())
                                            .or_insert(0) += 1;
                                    }
                                    // Count lines written from content
                                    let lines_written = input
                                        .get("content")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.lines().count() as u64)
                                        .unwrap_or(50); // Default estimate
                                    file_ops.lines_written += lines_written;
                                    // Estimate bytes written (average ~80 chars per line)
                                    file_ops.bytes_written += lines_written * 80;
                                }
                            }
                            "Bash" => file_ops.terminal_commands += 1,
                            "Glob" => file_ops.glob_searches += 1,
                            "Grep" => file_ops.grep_searches += 1,
                            "TodoWrite" => todo_stats.todo_writes += 1,
                            "TodoRead" => todo_stats.todo_reads += 1,
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Check tool use results for todo stats - secondary source for validation
    if let Some(tool_result) = &data.tool_use_result {
        if let (Some(old_todos), Some(new_todos)) = (&tool_result.old_todos, &tool_result.new_todos)
        {
            // Parse todo arrays to count status changes
            if let (Ok(old_array), Ok(new_array)) = (
                serde_json::from_value::<Vec<serde_json::Value>>(old_todos.clone()),
                serde_json::from_value::<Vec<serde_json::Value>>(new_todos.clone()),
            ) {
                // Count todos created (new array is larger)
                if new_array.len() > old_array.len() {
                    let created = (new_array.len() - old_array.len()) as u32;
                    todo_stats.todos_created += created;
                }

                // Count newly completed todos
                let old_completed = old_array
                    .iter()
                    .filter(|todo| todo.get("status").and_then(|s| s.as_str()) == Some("completed"))
                    .count();
                let new_completed = new_array
                    .iter()
                    .filter(|todo| todo.get("status").and_then(|s| s.as_str()) == Some("completed"))
                    .count();

                if new_completed > old_completed {
                    let completed = (new_completed - old_completed) as u32;
                    todo_stats.todos_completed += completed;
                }

                // Count status changes to in_progress
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
                    let in_progress = (new_in_progress - old_in_progress) as u32;
                    todo_stats.todos_in_progress += in_progress;
                }
            } else {
                // Failed to parse todo arrays
            }
        }
    }

    (file_ops, todo_stats)
}

/// Calculates the total cost for the given token usage information, using the pricing information
/// for the given model.
fn calculate_cost_from_tokens(usage: &Usage, model_name: &str) -> f64 {
    match MODEL_PRICING.get(model_name) {
        Some(pricing) => {
            usage.input_tokens as f64 * pricing.input_cost_per_token
                + usage.output_tokens as f64 * pricing.output_cost_per_token
                + usage.cache_creation_tokens as f64 * pricing.cache_creation_input_token_cost
                + usage.cache_read_tokens as f64 * pricing.cache_read_input_token_cost
        }
        None if model_name == "<synthetic>" => 0.0,
        None => {
            println!(
                "WARNING: Unknown model name: {}.  Ignoring this model's usage.",
                model_name
            );
            0.0
        }
    }
}

/// Loads up the entries from a JSONL file.
fn parse_jsonl_file(file_path: &Path) -> Vec<ConversationMessage> {
    let conversation_file = file_path.to_string_lossy().to_string();
    let mut entries = Vec::new();
    let mut entry_types_seen = std::collections::HashSet::new();

    let file = match File::open(file_path) {
        Ok(f) => f,
        Err(_) => return entries,
    };

    let reader = BufReader::with_capacity(64 * 1024, file); // 64KB buffer.

    // Loop over the lines.  Each line is a separate JSON object, and it represents a
    // message in the conversation, either from the user or AI.  (The file is one conversation.)
    let mut _line_no = 0;
    for line_result in reader.lines() {
        _line_no += 1;
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue, // TODO: We should log this and tell the user to report an issue.
        };

        // Skip empty lines -- although I don't think we should have any except for maybe one at
        // the end.
        if line.trim().is_empty() {
            continue;
        }

        // Parse the JSONL line, then get the message field and the nested usage field, either of
        // which could be optional. Try multiple parsing strategies for robustness.
        let data = match serde_json::from_str::<ClaudeCodeEntry>(&line) {
            Ok(data) => data,
            Err(_e) => {
                // Try to extract basic info even from malformed entries
                if let Ok(basic_json) = serde_json::from_str::<serde_json::Value>(&line) {
                    // Try to salvage what we can from the entry
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

                    // Create a minimal entry to avoid losing the data entirely
                    ClaudeCodeEntry {
                        message: None,
                        request_id,
                        cost_usd: None,
                        timestamp,
                        r#type: entry_type,
                        tool_use_result: None,
                        extra_fields: std::collections::HashMap::new(),
                    }
                } else {
                    // Skip malformed entries that can't be salvaged
                    continue;
                }
            }
        };
        let hash = hash_cc_entry(&data);

        // Track all entry types we encounter
        if let Some(ref entry_type) = data.r#type {
            entry_types_seen.insert(entry_type.clone());
        }

        // Extract tool usage statistics from this entry
        let (file_ops, todo_stats) = extract_tool_stats(&data);

        // Process user messages that might only have tool use results
        if data.message.is_none() {
            // This might be a user message with tool use results
            entries.push(ConversationMessage::User {
                timestamp: data.timestamp.unwrap_or_else(|| "".to_string()),
                conversation_file: conversation_file.clone(),
                todo_stats,
                analyzer_specific: std::collections::HashMap::new(),
            });
            continue;
        }

        let message = data.message.unwrap();

        // If the JSON has the actual cost, use it.  Otherwise just calculate it.
        let model_name = message.model.unwrap_or_else(|| "unknown".to_string());

        match message.usage {
            // AI message.
            Some(usage) => {
                entries.push(ConversationMessage::AI {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    
                    // Legacy fields for backward compatibility
                    cache_creation_tokens: usage.cache_creation_tokens,
                    cache_read_tokens: usage.cache_read_tokens,
                    
                    // New flexible caching structure
                    caching_info: if usage.cache_creation_tokens > 0 || usage.cache_read_tokens > 0 {
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
                    analyzer_specific: std::collections::HashMap::new(),
                });
            }
            // User message.
            None => entries.push(ConversationMessage::User {
                timestamp: data.timestamp.unwrap_or_else(|| "".to_string()),
                conversation_file: conversation_file.clone(),
                todo_stats,
                analyzer_specific: std::collections::HashMap::new(),
            }),
        }
    }

    // Entry types tracking completed

    entries
}

pub async fn get_claude_code_data() -> Result<(Vec<ConversationMessage>, u64)> {
    // Find all `.claude` dirs.  Usually, if not always, this will just be the one in ~.
    let claude_dirs = find_claude_dirs();
    if claude_dirs.is_empty() {
        println!("No `.claude` data directories found.");
        return Ok((Vec::new(), 0));
    }

    // Get all the JSONL files in the .claude directory, recursively.
    // TODO: We should be more specific not do recursive (the `**`).
    let mut all_jsonl_files: Vec<PathBuf> = Vec::new();
    for claude_dir in claude_dirs {
        for entry in glob::glob(&format!("{}/**/*.jsonl", claude_dir.display()))? {
            let path = entry?;
            all_jsonl_files.push(path);
        }
    }

    let num_conversations = all_jsonl_files.len() as u64;

    // Parse all the files in parallel.
    let all_entries: Vec<ConversationMessage> = all_jsonl_files
        .into_par_iter()
        .flat_map(|path| parse_jsonl_file(&path))
        .collect();

    // Deduplicate messages.  (After parallel processing for determinism.)
    let mut seen_hashes = HashSet::new();
    let mut duplicates_removed = 0;
    let _total_entries = all_entries.len();

    let deduplicated_entries: Vec<ConversationMessage> = all_entries
        .into_iter()
        .filter(|entry| {
            if let ConversationMessage::AI { hash, .. } = &entry
                && let Some(hash) = hash
            {
                if seen_hashes.contains(hash) {
                    duplicates_removed += 1;
                    false
                } else {
                    seen_hashes.insert(hash.clone());
                    true
                }
            } else {
                true // Keep user messages and entries without hashes.
            }
        })
        .collect();

    // Deduplication completed

    Ok((deduplicated_entries, num_conversations))
}

pub fn model_abbrs() -> ModelAbbreviations {
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
    abbrs.add(
        "synthetic".to_string(),
        "?".to_string(),
        "Not specified — denoted by <synthetic> internally in Claude Code".to_string(),
    );
    abbrs
}

pub async fn get_messages_later_than(
    date: i64,
    messages: Vec<ConversationMessage>,
) -> Result<Vec<ConversationMessage>> {
    let mut messages_later_than_date = Vec::new();
    for msg in messages {
        let timestamp = match &msg {
            ConversationMessage::AI { timestamp, .. } => timestamp,
            ConversationMessage::User { timestamp, .. } => timestamp,
        };
        if let Ok(timestamp) = DateTime::parse_from_rfc3339(timestamp)
            .with_context(|| format!("Failed to parse timestamp: {}", timestamp))
        {
            if timestamp.timestamp_millis() >= date {
                messages_later_than_date.push(msg);
            }
        }
    }

    Ok(messages_later_than_date)
}

pub async fn get_claude_code_stats() -> Result<AgenticCodingToolStats> {
    let (all_msgs, _) = get_claude_code_data().await?;
    let daily_stats = crate::utils::aggregate_by_date(&all_msgs);

    let num_conversations = daily_stats
        .values()
        .map(|stats| stats.conversations as u64)
        .sum();

    Ok(AgenticCodingToolStats {
        daily_stats,
        num_conversations,
        model_abbrs: model_abbrs(),
        messages: all_msgs,
        analyzer_name: "Claude Code".to_string(),
    })
}
