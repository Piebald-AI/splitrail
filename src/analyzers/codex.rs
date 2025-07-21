use anyhow::Result;
use async_trait::async_trait;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::analyzer::{
    Analyzer, AnalyzerCapabilities, CachingType, DataFormat, DataSource,
};
use crate::models::MODEL_PRICING;
use crate::types::{
    AgenticCodingToolStats, Application, CompositionStats, ConversationMessage, FileCategory, FileOperationStats, GeneralStats
};
use crate::utils::ModelAbbreviations;

pub struct CodexAnalyzer;

impl CodexAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Analyzer for CodexAnalyzer {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn get_capabilities(&self) -> AnalyzerCapabilities {
        AnalyzerCapabilities {
            supports_todos: false, // Codex doesn't have TodoWrite/TodoRead
            caching_type: Some(CachingType::InputOnly),
            supports_file_operations: true,
            supports_cost_tracking: true,
            supports_model_selection: true,
            supported_tools: vec![
                "local_shell_call".to_string(),
                "Bash".to_string(),
                "Grep".to_string(),
                "Read".to_string(),
                "Write".to_string(),
                "Edit".to_string(),
            ],
        }
    }

    #[rustfmt::skip]
    fn get_model_abbreviations(&self) -> ModelAbbreviations {
        let mut abbrs = ModelAbbreviations::new();
        
        // GPT-4 series
        abbrs.add("gpt-4.1-2025-04-14".to_string(), "GPT4.1".to_string(), "GPT-4.1".to_string());
        abbrs.add("gpt-4.1-mini-2025-04-14".to_string(), "GPT4.1m".to_string(), "GPT-4.1 Mini".to_string());
        abbrs.add("gpt-4.1-nano-2025-04-14".to_string(), "GPT4.1n".to_string(), "GPT-4.1 Nano".to_string());
        abbrs.add("gpt-4.5-preview-2025-02-27".to_string(), "GPT4.5".to_string(), "GPT-4.5 Preview".to_string());
        abbrs.add("gpt-4o-2024-08-06".to_string(), "GPT4o".to_string(), "GPT-4o".to_string());
        abbrs.add("gpt-4o".to_string(), "GPT4o".to_string(), "GPT-4o".to_string());
        abbrs.add("gpt-4o-mini-2024-07-18".to_string(), "GPT4om".to_string(), "GPT-4o Mini".to_string());
        abbrs.add("gpt-4o-mini".to_string(), "GPT4om".to_string(), "GPT-4o Mini".to_string());
        abbrs.add("gpt-4o-audio-preview-2024-12-17".to_string(), "GPT4oa".to_string(), "GPT-4o Audio".to_string());
        abbrs.add("gpt-4o-realtime-preview-2025-06-03".to_string(), "GPT4or".to_string(), "GPT-4o Realtime".to_string());
        abbrs.add("gpt-4o-mini-audio-preview-2024-12-17".to_string(), "GPT4oma".to_string(), "GPT-4o Mini Audio".to_string());
        abbrs.add("gpt-4o-mini-realtime-preview-2024-12-17".to_string(), "GPT4omr".to_string(), "GPT-4o Mini Realtime".to_string());
        abbrs.add("gpt-4o-search-preview-2025-03-11".to_string(), "GPT4os".to_string(), "GPT-4o Search".to_string());
        abbrs.add("gpt-4o-mini-search-preview-2025-03-11".to_string(), "GPT4oms".to_string(), "GPT-4o Mini Search".to_string());
        
        // o1 series
        abbrs.add("o1-2024-12-17".to_string(), "O1".to_string(), "OpenAI o1".to_string());
        abbrs.add("o1".to_string(), "O1".to_string(), "OpenAI o1".to_string());
        abbrs.add("o1-pro-2025-03-19".to_string(), "O1p".to_string(), "OpenAI o1-pro".to_string());
        abbrs.add("o1-mini-2024-09-12".to_string(), "O1m".to_string(), "OpenAI o1-mini".to_string());
        abbrs.add("o1-mini".to_string(), "O1m".to_string(), "OpenAI o1-mini".to_string());
        
        // o3 series
        abbrs.add("o3-pro-2025-06-10".to_string(), "O3p".to_string(), "OpenAI o3-pro".to_string());
        abbrs.add("o3-2025-04-16".to_string(), "O3".to_string(), "OpenAI o3".to_string());
        abbrs.add("o3-deep-research-2025-06-26".to_string(), "O3d".to_string(), "OpenAI o3-deep-research".to_string());
        abbrs.add("o3-mini-2025-01-31".to_string(), "O3m".to_string(), "OpenAI o3-mini".to_string());
        
        // o4 series
        abbrs.add("o4-mini-2025-04-16".to_string(), "O4m".to_string(), "OpenAI o4-mini".to_string());
        abbrs.add("o4-mini-deep-research-2025-06-26".to_string(), "O4md".to_string(), "OpenAI o4-mini-deep-research".to_string());
        
        // Codex models
        abbrs.add("codex-mini-latest".to_string(), "CXm".to_string(), "Codex Mini Latest".to_string());
        
        abbrs
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = std::env::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{}/.codex/sessions/**/*.jsonl", home_str));
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
                    sources.push(DataSource {
                        path,
                        format: DataFormat::JsonL,
                        metadata: std::collections::HashMap::new(),
                    });
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
            model_abbrs: self.get_model_abbreviations(),
            messages,
            analyzer_name: self.display_name().to_string(),
        })
    }

    fn is_available(&self) -> bool {
        self.discover_data_sources().map_or(false, |sources| !sources.is_empty())
    }
}

// Codex specific implementation functions

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
                if message.message_type == "message" {
                    if let Some(role) = &message.role {
                        match role.as_str() {
                            "user" => {
                                entries.push(ConversationMessage::User {
                                    timestamp: message.timestamp,
                                    conversation_file: conversation_file.clone(),
                                    todo_stats: None,
                                    analyzer_specific: HashMap::new(),
                                });
                            }
                            "assistant" => {
                                let model_name = session_model
                                    .clone()
                                    .unwrap_or_else(|| "unknown".to_string());

                                if let Some(usage) = message.token_usage {
                                    let total_output_tokens =
                                        usage.output_tokens + usage.reasoning_output_tokens;

                                    entries.push(ConversationMessage::AI {
                                        application: Application::CodexCLI,
                                        general_stats: GeneralStats {
                                            input_tokens: usage.input_tokens,
                                            output_tokens: total_output_tokens,
                                            cache_creation_tokens: 0,
                                            cache_read_tokens: 0,
                                            cached_tokens: usage.cached_input_tokens,
                                            cost: calculate_cost_from_tokens(&usage, &model_name),
                                            tool_calls: 0, // We'll count shell calls separately
                                        },
                                        model: model_name,
                                        timestamp: message.timestamp,
                                        hash: None,
                                        conversation_file: conversation_file.clone(),
                                        file_operations: FileOperationStats::default(),
                                        todo_stats: None,
                                        composition_stats: CompositionStats::default(),
                                        analyzer_specific: HashMap::new(),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            CodexEntry::ShellCall(shell_call) => {
                if shell_call.call_type == "local_shell_call" {
                    if let Some(call_id) = &shell_call.call_id {
                        pending_shell_calls.insert(call_id.clone(), shell_call.clone());
                    }

                    // Count as a tool call and create a user message to represent the shell call
                    let file_ops = if let Some(action) = &shell_call.action {
                        parse_shell_command_for_file_operations(&action.command)
                    } else {
                        FileOperationStats::default()
                    };

                    let timestamp = shell_call
                        .timestamp
                        .clone()
                        .unwrap_or_else(|| "".to_string());

                    entries.push(ConversationMessage::User {
                        timestamp,
                        conversation_file: conversation_file.clone(),
                        todo_stats: None,
                        analyzer_specific: {
                            let mut map = HashMap::new();
                            map.insert(
                                "shell_call".to_string(),
                                serde_json::to_value(&shell_call).unwrap_or_default(),
                            );
                            map.insert(
                                "file_operations".to_string(),
                                serde_json::to_value(&file_ops).unwrap_or_default(),
                            );
                            map
                        },
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

fn parse_shell_command_for_file_operations(command: &[String]) -> FileOperationStats {
    let mut file_ops = FileOperationStats::default();

    if command.is_empty() {
        return file_ops;
    }

    // Join the command for easier parsing
    let full_command = command.join(" ");

    // Count basic shell operations
    file_ops.terminal_commands += 1;

    // Parse the actual command (usually after "bash -lc")
    let actual_command = if command.len() >= 3 && command[0] == "bash" && command[1] == "-lc" {
        &command[2]
    } else {
        &full_command
    };

    // Detect file operations based on command patterns
    if actual_command.contains("rg ") || actual_command.contains("grep ") {
        file_ops.file_content_searches += 1;
    }

    if actual_command.contains("--files") || actual_command.contains("find ") {
        file_ops.file_searches += 1;
    }

    // Detect file reads
    if actual_command.contains("cat ")
        || actual_command.contains("head ")
        || actual_command.contains("tail ")
        || actual_command.contains("less ")
        || actual_command.contains("more ")
        || actual_command.contains("sed -n")
    {
        file_ops.files_read += 1;

        // Try to extract file paths and categorize
        extract_file_paths_from_command(actual_command, &mut file_ops);

        // Estimate lines read (rough approximation)
        if actual_command.contains("sed -n") {
            // Try to extract line numbers from sed command
            if let Some(lines) = extract_line_count_from_sed(actual_command) {
                file_ops.lines_read += lines;
                file_ops.bytes_read += lines * 80; // Rough estimate
            }
        } else {
            file_ops.lines_read += 100; // Default estimate
            file_ops.bytes_read += 8000; // Default estimate
        }
    }

    // Detect file writes
    if actual_command.contains(" > ")
        || actual_command.contains(" >> ")
        || actual_command.contains("tee ")
        || actual_command.contains("echo ")
    {
        file_ops.files_edited += 1;
        file_ops.lines_edited += 10; // Rough estimate
        file_ops.bytes_edited += 800; // Rough estimate
    }

    // Detect file edits
    if actual_command.contains("sed -i") || actual_command.contains("awk") {
        file_ops.files_edited += 1;
        file_ops.lines_edited += 5; // Rough estimate
        file_ops.bytes_edited += 400; // Rough estimate
    }

    file_ops
}

fn extract_file_paths_from_command(command: &str, file_ops: &mut FileOperationStats) {
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
                *file_ops
                    .file_types
                    .entry(category.as_str().to_string())
                    .or_insert(0) += 1;
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
            if range.ends_with('p') {
                let range_part = &range[..range.len() - 1];
                if let Some(comma) = range_part.find(',') {
                    let start_str = &range_part[..comma];
                    let end_str = &range_part[comma + 1..];
                    if let (Ok(start), Ok(end)) = (start_str.parse::<u64>(), end_str.parse::<u64>())
                    {
                        return Some(end - start + 1);
                    }
                }
            }
        }
    }
    None
}

fn calculate_cost_from_tokens(usage: &CodexTokenUsage, model_name: &str) -> f64 {
    match MODEL_PRICING.get(model_name) {
        Some(pricing) => {
            // For Codex, we have cached_input_tokens instead of separate creation/read
            let regular_input_cost = usage.input_tokens as f64 * pricing.input_cost_per_token;
            let output_cost = (usage.output_tokens + usage.reasoning_output_tokens) as f64
                * pricing.output_cost_per_token;
            let cached_input_cost =
                usage.cached_input_tokens as f64 * pricing.cache_read_input_token_cost;

            regular_input_cost + output_cost + cached_input_cost
        }
        None => {
            println!(
                "WARNING: Unknown model name: {}. Using fallback pricing.",
                model_name
            );
            // Fallback pricing - use reasonable estimates
            let input_cost = usage.input_tokens as f64 * 0.0000015; // $1.50 per 1M tokens
            let output_cost =
                (usage.output_tokens + usage.reasoning_output_tokens) as f64 * 0.000006; // $6.00 per 1M tokens
            let cached_cost = usage.cached_input_tokens as f64 * 0.000000375; // $0.375 per 1M tokens

            input_cost + output_cost + cached_cost
        }
    }
}
