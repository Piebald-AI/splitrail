use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::models::calculate_total_cost_for_service_tier_at;
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::{fast_hash, hash_text};
use walkdir::WalkDir;

// Type alias for parse_jsonl_file return type
type ParseResult = (
    Vec<ConversationMessage>,
    HashMap<String, String>,
    Vec<String>,
    Option<String>,
);

pub struct ClaudeCodeAnalyzer;

impl ClaudeCodeAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".claude").join("projects"))
    }

    fn parse_live_source(source: &DataSource) -> Result<Vec<ConversationMessage>> {
        let project_hash = extract_and_hash_project_id(&source.path);
        let conversation_hash = crate::utils::hash_text(&source.path.to_string_lossy());
        let file = File::open(&source.path)?;
        let (mut messages, summaries, _uuids, fallback) =
            parse_jsonl_file(&source.path, file, &project_hash, &conversation_hash)?;
        let name = summaries
            .into_iter()
            .next()
            .map(|(_, name)| name)
            .or(fallback);
        if let Some(name) = name {
            for message in &mut messages {
                if message.session_name.is_none() {
                    message.session_name = Some(name.clone());
                }
            }
        }
        Ok(messages)
    }
}

#[async_trait]
impl Analyzer for ClaudeCodeAnalyzer {
    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{home_str}/.claude/projects/*/*.jsonl"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|projects_dir| {
                WalkDir::new(projects_dir)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .map(|e| DataSource {
                path: e.into_path(),
            })
            .collect();

        Ok(sources)
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        let conversation_hash = crate::utils::hash_text(&source.path.to_string_lossy());
        let messages = Self::parse_live_source(source)?;
        Ok(deduplicate_messages(
            super::claude_code_history::merge_session(messages, &conversation_hash),
        ))
    }

    fn parse_sources_parallel_with_paths(
        &self,
        sources: &[DataSource],
    ) -> Vec<(PathBuf, Vec<ConversationMessage>)> {
        let grouped = sources
            .par_iter()
            .map(|source| match Self::parse_live_source(source) {
                Ok(messages) => (source.path.clone(), messages),
                Err(error) => {
                    eprintln!(
                        "Failed to parse {} source {:?}: {}",
                        self.display_name(),
                        source.path,
                        error
                    );
                    (source.path.clone(), Vec::new())
                }
            })
            .collect();
        super::claude_code_history::merge_grouped(grouped)
            .into_iter()
            .map(|(path, messages)| (path, deduplicate_messages(messages)))
            .collect()
    }

    // Claude Code has complex cross-file deduplication, so we override get_stats_with_sources
    fn get_stats_with_sources(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<crate::types::AgenticCodingToolStats> {
        let messages: Vec<ConversationMessage> = deduplicate_messages(
            self.parse_sources_parallel_with_paths(&sources)
                .into_iter()
                .flat_map(|(_, messages)| messages)
                .collect(),
        );

        // Aggregate stats
        let mut daily_stats = crate::utils::aggregate_by_date(&messages);
        daily_stats.retain(|date, _| date != "unknown");
        let num_conversations = daily_stats
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        Ok(crate::types::AgenticCodingToolStats {
            daily_stats,
            num_conversations,
            messages,
            analyzer_name: self.display_name().to_string(),
        })
    }

    fn remove_source_state(&self, path: &Path) -> Result<()> {
        super::claude_code_history::remove_session(&crate::utils::hash_text(
            &path.to_string_lossy(),
        ))
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .collect()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        // Must be a .jsonl file at depth 2 from projects dir
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "jsonl") {
            return false;
        }
        if let Some(data_dir) = Self::data_dir()
            && let Ok(relative) = path.strip_prefix(&data_dir)
        {
            return relative.components().count() == 2;
        }
        false
    }

    fn is_available(&self) -> bool {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|projects_dir| {
                WalkDir::new(projects_dir)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
            })
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }
}

// Claude Code specific implementation functions

type DedupEntry = (usize, ConversationMessage, HashSet<TokenFingerprint>);

pub(crate) fn deduplicate_messages(messages: Vec<ConversationMessage>) -> Vec<ConversationMessage> {
    let mut dedup_map: HashMap<String, DedupEntry> = HashMap::with_capacity(messages.len());
    let mut no_hash_messages = Vec::new();

    for (order, message) in messages.into_iter().enumerate() {
        let Some(local_hash) = message.local_hash.clone() else {
            no_hash_messages.push((order, message));
            continue;
        };
        let fingerprint = (
            message.stats.input_tokens,
            message.stats.output_tokens,
            message.stats.cache_creation_tokens,
            message.stats.cache_read_tokens,
            message.stats.cached_tokens,
        );
        dedup_map
            .entry(local_hash)
            .and_modify(|(_, existing, seen_fingerprints)| {
                merge_message_into(existing, &message, seen_fingerprints, fingerprint);
            })
            .or_insert_with(|| {
                let mut seen_fingerprints = HashSet::new();
                seen_fingerprints.insert(fingerprint);
                (order, message, seen_fingerprints)
            });
    }

    let mut result: Vec<_> = dedup_map
        .into_values()
        .map(|(order, message, _)| (order, message))
        .chain(no_hash_messages)
        .collect();
    result.sort_by_key(|(order, _)| *order);
    result.into_iter().map(|(_, message)| message).collect()
}

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
        #[serde(default)]
        content: Option<Content>, // e.g. "Found 4 files\nC:\\..." — absent for empty results
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
    // Catch-all for unknown/new content block types (e.g. tool_reference, redacted_thinking)
    #[serde(other)]
    Other,
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

/// Deserializes a JSON value as u64, treating null as 0.
/// Needed because some providers (e.g. OpenRouter) send `null` for token counts
/// instead of omitting the field, and `#[serde(default)]` only handles missing fields.
fn deserialize_u64_or_null<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or(0))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, deserialize_with = "deserialize_u64_or_null")]
    pub input_tokens: u64,
    #[serde(default, deserialize_with = "deserialize_u64_or_null")]
    pub output_tokens: u64,
    #[serde(default, deserialize_with = "deserialize_u64_or_null")]
    pub cache_creation_input_tokens: u64,
    #[serde(default, deserialize_with = "deserialize_u64_or_null")]
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
struct ClaudeCodeProgressEntry {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCodeQueueOperationEntry {
    operation: String, // "enqueue" or "dequeue"
    timestamp: DateTime<Utc>,
    #[allow(dead_code)]
    content: Option<simd_json::OwnedValue>, // Can be array of content blocks or string
    session_id: String,
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
    #[serde(rename = "queue-operation")]
    QueueOperation(ClaudeCodeQueueOperationEntry),
    #[serde(rename = "progress")]
    Progress(ClaudeCodeProgressEntry),
    // Catch-all for unknown/new entry types (e.g. last-prompt)
    #[serde(other)]
    Other,
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

#[cfg(test)]
pub fn calculate_cost_from_tokens(usage: &Usage, model_name: &str) -> f64 {
    calculate_total_cost_for_service_tier_at(
        model_name,
        crate::models::ServiceTier::Standard,
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        None,
    )
}

pub fn calculate_cost_from_tokens_at(
    usage: &Usage,
    model_name: &str,
    effective_at: DateTime<Utc>,
) -> f64 {
    calculate_total_cost_for_service_tier_at(
        model_name,
        crate::models::ServiceTier::Standard,
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        Some(effective_at),
    )
}

pub fn parse_jsonl_file<R: Read>(
    path: &Path,
    mut reader: R,
    project_hash: &str,
    conversation_hash: &str,
) -> Result<ParseResult> {
    // Pre-allocate collections based on typical file sizes
    let estimated_messages = 50;
    let mut messages = Vec::with_capacity(estimated_messages);
    let mut summaries = HashMap::with_capacity(10);
    let mut all_uuids = Vec::with_capacity(estimated_messages);
    let mut fallback_session_name = None;

    let mut current_model = None;

    // Read entire file at once to avoid per-line allocations
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)?;

    for (i, line) in buffer.split(|&b| b == b'\n').enumerate() {
        // Skip empty lines
        if line.is_empty() || line.iter().all(|&b| b.is_ascii_whitespace()) {
            continue;
        }

        // simd_json needs mutable slice - copy this line only
        let mut line_buf = line.to_vec();
        let parsed_line = simd_json::from_slice::<ClaudeCodeEntry>(&mut line_buf);
        match parsed_line {
            Ok(ClaudeCodeEntry::Summary(summary)) => {
                summaries.insert(summary.leaf_uuid, summary.summary);
            }
            Ok(ClaudeCodeEntry::Message(entry)) => {
                // Track all UUIDs for summary linking, even if we skip the message
                all_uuids.push(entry.uuid.clone());

                let model = entry.message.as_ref().and_then(|m| m.model.clone());
                if let Some(m) = &model {
                    current_model = Some(m.clone());
                }

                let timestamp = entry.timestamp;
                let tool_use_result = entry.tool_use_result;
                let request_id = entry.request_id;
                let uuid = Some(entry.uuid);

                // Skip synthetic messages (internal reasoning/planning)
                if !matches!(model.as_deref(), Some("<synthetic>")) {
                    let content = entry.message.as_ref().and_then(|m| m.content.clone());
                    let usage = entry.message.as_ref().and_then(|m| m.usage.clone());
                    let role = entry.message.as_ref().and_then(|m| m.role.clone());
                    let message_id = entry.message.as_ref().and_then(|m| m.id.clone());

                    let mut msg = ConversationMessage {
                        global_hash: hash_text(&format!(
                            "{}_{}",
                            conversation_hash,
                            uuid.as_ref().unwrap_or(&"".to_string())
                        )),
                        local_hash: None,
                        application: Application::ClaudeCode,
                        model: model.clone(),
                        date: timestamp,
                        project_hash: project_hash.to_string(),
                        conversation_hash: conversation_hash.to_string(),
                        stats: Stats::default(), // Will be filled below
                        role: match role.as_deref() {
                            Some("user") => MessageRole::User,
                            _ => MessageRole::Assistant,
                        },
                        uuid,
                        session_name: None, // Will be populated later
                    };

                    // Always extract tool stats from content if present
                    if let Some(content_val) = &content {
                        msg.stats = extract_tool_stats(content_val, &tool_use_result);
                        msg.stats.tool_calls = match content_val {
                            Content::Blocks(blocks) => blocks
                                .iter()
                                .filter(|c| matches!(c, ContentBlock::ToolUse { .. }))
                                .count()
                                as u32,
                            _ => 0,
                        };
                    }

                    if let Some(usage_val) = usage {
                        let model_name = model
                            .as_ref()
                            .unwrap_or(&current_model.clone().unwrap_or_default())
                            .to_owned();

                        msg.stats.input_tokens = usage_val.input_tokens;
                        msg.stats.output_tokens = usage_val.output_tokens;
                        msg.stats.cache_creation_tokens = usage_val.cache_creation_input_tokens;
                        msg.stats.cache_read_tokens = usage_val.cache_read_input_tokens;
                        msg.stats.cached_tokens = usage_val.cache_creation_input_tokens
                            + usage_val.cache_read_input_tokens;
                        msg.stats.cost =
                            calculate_cost_from_tokens_at(&usage_val, &model_name, timestamp);

                        if let Some(request_id) = request_id
                            && let Some(message_id) = message_id
                        {
                            msg.local_hash = Some(fast_hash(&format!("{request_id}_{message_id}")));
                        }
                    } else {
                        // If no usage, it's likely a user message
                        msg.role = MessageRole::User;
                    }

                    // Capture fallback session name from the first user message
                    // or first assistant message (for agent sub-sessions that start with assistant)
                    if fallback_session_name.is_none()
                        && let Some(content_val) = &content
                    {
                        // Extract user-visible text from either blocks or string content
                        let text_opt: Option<String> = match content_val {
                            Content::Blocks(blocks) => {
                                let mut result = None;
                                for block in blocks {
                                    if let ContentBlock::Text { text } = block {
                                        let text_str = String::from_utf8_lossy(text);
                                        result = Some(text_str.to_string());
                                        break;
                                    }
                                }
                                result
                            }
                            Content::String(bytes) => {
                                let text_str = String::from_utf8_lossy(bytes);
                                Some(text_str.to_string())
                            }
                        };

                        if let Some(text_str) = text_opt {
                            let truncated = if text_str.chars().count() > 50 {
                                let chars: String = text_str.chars().take(50).collect();
                                format!("{}...", chars)
                            } else {
                                text_str
                            };
                            fallback_session_name = Some(truncated);
                        }
                    }

                    messages.push(msg);
                }
            }
            Err(e) => {
                crate::utils::warn_once(format!(
                    "Skipping invalid entry in {} line {}: {}",
                    path.display(),
                    i + 1,
                    e
                ));
                continue;
            }
            _ => continue, // Skip other entry types like FileHistorySnapshot, QueueOperation, Progress
        };
    }

    Ok((messages, summaries, all_uuids, fallback_session_name))
}

// Type alias for token fingerprint
pub type TokenFingerprint = (u64, u64, u64, u64, u64);

/// Merge stats from `src` into `dst` based on fingerprint comparison.
/// If the fingerprint was already seen, uses max() for non-token stats (redundant duplicate).
/// If it's a new fingerprint, uses sum() for all stats (split message).
pub fn merge_message_into(
    dst: &mut ConversationMessage,
    src: &ConversationMessage,
    seen_fps: &mut HashSet<TokenFingerprint>,
    src_fp: TokenFingerprint,
) {
    // Preserve session name
    if dst.session_name.is_none() && src.session_name.is_some() {
        dst.session_name = src.session_name.clone();
    }

    if seen_fps.contains(&src_fp) {
        // Redundant duplicate: merge non-token stats with max()
        dst.stats.tool_calls = dst.stats.tool_calls.max(src.stats.tool_calls);
        dst.stats.files_read = dst.stats.files_read.max(src.stats.files_read);
        dst.stats.files_edited = dst.stats.files_edited.max(src.stats.files_edited);
        dst.stats.files_added = dst.stats.files_added.max(src.stats.files_added);
        dst.stats.terminal_commands = dst.stats.terminal_commands.max(src.stats.terminal_commands);
        dst.stats.file_searches = dst.stats.file_searches.max(src.stats.file_searches);
        dst.stats.file_content_searches = dst
            .stats
            .file_content_searches
            .max(src.stats.file_content_searches);
        dst.stats.todo_writes = dst.stats.todo_writes.max(src.stats.todo_writes);
        dst.stats.todo_reads = dst.stats.todo_reads.max(src.stats.todo_reads);
        dst.stats.todos_created = dst.stats.todos_created.max(src.stats.todos_created);
        dst.stats.todos_completed = dst.stats.todos_completed.max(src.stats.todos_completed);
        dst.stats.todos_in_progress = dst.stats.todos_in_progress.max(src.stats.todos_in_progress);
    } else {
        // New fingerprint: aggregate all stats with sum()
        seen_fps.insert(src_fp);

        dst.stats.input_tokens += src.stats.input_tokens;
        dst.stats.output_tokens += src.stats.output_tokens;
        dst.stats.cache_creation_tokens += src.stats.cache_creation_tokens;
        dst.stats.cache_read_tokens += src.stats.cache_read_tokens;
        dst.stats.cached_tokens += src.stats.cached_tokens;

        dst.stats.tool_calls += src.stats.tool_calls;
        dst.stats.files_read += src.stats.files_read;
        dst.stats.files_edited += src.stats.files_edited;
        dst.stats.files_added += src.stats.files_added;
        dst.stats.terminal_commands += src.stats.terminal_commands;
        dst.stats.file_searches += src.stats.file_searches;
        dst.stats.file_content_searches += src.stats.file_content_searches;
        dst.stats.todo_writes += src.stats.todo_writes;
        dst.stats.todo_reads += src.stats.todo_reads;
        dst.stats.todos_created += src.stats.todos_created;
        dst.stats.todos_completed += src.stats.todos_completed;
        dst.stats.todos_in_progress += src.stats.todos_in_progress;

        // Preserve each message's timestamp-aware price: `dst.stats.cost` was
        // already priced at `dst.date`, and `src.stats.cost` was already
        // priced at `src.date`, so just accumulate rather than recomputing
        // the whole total at `dst.date` (which would reprice `src`'s tokens).
        dst.stats.cost += src.stats.cost;
    }
}
