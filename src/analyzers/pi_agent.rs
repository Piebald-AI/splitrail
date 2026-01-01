use crate::analyzer::{Analyzer, DataSource};
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct PiAgentAnalyzer;

impl PiAgentAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".pi").join("agent").join("sessions"))
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
                if let PiContentBlock::Text { text } = block
                    && !text.is_empty()
                {
                    let truncated = if text.chars().count() > 50 {
                        let chars: String = text.chars().take(50).collect();
                        format!("{}...", chars)
                    } else {
                        text.clone()
                    };
                    return Some(truncated);
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

    if let Some(parent) = file_path.parent()
        && let Some(project_dir) = parent.file_name().and_then(|name| name.to_str())
    {
        return hash_text(project_dir);
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
                    if fallback_session_name.is_none()
                        && let Some(content) = &msg.content
                    {
                        fallback_session_name = extract_first_text(content);
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
        let sources = Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|sessions_dir| {
                WalkDir::new(sessions_dir)
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

    fn is_available(&self) -> bool {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|sessions_dir| {
                WalkDir::new(sessions_dir)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
            })
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        let project_hash = extract_and_hash_project_id(&source.path);
        let conversation_hash = hash_text(&source.path.to_string_lossy());

        let file = File::open(&source.path)?;
        let (messages, _) =
            parse_jsonl_file(&source.path, file, &project_hash, &conversation_hash)?;
        Ok(messages)
    }

    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        let all_messages: Vec<ConversationMessage> = sources
            .par_iter()
            .flat_map(|source| self.parse_source(source).unwrap_or_default())
            .collect();
        crate::utils::deduplicate_by_local_hash(all_messages)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .collect()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        // Must be a .jsonl file at depth 2 from sessions dir
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
}
