use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::models::calculate_total_cost;
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use super::copilot::{count_tokens, extract_model_from_model_id, is_probably_tool_json_text};

pub struct CopilotCliAnalyzer;

const COPILOT_CLI_STATE_DIRS: &[&str] = &["session-state", "history-session-state"];

impl CopilotCliAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

fn copilot_cli_session_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home_dir) = dirs::home_dir() {
        let copilot_dir = home_dir.join(".copilot");
        for dir_name in COPILOT_CLI_STATE_DIRS {
            let session_dir = copilot_dir.join(dir_name);
            if session_dir.is_dir() {
                dirs.push(session_dir);
            }
        }
    }

    dirs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CopilotCliEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    data: simd_json::OwnedValue,
}

#[derive(Debug, Clone)]
struct CopilotCliTurn {
    user_text: String,
    user_date: DateTime<Utc>,
    assistant_date: Option<DateTime<Utc>>,
    assistant_text_parts: Vec<String>,
    reasoning_parts: Vec<String>,
    tool_request_parts: Vec<String>,
    tool_result_parts: Vec<String>,
    stats: Stats,
    model: Option<String>,
    exact_output_tokens: u64,
}

#[derive(Debug, Clone)]
struct CopilotCliPendingUser {
    text: String,
    date: DateTime<Utc>,
    emitted: bool,
}

#[derive(Debug, Clone, Default)]
struct CopilotCliUsageTotals {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
}

#[derive(Debug, Clone, Default)]
struct CopilotCliLiveContext {
    reusable_input_tokens: u64,
    static_prompt_tokens: u64,
}

impl CopilotCliTurn {
    fn new(user_text: String, user_date: DateTime<Utc>, model: Option<String>) -> Self {
        Self {
            user_text,
            user_date,
            assistant_date: None,
            assistant_text_parts: Vec::new(),
            reasoning_parts: Vec::new(),
            tool_request_parts: Vec::new(),
            tool_result_parts: Vec::new(),
            stats: Stats::default(),
            model,
            exact_output_tokens: 0,
        }
    }

    fn has_assistant_content(&self) -> bool {
        !self.assistant_text_parts.is_empty()
            || !self.reasoning_parts.is_empty()
            || !self.tool_request_parts.is_empty()
            || !self.tool_result_parts.is_empty()
            || self.stats.tool_calls > 0
            || self.exact_output_tokens > 0
    }

    fn input_text(&self, include_user_text: bool) -> String {
        let mut parts = Vec::with_capacity(1 + self.tool_result_parts.len());
        if include_user_text && !self.user_text.trim().is_empty() {
            parts.push(self.user_text.as_str());
        }
        parts.extend(
            self.tool_result_parts
                .iter()
                .map(String::as_str)
                .filter(|text| !text.trim().is_empty()),
        );
        parts.join("\n")
    }

    fn output_text(&self) -> String {
        self.reasoning_parts
            .iter()
            .chain(self.tool_request_parts.iter())
            .chain(self.assistant_text_parts.iter())
            .map(String::as_str)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn reasoning_text(&self) -> String {
        self.reasoning_parts
            .iter()
            .map(String::as_str)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn reasoning_tokens(&self) -> u64 {
        count_tokens(&self.reasoning_text())
    }

    fn visible_output_tokens(&self) -> u64 {
        if self.exact_output_tokens > 0 {
            self.exact_output_tokens
        } else {
            let visible_output = self
                .tool_request_parts
                .iter()
                .chain(self.assistant_text_parts.iter())
                .map(String::as_str)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            count_tokens(&visible_output)
        }
    }

    fn reusable_context_tokens(&self, include_user_text: bool) -> u64 {
        count_tokens(&self.input_text(include_user_text))
            .saturating_add(self.visible_output_tokens())
    }
}

impl CopilotCliLiveContext {
    fn estimated_input_tokens(&self, turn: &CopilotCliTurn, include_user_text: bool) -> u64 {
        self.reusable_input_tokens
            .saturating_add(count_tokens(&turn.input_text(include_user_text)))
    }

    fn estimated_cache_read_tokens(&self) -> u64 {
        self.reusable_input_tokens
    }

    fn absorb_turn(&mut self, turn: &CopilotCliTurn, include_user_text: bool) {
        self.reusable_input_tokens = self
            .reusable_input_tokens
            .saturating_add(turn.reusable_context_tokens(include_user_text));
    }

    fn apply_compaction(&mut self, event_data: &simd_json::OwnedValue) {
        let Some(data) = event_data.as_object() else {
            return;
        };

        let compacted_tokens = data
            .get("postCompactionTokens")
            .and_then(|value| value.as_u64())
            .or_else(|| {
                data.get("summaryContent")
                    .map(extract_text_from_cli_value)
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| count_tokens(&text))
            });

        if let Some(compacted_tokens) = compacted_tokens {
            self.reusable_input_tokens = compacted_tokens;
        }
    }

    fn update_static_prompt_tokens(&mut self, event_data: &simd_json::OwnedValue) {
        if let Some(tool_definition_tokens) = event_data
            .as_object()
            .and_then(|data| data.get("toolDefinitionsTokens"))
            .and_then(|value| value.as_u64())
        {
            self.static_prompt_tokens = tool_definition_tokens;
        }
    }
}

fn calculate_copilot_cli_cost(stats: &Stats, model_name: &str) -> f64 {
    let actual_input_tokens = stats.input_tokens.saturating_sub(stats.cache_read_tokens);
    calculate_total_cost(
        model_name,
        actual_input_tokens,
        stats.output_tokens,
        stats.cache_creation_tokens,
        stats.cache_read_tokens,
    )
}

fn parse_rfc3339_timestamp(timestamp: Option<&str>) -> Option<DateTime<Utc>> {
    timestamp.and_then(|ts| {
        DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    })
}

fn extract_text_from_cli_value(value: &simd_json::OwnedValue) -> String {
    match value {
        simd_json::OwnedValue::String(s) => s.to_string(),
        simd_json::OwnedValue::Array(arr) => arr
            .iter()
            .map(extract_text_from_cli_value)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        simd_json::OwnedValue::Object(obj) => {
            for key in ["content", "text", "message", "output", "result", "error"] {
                if let Some(value) = obj.get(key) {
                    let text = extract_text_from_cli_value(value);
                    if !text.trim().is_empty() {
                        return text;
                    }
                }
            }

            obj.iter()
                .map(|(_, value)| extract_text_from_cli_value(value))
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => String::new(),
    }
}

fn value_to_json_string(value: &simd_json::OwnedValue) -> String {
    simd_json::to_vec(value)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

fn extract_cli_tool_text(tool_name: &str, arguments: &simd_json::OwnedValue) -> String {
    let arguments_text = value_to_json_string(arguments);
    if arguments_text.is_empty() {
        tool_name.to_string()
    } else {
        format!("{tool_name} {arguments_text}")
    }
}

fn apply_cli_tool_stats(stats: &mut Stats, tool_name: &str) {
    match tool_name {
        "read_file" => stats.files_read += 1,
        "replace_string_in_file" | "multi_replace_string_in_file" => stats.files_edited += 1,
        "create_file" => stats.files_added += 1,
        "delete_file" => stats.files_deleted += 1,
        "file_search" => stats.file_searches += 1,
        "grep_search" | "semantic_search" => stats.file_content_searches += 1,
        "run_in_terminal" | "bash" | "shell" | "powershell" => stats.terminal_commands += 1,
        _ => {}
    }
}

pub(crate) fn is_copilot_cli_session_file(path: &Path) -> bool {
    if path.extension().is_none_or(|ext| ext != "jsonl") {
        return false;
    }

    if path.file_name().is_some_and(|name| name == "events.jsonl") {
        return path
            .parent()
            .and_then(|parent| parent.parent())
            .and_then(|grandparent| grandparent.file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| COPILOT_CLI_STATE_DIRS.contains(&name));
    }

    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| COPILOT_CLI_STATE_DIRS.contains(&name))
}

fn extract_copilot_cli_project_hash(workspace_path: Option<&str>) -> String {
    workspace_path
        .map(hash_text)
        .unwrap_or_else(|| hash_text("copilot-global"))
}

fn push_copilot_cli_user_message(
    entries: &mut Vec<ConversationMessage>,
    pending_user: &CopilotCliPendingUser,
    user_index: &mut usize,
    conversation_hash: &str,
    project_hash: &str,
    session_name: Option<&String>,
) {
    let user_local_hash = format!("{conversation_hash}-cli-user-{}", *user_index);
    let user_global_hash = hash_text(&format!(
        "{project_hash}:{conversation_hash}:cli:user:{}:{}",
        *user_index,
        pending_user.date.to_rfc3339()
    ));

    entries.push(ConversationMessage {
        application: Application::CopilotCli,
        date: pending_user.date,
        project_hash: project_hash.to_string(),
        conversation_hash: conversation_hash.to_string(),
        local_hash: Some(user_local_hash),
        global_hash: user_global_hash,
        model: None,
        stats: Stats::default(),
        role: MessageRole::User,
        uuid: None,
        session_name: session_name.cloned(),
    });

    *user_index += 1;
}

fn distribute_total(total: u64, weights: &[u64]) -> Vec<u64> {
    if weights.is_empty() {
        return Vec::new();
    }

    if total == 0 {
        return vec![0; weights.len()];
    }

    let normalized_weights: Vec<u64> = if weights.iter().any(|weight| *weight > 0) {
        weights.to_vec()
    } else {
        vec![1; weights.len()]
    };
    let weight_sum: u128 = normalized_weights
        .iter()
        .map(|weight| *weight as u128)
        .sum();

    let mut distributed = Vec::with_capacity(normalized_weights.len());
    let mut assigned = 0u64;
    for (idx, weight) in normalized_weights.iter().enumerate() {
        let value = if idx + 1 == normalized_weights.len() {
            total.saturating_sub(assigned)
        } else {
            ((total as u128 * *weight as u128) / weight_sum) as u64
        };
        assigned = assigned.saturating_add(value);
        distributed.push(value);
    }

    distributed
}

fn extract_copilot_cli_shutdown_metrics(
    event_data: &simd_json::OwnedValue,
) -> BTreeMap<String, CopilotCliUsageTotals> {
    let mut metrics = BTreeMap::new();

    let Some(model_metrics) = event_data
        .as_object()
        .and_then(|data| data.get("modelMetrics"))
        .and_then(|value| value.as_object())
    else {
        return metrics;
    };

    for (model_name, metrics_value) in model_metrics {
        let Some(usage_obj) = metrics_value
            .as_object()
            .and_then(|metrics_map| metrics_map.get("usage"))
            .and_then(|value| value.as_object())
        else {
            continue;
        };

        let normalized_model =
            extract_model_from_model_id(model_name).unwrap_or_else(|| model_name.to_string());

        metrics.insert(
            normalized_model,
            CopilotCliUsageTotals {
                input_tokens: usage_obj
                    .get("inputTokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                output_tokens: usage_obj
                    .get("outputTokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                cache_read_tokens: usage_obj
                    .get("cacheReadTokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                cache_write_tokens: usage_obj
                    .get("cacheWriteTokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
            },
        );
    }

    metrics
}

fn apply_copilot_cli_shutdown_metrics(
    entries: &mut [ConversationMessage],
    shutdown_metrics: &BTreeMap<String, CopilotCliUsageTotals>,
) {
    for (model_name, usage) in shutdown_metrics {
        let assistant_indexes: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, message)| {
                message.application == Application::CopilotCli
                    && message.role == MessageRole::Assistant
                    && message.model.as_deref() == Some(model_name.as_str())
            })
            .map(|(idx, _)| idx)
            .collect();

        if assistant_indexes.is_empty() {
            continue;
        }

        let output_weights: Vec<u64> = assistant_indexes
            .iter()
            .map(|idx| entries[*idx].stats.output_tokens)
            .collect();

        let input_distribution = distribute_total(usage.input_tokens, &output_weights);
        let output_distribution = distribute_total(usage.output_tokens, &output_weights);
        let cache_read_distribution = distribute_total(usage.cache_read_tokens, &output_weights);
        let cache_write_distribution = distribute_total(usage.cache_write_tokens, &output_weights);

        for (position, message_index) in assistant_indexes.iter().enumerate() {
            let message = &mut entries[*message_index];
            message.stats.input_tokens = input_distribution[position];
            message.stats.output_tokens = output_distribution[position];
            message.stats.reasoning_tokens = message
                .stats
                .reasoning_tokens
                .min(message.stats.output_tokens);
            message.stats.cache_read_tokens = cache_read_distribution[position];
            message.stats.cache_creation_tokens = cache_write_distribution[position];
            message.stats.cached_tokens =
                message.stats.cache_read_tokens + message.stats.cache_creation_tokens;
            message.stats.cost = calculate_copilot_cli_cost(&message.stats, model_name);
        }
    }
}

fn fill_missing_copilot_cli_models(
    entries: &mut [ConversationMessage],
    shutdown_metrics: &BTreeMap<String, CopilotCliUsageTotals>,
) {
    if shutdown_metrics.len() != 1 {
        return;
    }

    let Some(model_name) = shutdown_metrics.keys().next().cloned() else {
        return;
    };

    for message in entries.iter_mut() {
        if message.application == Application::CopilotCli
            && message.role == MessageRole::Assistant
            && message.model.is_none()
        {
            message.model = Some(model_name.clone());
        }
    }
}

fn apply_copilot_cli_live_prompt_overhead(
    entries: &mut [ConversationMessage],
    static_prompt_tokens: u64,
) {
    if static_prompt_tokens == 0 {
        return;
    }

    for message in entries.iter_mut() {
        if message.application != Application::CopilotCli || message.role != MessageRole::Assistant
        {
            continue;
        }

        message.stats.input_tokens = message
            .stats
            .input_tokens
            .saturating_add(static_prompt_tokens);
        message.stats.cache_read_tokens = message
            .stats
            .cache_read_tokens
            .saturating_add(static_prompt_tokens);
        message.stats.cached_tokens = message
            .stats
            .cache_creation_tokens
            .saturating_add(message.stats.cache_read_tokens);
        if let Some(model_name) = message.model.as_deref() {
            message.stats.cost = calculate_copilot_cli_cost(&message.stats, model_name);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn flush_copilot_cli_turn(
    entries: &mut Vec<ConversationMessage>,
    current_turn: &mut Option<CopilotCliTurn>,
    live_context: &mut CopilotCliLiveContext,
    pending_user: &mut Option<CopilotCliPendingUser>,
    user_index: &mut usize,
    assistant_index: &mut usize,
    conversation_hash: &str,
    project_hash: &str,
    session_name: Option<&String>,
) {
    let Some(turn) = current_turn.take() else {
        return;
    };

    let Some(pending_user) = pending_user.as_mut() else {
        return;
    };

    let include_user_text = !pending_user.emitted;

    if !pending_user.emitted {
        push_copilot_cli_user_message(
            entries,
            pending_user,
            user_index,
            conversation_hash,
            project_hash,
            session_name,
        );
        pending_user.emitted = true;
    }

    if turn.has_assistant_content() {
        let assistant_date = turn.assistant_date.unwrap_or(turn.user_date);
        let assistant_local_hash =
            format!("{conversation_hash}-cli-assistant-{}", *assistant_index);
        let assistant_global_hash = hash_text(&format!(
            "{project_hash}:{conversation_hash}:cli:assistant:{}:{}",
            *assistant_index,
            assistant_date.to_rfc3339()
        ));

        let output_text = turn.output_text();
        let estimated_input_tokens = live_context.estimated_input_tokens(&turn, include_user_text);
        let estimated_cache_read_tokens = live_context.estimated_cache_read_tokens();
        let output_tokens = if turn.exact_output_tokens > 0 {
            turn.exact_output_tokens
        } else {
            count_tokens(&output_text)
        };
        let reasoning_tokens = turn.reasoning_tokens().min(output_tokens);
        let model = turn.model.clone();
        live_context.absorb_turn(&turn, include_user_text);

        let mut assistant_stats = turn.stats;
        assistant_stats.input_tokens = estimated_input_tokens;
        assistant_stats.cache_read_tokens = estimated_cache_read_tokens;
        assistant_stats.output_tokens = output_tokens;
        assistant_stats.reasoning_tokens = reasoning_tokens;
        assistant_stats.cached_tokens =
            assistant_stats.cache_read_tokens + assistant_stats.cache_creation_tokens;
        if let Some(model_name) = model.as_deref() {
            assistant_stats.cost = calculate_copilot_cli_cost(&assistant_stats, model_name);
        }

        entries.push(ConversationMessage {
            application: Application::CopilotCli,
            date: assistant_date,
            project_hash: project_hash.to_string(),
            conversation_hash: conversation_hash.to_string(),
            local_hash: Some(assistant_local_hash),
            global_hash: assistant_global_hash,
            model,
            stats: assistant_stats,
            role: MessageRole::Assistant,
            uuid: None,
            session_name: session_name.cloned(),
        });

        *assistant_index += 1;
    }
}

pub(crate) fn parse_copilot_cli_session_file(
    session_file: &Path,
) -> Result<Vec<ConversationMessage>> {
    let session_content = std::fs::read_to_string(session_file)?;
    let mut events = Vec::new();

    for line in session_content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut event_bytes = trimmed.as_bytes().to_vec();
        let event: CopilotCliEvent =
            simd_json::from_slice(&mut event_bytes).context("Failed to parse Copilot CLI event")?;
        events.push(event);
    }

    if events.is_empty() {
        return Ok(Vec::new());
    }

    let mut session_id = session_file
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_string);
    let mut workspace_path: Option<String> = None;
    let mut session_name: Option<String> = None;
    let mut current_model: Option<String> = None;

    for event in &events {
        if event.event_type == "session.start"
            && let Some(data) = event.data.as_object()
        {
            if let Some(start_data) = data.get("sessionId").and_then(|value| value.as_str()) {
                session_id = Some(start_data.to_string());
            }

            if let Some(context) = data.get("context").and_then(|value| value.as_object()) {
                workspace_path = context
                    .get("cwd")
                    .and_then(|value| value.as_str())
                    .or_else(|| context.get("gitRoot").and_then(|value| value.as_str()))
                    .map(str::to_string);

                current_model = context
                    .get("model")
                    .and_then(|value| value.as_str())
                    .and_then(extract_model_from_model_id);
            }
        }
    }

    let conversation_hash = session_id
        .as_ref()
        .map(|id| hash_text(id))
        .unwrap_or_else(|| hash_text(&session_file.to_string_lossy()));
    let project_hash = extract_copilot_cli_project_hash(workspace_path.as_deref());

    let mut entries = Vec::new();
    let mut pending_user: Option<CopilotCliPendingUser> = None;
    let mut user_index = 0usize;
    let mut assistant_index = 0usize;
    let mut current_turn: Option<CopilotCliTurn> = None;
    let mut live_context = CopilotCliLiveContext::default();
    let mut shutdown_segment_start = 0usize;

    for event in events {
        let event_timestamp = parse_rfc3339_timestamp(event.timestamp.as_deref());
        let event_data = event.data;

        match event.event_type.as_str() {
            "session.model_change" => {
                if let Some(new_model) = event_data
                    .as_object()
                    .and_then(|data| data.get("newModel"))
                    .and_then(|value| value.as_str())
                {
                    current_model = extract_model_from_model_id(new_model);
                }
            }
            "user.message" => {
                flush_copilot_cli_turn(
                    &mut entries,
                    &mut current_turn,
                    &mut live_context,
                    &mut pending_user,
                    &mut user_index,
                    &mut assistant_index,
                    &conversation_hash,
                    &project_hash,
                    session_name.as_ref(),
                );

                if let Some(previous_user) = pending_user.take()
                    && !previous_user.emitted
                {
                    push_copilot_cli_user_message(
                        &mut entries,
                        &previous_user,
                        &mut user_index,
                        &conversation_hash,
                        &project_hash,
                        session_name.as_ref(),
                    );
                }

                let user_text = event_data
                    .as_object()
                    .and_then(|data| data.get("content"))
                    .map(extract_text_from_cli_value)
                    .unwrap_or_default();

                if session_name.is_none()
                    && !user_text.is_empty()
                    && !is_probably_tool_json_text(&user_text)
                {
                    let truncated = if user_text.chars().count() > 50 {
                        format!("{}...", user_text.chars().take(50).collect::<String>())
                    } else {
                        user_text.clone()
                    };
                    session_name = Some(truncated);
                }

                pending_user = Some(CopilotCliPendingUser {
                    text: user_text,
                    date: event_timestamp.unwrap_or_else(Utc::now),
                    emitted: false,
                });
            }
            "assistant.turn_start" => {
                flush_copilot_cli_turn(
                    &mut entries,
                    &mut current_turn,
                    &mut live_context,
                    &mut pending_user,
                    &mut user_index,
                    &mut assistant_index,
                    &conversation_hash,
                    &project_hash,
                    session_name.as_ref(),
                );

                let Some(pending_user) = pending_user.as_ref() else {
                    continue;
                };
                current_turn = Some(CopilotCliTurn::new(
                    pending_user.text.clone(),
                    pending_user.date,
                    current_model.clone(),
                ));
                if let Some(turn) = current_turn.as_mut() {
                    turn.assistant_date
                        .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));
                }
            }
            "assistant.turn_end" => {
                flush_copilot_cli_turn(
                    &mut entries,
                    &mut current_turn,
                    &mut live_context,
                    &mut pending_user,
                    &mut user_index,
                    &mut assistant_index,
                    &conversation_hash,
                    &project_hash,
                    session_name.as_ref(),
                );
            }
            "assistant.message" | "assistant.message.delta" => {
                if current_turn.is_none() {
                    let Some(pending_user) = pending_user.as_ref() else {
                        continue;
                    };
                    current_turn = Some(CopilotCliTurn::new(
                        pending_user.text.clone(),
                        pending_user.date,
                        current_model.clone(),
                    ));
                }
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));

                if let Some(data) = event_data.as_object() {
                    if let Some(model) = data
                        .get("model")
                        .and_then(|value| value.as_str())
                        .and_then(extract_model_from_model_id)
                    {
                        current_model = Some(model.clone());
                        turn.model = Some(model);
                    } else {
                        turn.model = current_model.clone().or_else(|| turn.model.clone());
                    }

                    if let Some(content) = data.get("content") {
                        let text = extract_text_from_cli_value(content);
                        if !text.trim().is_empty() {
                            turn.assistant_text_parts.push(text);
                        }
                    }

                    if let Some(reasoning_text) = data.get("reasoningText") {
                        let text = extract_text_from_cli_value(reasoning_text);
                        if !text.trim().is_empty() {
                            turn.reasoning_parts.push(text);
                        }
                    }

                    if let Some(output_tokens) =
                        data.get("outputTokens").and_then(|value| value.as_u64())
                    {
                        turn.exact_output_tokens += output_tokens;
                    }

                    if let Some(tool_requests) =
                        data.get("toolRequests").and_then(|value| value.as_array())
                    {
                        for request in tool_requests {
                            if let Some(request_obj) = request.as_object() {
                                let tool_name = request_obj
                                    .get("toolName")
                                    .and_then(|value| value.as_str())
                                    .or_else(|| {
                                        request_obj.get("name").and_then(|value| value.as_str())
                                    });

                                let arguments = request_obj
                                    .get("arguments")
                                    .cloned()
                                    .unwrap_or_else(simd_json::OwnedValue::null);

                                if let Some("report_intent") = tool_name
                                    && session_name.is_none()
                                    && let Some(intent) = arguments
                                        .as_object()
                                        .and_then(|args| args.get("intent"))
                                        .and_then(|value| value.as_str())
                                {
                                    session_name = Some(intent.to_string());
                                }
                            }
                        }
                    }
                }
            }
            "assistant.reasoning" => {
                if current_turn.is_none() {
                    let Some(pending_user) = pending_user.as_ref() else {
                        continue;
                    };
                    current_turn = Some(CopilotCliTurn::new(
                        pending_user.text.clone(),
                        pending_user.date,
                        current_model.clone(),
                    ));
                }
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));
                turn.model = current_model.clone().or_else(|| turn.model.clone());

                let text = event_data
                    .as_object()
                    .and_then(|data| data.get("content"))
                    .map(extract_text_from_cli_value)
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    turn.reasoning_parts.push(text);
                }
            }
            "tool.execution_start" => {
                if current_turn.is_none() {
                    let Some(pending_user) = pending_user.as_ref() else {
                        continue;
                    };
                    current_turn = Some(CopilotCliTurn::new(
                        pending_user.text.clone(),
                        pending_user.date,
                        current_model.clone(),
                    ));
                }
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));
                turn.stats.tool_calls += 1;

                if let Some(data) = event_data.as_object() {
                    if let Some(model) = data
                        .get("model")
                        .and_then(|value| value.as_str())
                        .and_then(extract_model_from_model_id)
                    {
                        current_model = Some(model.clone());
                        turn.model = Some(model);
                    } else {
                        turn.model = current_model.clone().or_else(|| turn.model.clone());
                    }

                    let tool_name = data
                        .get("toolName")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown");
                    let arguments = data
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(simd_json::OwnedValue::null);

                    apply_cli_tool_stats(&mut turn.stats, tool_name);
                    turn.tool_request_parts
                        .push(extract_cli_tool_text(tool_name, &arguments));

                    if tool_name == "report_intent"
                        && session_name.is_none()
                        && let Some(intent) = arguments
                            .as_object()
                            .and_then(|args| args.get("intent"))
                            .and_then(|value| value.as_str())
                    {
                        session_name = Some(intent.to_string());
                    }
                }
            }
            "tool.execution_complete" => {
                if current_turn.is_none() {
                    let Some(pending_user) = pending_user.as_ref() else {
                        continue;
                    };
                    current_turn = Some(CopilotCliTurn::new(
                        pending_user.text.clone(),
                        pending_user.date,
                        current_model.clone(),
                    ));
                }
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));

                if let Some(data) = event_data.as_object() {
                    if let Some(model) = data
                        .get("model")
                        .and_then(|value| value.as_str())
                        .and_then(extract_model_from_model_id)
                    {
                        current_model = Some(model.clone());
                        turn.model = Some(model);
                    } else {
                        turn.model = current_model.clone().or_else(|| turn.model.clone());
                    }

                    if let Some(result) = data.get("result") {
                        let text = extract_text_from_cli_value(result);
                        if !text.trim().is_empty() {
                            turn.tool_result_parts.push(text);
                        }
                    }
                }
            }
            "session.shutdown" => {
                let metrics = extract_copilot_cli_shutdown_metrics(&event_data);
                if !metrics.is_empty() {
                    let segment_entries = &mut entries[shutdown_segment_start..];
                    fill_missing_copilot_cli_models(segment_entries, &metrics);
                    apply_copilot_cli_shutdown_metrics(segment_entries, &metrics);
                    shutdown_segment_start = entries.len();
                }
            }
            "session.compaction_start" => {
                live_context.update_static_prompt_tokens(&event_data);
            }
            "session.compaction_complete" => {
                live_context.apply_compaction(&event_data);
            }
            "abort" | "session.error" => {
                if current_turn.is_none() {
                    let Some(pending_user) = pending_user.as_ref() else {
                        continue;
                    };
                    current_turn = Some(CopilotCliTurn::new(
                        pending_user.text.clone(),
                        pending_user.date,
                        current_model.clone(),
                    ));
                }
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));

                let text = extract_text_from_cli_value(&event_data);
                if !text.trim().is_empty() {
                    turn.assistant_text_parts.push(text);
                }
            }
            _ => {}
        }
    }

    flush_copilot_cli_turn(
        &mut entries,
        &mut current_turn,
        &mut live_context,
        &mut pending_user,
        &mut user_index,
        &mut assistant_index,
        &conversation_hash,
        &project_hash,
        session_name.as_ref(),
    );

    if let Some(pending_user) = pending_user.take()
        && !pending_user.emitted
    {
        push_copilot_cli_user_message(
            &mut entries,
            &pending_user,
            &mut user_index,
            &conversation_hash,
            &project_hash,
            session_name.as_ref(),
        );
    }

    apply_copilot_cli_live_prompt_overhead(
        &mut entries[shutdown_segment_start..],
        live_context.static_prompt_tokens,
    );

    Ok(entries)
}

#[async_trait]
impl Analyzer for CopilotCliAnalyzer {
    fn display_name(&self) -> &'static str {
        "GitHub Copilot CLI"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            for dir_name in COPILOT_CLI_STATE_DIRS {
                patterns.push(format!("{home_str}/.copilot/{dir_name}/*.jsonl"));
                patterns.push(format!("{home_str}/.copilot/{dir_name}/*/events.jsonl"));
            }
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = copilot_cli_session_dirs()
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).min_depth(1).max_depth(2).into_iter())
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_type().is_file() && is_copilot_cli_session_file(entry.path())
            })
            .map(|entry| DataSource {
                path: entry.into_path(),
            })
            .collect();

        Ok(sources)
    }

    fn is_available(&self) -> bool {
        copilot_cli_session_dirs()
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).min_depth(1).max_depth(2).into_iter())
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_type().is_file() && is_copilot_cli_session_file(entry.path()))
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        parse_copilot_cli_session_file(&source.path)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        copilot_cli_session_dirs()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        is_copilot_cli_session_file(path)
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }
}
