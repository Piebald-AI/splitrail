use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::models::{calculate_cache_cost, calculate_input_cost, calculate_output_cost};
use crate::types::{Application, ConversationMessage, FileCategory, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct QwenCodeAnalyzer;

impl QwenCodeAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".qwen").join("projects"))
    }
}

// Qwen Code session log format (since Qwen Code ~0.14+). Each session is a
// JSONL file under `~/.qwen/projects/{PROJECT}/chats/{session}.jsonl`, where
// every line is a record tagged with a `type` field. This mirrors the
// Claude-Code-style transcript format that Qwen Code (and recent Gemini CLI
// builds) adopted, replacing the old `~/.qwen/tmp/*/chats/*.json` single-blob
// session format. See issue #190.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct QwenCodeRecord {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(rename = "type", default)]
    record_type: String,
    #[serde(default, deserialize_with = "deserialize_optional_utc_timestamp")]
    timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    message: Option<QwenCodeMessageBody>,
    #[serde(rename = "usageMetadata", default)]
    usage_metadata: Option<QwenCodeUsageMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct QwenCodeMessageBody {
    #[serde(default)]
    parts: Vec<QwenCodePart>,
}

/// A single `Part` of a Qwen Code message. A part may carry plain `text`
/// (optionally flagged as a `thought`), a `functionCall` (a tool invocation),
/// or a `functionResponse` (a tool result). Unknown part kinds are ignored.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct QwenCodePart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thought: Option<bool>,
    #[serde(rename = "functionCall", default)]
    function_call: Option<QwenCodeFunctionCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct QwenCodeFunctionCall {
    #[serde(default)]
    name: String,
    #[serde(default)]
    args: Option<simd_json::OwnedValue>,
}

/// Token accounting attached to each `assistant` record. Field names match the
/// Gemini `usageMetadata` schema that Qwen Code emits. Note that
/// `promptTokenCount` is the *full* input token count and already includes
/// `cachedContentTokenCount`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct QwenCodeUsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt: u64,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates: u64,
    #[serde(rename = "thoughtsTokenCount", default)]
    thoughts: u64,
    #[serde(rename = "cachedContentTokenCount", default)]
    cached: u64,
    #[serde(rename = "totalTokenCount", default)]
    #[allow(dead_code)]
    total: u64,
}

fn deserialize_optional_utc_timestamp<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Tolerate a missing/null/empty timestamp instead of aborting the parse.
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        Some(s) if !s.is_empty() => DateTime::parse_from_rfc3339(&s)
            .map(|dt| Some(dt.into()))
            .map_err(serde::de::Error::custom),
        _ => Ok(None),
    }
}

impl QwenCodePart {
    /// Plain text of this part, ignoring "thought" parts (model reasoning) and
    /// non-text parts (tool calls/results).
    fn visible_text(&self) -> Option<&str> {
        if self.thought.unwrap_or(false) {
            return None;
        }
        self.text.as_deref().filter(|t| !t.is_empty())
    }
}

impl QwenCodeMessageBody {
    fn concatenated_text(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            if let Some(text) = part.visible_text() {
                out.push_str(text);
            }
        }
        out
    }

    fn function_calls(&self) -> impl Iterator<Item = &QwenCodeFunctionCall> {
        self.parts
            .iter()
            .filter_map(|part| part.function_call.as_ref())
    }
}

// Tool extraction and file operation mapping. Tool invocations now appear as
// `functionCall` parts inside `assistant` records rather than a dedicated
// `toolCalls` array.
fn extract_tool_stats<'a>(function_calls: impl Iterator<Item = &'a QwenCodeFunctionCall>) -> Stats {
    let mut stats = Stats::default();

    for call in function_calls {
        stats.tool_calls += 1;
        match call.name.as_str() {
            "read_many_files" => {
                let paths = call
                    .args
                    .as_ref()
                    .and_then(|v| v.get("paths"))
                    .and_then(|v| v.as_array());
                let Some(paths) = paths else {
                    continue;
                };
                stats.files_read += paths.len() as u64;

                for path in paths {
                    let Some(path_str) = path.as_str() else {
                        continue;
                    };
                    let ext = std::path::Path::new(path_str)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    let category = FileCategory::from_extension(ext);
                    let estimated_lines = 100; // Estimate lines per file
                    match category {
                        FileCategory::SourceCode => stats.code_lines += estimated_lines,
                        FileCategory::Documentation => stats.docs_lines += estimated_lines,
                        FileCategory::Data => stats.data_lines += estimated_lines,
                        FileCategory::Media => stats.media_lines += estimated_lines,
                        FileCategory::Config => stats.config_lines += estimated_lines,
                        FileCategory::Other => stats.other_lines += estimated_lines,
                    }
                }

                stats.lines_read += (paths.len() as u64) * 100;
                stats.bytes_read += (paths.len() as u64) * 8000;
            }
            "read_file" => {
                stats.files_read += 1;
                stats.lines_read += 100;
                stats.bytes_read += 8000;
            }
            "replace" | "edit" => {
                stats.files_edited += 1;
                stats.lines_edited += 10; // Conservative estimate
                stats.bytes_edited += 800;
            }
            "write_file" => {
                stats.files_added += 1;
                stats.lines_added += 10;
                stats.bytes_added += 800;
            }
            "run_shell_command" => {
                stats.terminal_commands += 1;
            }
            "list_directory" => {
                // Treat as a lightweight read operation.
                stats.files_read += 1;
            }
            _ => {} // Unknown tools - just count the call above.
        }
    }

    // Estimate add/delete splits for edits (mirrors the historical behaviour).
    // Accumulate rather than replace, so a turn mixing `write_file` with
    // `edit`/`replace` keeps both contributions.
    if stats.lines_edited > 0 {
        stats.lines_added += (stats.lines_edited / 2).max(1);
        stats.lines_deleted += (stats.lines_edited / 3).max(1);
    }

    stats
}

// Helper function to extract project ID from Qwen Code file path and hash it.
fn extract_and_hash_project_id_qwen_code(file_path: &Path) -> String {
    // Qwen Code path format: ~/.qwen/projects/{PROJECT_ID}/chats/{session}.jsonl
    let path_components: Vec<_> = file_path.components().collect();
    for (i, component) in path_components.iter().enumerate() {
        if let std::path::Component::Normal(name) = component
            && name.to_str() == Some("projects")
            && i + 1 < path_components.len()
            && let std::path::Component::Normal(project_id) = &path_components[i + 1]
            && let Some(project_id_str) = project_id.to_str()
        {
            return hash_text(project_id_str);
        }
    }

    hash_text(&file_path.to_string_lossy())
}

// Cost calculation using the centralized model system.
fn calculate_qwen_cost(usage: &QwenCodeUsageMetadata, model_name: &str) -> f64 {
    // `prompt` includes the cached portion; bill non-cached input at the input
    // rate and cached input at the cache-read rate. Reasoning (`thoughts`)
    // tokens are billed at the *output* rate, matching Qwen's pricing.
    let non_cached_input = usage.prompt.saturating_sub(usage.cached);

    let input_cost = calculate_input_cost(model_name, non_cached_input);
    let output_cost = calculate_output_cost(model_name, usage.candidates + usage.thoughts);
    let cache_cost = calculate_cache_cost(model_name, 0, usage.cached); // No cache-creation concept here.

    input_cost + output_cost + cache_cost
}

fn is_qwen_code_chat_path(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "jsonl" || ext == "json")
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == "chats")
}

fn is_internal_session_context(text: &str) -> bool {
    text.trim_start().starts_with("<session_context>")
}

// JSONL session parsing.
pub fn parse_jsonl_session_file(file_path: &Path) -> Result<Vec<ConversationMessage>> {
    let project_hash = extract_and_hash_project_id_qwen_code(file_path);
    let file_path_str = file_path.to_string_lossy();
    let conversation_hash = hash_text(&file_path.to_string_lossy());
    let content = std::fs::read_to_string(file_path)?;

    let mut entries = Vec::new();
    let mut fallback_session_name: Option<String> = None;

    for (line_idx, line) in content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let mut line_bytes = line.as_bytes().to_vec();
        let record: QwenCodeRecord = match simd_json::from_slice(&mut line_bytes) {
            Ok(record) => record,
            // Skip malformed lines rather than aborting the whole session.
            Err(_) => continue,
        };

        let Some(timestamp) = record.timestamp else {
            continue;
        };

        let global_hash = match &record.uuid {
            Some(uuid) => hash_text(&format!("{file_path_str}_{uuid}")),
            // No uuid: fall back to path + timestamp + line index so that two
            // records sharing a timestamp don't collide and get deduplicated.
            None => hash_text(&format!(
                "{}_{}_{}",
                file_path_str,
                timestamp.to_rfc3339(),
                line_idx
            )),
        };

        match record.record_type.as_str() {
            "user" => {
                let text = record
                    .message
                    .as_ref()
                    .map(QwenCodeMessageBody::concatenated_text)
                    .unwrap_or_default();

                // Skip system-injected context that is not a real user turn.
                if is_internal_session_context(&text) {
                    continue;
                }

                if fallback_session_name.is_none() && !text.is_empty() {
                    let truncated = if text.chars().count() > 50 {
                        let chars: String = text.chars().take(50).collect();
                        format!("{chars}...")
                    } else {
                        text
                    };
                    fallback_session_name = Some(truncated);
                }

                entries.push(ConversationMessage {
                    date: timestamp,
                    application: Application::QwenCode,
                    project_hash: project_hash.clone(),
                    local_hash: None,
                    global_hash,
                    conversation_hash: conversation_hash.clone(),
                    model: None,
                    stats: Stats::default(),
                    role: MessageRole::User,
                    uuid: record.uuid.clone(),
                    session_name: fallback_session_name.clone(),
                });
            }
            "assistant" => {
                let Some(usage) = record.usage_metadata else {
                    // An assistant record without usage metadata carries no
                    // token information worth recording.
                    continue;
                };

                let mut stats = record
                    .message
                    .as_ref()
                    .map(|body| extract_tool_stats(body.function_calls()))
                    .unwrap_or_default();

                // `promptTokenCount` already includes the cached tokens, so
                // record only the non-cached portion as input to avoid
                // double-counting in the input/cached columns.
                stats.input_tokens = usage.prompt.saturating_sub(usage.cached);
                stats.output_tokens = usage.candidates;
                stats.reasoning_tokens = usage.thoughts;
                stats.cache_creation_tokens = 0;
                stats.cache_read_tokens = 0;
                stats.cached_tokens = usage.cached;
                let model = record.model.unwrap_or_default();
                stats.cost = calculate_qwen_cost(&usage, &model);

                entries.push(ConversationMessage {
                    application: Application::QwenCode,
                    model: Some(model),
                    local_hash: None,
                    global_hash,
                    date: timestamp,
                    project_hash: project_hash.clone(),
                    conversation_hash: conversation_hash.clone(),
                    stats,
                    role: MessageRole::Assistant,
                    uuid: record.uuid.clone(),
                    session_name: fallback_session_name.clone(),
                });
            }
            // `tool_result`, `system` (telemetry, snapshots, slash commands),
            // and any other record types carry no billable usage of their own.
            _ => {}
        }
    }

    Ok(entries)
}

#[async_trait]
impl Analyzer for QwenCodeAnalyzer {
    fn display_name(&self) -> &'static str {
        "Qwen Code"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{home_str}/.qwen/projects/*/chats/*.jsonl"));
            patterns.push(format!("{home_str}/.qwen/projects/*/chats/*.json"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|projects_dir| WalkDir::new(projects_dir).into_iter())
            .filter_map(|e| e.ok())
            .filter(|e| is_qwen_code_chat_path(e.path()))
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
            .flat_map(|projects_dir| WalkDir::new(projects_dir).into_iter())
            .filter_map(|e| e.ok())
            .any(|e| is_qwen_code_chat_path(e.path()))
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        parse_jsonl_session_file(&source.path)
    }

    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        let all_messages: Vec<ConversationMessage> = sources
            .par_iter()
            .flat_map(|source| self.parse_source(source).unwrap_or_default())
            .collect();
        crate::utils::deduplicate_by_global_hash(all_messages)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .collect()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        is_qwen_code_chat_path(path)
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }
}
