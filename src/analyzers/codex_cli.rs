use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::analyzer::{Analyzer, DataSource};
use crate::models::calculate_total_cost;
use crate::types::{AgenticCodingToolStats, Application, ConversationMessage, MessageRole, Stats};
use crate::utils::{deserialize_utc_timestamp, hash_text, warn_once};

const DEFAULT_FALLBACK_MODEL: &str = "gpt-5";

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

#[derive(Debug, Clone)]
struct SessionModel {
    name: String,
}

impl SessionModel {
    fn explicit(name: String) -> Self {
        Self { name }
    }

    fn inferred(name: String) -> Self {
        Self { name }
    }
}

pub(crate) fn parse_codex_cli_jsonl_file(file_path: &Path) -> Result<Vec<ConversationMessage>> {
    let mut entries = Vec::new();
    let file_path_str = file_path.to_string_lossy().into_owned();

    let file = File::open(file_path)?;
    let reader = BufReader::with_capacity(64 * 1024, file);

    let mut session_model: Option<SessionModel> = None;
    let mut previous_total_usage: Option<CodexCliTokenUsage> = None;
    let mut saw_token_usage = false;
    let mut _turn_context: Option<CodexCliTurnContext> = None;
    let mut current_tool_call_ids: HashSet<String> = HashSet::new();

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
                    session_model =
                        extract_model_from_value(&wrapper.payload).map(SessionModel::explicit);
                }
            }
            "turn_context" => {
                let mut payload_bytes = simd_json::to_vec(&wrapper.payload)?;
                if let Ok(context) =
                    simd_json::from_slice::<CodexCliTurnContext>(&mut payload_bytes)
                {
                    if let Some(model_name) = extract_model_from_value(&wrapper.payload) {
                        session_model = Some(SessionModel::explicit(model_name));
                    }
                    _turn_context = Some(context);
                }
            }
            "response_item" => {
                if let simd_json::OwnedValue::Object(map) = &wrapper.payload
                    && let Some(simd_json::OwnedValue::String(item_type)) = map.get("type")
                    && item_type == "function_call"
                {
                    if let Some(simd_json::OwnedValue::String(call_id)) = map.get("call_id") {
                        current_tool_call_ids.insert(call_id.clone());
                    } else {
                        current_tool_call_ids.insert(format!(
                            "{}_{}",
                            wrapper.timestamp.to_rfc3339(),
                            current_tool_call_ids.len()
                        ));
                    }
                    continue;
                }

                let mut payload_bytes = simd_json::to_vec(&wrapper.payload)?;
                if let Ok(message) = simd_json::from_slice::<CodexCliMessage>(&mut payload_bytes)
                    && message.message_type == "message"
                    && let Some(role) = &message.role
                {
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
                            // Token usage is now emitted immediately when processing token_count
                            // events. We still track assistant messages without additional stats
                            // to avoid double-counting when Codex emits separate reasoning/tool
                            // outputs.
                            if !saw_token_usage {
                                let model_state = session_model.clone().unwrap_or_else(|| {
                                    let fallback = SessionModel::inferred(
                                        DEFAULT_FALLBACK_MODEL.to_string(),
                                    );
                                    warn_once(format!(
                                        "WARNING: session {file_path_str} missing model metadata; using fallback model {} for cost estimation.",
                                        fallback.name
                                    ));
                                    session_model = Some(fallback.clone());
                                    fallback
                                });

                                entries.push(ConversationMessage {
                                    application: Application::CodexCli,
                                    model: Some(model_state.name.clone()),
                                    global_hash: hash_text(&format!(
                                        "{}_{}_assistant",
                                        file_path_str,
                                        wrapper.timestamp.to_rfc3339()
                                    )),
                                    local_hash: None,
                                    conversation_hash: hash_text(&file_path_str),
                                    date: wrapper.timestamp,
                                    project_hash: "".to_string(),
                                    stats: Stats::default(),
                                    role: MessageRole::Assistant,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "event_msg" => {
                let mut payload_bytes = simd_json::to_vec(&wrapper.payload)?;
                if let Ok(event) = simd_json::from_slice::<CodexCliEventMsg>(&mut payload_bytes)
                    && event.event_type == "token_count"
                {
                    if let Some(model_name) = extract_model_from_token_event(&wrapper.payload) {
                        session_model = Some(SessionModel::explicit(model_name));
                    }

                    if let Some(info) = event.info {
                        let usage = if let Some(last_usage) = info.last_token_usage.clone() {
                            Some(last_usage)
                        } else {
                            info.total_token_usage.clone().map(|total_usage| {
                                subtract_token_usage(&total_usage, previous_total_usage.as_ref())
                            })
                        };

                        if let Some(total_usage) = info.total_token_usage {
                            previous_total_usage = Some(total_usage);
                        }

                        if let Some(token_usage) = usage {
                            let model_state = session_model.clone().unwrap_or_else(|| {
                                let fallback = SessionModel::inferred(
                                    DEFAULT_FALLBACK_MODEL.to_string(),
                                );
                                warn_once(format!(
                                    "WARNING: session {file_path_str} missing model metadata; using fallback model {} for cost estimation.",
                                    fallback.name
                                ));
                                session_model = Some(fallback.clone());
                                fallback
                            });

                            let mut stats = stats_from_usage(&token_usage, &model_state.name);
                            stats.tool_calls = current_tool_call_ids.len() as u32;
                            current_tool_call_ids.clear();

                            entries.push(ConversationMessage {
                                application: Application::CodexCli,
                                model: Some(model_state.name.clone()),
                                global_hash: hash_text(&format!(
                                    "{}_{}_token",
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

                            saw_token_usage = true;
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
    // Codex's output_tokens already include any reasoning tokens. Treat them as-is
    // so we don't double-charge for structured reasoning output.
    let total_output_tokens = usage.output_tokens;

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

fn stats_from_usage(usage: &CodexCliTokenUsage, model_name: &str) -> Stats {
    // Keep the reported output token count; reasoning tokens are informational only.
    let total_output_tokens = usage.output_tokens;
    let actual_input_tokens = usage.input_tokens.saturating_sub(usage.cached_input_tokens);

    let cost = calculate_cost_from_tokens(usage, model_name);

    Stats {
        input_tokens: actual_input_tokens,
        output_tokens: total_output_tokens,
        reasoning_tokens: usage.reasoning_output_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        cached_tokens: usage.cached_input_tokens,
        cost,
        tool_calls: 0,
        ..Default::default()
    }
}

fn subtract_token_usage(
    current: &CodexCliTokenUsage,
    previous: Option<&CodexCliTokenUsage>,
) -> CodexCliTokenUsage {
    let prev_input = previous.map_or(0, |p| p.input_tokens);
    let prev_cached = previous.map_or(0, |p| p.cached_input_tokens);
    let prev_output = previous.map_or(0, |p| p.output_tokens);
    let prev_reasoning = previous.map_or(0, |p| p.reasoning_output_tokens);
    let prev_total = previous.map_or(0, |p| p.total_tokens);

    CodexCliTokenUsage {
        input_tokens: current.input_tokens.saturating_sub(prev_input),
        cached_input_tokens: current.cached_input_tokens.saturating_sub(prev_cached),
        output_tokens: current.output_tokens.saturating_sub(prev_output),
        reasoning_output_tokens: current
            .reasoning_output_tokens
            .saturating_sub(prev_reasoning),
        total_tokens: current.total_tokens.saturating_sub(prev_total),
    }
}

fn extract_model_from_token_event(payload: &simd_json::OwnedValue) -> Option<String> {
    if let simd_json::OwnedValue::Object(map) = payload
        && let Some(info_value) = map.get("info")
    {
        return extract_model_from_value(info_value);
    }
    None
}

fn extract_model_from_value(value: &simd_json::OwnedValue) -> Option<String> {
    extract_model_from_value_rec(value, 0)
}

fn extract_model_from_value_rec(value: &simd_json::OwnedValue, depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }

    match value {
        simd_json::OwnedValue::Object(map) => {
            for key in ["model", "model_name", "modelName"] {
                if let Some(simd_json::OwnedValue::String(model)) = map.get(key)
                    && let Some(normalized) = normalize_model_name(model)
                {
                    return Some(normalized);
                }
            }

            for key in ["metadata", "info"] {
                if let Some(nested) = map.get(key)
                    && let Some(model) = extract_model_from_value_rec(nested, depth + 1)
                {
                    return Some(model);
                }
            }

            None
        }
        simd_json::OwnedValue::Array(items) => items
            .iter()
            .find_map(|item| extract_model_from_value_rec(item, depth + 1)),
        _ => None,
    }
}

fn normalize_model_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
