use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::analyzer::{Analyzer, DataSource};
use crate::models::calculate_total_cost;
use crate::types::{AgenticCodingToolStats, Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use std::collections::{HashMap, HashSet};

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
            .flat_map(|source| {
                let file = match File::open(&source.path) {
                    Ok(file) => file,
                    Err(e) => {
                        eprintln!("Failed to open file: {e}");
                        return Vec::new();
                    }
                };
                let mut reader = BufReader::new(file);
                parse_jsonl_file(&source.path, &mut reader)
            })
            .collect();

        Ok(deduplicate_messages_by_local_hash(all_entries))
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

// Helper function to extract project ID from Claude Code file path and hash it
pub fn extract_and_hash_project_id(file_path: &Path) -> String {
    // Claude Code path format: ~/.claude/projects/{PROJECT_ID}/{conversation_uuid}.jsonl

    if let Some(parent) = file_path.parent()
        && let Some(project_id) = parent.file_name().and_then(|name| name.to_str())
    {
        return hash_text(project_id);
    }

    // Fallback: hash the full file path if we can't extract project ID
    hash_text(&file_path.to_string_lossy())
}

// CLAUDE CODE JSONL FILES SCHEMA

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")] // Inconsistently, this data is snake_case in the JSONL files.
pub enum ContentBlock {
    ToolUse {
        id: String,
        name: String,
        input: simd_json::OwnedValue, // e.g. "toolu_01K7hbuwktKtti8mQb1wH2q8"
    },
    ToolResult {
        tool_use_id: String, // e.g. "toolu_01K7hbuwktKtti8mQb1wH2q8"
        content: Content,    // e.g. "Found 4 files\nC:\\..."
    },
    Text {
        text: serde_bytes::ByteBuf,
    },
    Thinking {
        thinking: serde_bytes::ByteBuf,
        signature: String,
    },
    Image {
        source: ImageSource,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    String(serde_bytes::ByteBuf),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// This does NOT get renamed to camelCase, inconsistently enough.
struct Message {
    id: Option<String>,
    r#type: Option<String>, // "message"
    role: Option<String>,   // "assistant" or "user"
    model: Option<String>,  // e.g. "claude-sonnet-4-20250514"
    content: Option<Content>,
    stop_reason: Option<String>,
    stop_sequence: Option<String>,
    usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCodeSummaryEntry {
    summary: String,
    leaf_uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCodeFileHistorySnapshotEntry {
    #[serde(flatten)]
    fields: simd_json::OwnedValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCodeMessageEntry {
    r#type: Option<String>,      // "assistant" or "user"
    parent_uuid: Option<String>, // e.g. "773f9fdc-51ed-41cc-b107-19e5418bcf13"
    is_sidechain: Option<bool>,
    user_type: Option<String>,  // e.g. "external"
    cwd: Option<String>,        // e.g. "C:\test"
    session_id: Option<String>, // e.g. "92a07d6b-b12d-40d7-b184-aa04762ba0d6"
    version: Option<String>,    // e.g. "1.0.61"
    message: Option<Message>,
    tool_use_result: Option<simd_json::OwnedValue>, // For user messages only.
    request_id: Option<String>,                     // e.g. "req_0191C3ttfWOg3zRCDNdSFGv3"
    uuid: String,                                   // e.g. "a6ae4765-8274-4d00-8433-4fb28f4b387b"
    timestamp: DateTime<Utc>,                       // e.g. "2025-07-12T22:12:00.572Z"
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ClaudeCodeEntry {
    #[serde(alias = "summary")]
    Summary(ClaudeCodeSummaryEntry),
    #[serde(rename = "file-history-snapshot")]
    FileHistorySnapshot(ClaudeCodeFileHistorySnapshotEntry),
    #[serde(alias = "user", alias = "assistant", alias = "system")]
    Message(ClaudeCodeMessageEntry),
}

pub mod tool_schema {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TodoWriteInputTodo {
        pub id: String,       // e.g. "1"
        pub title: String,    // e.g. "Explore current directory structure"
        pub status: String,   // e.g. "completed"
        pub priority: String, // e.g. "high"
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[allow(dead_code)]
    pub struct TodoWriteInput {
        pub todos: Vec<TodoWriteInputTodo>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[allow(dead_code)]
    pub struct TodoWriteResultTodo {
        pub content: String,
        pub status: String,
        pub priority: String,
        pub id: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct TodoWriteResult {
        pub old_todos: Vec<TodoWriteInputTodo>,
        pub new_todos: Vec<TodoWriteInputTodo>,
    }
}

pub fn extract_tool_stats(
    message_content: &Content,
    tool_use_result: &Option<simd_json::OwnedValue>,
) -> Stats {
    let mut stats = Stats::default();

    if let Content::Blocks(blocks) = message_content {
        for block in blocks {
            let tool_name = match block {
                ContentBlock::ToolUse { name, .. } => name,
                _ => continue,
            };

            match tool_name.as_str() {
                "Read" => stats.files_read += 1,
                "Edit" | "MultiEdit" => stats.files_edited += 1,
                "Write" => stats.files_added += 1,
                "Bash" => stats.terminal_commands += 1,
                "Glob" => stats.file_searches += 1,
                "Grep" => stats.file_content_searches += 1,
                "TodoWrite" => stats.todo_writes += 1,
                "TodoRead" => stats.todo_reads += 1,
                _ => {}
            }
        }
    }

    if let Some(tool_result) = &tool_use_result
        && let Ok(todo_write_result) =
            simd_json::serde::from_owned_value::<tool_schema::TodoWriteResult>(tool_result.clone())
    {
        let old_todos = todo_write_result.old_todos;
        let new_todos = todo_write_result.new_todos;

        if new_todos.len() > old_todos.len() {
            let created = (new_todos.len() - old_todos.len()) as u64;
            stats.todos_created += created;
        }

        let old_completed = old_todos
            .iter()
            .filter(|todo| todo.status == "completed")
            .count();
        let new_completed = new_todos
            .iter()
            .filter(|todo| todo.status == "completed")
            .count();

        if new_completed > old_completed {
            let completed = (new_completed - old_completed) as u64;
            stats.todos_completed += completed;
        }

        let old_in_progress = old_todos
            .iter()
            .filter(|todo| todo.status == "in_progress")
            .count();
        let new_in_progress = new_todos
            .iter()
            .filter(|todo| todo.status == "in_progress")
            .count();

        if new_in_progress > old_in_progress {
            let in_progress = (new_in_progress - old_in_progress) as u64;
            stats.todos_in_progress += in_progress;
        }
    }

    stats
}

pub fn calculate_cost_from_tokens(usage: &Usage, model_name: &str) -> f64 {
    calculate_total_cost(
        model_name,
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
    )
}

pub fn parse_jsonl_file<T>(
    file_path: &Path,
    buffer_reader: &mut BufReader<T>,
) -> Vec<ConversationMessage>
where
    T: Read,
{
    // We do this instead of using the `sessionId` property on the message objects because
    // forked conversations keep the `sessionId` from the parent.
    let session_id = file_path.file_stem().unwrap().to_str().unwrap();

    let project_hash = extract_and_hash_project_id(file_path);
    let mut entries = Vec::new();
    let file_path_str = file_path.to_string_lossy();

    for (i, line_result) in buffer_reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };

        // Parse the line, filtering various invalid scenarios.
        let parsed_line = simd_json::from_slice::<ClaudeCodeEntry>(&mut line.clone().into_bytes());
        let (message_id, model, content, usage, timestamp, request_id, tool_use_result, uuid) =
            match parsed_line {
                // Only get real user/AI messages; filter out summaries and any garbage message entries.
                Ok(ClaudeCodeEntry::Message(ClaudeCodeMessageEntry {
                                                message:
                                                Some(Message {
                                                         id: message_id,
                                                         model,
                                                         content: Some(content),
                                                         usage,
                                                         ..
                                                     }),
                                                timestamp,
                                                tool_use_result,
                                                request_id,
                                                uuid,
                                                ..
                                                // Skip messages for which no model is specified, or the model is `<synthetic>`;
                                                // i.e. Claude Code-generated system messages.  These have their token usage all
                                                // 0 anyway.
                                            })) if !matches!(model.as_deref(), Some("<synthetic>")) => {
                    (
                        message_id,
                        model,
                        content,
                        usage,
                        timestamp,
                        request_id,
                        tool_use_result,
                        uuid,
                    )
                }
                Err(e) => {
                    crate::utils::warn_once(format!(
                        "Skipping invalid entry in {} line {}: {}",
                        file_path.display(),
                        i + 1,
                        e
                    ));
                    continue;
                }
                _ => continue,
            };

        let mut msg = ConversationMessage {
            global_hash: hash_text(&format!("{session_id}_{uuid}")),
            local_hash: None,
            application: Application::ClaudeCode,
            model: model.clone(),
            date: timestamp,
            project_hash: project_hash.clone(),
            conversation_hash: hash_text(&file_path_str),
            stats: extract_tool_stats(&content, &tool_use_result),
            // Default to AI.
            role: MessageRole::Assistant,
        };
        match usage {
            Some(usage) => {
                let model = match &model {
                    Some(m) => m.to_owned(),
                    None => continue, // Invalid entry.
                };

                msg.stats.input_tokens = usage.input_tokens;
                msg.stats.output_tokens = usage.output_tokens;
                msg.stats.cache_creation_tokens = usage.cache_creation_input_tokens;
                msg.stats.cache_read_tokens = usage.cache_read_input_tokens;
                msg.stats.cached_tokens =
                    usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
                msg.stats.cost = calculate_cost_from_tokens(&usage, &model);
                msg.stats.tool_calls = match content {
                    Content::Blocks(blocks) => blocks
                        .iter()
                        .filter(|c| matches!(c, ContentBlock::ToolUse { .. }))
                        .count() as u32,
                    _ => 0,
                };

                if let Some(request_id) = request_id
                    && let Some(message_id) = message_id
                {
                    // Claude Code stores the different parts of a message--thinking, tool calls, and normal
                    // text--each in their own row in the JSONL file, with the same request ID and, redundantly,
                    // the same token usage for each; therefore, we skip ones we've seen before to avoid
                    // accumulating the redundant token counts.  It would be simpler to just skip
                    // them here, but actually some message _across files_ can have the same
                    // message ID and request ID (probably due to resuming sessions), so we need to
                    // do it later when we have a complete pool of messages.
                    msg.local_hash = Some(hash_text(&format!("{request_id}_{message_id}")));
                }
            }
            None => {
                msg.role = MessageRole::User;
            }
        }
        entries.push(msg);
    }

    entries
}

// Type alias for token fingerprint tracking to avoid type complexity
type TokenFingerprint = (u64, u64, u64, u64, u64);
type TokenFingerprintMap = HashMap<String, HashSet<TokenFingerprint>>;

pub fn deduplicate_messages_by_local_hash(
    messages: Vec<ConversationMessage>,
) -> Vec<ConversationMessage> {
    let mut seen_hashes = HashMap::<String, usize>::new(); // hash -> index in result
    // Track distinct token tuples seen per local_hash to avoid double-counting
    let mut seen_token_fingerprints: TokenFingerprintMap = HashMap::new();
    let mut deduplicated_entries: Vec<ConversationMessage> = Vec::new();

    for message in messages {
        if let Some(local_hash) = &message.local_hash {
            if let Some(&existing_index) = seen_hashes.get(local_hash) {
                // Compute the token tuple fingerprint for this message
                let fp = (
                    message.stats.input_tokens,
                    message.stats.output_tokens,
                    message.stats.cache_creation_tokens,
                    message.stats.cache_read_tokens,
                    message.stats.cached_tokens,
                );

                // Initialize set for this hash if missing
                let set = seen_token_fingerprints
                    .entry(local_hash.clone())
                    .or_default();

                // If we've already seen this exact token tuple for this local_hash,
                // it's a redundant duplicate (old format or repeated identical row).
                if set.contains(&fp) {
                    // Merge non-token stats so we don't lose tool/activity counts
                    let existing_mut = &mut deduplicated_entries[existing_index];
                    existing_mut.stats.tool_calls =
                        existing_mut.stats.tool_calls.max(message.stats.tool_calls);
                    existing_mut.stats.files_read =
                        existing_mut.stats.files_read.max(message.stats.files_read);
                    existing_mut.stats.files_edited = existing_mut
                        .stats
                        .files_edited
                        .max(message.stats.files_edited);
                    existing_mut.stats.files_added = existing_mut
                        .stats
                        .files_added
                        .max(message.stats.files_added);
                    existing_mut.stats.terminal_commands = existing_mut
                        .stats
                        .terminal_commands
                        .max(message.stats.terminal_commands);
                    existing_mut.stats.file_searches = existing_mut
                        .stats
                        .file_searches
                        .max(message.stats.file_searches);
                    existing_mut.stats.file_content_searches = existing_mut
                        .stats
                        .file_content_searches
                        .max(message.stats.file_content_searches);
                    existing_mut.stats.todo_writes = existing_mut
                        .stats
                        .todo_writes
                        .max(message.stats.todo_writes);
                    existing_mut.stats.todo_reads =
                        existing_mut.stats.todo_reads.max(message.stats.todo_reads);
                    existing_mut.stats.todos_created = existing_mut
                        .stats
                        .todos_created
                        .max(message.stats.todos_created);
                    existing_mut.stats.todos_completed = existing_mut
                        .stats
                        .todos_completed
                        .max(message.stats.todos_completed);
                    existing_mut.stats.todos_in_progress = existing_mut
                        .stats
                        .todos_in_progress
                        .max(message.stats.todos_in_progress);
                    continue;
                }

                // Mark this distinct tuple as seen and aggregate its values
                set.insert(fp);

                // This is a split message with different/partial token counts.
                // Aggregate all stats together.
                let existing = &mut deduplicated_entries[existing_index];

                // Aggregate token counts
                existing.stats.input_tokens += message.stats.input_tokens;
                existing.stats.output_tokens += message.stats.output_tokens;
                existing.stats.cache_creation_tokens += message.stats.cache_creation_tokens;
                existing.stats.cache_read_tokens += message.stats.cache_read_tokens;
                existing.stats.cached_tokens += message.stats.cached_tokens;

                // Aggregate tool stats
                existing.stats.tool_calls += message.stats.tool_calls;
                existing.stats.files_read += message.stats.files_read;
                existing.stats.files_edited += message.stats.files_edited;
                existing.stats.files_added += message.stats.files_added;
                existing.stats.terminal_commands += message.stats.terminal_commands;
                existing.stats.file_searches += message.stats.file_searches;
                existing.stats.file_content_searches += message.stats.file_content_searches;
                existing.stats.todo_writes += message.stats.todo_writes;
                existing.stats.todo_reads += message.stats.todo_reads;
                existing.stats.todos_created += message.stats.todos_created;
                existing.stats.todos_completed += message.stats.todos_completed;
                existing.stats.todos_in_progress += message.stats.todos_in_progress;

                // Recalculate cost with the aggregated tokens
                if let Some(model) = &existing.model {
                    existing.stats.cost = calculate_total_cost(
                        model,
                        existing.stats.input_tokens,
                        existing.stats.output_tokens,
                        existing.stats.cache_creation_tokens,
                        existing.stats.cache_read_tokens,
                    );
                }
            } else {
                // First time seeing this hash, add it
                seen_hashes.insert(local_hash.clone(), deduplicated_entries.len());
                // Seed the seen tuple set for this local_hash with this message's tokens
                let fp = (
                    message.stats.input_tokens,
                    message.stats.output_tokens,
                    message.stats.cache_creation_tokens,
                    message.stats.cache_read_tokens,
                    message.stats.cached_tokens,
                );
                seen_token_fingerprints
                    .entry(local_hash.clone())
                    .or_default()
                    .insert(fp);

                deduplicated_entries.push(message);
            }
        } else {
            // No local hash, always keep
            deduplicated_entries.push(message);
        }
    }

    deduplicated_entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MessageRole, Stats};

    #[test]
    fn test_deduplicate_partial_split_messages() {
        // Test the new format (Oct 18+ 2025): Split messages with DIFFERENT partial tokens
        let hash = "test_partial_split".to_string();

        let messages = vec![
            ConversationMessage {
                global_hash: "unique1".to_string(),
                local_hash: Some(hash.clone()),
                application: Application::ClaudeCode,
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                date: chrono::Utc::now(),
                project_hash: "proj1".to_string(),
                conversation_hash: "conv1".to_string(),
                role: MessageRole::Assistant,
                stats: Stats {
                    input_tokens: 10, // Different from others
                    output_tokens: 2, // thinking block
                    tool_calls: 0,
                    ..Default::default()
                },
            },
            ConversationMessage {
                global_hash: "unique2".to_string(),
                local_hash: Some(hash.clone()),
                application: Application::ClaudeCode,
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                date: chrono::Utc::now(),
                project_hash: "proj1".to_string(),
                conversation_hash: "conv1".to_string(),
                role: MessageRole::Assistant,
                stats: Stats {
                    input_tokens: 5,  // Different from others
                    output_tokens: 2, // text block
                    tool_calls: 0,
                    ..Default::default()
                },
            },
            ConversationMessage {
                global_hash: "unique3".to_string(),
                local_hash: Some(hash.clone()),
                application: Application::ClaudeCode,
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                date: chrono::Utc::now(),
                project_hash: "proj1".to_string(),
                conversation_hash: "conv1".to_string(),
                role: MessageRole::Assistant,
                stats: Stats {
                    input_tokens: 0,    // Different from others
                    output_tokens: 447, // tool_use block
                    tool_calls: 1,
                    ..Default::default()
                },
            },
        ];

        let deduplicated = deduplicate_messages_by_local_hash(messages);

        // Should have exactly 1 message (all 3 merged)
        assert_eq!(deduplicated.len(), 1);

        // Output tokens should be summed: 2 + 2 + 447 = 451
        assert_eq!(deduplicated[0].stats.output_tokens, 451);

        // Input tokens should be summed: 10 + 5 + 0 = 15
        assert_eq!(deduplicated[0].stats.input_tokens, 15);

        // Tool calls should be summed too
        assert_eq!(deduplicated[0].stats.tool_calls, 1);
    }

    #[test]
    fn test_deduplicate_redundant_split_messages() {
        // Test the old format (Oct 16-17 2025): Split messages with IDENTICAL redundant tokens
        let hash = "test_redundant_split".to_string();

        let messages = vec![
            ConversationMessage {
                global_hash: "unique1".to_string(),
                local_hash: Some(hash.clone()),
                application: Application::ClaudeCode,
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                date: chrono::Utc::now(),
                project_hash: "proj1".to_string(),
                conversation_hash: "conv1".to_string(),
                role: MessageRole::Assistant,
                stats: Stats {
                    output_tokens: 4, // All blocks report same total
                    input_tokens: 100,
                    tool_calls: 2,
                    ..Default::default()
                },
            },
            ConversationMessage {
                global_hash: "unique2".to_string(),
                local_hash: Some(hash.clone()),
                application: Application::ClaudeCode,
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                date: chrono::Utc::now(),
                project_hash: "proj1".to_string(),
                conversation_hash: "conv1".to_string(),
                role: MessageRole::Assistant,
                stats: Stats {
                    output_tokens: 4,  // Identical
                    input_tokens: 100, // Identical
                    tool_calls: 2,     // Not checked for identity, but will be kept
                    ..Default::default()
                },
            },
        ];

        let deduplicated = deduplicate_messages_by_local_hash(messages);

        // Should have exactly 1 message (duplicate skipped)
        assert_eq!(deduplicated.len(), 1);

        // Tokens should NOT be summed (identical entries)
        assert_eq!(deduplicated[0].stats.output_tokens, 4);
        assert_eq!(deduplicated[0].stats.input_tokens, 100);

        // Tool calls are from the first entry only
        assert_eq!(deduplicated[0].stats.tool_calls, 2);
    }

    #[test]
    fn test_identical_tokens_merge_tool_stats() {
        // First row has no tools; second row has tool_calls=1, tokens identical
        let hash = "identical_merge_tools".to_string();

        let msg1 = ConversationMessage {
            global_hash: "g1".to_string(),
            local_hash: Some(hash.clone()),
            application: Application::ClaudeCode,
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            date: chrono::Utc::now(),
            project_hash: "proj".to_string(),
            conversation_hash: "conv".to_string(),
            role: MessageRole::Assistant,
            stats: Stats {
                input_tokens: 100,
                output_tokens: 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cached_tokens: 0,
                tool_calls: 0,
                ..Default::default()
            },
        };

        let msg2 = ConversationMessage {
            global_hash: "g2".to_string(),
            local_hash: Some(hash.clone()),
            application: Application::ClaudeCode,
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            date: chrono::Utc::now(),
            project_hash: "proj".to_string(),
            conversation_hash: "conv".to_string(),
            role: MessageRole::Assistant,
            stats: Stats {
                input_tokens: 100,
                output_tokens: 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cached_tokens: 0,
                tool_calls: 1,
                ..Default::default()
            },
        };

        let dedup = deduplicate_messages_by_local_hash(vec![msg1, msg2]);
        assert_eq!(dedup.len(), 1);
        // Tokens unchanged
        assert_eq!(dedup[0].stats.input_tokens, 100);
        assert_eq!(dedup[0].stats.output_tokens, 4);
        // Tool calls merged from second row
        assert_eq!(dedup[0].stats.tool_calls, 1);
    }

    #[test]
    fn test_deduplicate_skips_identical_after_partial_aggregate() {
        // Mix of partials and redundant duplicates for the same local_hash
        let hash = "test_mixed_duplicates".to_string();

        let a1 = ConversationMessage {
            global_hash: "ga1".to_string(),
            local_hash: Some(hash.clone()),
            application: Application::ClaudeCode,
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            date: chrono::Utc::now(),
            project_hash: "proj".to_string(),
            conversation_hash: "conv".to_string(),
            role: MessageRole::Assistant,
            stats: Stats {
                input_tokens: 10,
                output_tokens: 2,
                ..Default::default()
            },
        };
        // Exact duplicate of a1 (should be skipped)
        let a2 = ConversationMessage {
            global_hash: "ga2".to_string(),
            ..a1.clone()
        };

        // Tool-use partial
        let b1 = ConversationMessage {
            global_hash: "gb1".to_string(),
            local_hash: Some(hash.clone()),
            application: Application::ClaudeCode,
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            date: chrono::Utc::now(),
            project_hash: "proj".to_string(),
            conversation_hash: "conv".to_string(),
            role: MessageRole::Assistant,
            stats: Stats {
                input_tokens: 0,
                output_tokens: 447,
                tool_calls: 1,
                ..Default::default()
            },
        };
        // Exact duplicate of b1 (should be skipped)
        let b2 = ConversationMessage {
            global_hash: "gb2".to_string(),
            ..b1.clone()
        };

        // Another duplicate of a1 after aggregation (should still be skipped)
        let a3 = ConversationMessage {
            global_hash: "ga3".to_string(),
            ..a1.clone()
        };

        let messages = vec![a1, a2, b1, b2, a3];
        let deduplicated = deduplicate_messages_by_local_hash(messages);

        assert_eq!(deduplicated.len(), 1);
        // Should include A once and B once
        assert_eq!(deduplicated[0].stats.input_tokens, 10);
        assert_eq!(deduplicated[0].stats.output_tokens, 2 + 447);
        assert_eq!(deduplicated[0].stats.tool_calls, 1);
    }
}
