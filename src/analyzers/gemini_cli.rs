use crate::analyzer::{Analyzer, DataSource};
use crate::models::{calculate_cache_cost, calculate_input_cost, calculate_output_cost};
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, FileCategory, MessageRole, Stats,
};
use anyhow::Result;
use async_trait::async_trait;
use glob::glob;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;

pub struct GeminiCliAnalyzer;

impl GeminiCliAnalyzer {
    pub fn new() -> Self {
        Self
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
        timestamp: String,
        content: String,
    },
    Gemini {
        id: String,
        timestamp: String,
        content: String,
        model: String,
        #[serde(default)]
        thoughts: Vec<serde_json::Value>,
        tokens: Option<GeminiCliTokens>,
        #[serde(rename = "toolCalls", default)]
        tool_calls: Vec<serde_json::Value>,
    },
    System {
        id: String,
        timestamp: String,
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
fn extract_tool_stats(tool_calls: &[serde_json::Value]) -> Stats {
    let mut stats = Stats::default();

    for tool_call in tool_calls {
        if let Some(tool_name) = tool_call.get("name").and_then(|v| v.as_str()) {
            match tool_name {
                "read_many_files" => {
                    if let Some(paths) = tool_call
                        .get("args")
                        .and_then(|v| v.get("paths"))
                        .and_then(|v| v.as_array())
                    {
                        stats.files_read += paths.len() as u64;

                        // Categorize files and estimate composition stats
                        for path in paths {
                            if let Some(path_str) = path.as_str() {
                                let ext = std::path::Path::new(path_str)
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .unwrap_or("");
                                let category = FileCategory::from_extension(ext);
                                let estimated_lines = 100; // Estimate lines per file
                                match category {
                                    FileCategory::SourceCode => stats.code_lines += estimated_lines,
                                    FileCategory::Documentation => {
                                        stats.docs_lines += estimated_lines
                                    }
                                    FileCategory::Data => stats.data_lines += estimated_lines,
                                    FileCategory::Media => stats.media_lines += estimated_lines,
                                    FileCategory::Config => stats.config_lines += estimated_lines,
                                    FileCategory::Other => stats.other_lines += estimated_lines,
                                }
                            }
                        }

                        // Simple estimation without complex heuristics
                        stats.lines_read += (paths.len() as u64) * 100;
                        stats.bytes_read += (paths.len() as u64) * 8000;
                    }
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
    }

    // Use existing utility functions for line estimation
    stats.lines_added = (stats.lines_edited / 2).max(1); // Simple estimate
    stats.lines_deleted = (stats.lines_edited / 3).max(1); // Simple estimate

    stats
}

// Helper function to generate hash from conversation file path and timestamp
fn generate_conversation_hash(conversation_file: &str, timestamp: &str) -> String {
    let input = format!("{conversation_file}:{timestamp}");
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..8]) // Use first 8 bytes (16 hex chars) for consistency
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
            // Hash the project ID using the same algorithm as the rest of the app
            let mut hasher = Sha256::new();
            hasher.update(project_id_str.as_bytes());
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

// Cost calculation using the centralized model system
fn calculate_gemini_cost(tokens: &GeminiCliTokens, model_name: &str) -> f64 {
    let total_input_tokens = tokens.input + tokens.thoughts + tokens.tool;

    let input_cost = calculate_input_cost(model_name, total_input_tokens);
    let output_cost = calculate_output_cost(model_name, tokens.output);
    let cache_cost = calculate_cache_cost(model_name, 0, tokens.cached); // Gemini CLI doesn't have cache creation

    input_cost + output_cost + cache_cost
}

// JSON session parsing (not JSONL)
fn parse_json_session_file(file_path: &Path) -> Vec<ConversationMessage> {
    let conversation_file = file_path.to_string_lossy().to_string();
    let project_hash = extract_and_hash_project_id_gemini_cli(file_path);
    let mut entries = Vec::new();

    // Read entire file (not line-by-line like JSONL)
    let file_content = match std::fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!(
                "Failed to read Gemini CLI session file {}: {}",
                file_path.display(),
                e
            );
            return entries;
        }
    };

    // Parse the complete session JSON
    let session: GeminiCliSession = match serde_json::from_str(&file_content) {
        Ok(session) => session,
        Err(e) => {
            eprintln!(
                "Failed to parse Gemini CLI session JSON {}: {}",
                file_path.display(),
                e
            );
            return entries;
        }
    };

    // Process each message in the session
    for message in session.messages {
        match message {
            GeminiCliMessage::User {
                id: _,
                timestamp,
                content: _,
            } => {
                entries.push(ConversationMessage {
                    timestamp: timestamp.clone(),
                    application: Application::GeminiCli,
                    hash: generate_conversation_hash(&conversation_file, &timestamp),
                    project_hash: project_hash.clone(),
                    model: None,
                    stats: Stats::default(),
                    role: MessageRole::User,
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
                let hash = generate_conversation_hash(&conversation_file, &timestamp);

                // Update stats with token information
                stats.input_tokens = tokens.input;
                stats.output_tokens = tokens.output;
                stats.cache_creation_tokens = 0;
                stats.cache_read_tokens = 0;
                stats.cached_tokens = tokens.cached;
                stats.cost = calculate_gemini_cost(&tokens, &model);
                stats.tool_calls = tool_calls.len() as u32;

                entries.push(ConversationMessage {
                    application: Application::GeminiCli,
                    model: Some(model),
                    timestamp,
                    hash,
                    project_hash: project_hash.clone(),
                    stats,
                    role: MessageRole::Assistant,
                });
            }
            _ => {}
        }
    }

    entries
}

#[async_trait]
impl Analyzer for GeminiCliAnalyzer {
    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = std::env::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{home_str}/.gemini/tmp/*/chats/*.json"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let patterns = self.get_data_glob_patterns();
        let mut sources = Vec::new();

        for pattern in patterns {
            for entry in glob(&pattern)? {
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
        // Parse all session files in parallel
        let all_entries: Vec<ConversationMessage> = sources
            .into_par_iter()
            .flat_map(|source| parse_json_session_file(&source.path))
            .collect();

        // Deduplicate based on hash
        let mut seen_hashes = HashSet::new();
        let deduplicated_entries: Vec<ConversationMessage> = all_entries
            .into_iter()
            .filter(|entry| {
                if seen_hashes.contains(&entry.hash) {
                    false
                } else {
                    seen_hashes.insert(entry.hash.clone());
                    true
                }
            })
            .collect();

        Ok(deduplicated_entries)
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
}
