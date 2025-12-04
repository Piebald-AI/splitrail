use crate::analyzer::{Analyzer, DataSource};
use crate::cache::FileCacheEntry;
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, FileMetadata, MessageRole, Stats,
};
use crate::utils::hash_text;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jwalk::WalkDir;
use rayon::prelude::*;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub struct PiAgentAnalyzer;

impl PiAgentAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

// Pi Agent session entry types

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct PiUsageCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    #[serde(rename = "cacheRead")]
    cache_read: f64,
    #[serde(default)]
    #[serde(rename = "cacheWrite")]
    cache_write: f64,
    #[serde(default)]
    total: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct PiUsage {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    #[serde(rename = "cacheRead")]
    cache_read: u64,
    #[serde(default)]
    #[serde(rename = "cacheWrite")]
    cache_write: u64,
    #[serde(default)]
    cost: Option<PiUsageCost>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiToolCall {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[allow(dead_code)]
enum PiContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    ToolCall(PiToolCall),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum PiContent {
    String(String),
    Blocks(Vec<PiContentBlock>),
}

// Unified message struct - role determines user vs assistant
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiMessage {
    role: String,
    #[serde(default)]
    content: Option<PiContent>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<PiUsage>,
    #[serde(default)]
    timestamp: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct PiSessionHeader {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PiMessageEntry {
    timestamp: String,
    message: PiMessage,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum PiSessionEntry {
    Session(PiSessionHeader),
    Message(PiMessageEntry),
    #[serde(rename = "model_change")]
    ModelChange {
        timestamp: String,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange {
        timestamp: String,
        #[serde(rename = "thinkingLevel")]
        thinking_level: String,
    },
    Compaction {
        timestamp: String,
        summary: String,
    },
    #[serde(other)]
    Unknown,
}

// Extract tool stats from content
fn extract_tool_stats(content: &PiContent) -> Stats {
    let mut stats = Stats::default();

    if let PiContent::Blocks(blocks) = content {
        for block in blocks {
            if let PiContentBlock::ToolCall(tool) = block {
                stats.tool_calls += 1;

                // Map Pi Agent tool names to stats
                match tool.name.as_str() {
                    "read" | "Read" => stats.files_read += 1,
                    "edit" | "Edit" | "multiEdit" | "MultiEdit" => stats.files_edited += 1,
                    "write" | "Write" => stats.files_added += 1,
                    "bash" | "Bash" => stats.terminal_commands += 1,
                    "glob" | "Glob" => stats.file_searches += 1,
                    "grep" | "Grep" => stats.file_content_searches += 1,
                    _ => {}
                }
            }
        }
    }

    stats
}

// Extract first text content for session name fallback
fn extract_first_text(content: &PiContent) -> Option<String> {
    match content {
        PiContent::String(s) if !s.is_empty() => {
            let truncated = if s.chars().count() > 50 {
                let chars: String = s.chars().take(50).collect();
                format!("{}...", chars)
            } else {
                s.clone()
            };
            Some(truncated)
        }
        PiContent::Blocks(blocks) => {
            for block in blocks {
                if let PiContentBlock::Text { text } = block {
                    if !text.is_empty() {
                        let truncated = if text.chars().count() > 50 {
                            let chars: String = text.chars().take(50).collect();
                            format!("{}...", chars)
                        } else {
                            text.clone()
                        };
                        return Some(truncated);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

// Helper function to extract project ID from Pi Agent file path and hash it
fn extract_and_hash_project_id(file_path: &Path) -> String {
    // Pi Agent path format: ~/.pi/agent/sessions/--<path>--/<timestamp>_<uuid>.jsonl
    // The parent directory name (--<path>--) represents the project

    if let Some(parent) = file_path.parent() {
        if let Some(project_dir) = parent.file_name().and_then(|name| name.to_str()) {
            return hash_text(project_dir);
        }
    }

    // Fallback: hash the full file path
    hash_text(&file_path.to_string_lossy())
}

type ParseResult = (Vec<ConversationMessage>, Option<String>);

fn parse_jsonl_file<R: Read>(
    path: &Path,
    mut reader: R,
    project_hash: &str,
    conversation_hash: &str,
) -> Result<ParseResult> {
    let mut messages = Vec::with_capacity(50);
    let mut fallback_session_name = None;
    let mut current_model: Option<String> = None;
    let mut current_provider: Option<String> = None;

    // Read entire file at once
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)?;

    for (i, line) in buffer.split(|&b| b == b'\n').enumerate() {
        // Skip empty lines
        if line.is_empty() || line.iter().all(|&b| b.is_ascii_whitespace()) {
            continue;
        }

        let mut line_buf = line.to_vec();
        let parsed_line = simd_json::from_slice::<PiSessionEntry>(&mut line_buf);

        match parsed_line {
            Ok(PiSessionEntry::Session(header)) => {
                // Track initial model from session header
                if let Some(model_id) = &header.model_id {
                    current_model = Some(model_id.clone());
                }
                if let Some(provider) = &header.provider {
                    current_provider = Some(provider.clone());
                }
            }
            Ok(PiSessionEntry::ModelChange {
                provider, model_id, ..
            }) => {
                current_model = Some(model_id);
                current_provider = Some(provider);
            }
            Ok(PiSessionEntry::Message(entry)) => {
                let timestamp = DateTime::parse_from_rfc3339(&entry.timestamp)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                let msg = &entry.message;

                if msg.role == "assistant" {
                    let model = msg.model.clone().or_else(|| current_model.clone());
                    let provider = msg.provider.clone().or_else(|| current_provider.clone());

                    // Update current model/provider for future messages
                    if let Some(m) = &msg.model {
                        current_model = Some(m.clone());
                    }
                    if let Some(p) = &msg.provider {
                        current_provider = Some(p.clone());
                    }

                    // Build model string: "provider/model" or just "model"
                    let model_str = match (&provider, &model) {
                        (Some(p), Some(m)) => Some(format!("{}/{}", p, m)),
                        (None, Some(m)) => Some(m.clone()),
                        _ => None,
                    };

                    // Extract tool stats
                    let mut stats = if let Some(content) = &msg.content {
                        extract_tool_stats(content)
                    } else {
                        Stats::default()
                    };

                    // Set token counts and cost from usage
                    if let Some(usage) = &msg.usage {
                        // Total input = input + cacheRead + cacheWrite (per Pi's scheme)
                        stats.input_tokens = usage.input;
                        stats.output_tokens = usage.output;
                        stats.cache_read_tokens = usage.cache_read;
                        stats.cache_creation_tokens = usage.cache_write;
                        stats.cached_tokens = usage.cache_read + usage.cache_write;

                        // Cost comes directly from Pi's calculation
                        if let Some(cost) = &usage.cost {
                            stats.cost = cost.total;
                        }
                    }

                    // Generate unique hash for this message
                    let msg_timestamp = msg.timestamp.unwrap_or(0);
                    let global_hash = hash_text(&format!(
                        "{}_{}_{}_{}",
                        conversation_hash,
                        timestamp.to_rfc3339(),
                        msg_timestamp,
                        stats.output_tokens
                    ));

                    messages.push(ConversationMessage {
                        application: Application::PiAgent,
                        date: timestamp,
                        project_hash: project_hash.to_string(),
                        conversation_hash: conversation_hash.to_string(),
                        local_hash: Some(global_hash.clone()),
                        global_hash,
                        model: model_str,
                        stats,
                        role: MessageRole::Assistant,
                        uuid: None,
                        session_name: None,
                    });
                } else if msg.role == "user" {
                    // Capture fallback session name from first user message
                    if fallback_session_name.is_none() {
                        if let Some(content) = &msg.content {
                            fallback_session_name = extract_first_text(content);
                        }
                    }

                    let global_hash = hash_text(&format!(
                        "{}_{}_user",
                        conversation_hash,
                        timestamp.to_rfc3339()
                    ));

                    messages.push(ConversationMessage {
                        application: Application::PiAgent,
                        date: timestamp,
                        project_hash: project_hash.to_string(),
                        conversation_hash: conversation_hash.to_string(),
                        local_hash: None,
                        global_hash,
                        model: None,
                        stats: Stats::default(),
                        role: MessageRole::User,
                        uuid: None,
                        session_name: None,
                    });
                }
                // Skip other roles (e.g., toolResult)
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
            _ => continue, // Skip other entry types
        }
    }

    // Apply session name to all messages
    if let Some(ref session_name) = fallback_session_name {
        for msg in &mut messages {
            msg.session_name = Some(session_name.clone());
        }
    }

    Ok((messages, fallback_session_name))
}

/// Parse JSONL file starting from a byte offset (delta parsing).
fn parse_jsonl_file_delta(
    path: &Path,
    start_offset: u64,
    expected_size: u64,
    project_hash: &str,
    conversation_hash: &str,
) -> Result<(Vec<ConversationMessage>, u64)> {
    let file = File::open(path)?;
    let file_size = file.metadata()?.len();

    // Race condition protection
    if file_size < expected_size {
        anyhow::bail!("file was truncated during delta parse");
    }

    if start_offset >= file_size {
        return Ok((Vec::new(), start_offset));
    }

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(start_offset))?;

    let mut current_offset = start_offset;
    if start_offset > 0 {
        let mut skip_buf = Vec::new();
        let bytes_skipped = reader.read_until(b'\n', &mut skip_buf)?;
        if bytes_skipped == 0 {
            return Ok((Vec::new(), start_offset));
        }
        current_offset += bytes_skipped as u64;
    }

    let mut messages = Vec::new();
    let mut current_model: Option<String> = None;
    let mut current_provider: Option<String> = None;
    let mut last_successful_offset = current_offset;

    loop {
        let mut line_buf = String::new();
        let bytes_read = reader.read_line(&mut line_buf)?;

        if bytes_read == 0 {
            break;
        }

        let line = line_buf.trim();

        if line.is_empty() {
            current_offset += bytes_read as u64;
            last_successful_offset = current_offset;
            continue;
        }

        let is_complete_line = line_buf.ends_with('\n');

        let mut line_bytes = line.as_bytes().to_vec();
        match simd_json::from_slice::<PiSessionEntry>(&mut line_bytes) {
            Ok(PiSessionEntry::Session(header)) => {
                if let Some(model_id) = &header.model_id {
                    current_model = Some(model_id.clone());
                }
                if let Some(provider) = &header.provider {
                    current_provider = Some(provider.clone());
                }
                current_offset += bytes_read as u64;
                last_successful_offset = current_offset;
            }
            Ok(PiSessionEntry::ModelChange {
                provider, model_id, ..
            }) => {
                current_model = Some(model_id);
                current_provider = Some(provider);
                current_offset += bytes_read as u64;
                last_successful_offset = current_offset;
            }
            Ok(PiSessionEntry::Message(entry)) => {
                let timestamp = DateTime::parse_from_rfc3339(&entry.timestamp)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                let msg = &entry.message;

                if msg.role == "assistant" {
                    let model = msg.model.clone().or_else(|| current_model.clone());
                    let provider = msg.provider.clone().or_else(|| current_provider.clone());

                    if let Some(m) = &msg.model {
                        current_model = Some(m.clone());
                    }
                    if let Some(p) = &msg.provider {
                        current_provider = Some(p.clone());
                    }

                    let model_str = match (&provider, &model) {
                        (Some(p), Some(m)) => Some(format!("{}/{}", p, m)),
                        (None, Some(m)) => Some(m.clone()),
                        _ => None,
                    };

                    let mut stats = if let Some(content) = &msg.content {
                        extract_tool_stats(content)
                    } else {
                        Stats::default()
                    };

                    if let Some(usage) = &msg.usage {
                        stats.input_tokens = usage.input;
                        stats.output_tokens = usage.output;
                        stats.cache_read_tokens = usage.cache_read;
                        stats.cache_creation_tokens = usage.cache_write;
                        stats.cached_tokens = usage.cache_read + usage.cache_write;

                        if let Some(cost) = &usage.cost {
                            stats.cost = cost.total;
                        }
                    }

                    let msg_timestamp = msg.timestamp.unwrap_or(0);
                    let global_hash = hash_text(&format!(
                        "{}_{}_{}_{}",
                        conversation_hash,
                        timestamp.to_rfc3339(),
                        msg_timestamp,
                        stats.output_tokens
                    ));

                    messages.push(ConversationMessage {
                        application: Application::PiAgent,
                        date: timestamp,
                        project_hash: project_hash.to_string(),
                        conversation_hash: conversation_hash.to_string(),
                        local_hash: Some(global_hash.clone()),
                        global_hash,
                        model: model_str,
                        stats,
                        role: MessageRole::Assistant,
                        uuid: None,
                        session_name: None,
                    });
                } else if msg.role == "user" {
                    let global_hash = hash_text(&format!(
                        "{}_{}_user",
                        conversation_hash,
                        timestamp.to_rfc3339()
                    ));

                    messages.push(ConversationMessage {
                        application: Application::PiAgent,
                        date: timestamp,
                        project_hash: project_hash.to_string(),
                        conversation_hash: conversation_hash.to_string(),
                        local_hash: None,
                        global_hash,
                        model: None,
                        stats: Stats::default(),
                        role: MessageRole::User,
                        uuid: None,
                        session_name: None,
                    });
                }
                // Skip other roles (e.g., toolResult)

                current_offset += bytes_read as u64;
                last_successful_offset = current_offset;
            }
            Ok(_) => {
                current_offset += bytes_read as u64;
                last_successful_offset = current_offset;
            }
            Err(e) => {
                if !is_complete_line {
                    break;
                } else {
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

#[async_trait]
impl Analyzer for PiAgentAnalyzer {
    fn display_name(&self) -> &'static str {
        "Pi Agent"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{home_str}/.pi/agent/sessions/*/*.jsonl"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let mut sources = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let sessions_dir = home_dir.join(".pi").join("agent").join("sessions");

            if sessions_dir.is_dir() {
                // Pattern: ~/.pi/agent/sessions/*/*.jsonl
                for entry in WalkDir::new(&sessions_dir)
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
        let all_entries: Vec<ConversationMessage> = sources
            .into_par_iter()
            .filter_map(|source| {
                let project_hash = extract_and_hash_project_id(&source.path);
                let conversation_hash = hash_text(&source.path.to_string_lossy());

                match File::open(&source.path) {
                    Ok(file) => {
                        match parse_jsonl_file(&source.path, file, &project_hash, &conversation_hash)
                        {
                            Ok((messages, _)) => Some(messages),
                            Err(e) => {
                                eprintln!(
                                    "Failed to parse Pi Agent session {}: {e:#}",
                                    source.path.display()
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Failed to open Pi Agent session {}: {e}",
                            source.path.display()
                        );
                        None
                    }
                }
            })
            .flat_map(|messages| messages)
            .collect();

        // Deduplicate by local hash
        Ok(crate::utils::deduplicate_by_local_hash_parallel(all_entries))
    }

    async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
        let sources = self.discover_data_sources()?;
        let messages = self.parse_conversations(sources).await?;
        let daily_stats = crate::utils::aggregate_by_date(&messages);

        let num_conversations = daily_stats
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        Ok(AgenticCodingToolStats {
            analyzer_name: self.display_name().to_string(),
            daily_stats,
            messages,
            num_conversations,
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
        let (messages, _) = parse_jsonl_file(&source.path, file, &project_hash, &conversation_hash)?;

        metadata.last_parsed_offset = metadata.size;

        let daily_contributions = crate::utils::aggregate_by_date_simple(&messages);

        Ok(FileCacheEntry {
            metadata,
            messages,
            daily_contributions,
            cached_model: None,
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

        if let Some(cached_entry) = cached {
            // Check for truncation - requires full reparse
            if cached_entry.metadata.needs_full_reparse(&current_meta) {
                return self.parse_single_file(source);
            }

            // Check for append - can do delta parsing
            if cached_entry.metadata.is_append_only(&current_meta) {
                let project_hash = extract_and_hash_project_id(&source.path);
                let conversation_hash = hash_text(&source.path.to_string_lossy());

                let delta_result = parse_jsonl_file_delta(
                    &source.path,
                    cached_entry.metadata.last_parsed_offset,
                    current_meta.size,
                    &project_hash,
                    &conversation_hash,
                );

                let (new_messages, new_offset) = match delta_result {
                    Ok(result) => result,
                    Err(_) => return self.parse_single_file(source),
                };

                let mut all_messages = cached_entry.messages.clone();
                all_messages.extend(new_messages);

                let daily_contributions = crate::utils::aggregate_by_date_simple(&all_messages);

                return Ok(FileCacheEntry {
                    metadata: FileMetadata {
                        size: current_meta.size,
                        modified: current_meta.modified,
                        last_parsed_offset: new_offset,
                    },
                    messages: all_messages,
                    daily_contributions,
                    cached_model: None,
                });
            }

            // File unchanged - use cached entry directly
            if cached_entry.metadata.is_unchanged(&current_meta) {
                return Ok(cached_entry.clone());
            }
        }

        // No cache or file changed - full reparse
        self.parse_single_file(source)
    }
}
