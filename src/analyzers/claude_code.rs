use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::analyzer::{Analyzer, DataSource};
use crate::cache::FileCacheEntry;
use crate::models::calculate_total_cost;
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, FileMetadata, MessageRole, Stats,
};
use crate::utils::{fast_hash, hash_text};
use jwalk::WalkDir;

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
        let mut sources = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let projects_dir = home_dir.join(".claude").join("projects");

            if projects_dir.is_dir() {
                // jwalk walks directories in parallel
                for entry in WalkDir::new(&projects_dir)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
                {
                    sources.push(DataSource { path: entry.path() });
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

        // Type for concurrent deduplication entry: (insertion_order, message, seen_fingerprints)
        type TokenFingerprint = (u64, u64, u64, u64, u64);
        type DedupEntry = (usize, ConversationMessage, HashSet<TokenFingerprint>);

        // Concurrent deduplication map and session tracking
        let dedup_map: DashMap<String, DedupEntry> = DashMap::with_capacity(sources.len() * 50);
        let insertion_counter = AtomicUsize::new(0);
        let no_hash_counter = AtomicUsize::new(0);

        // Concurrent session name mappings
        let session_names: DashMap<String, String> = DashMap::new();
        let conversation_summaries: DashMap<String, String> = DashMap::new();
        let conversation_fallbacks: DashMap<String, String> = DashMap::new();
        let conversation_uuids: DashMap<String, Vec<String>> = DashMap::new();

        // Parse all files in parallel, deduplicating as we go
        sources.into_par_iter().for_each(|source| {
            let project_hash = extract_and_hash_project_id(&source.path);
            let conversation_hash = crate::utils::hash_text(&source.path.to_string_lossy());

            if let Ok(file) = File::open(&source.path)
                && let Ok((msgs, summaries, uuids, fallback)) =
                    parse_jsonl_file(&source.path, file, &project_hash, &conversation_hash)
            {
                // Store summaries
                for (uuid, name) in summaries {
                    session_names.insert(uuid, name);
                }

                // Store UUIDs for this conversation
                conversation_uuids.insert(conversation_hash.clone(), uuids);

                // Store fallback
                if let Some(fb) = fallback {
                    conversation_fallbacks.insert(conversation_hash.clone(), fb);
                }

                // Deduplicate messages as we insert
                for msg in msgs {
                    if let Some(local_hash) = &msg.local_hash {
                        let order = insertion_counter.fetch_add(1, Ordering::Relaxed);
                        let fp = (
                            msg.stats.input_tokens,
                            msg.stats.output_tokens,
                            msg.stats.cache_creation_tokens,
                            msg.stats.cache_read_tokens,
                            msg.stats.cached_tokens,
                        );

                        dedup_map
                            .entry(local_hash.clone())
                            .and_modify(|(_, existing, seen_fps)| {
                                merge_message_into(existing, &msg, seen_fps, fp);
                            })
                            .or_insert_with(|| {
                                let mut fps = HashSet::new();
                                fps.insert(fp);
                                (order, msg, fps)
                            });
                    } else {
                        // No local hash, always keep with unique key
                        let order = insertion_counter.fetch_add(1, Ordering::Relaxed);
                        let unique_key = format!(
                            "__no_hash_{}",
                            no_hash_counter.fetch_add(1, Ordering::Relaxed)
                        );
                        let fp = (
                            msg.stats.input_tokens,
                            msg.stats.output_tokens,
                            msg.stats.cache_creation_tokens,
                            msg.stats.cache_read_tokens,
                            msg.stats.cached_tokens,
                        );
                        let mut fps = HashSet::new();
                        fps.insert(fp);
                        dedup_map.insert(unique_key, (order, msg, fps));
                    }
                }
            }
        });

        // Link session names to conversations (after all parsing complete)
        for entry in conversation_uuids.iter() {
            let conversation_hash = entry.key();
            let uuids = entry.value();

            let mut found_summary = false;
            for uuid in uuids {
                if let Some(name) = session_names.get(uuid) {
                    conversation_summaries.insert(conversation_hash.clone(), name.clone());
                    found_summary = true;
                    break;
                }
            }

            if !found_summary && let Some(fb) = conversation_fallbacks.get(conversation_hash) {
                conversation_summaries.insert(conversation_hash.clone(), fb.clone());
            }
        }

        // Apply session names to messages and collect results
        let mut result: Vec<_> = dedup_map
            .into_iter()
            .map(|(_, (order, mut msg, _))| {
                // Apply session name if available
                if msg.session_name.is_none()
                    && let Some(name) = conversation_summaries.get(&msg.conversation_hash)
                {
                    msg.session_name = Some(name.clone());
                }
                (order, msg)
            })
            .collect();

        // Sort by insertion order for deterministic output
        result.sort_by_key(|(order, _)| *order);

        Ok(result.into_iter().map(|(_, msg)| msg).collect())
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

    fn supports_caching(&self) -> bool {
        true
    }

    fn parse_single_file(&self, source: &DataSource) -> Result<FileCacheEntry> {
        let mut metadata = FileMetadata::from_path(&source.path)?;
        let project_hash = extract_and_hash_project_id(&source.path);
        let conversation_hash = hash_text(&source.path.to_string_lossy());

        let file = File::open(&source.path)?;
        let (messages, _summaries, _uuids, _fallback) =
            parse_jsonl_file(&source.path, file, &project_hash, &conversation_hash)?;

        // Set last_parsed_offset to file size (we've parsed everything)
        metadata.last_parsed_offset = metadata.size;

        // Pre-aggregate daily contributions for this file
        let daily_contributions = crate::utils::aggregate_by_date_simple(&messages);

        Ok(FileCacheEntry {
            metadata,
            messages,
            daily_contributions,
            cached_model: None, // Claude Code has model per-message, no session-level caching needed
        })
    }

    fn supports_delta_parsing(&self) -> bool {
        true
    }

    fn parse_single_file_incremental(
        &self,
        source: &DataSource,
        cached: Option<&FileCacheEntry>,
    ) -> Result<FileCacheEntry> {
        let current_meta = FileMetadata::from_path(&source.path)?;

        // Check if we can do delta parsing
        if let Some(cached_entry) = cached {
            // Check for truncation - requires full reparse
            if cached_entry.metadata.needs_full_reparse(&current_meta) {
                return self.parse_single_file(source);
            }

            // Check for append - can do delta parsing
            if cached_entry.metadata.is_append_only(&current_meta) {
                let project_hash = extract_and_hash_project_id(&source.path);
                let conversation_hash = hash_text(&source.path.to_string_lossy());

                // Delta parse only new bytes (pass expected_size to detect races)
                let delta_result = parse_jsonl_file_delta(
                    &source.path,
                    cached_entry.metadata.last_parsed_offset,
                    current_meta.size,
                    &project_hash,
                    &conversation_hash,
                );

                // If delta parse fails (e.g., file truncated), fall back to full reparse
                let (new_messages, new_offset) = match delta_result {
                    Ok(result) => result,
                    Err(_) => return self.parse_single_file(source),
                };

                // Merge with cached messages
                let mut all_messages = cached_entry.messages.clone();
                all_messages.extend(new_messages);

                // Re-aggregate daily contributions
                let daily_contributions = crate::utils::aggregate_by_date_simple(&all_messages);

                return Ok(FileCacheEntry {
                    metadata: FileMetadata {
                        size: current_meta.size,
                        modified: current_meta.modified,
                        last_parsed_offset: new_offset,
                    },
                    messages: all_messages,
                    daily_contributions,
                    cached_model: None, // Claude Code has model per-message
                });
            }

            // File unchanged - use cached entry directly
            if cached_entry.metadata.is_unchanged(&current_meta) {
                return Ok(cached_entry.clone());
            }
        }

        // No cache or mtime changed without size change - full reparse
        self.parse_single_file(source)
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
                        msg.stats.cost = calculate_cost_from_tokens(&usage_val, &model_name);

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
            _ => continue, // Skip other entry types like FileHistorySnapshot, QueueOperation
        };
    }

    Ok((messages, summaries, all_uuids, fallback_session_name))
}

/// Parse JSONL file starting from a byte offset (delta parsing).
/// Returns (new_messages, new_end_offset).
///
/// Key behaviors:
/// - If start_offset > 0, skips to first newline to handle partial lines
/// - Handles incomplete line at EOF gracefully (doesn't advance offset past it)
/// - Returns only newly parsed messages, not the entire file
pub fn parse_jsonl_file_delta(
    path: &Path,
    start_offset: u64,
    expected_size: u64,
    project_hash: &str,
    conversation_hash: &str,
) -> Result<(Vec<ConversationMessage>, u64)> {
    let file = File::open(path)?;
    let file_size = file.metadata()?.len();

    // Race condition protection: if file was truncated between the caller's
    // metadata check and now, bail out so caller can do a full reparse
    if file_size < expected_size {
        anyhow::bail!("file was truncated during delta parse");
    }

    // Nothing new to parse
    if start_offset >= file_size {
        return Ok((Vec::new(), start_offset));
    }

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(start_offset))?;

    // If starting mid-file, skip to first complete line
    // (the previous parse may have ended mid-line)
    let mut current_offset = start_offset;
    if start_offset > 0 {
        let mut skip_buf = Vec::new();
        let bytes_skipped = reader.read_until(b'\n', &mut skip_buf)?;
        if bytes_skipped == 0 {
            // EOF reached while looking for newline
            return Ok((Vec::new(), start_offset));
        }
        current_offset += bytes_skipped as u64;
    }

    let mut messages = Vec::new();
    let mut current_model: Option<String> = None;
    let mut last_successful_offset = current_offset;

    loop {
        let mut line_buf = String::new();
        let bytes_read = reader.read_line(&mut line_buf)?;

        if bytes_read == 0 {
            // EOF
            break;
        }

        let line = line_buf.trim();

        // Skip empty lines
        if line.is_empty() {
            current_offset += bytes_read as u64;
            last_successful_offset = current_offset;
            continue;
        }

        // Check if line is complete (ends with newline)
        let is_complete_line = line_buf.ends_with('\n');

        // Try to parse
        let mut line_bytes = line.as_bytes().to_vec();
        match simd_json::from_slice::<ClaudeCodeEntry>(&mut line_bytes) {
            Ok(ClaudeCodeEntry::Message(entry)) => {
                let model = entry.message.as_ref().and_then(|m| m.model.clone());
                if let Some(m) = &model {
                    current_model = Some(m.clone());
                }

                // Skip synthetic messages
                if !matches!(model.as_deref(), Some("<synthetic>")) {
                    let content = entry.message.as_ref().and_then(|m| m.content.clone());
                    let usage = entry.message.as_ref().and_then(|m| m.usage.clone());
                    let role = entry.message.as_ref().and_then(|m| m.role.clone());
                    let message_id = entry.message.as_ref().and_then(|m| m.id.clone());
                    let request_id = entry.request_id;
                    let tool_use_result = entry.tool_use_result;
                    let uuid = Some(entry.uuid.clone());

                    let mut msg = ConversationMessage {
                        global_hash: hash_text(&format!("{}_{}", conversation_hash, entry.uuid)),
                        local_hash: None,
                        application: Application::ClaudeCode,
                        model: model.clone(),
                        date: entry.timestamp,
                        project_hash: project_hash.to_string(),
                        conversation_hash: conversation_hash.to_string(),
                        stats: Stats::default(),
                        role: match role.as_deref() {
                            Some("user") => MessageRole::User,
                            _ => MessageRole::Assistant,
                        },
                        uuid,
                        session_name: None,
                    };

                    // Extract tool stats from content
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
                        msg.stats.cost = calculate_cost_from_tokens(&usage_val, &model_name);

                        if let Some(request_id) = request_id
                            && let Some(message_id) = message_id
                        {
                            msg.local_hash = Some(fast_hash(&format!("{request_id}_{message_id}")));
                        }
                    } else {
                        msg.role = MessageRole::User;
                    }

                    messages.push(msg);
                }

                current_offset += bytes_read as u64;
                last_successful_offset = current_offset;
            }
            Ok(ClaudeCodeEntry::Summary(_)) | Ok(_) => {
                // Skip summaries and other entry types in delta parsing
                current_offset += bytes_read as u64;
                last_successful_offset = current_offset;
            }
            Err(e) => {
                if !is_complete_line {
                    // Incomplete line at EOF - don't advance offset past it
                    // This will be re-read on next delta parse when more data is available
                    break;
                } else {
                    // Complete line but parse error - log and skip
                    crate::utils::warn_once(format!(
                        "Skipping invalid entry in {} at offset {}: {}",
                        path.display(),
                        current_offset,
                        e
                    ));
                    current_offset += bytes_read as u64;
                    last_successful_offset = current_offset;
                }
            }
        }
    }

    Ok((messages, last_successful_offset))
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

        // Recalculate cost
        if let Some(model) = &dst.model {
            dst.stats.cost = calculate_total_cost(
                model,
                dst.stats.input_tokens,
                dst.stats.output_tokens,
                dst.stats.cache_creation_tokens,
                dst.stats.cache_read_tokens,
            );
        }
    }
}

/// Deduplicate messages by local_hash, merging stats for duplicates.
/// This is used for incremental cache loading where messages from multiple
/// files need to be deduplicated after loading.
pub fn deduplicate_messages(messages: Vec<ConversationMessage>) -> Vec<ConversationMessage> {
    let estimated_unique = messages.len() / 2 + 1;
    let mut seen_hashes = HashMap::<String, usize>::with_capacity(estimated_unique);
    let mut seen_token_fingerprints: HashMap<String, HashSet<TokenFingerprint>> =
        HashMap::with_capacity(estimated_unique);
    let mut deduplicated_entries: Vec<ConversationMessage> = Vec::with_capacity(estimated_unique);

    for message in messages {
        if let Some(local_hash) = &message.local_hash {
            let fp = (
                message.stats.input_tokens,
                message.stats.output_tokens,
                message.stats.cache_creation_tokens,
                message.stats.cache_read_tokens,
                message.stats.cached_tokens,
            );

            if let Some(&existing_index) = seen_hashes.get(local_hash) {
                let seen_fps = seen_token_fingerprints
                    .entry(local_hash.clone())
                    .or_default();
                merge_message_into(
                    &mut deduplicated_entries[existing_index],
                    &message,
                    seen_fps,
                    fp,
                );
            } else {
                seen_hashes.insert(local_hash.clone(), deduplicated_entries.len());
                seen_token_fingerprints
                    .entry(local_hash.clone())
                    .or_default()
                    .insert(fp);
                deduplicated_entries.push(message);
            }
        } else {
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
                uuid: None,
                session_name: None,
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
                uuid: None,
                session_name: None,
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
                uuid: None,
                session_name: None,
            },
        ];

        let deduplicated = deduplicate_messages(messages);

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
                uuid: None,
                session_name: None,
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
                uuid: None,
                session_name: None,
            },
        ];

        let deduplicated = deduplicate_messages(messages);

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
            uuid: None,
            session_name: None,
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
            uuid: None,
            session_name: None,
        };

        let dedup = deduplicate_messages(vec![msg1, msg2]);
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
            uuid: None,
            session_name: None,
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
            uuid: None,
            session_name: None,
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
        let deduplicated = deduplicate_messages(messages);

        assert_eq!(deduplicated.len(), 1);
        // Should include A once and B once
        assert_eq!(deduplicated[0].stats.input_tokens, 10);
        assert_eq!(deduplicated[0].stats.output_tokens, 2 + 447);
        assert_eq!(deduplicated[0].stats.tool_calls, 1);
    }
}
