use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::analyzer::{Analyzer, DataSource};
use crate::models::calculate_total_cost;
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, FileCategory, MessageRole, Stats,
};

pub struct CodexAnalyzer;

impl CodexAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Analyzer for CodexAnalyzer {
    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = std::env::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{home_str}/.codex/sessions/**/*.jsonl"));
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
        // Parse all data sources in parallel while properly propagating any
        // error that occurs while processing an individual file.  Rayon’s
        // `try_reduce` utility allows us to aggregate `Result` values coming
        // from each parallel worker without having to fall back to
        // sequential processing.

        use rayon::prelude::*;

        let aggregated: Result<Vec<ConversationMessage>> = sources
            .into_par_iter()
            .map(|source| parse_codex_jsonl_file(&source.path))
            // Start the reduction with an empty vector and extend it with the
            // entries coming from each successfully-parsed file.
            .try_reduce(Vec::new, |mut acc, mut entries| {
                acc.append(&mut entries);
                Ok(acc)
            });

        // For Codex, we don't need to deduplicate since each session is separate
        // but we keep the logic encapsulated for future changes.
        aggregated
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

// Codex specific implementation functions

// Helper function to generate hash from conversation file path and timestamp
fn generate_conversation_hash(conversation_file: &str, timestamp: &str) -> String {
    let input = format!("{conversation_file}:{timestamp}");
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..8]) // Use first 8 bytes (16 hex chars) for consistency
}

// CODEX JSONL FILES SCHEMA

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexTokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexMessage {
    #[serde(rename = "type")]
    message_type: String,
    role: Option<String>,
    content: Option<serde_json::Value>,
    token_usage: Option<CodexTokenUsage>,
    timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexShellAction {
    #[serde(rename = "type")]
    action_type: String,
    command: Vec<String>,
    timeout_ms: Option<u64>,
    working_directory: Option<String>,
    env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexShellCall {
    #[serde(rename = "type")]
    call_type: String,
    id: Option<String>,
    call_id: Option<String>,
    status: Option<String>,
    action: Option<CodexShellAction>,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexFunctionOutput {
    #[serde(rename = "type")]
    output_type: String,
    call_id: Option<String>,
    output: Option<serde_json::Value>,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexSessionHeader {
    id: String,
    timestamp: String,
    instructions: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum CodexEntry {
    SessionHeader(CodexSessionHeader),
    Message(CodexMessage),
    ShellCall(CodexShellCall),
    FunctionOutput(CodexFunctionOutput),
    // Fallback for unknown entries
    Unknown(serde_json::Value),
}

fn parse_codex_jsonl_file(file_path: &Path) -> Result<Vec<ConversationMessage>> {
    let conversation_file = file_path.to_string_lossy().to_string();
    let mut entries = Vec::new();

    let file = File::open(file_path)?;

    let reader = BufReader::with_capacity(64 * 1024, file);
    let mut session_model: Option<String> = None;
    let mut pending_shell_calls: HashMap<String, CodexShellCall> = HashMap::new();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.trim().is_empty() {
            continue;
        }

        let entry = match serde_json::from_str::<CodexEntry>(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        match entry {
            CodexEntry::SessionHeader(header) => {
                session_model = header.model;
            }
            CodexEntry::Message(message) => {
                if message.message_type == "message"
                    && let Some(role) = &message.role
                {
                    match role.as_str() {
                        "user" => {
                            entries.push(ConversationMessage {
                                timestamp: message.timestamp.clone(),
                                application: Application::CodexCli,
                                hash: generate_conversation_hash(
                                    &conversation_file,
                                    &message.timestamp,
                                ),
                                project_hash: "".to_string(),
                                model: None,
                                stats: Stats::default(),
                                role: MessageRole::User,
                            });
                        }
                        "assistant" => {
                            let model_name = session_model
                                .clone()
                                .unwrap_or_else(|| "unknown".to_string());

                            if let Some(usage) = message.token_usage {
                                let total_output_tokens =
                                    usage.output_tokens + usage.reasoning_output_tokens;

                                let timestamp = message.timestamp.clone();
                                let stats = Stats {
                                    input_tokens: usage.input_tokens,
                                    output_tokens: total_output_tokens,
                                    cache_creation_tokens: 0,
                                    cache_read_tokens: 0,
                                    cached_tokens: usage.cached_input_tokens,
                                    cost: calculate_cost_from_tokens(&usage, &model_name),
                                    tool_calls: 0,
                                    ..Default::default()
                                };

                                entries.push(ConversationMessage {
                                    application: Application::CodexCli,
                                    model: Some(model_name),
                                    timestamp: timestamp.clone(),
                                    hash: generate_conversation_hash(
                                        &conversation_file,
                                        &timestamp,
                                    ),
                                    project_hash: "".to_string(),
                                    stats,
                                    role: MessageRole::Assistant,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            CodexEntry::ShellCall(shell_call) => {
                if shell_call.call_type == "local_shell_call" {
                    if let Some(call_id) = &shell_call.call_id {
                        pending_shell_calls.insert(call_id.clone(), shell_call.clone());
                    }

                    // Count as a tool call and create a user message to represent the shell call
                    let stats = if let Some(action) = &shell_call.action {
                        parse_shell_command_for_file_operations(&action.command)
                    } else {
                        Stats::default()
                    };

                    let timestamp = shell_call
                        .timestamp
                        .clone()
                        .unwrap_or_else(|| "".to_string());

                    entries.push(ConversationMessage {
                        timestamp: timestamp.clone(),
                        application: Application::CodexCli,
                        hash: generate_conversation_hash(&conversation_file, &timestamp),
                        project_hash: "".to_string(),
                        model: None,
                        stats,
                        role: MessageRole::User,
                    });
                }
            }
            CodexEntry::FunctionOutput(_) => {
                // We can track function outputs if needed, but for now just skip
                // Could be used to track command success/failure status
            }
            CodexEntry::Unknown(_) => {
                // Skip unknown entries
            }
        }
    }

    Ok(entries)
}

fn parse_shell_command_for_file_operations(command: &[String]) -> Stats {
    let mut stats = Stats::default();

    if command.is_empty() {
        return stats;
    }

    // Join the command for easier parsing
    let full_command = command.join(" ");

    // Count basic shell operations
    stats.terminal_commands += 1;

    // Parse the actual command (usually after "bash -lc")
    let actual_command = if command.len() >= 3 && command[0] == "bash" && command[1] == "-lc" {
        &command[2]
    } else {
        &full_command
    };

    // Detect file operations based on command patterns
    if actual_command.contains("rg ") || actual_command.contains("grep ") {
        stats.file_content_searches += 1;
    }

    if actual_command.contains("--files") || actual_command.contains("find ") {
        stats.file_searches += 1;
    }

    // Detect file reads
    if actual_command.contains("cat ")
        || actual_command.contains("head ")
        || actual_command.contains("tail ")
        || actual_command.contains("less ")
        || actual_command.contains("more ")
        || actual_command.contains("sed -n")
    {
        stats.files_read += 1;

        // Try to extract file paths and categorize
        extract_file_paths_from_command(actual_command, &mut stats);

        // Estimate lines read (rough approximation)
        if actual_command.contains("sed -n") {
            // Try to extract line numbers from sed command
            if let Some(lines) = extract_line_count_from_sed(actual_command) {
                stats.lines_read += lines;
                stats.bytes_read += lines * 80; // Rough estimate
            }
        } else {
            stats.lines_read += 100; // Default estimate
            stats.bytes_read += 8000; // Default estimate
        }
    }

    // Detect file writes
    if actual_command.contains(" > ")
        || actual_command.contains(" >> ")
        || actual_command.contains("tee ")
        || actual_command.contains("echo ")
    {
        stats.files_edited += 1;
        stats.lines_edited += 10; // Rough estimate
        stats.bytes_edited += 800; // Rough estimate
    }

    // Detect file edits
    if actual_command.contains("sed -i") || actual_command.contains("awk") {
        stats.files_edited += 1;
        stats.lines_edited += 5; // Rough estimate
        stats.bytes_edited += 400; // Rough estimate
    }

    stats
}

fn extract_file_paths_from_command(command: &str, stats: &mut Stats) {
    // This is a rough implementation - it tries to find file paths in the command
    let words: Vec<&str> = command.split_whitespace().collect();

    for word in words {
        // Skip flags and common non-file words
        if word.starts_with('-') || word.starts_with('/') && word.len() < 3 {
            continue;
        }

        // Look for file extensions
        if let Some(dot_pos) = word.rfind('.') {
            let ext = &word[dot_pos + 1..];
            if ext.len() <= 5 && ext.chars().all(|c| c.is_alphabetic()) {
                let category = FileCategory::from_extension(ext);
                // Estimate lines per file operation
                let estimated_lines = 50; // Conservative estimate
                match category {
                    FileCategory::SourceCode => stats.code_lines += estimated_lines,
                    FileCategory::Documentation => stats.docs_lines += estimated_lines,
                    FileCategory::Data => stats.data_lines += estimated_lines,
                    FileCategory::Media => stats.media_lines += estimated_lines,
                    FileCategory::Config => stats.config_lines += estimated_lines,
                    FileCategory::Other => stats.other_lines += estimated_lines,
                }
            }
        }
    }
}

fn extract_line_count_from_sed(command: &str) -> Option<u64> {
    // Try to extract line numbers from sed -n 'X,Yp' commands
    if let Some(start) = command.find("sed -n '") {
        let after_quote = &command[start + 8..];
        if let Some(end) = after_quote.find('\'') {
            let range = &after_quote[..end];
            if let Some(range_part) = range.strip_suffix('p')
                && let Some(comma) = range_part.find(',')
            {
                let start_str = &range_part[..comma];
                let end_str = &range_part[comma + 1..];
                if let (Ok(start), Ok(end)) = (start_str.parse::<u64>(), end_str.parse::<u64>()) {
                    return Some(end - start + 1);
                }
            }
        }
    }
    None
}

fn calculate_cost_from_tokens(usage: &CodexTokenUsage, model_name: &str) -> f64 {
    let total_output_tokens = usage.output_tokens + usage.reasoning_output_tokens;

    calculate_total_cost(
        model_name,
        usage.input_tokens,
        total_output_tokens,
        0, // Codex doesn't have separate cache creation tokens
        usage.cached_input_tokens,
    )
}
