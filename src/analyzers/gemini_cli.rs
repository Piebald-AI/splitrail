use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::models::{calculate_cache_cost, calculate_input_cost, calculate_output_cost};
use crate::types::{Application, ConversationMessage, FileCategory, MessageRole, Stats};
use crate::utils::{deserialize_utc_timestamp, hash_text};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct GeminiCliAnalyzer;

impl GeminiCliAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".gemini").join("tmp"))
    }
}

// Gemini CLI-specific data structures following the plan's simplified flat approach
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCliSession {
    session_id: String,
    project_hash: String,
    start_time: String,
    last_updated: String,
    messages: Vec<GeminiCliMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum GeminiCliMessage {
    User {
        id: String,
        #[serde(deserialize_with = "deserialize_utc_timestamp")]
        timestamp: DateTime<Utc>,
        content: String,
    },
    Gemini {
        id: String,
        #[serde(deserialize_with = "deserialize_utc_timestamp")]
        timestamp: DateTime<Utc>,
        content: String,
        model: String,
        #[serde(default)]
        thoughts: Vec<simd_json::OwnedValue>,
        tokens: Option<GeminiCliTokens>,
        #[serde(rename = "toolCalls", default)]
        tool_calls: Vec<simd_json::OwnedValue>,
    },
    System {
        id: String,
        #[serde(deserialize_with = "deserialize_utc_timestamp")]
        timestamp: DateTime<Utc>,
        content: String,
    },
    Error {
        id: String,
        #[serde(deserialize_with = "deserialize_utc_timestamp")]
        timestamp: DateTime<Utc>,
        content: String,
    },
    Info {
        id: String,
        #[serde(deserialize_with = "deserialize_utc_timestamp")]
        timestamp: DateTime<Utc>,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiCliTokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cached: u64,
    #[serde(default)]
    thoughts: u64,
    #[serde(default)]
    tool: u64,
    #[serde(default)]
    total: u64,
}

// Tool extraction and file operation mapping
fn extract_tool_stats(tool_calls: &[simd_json::OwnedValue]) -> Stats {
    let mut stats = Stats::default();

    for tool_call in tool_calls {
        let tool_name = if let Some(tool_name) = tool_call.get("name").and_then(|v| v.as_str()) {
            tool_name
        } else {
            continue;
        };
        match tool_name {
            "read_many_files" => {
                let paths = if let Some(paths) = tool_call
                    .get("args")
                    .and_then(|v| v.get("paths"))
                    .and_then(|v| v.as_array())
                {
                    paths
                } else {
                    continue;
                };
                stats.files_read += paths.len() as u64;

                // Categorize files and estimate composition stats
                for path in paths {
                    let path_str = if let Some(path_str) = path.as_str() {
                        path_str
                    } else {
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

                // Simple estimation without complex heuristics
                stats.lines_read += (paths.len() as u64) * 100;
                stats.bytes_read += (paths.len() as u64) * 8000;
            }
            "replace" => {
                stats.files_edited += 1;
                // Simple counting without complex content analysis
                stats.lines_edited += 10; // Conservative estimate
                stats.bytes_edited += 800;
            }
            "run_shell_command" => {
                stats.terminal_commands += 1;
            }
            "list_directory" => {
                // Treat as a lightweight read operation
                stats.files_read += 1;
            }
            _ => {} // Unknown tools - just skip
        }
    }

    // Use existing utility functions for line estimation
    stats.lines_added = (stats.lines_edited / 2).max(1); // Simple estimate
    stats.lines_deleted = (stats.lines_edited / 3).max(1); // Simple estimate

    stats
}

// Helper function to extract project ID from Gemini CLI file path and hash it
fn extract_and_hash_project_id_gemini_cli(file_path: &Path) -> String {
    // Gemini CLI path format: ~/.gemini/tmp/{PROJECT_ID}/chats/{session}.json
    // Example: "/home/user/.gemini/tmp/project-abc123/chats/session.json"

    let path_components: Vec<_> = file_path.components().collect();
    for (i, component) in path_components.iter().enumerate() {
        if let std::path::Component::Normal(name) = component
            && name.to_str() == Some("tmp")
            && i + 1 < path_components.len()
            && let std::path::Component::Normal(project_id) = &path_components[i + 1]
            && let Some(project_id_str) = project_id.to_str()
        {
            return hash_text(project_id_str);
        }
    }

    hash_text(&file_path.to_string_lossy())
}

// Cost calculation using the centralized model system
fn calculate_gemini_cost(tokens: &GeminiCliTokens, model_name: &str) -> f64 {
    let total_input_tokens = tokens.input + tokens.thoughts + tokens.tool;

    let input_cost = calculate_input_cost(model_name, total_input_tokens);
    let output_cost = calculate_output_cost(model_name, tokens.output);
    let cache_cost = calculate_cache_cost(model_name, 0, tokens.cached); // Gemini CLI doesn't have cache creation

    input_cost + output_cost + cache_cost
}

// JSON session parsing (not JSONL)
fn parse_json_session_file(file_path: &Path) -> Result<Vec<ConversationMessage>> {
    let project_hash = extract_and_hash_project_id_gemini_cli(file_path);
    let file_path_str = file_path.to_string_lossy();
    let mut entries = Vec::new();
    let mut fallback_session_name: Option<String> = None;

    // Parse the complete session JSON
    let session: GeminiCliSession =
        simd_json::from_slice(&mut std::fs::read_to_string(file_path)?.into_bytes())?;

    // Process each message in the session
    for message in session.messages {
        match message {
            GeminiCliMessage::User {
                id: _,
                timestamp,
                content,
            } => {
                if fallback_session_name.is_none() && !content.is_empty() {
                    let text_str = content;
                    let truncated = if text_str.chars().count() > 50 {
                        let chars: String = text_str.chars().take(50).collect();
                        format!("{}...", chars)
                    } else {
                        text_str
                    };
                    fallback_session_name = Some(truncated);
                }

                entries.push(ConversationMessage {
                    date: timestamp,
                    application: Application::GeminiCli,
                    project_hash: project_hash.clone(),
                    local_hash: None,
                    global_hash: hash_text(&format!(
                        "{}_{}",
                        file_path_str,
                        timestamp.to_rfc3339()
                    )),
                    conversation_hash: hash_text(&file_path.to_string_lossy()),
                    model: None,
                    stats: Stats::default(),
                    role: MessageRole::User,
                    uuid: None,
                    session_name: fallback_session_name.clone(),
                });
            }
            GeminiCliMessage::Gemini {
                id: _,
                timestamp,
                content: _,
                model,
                thoughts: _,
                tokens: Some(tokens),
                tool_calls,
            } => {
                let mut stats = extract_tool_stats(&tool_calls);

                // Update stats with token information
                stats.input_tokens = tokens.input;
                stats.output_tokens = tokens.output;
                stats.reasoning_tokens = tokens.thoughts;
                stats.cache_creation_tokens = 0;
                stats.cache_read_tokens = 0;
                stats.cached_tokens = tokens.cached;
                stats.cost = calculate_gemini_cost(&tokens, &model);
                stats.tool_calls = tool_calls.len() as u32;

                entries.push(ConversationMessage {
                    application: Application::GeminiCli,
                    model: Some(model),
                    local_hash: None,
                    global_hash: hash_text(&format!(
                        "{}_{}",
                        file_path_str,
                        timestamp.to_rfc3339()
                    )),
                    date: timestamp,
                    project_hash: project_hash.clone(),
                    conversation_hash: hash_text(&file_path.to_string_lossy()),
                    stats,
                    role: MessageRole::Assistant,
                    uuid: None,
                    session_name: fallback_session_name.clone(),
                });
            }
            _ => {}
        }
    }

    Ok(entries)
}

#[async_trait]
impl Analyzer for GeminiCliAnalyzer {
    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{home_str}/.gemini/tmp/*/chats/*.json"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|tmp_dir| WalkDir::new(tmp_dir).min_depth(3).max_depth(3).into_iter())
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path().extension().is_some_and(|ext| ext == "json")
                    && e.path()
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|name| name == "chats")
            })
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
            .flat_map(|tmp_dir| WalkDir::new(tmp_dir).min_depth(3).max_depth(3).into_iter())
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_type().is_file()
                    && e.path().extension().is_some_and(|ext| ext == "json")
                    && e.path()
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|name| name == "chats")
            })
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        parse_json_session_file(&source.path)
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
        // Must be a .json file in a "chats" directory
        path.is_file()
            && path.extension().is_some_and(|ext| ext == "json")
            && path
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|name| name == "chats")
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }
}
