use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::analyzer::{Analyzer, DataSource};
use crate::models::calculate_total_cost;
use crate::types::{AgenticCodingToolStats, Application, ConversationMessage, MessageRole, Stats};
use crate::utils::{deserialize_utc_timestamp, hash_text};

pub struct CodexCliAnalyzer;

impl CodexCliAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Analyzer for CodexCliAnalyzer {
    fn display_name(&self) -> &'static str {
        "Codex CLI"
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
            .map(|source| parse_codex_cli_jsonl_file(&source.path))
            // Start the reduction with an empty vector and extend it with the
            // entries coming from each successfully-parsed file.
            .try_reduce(Vec::new, |mut acc, mut entries| {
                acc.append(&mut entries);
                Ok(acc)
            });

        // For Codex CLI, we don't need to deduplicate since each session is separate
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

// CODEX CLI JSONL FILES SCHEMA - NEW WRAPPER FORMAT

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexCliTokenUsage {
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
struct CodexCliTokenCountInfo {
    total_token_usage: Option<CodexCliTokenUsage>,
    last_token_usage: Option<CodexCliTokenUsage>,
    model_context_window: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexCliGitInfo {
    commit_hash: Option<String>,
    branch: Option<String>,
    repository_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexCliSessionMeta {
    id: String,
    #[serde(deserialize_with = "deserialize_utc_timestamp")]
    timestamp: DateTime<Utc>,
    cwd: Option<String>,
    originator: Option<String>,
    cli_version: Option<String>,
    instructions: Option<String>,
    git: Option<CodexCliGitInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexCliMessage {
    #[serde(rename = "type")]
    message_type: String,
    role: Option<String>,
    content: Option<simd_json::OwnedValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexCliTurnContext {
    cwd: Option<String>,
    approval_policy: Option<String>,
    model: Option<String>,
    summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexCliEventMsg {
    #[serde(rename = "type")]
    event_type: String,
    message: Option<String>,
    text: Option<String>,
    info: Option<CodexCliTokenCountInfo>,
}

// Wrapper structure for all entries
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexCliWrapper {
    #[serde(deserialize_with = "deserialize_utc_timestamp")]
    timestamp: DateTime<Utc>,
    #[serde(rename = "type")]
    entry_type: String,
    payload: simd_json::OwnedValue,
}

pub(crate) fn parse_codex_cli_jsonl_file(file_path: &Path) -> Result<Vec<ConversationMessage>> {
    let mut entries = Vec::new();
    let file_path_str = file_path.to_string_lossy();

    let file = File::open(file_path)?;
    let reader = BufReader::with_capacity(64 * 1024, file);

    let mut session_model: Option<String> = None;
    let mut current_token_usage: Option<CodexCliTokenUsage> = None;
    let mut _turn_context: Option<CodexCliTurnContext> = None;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.trim().is_empty() {
            continue;
        }

        let wrapper = match simd_json::from_slice::<CodexCliWrapper>(&mut line.clone().into_bytes())
        {
            Ok(wrapper) => wrapper,
            Err(_) => continue,
        };

        match wrapper.entry_type.as_str() {
            "session_meta" => {
                // Try to parse the payload as session metadata
                let mut payload_bytes = simd_json::to_vec(&wrapper.payload)?;
                if let Ok(_session_meta) =
                    simd_json::from_slice::<CodexCliSessionMeta>(&mut payload_bytes)
                {
                    // Extract model from session if available - in new format it might be in turn_context
                    session_model = None; // Will get from turn_context
                }
            }
            "turn_context" => {
                let mut payload_bytes = simd_json::to_vec(&wrapper.payload)?;
                if let Ok(context) =
                    simd_json::from_slice::<CodexCliTurnContext>(&mut payload_bytes)
                {
                    session_model = context.model.clone();
                    _turn_context = Some(context);
                }
            }
            "response_item" => {
                let mut payload_bytes = simd_json::to_vec(&wrapper.payload)?;
                if let Ok(message) = simd_json::from_slice::<CodexCliMessage>(&mut payload_bytes) {
                    if message.message_type == "message" {
                        if let Some(role) = &message.role {
                            match role.as_str() {
                                "user" => {
                                    entries.push(ConversationMessage {
                                        date: wrapper.timestamp,
                                        global_hash: hash_text(&format!(
                                            "{}_{}",
                                            file_path_str,
                                            wrapper.timestamp.to_rfc3339()
                                        )),
                                        local_hash: None,
                                        conversation_hash: hash_text(&file_path_str),
                                        application: Application::CodexCli,
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

                                    // Use token usage if we have it
                                    let stats = if let Some(usage) = &current_token_usage {
                                        let total_output_tokens =
                                            usage.output_tokens + usage.reasoning_output_tokens;

                                        // Subtract cached input tokens from total input tokens
                                        // since Codex input_tokens is a superset that includes cached tokens
                                        let actual_input_tokens = usage.input_tokens.saturating_sub(usage.cached_input_tokens);

                                        Stats {
                                            input_tokens: actual_input_tokens,
                                            output_tokens: total_output_tokens,
                                            cache_creation_tokens: 0,
                                            cache_read_tokens: 0,
                                            cached_tokens: usage.cached_input_tokens,
                                            cost: calculate_cost_from_tokens(usage, &model_name),
                                            tool_calls: 0,
                                            ..Default::default()
                                        }
                                    } else {
                                        Stats::default()
                                    };

                                    entries.push(ConversationMessage {
                                        application: Application::CodexCli,
                                        model: Some(model_name),
                                        global_hash: hash_text(&format!(
                                            "{}_{}",
                                            file_path_str,
                                            wrapper.timestamp.to_rfc3339()
                                        )),
                                        local_hash: None,
                                        conversation_hash: hash_text(&file_path_str),
                                        date: wrapper.timestamp,
                                        project_hash: "".to_string(),
                                        stats,
                                        role: MessageRole::Assistant,
                                    });

                                    // Clear token usage after using it
                                    current_token_usage = None;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            "event_msg" => {
                let mut payload_bytes = simd_json::to_vec(&wrapper.payload)?;
                if let Ok(event) = simd_json::from_slice::<CodexCliEventMsg>(&mut payload_bytes) {
                    if event.event_type == "token_count" {
                        if let Some(info) = event.info {
                            // Use last_token_usage if available, otherwise total_token_usage
                            current_token_usage = info.last_token_usage.or(info.total_token_usage);
                        }
                    }
                }
            }
            _ => {
                // Skip other types for now
            }
        }
    }

    Ok(entries)
}

fn calculate_cost_from_tokens(usage: &CodexCliTokenUsage, model_name: &str) -> f64 {
    let total_output_tokens = usage.output_tokens + usage.reasoning_output_tokens;

    // Subtract cached input tokens from total input tokens
    // since Codex input_tokens is a superset that includes cached tokens
    let actual_input_tokens = usage.input_tokens.saturating_sub(usage.cached_input_tokens);

    calculate_total_cost(
        model_name,
        actual_input_tokens,
        total_output_tokens,
        0, // Codex CLI doesn't have separate cache creation tokens
        usage.cached_input_tokens,
    )
}
