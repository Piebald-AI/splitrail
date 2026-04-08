use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use simd_json::prelude::*;
use std::path::{Path, PathBuf};
use tiktoken_rs::get_bpe_from_model;
use walkdir::WalkDir;

pub struct CopilotAnalyzer;

/// VSCode forks that might have Copilot installed
const COPILOT_VSCODE_FORKS: &[&str] = &[
    "Code",
    "Cursor",
    "Windsurf",
    "VSCodium",
    "Positron",
    "Code - Insiders",
    "Antigravity",
];

const COPILOT_CLI_STATE_DIRS: &[&str] = &["session-state", "history-session-state"];

impl CopilotAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn workspace_storage_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            // macOS paths: ~/Library/Application Support/{fork}/User/workspaceStorage
            let app_support = home_dir.join("Library/Application Support");

            for fork in COPILOT_VSCODE_FORKS {
                let workspace_storage = app_support.join(fork).join("User/workspaceStorage");
                if workspace_storage.is_dir() {
                    dirs.push(workspace_storage);
                }
            }
        }

        dirs
    }

    fn cli_session_dirs() -> Vec<PathBuf> {
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
}

// GitHub Copilot-specific data structures based on the chat log format

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopilotChatSession {
    version: u32,
    requester_username: String,
    responder_username: String,
    initial_location: String,
    requests: Vec<CopilotRequest>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    creation_date: Option<i64>,
    #[serde(default)]
    last_message_date: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopilotRequest {
    request_id: String,
    message: CopilotMessage,
    response: Vec<CopilotResponsePart>,
    #[serde(default)]
    result: Option<CopilotResult>,
    timestamp: i64,
    #[serde(default)]
    model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CopilotMessage {
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum CopilotResponsePart {
    WithKind {
        kind: String,
        #[serde(flatten)]
        data: simd_json::OwnedValue,
    },
    PlainValue {
        value: String,
        #[serde(flatten)]
        other: simd_json::OwnedValue,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CopilotResult {
    #[serde(default)]
    metadata: Option<CopilotMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopilotMetadata {
    #[serde(default)]
    tool_call_results: Option<simd_json::OwnedValue>,
    #[serde(default)]
    tool_call_rounds: Option<Vec<CopilotToolCallRound>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopilotToolCallRound {
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    tool_calls: Vec<CopilotToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CopilotToolCall {
    name: String,
    arguments: String,
    #[serde(default)]
    id: Option<String>,
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
        }
    }

    fn has_assistant_content(&self) -> bool {
        !self.assistant_text_parts.is_empty()
            || !self.reasoning_parts.is_empty()
            || !self.tool_request_parts.is_empty()
            || !self.tool_result_parts.is_empty()
            || self.stats.tool_calls > 0
    }
}

// Helper function to count tokens in a string using tiktoken
fn count_tokens(text: &str) -> u64 {
    // Use o200k_base encoding (GPT-4o and newer models)
    match get_bpe_from_model("o200k_base") {
        Ok(bpe) => {
            let count = bpe.encode_with_special_tokens(text).len();
            count as u64
        }
        Err(_) => {
            // Fallback: rough estimate of ~4 characters per token
            (text.len() / 4) as u64
        }
    }
}

// Recursively extract all text content from a nested JSON structure
fn extract_text_from_value(value: &simd_json::OwnedValue, accumulated_text: &mut String) {
    match value {
        // Only accumulate if it's a "text" field value, not metadata like URIs
        simd_json::OwnedValue::String(s)
            if !s.starts_with("vscode-")
                && !s.starts_with("file://")
                && !s.starts_with("ssh-remote") =>
        {
            accumulated_text.push_str(s);
            accumulated_text.push(' ');
        }
        simd_json::OwnedValue::Object(obj) => {
            // Look for "text" fields specifically
            if let Some(text_value) = obj.get("text")
                && let Some(text_str) = text_value.as_str()
            {
                accumulated_text.push_str(text_str);
                accumulated_text.push(' ');
            }
            // Recursively process all other fields
            for (_key, val) in obj.iter() {
                extract_text_from_value(val, accumulated_text);
            }
        }
        simd_json::OwnedValue::Array(arr) => {
            for item in arr.iter() {
                extract_text_from_value(item, accumulated_text);
            }
        }
        _ => {
            // Skip other types (numbers, booleans, null)
        }
    }
}

// Helper function to extract project ID from Copilot file path and hash it
fn extract_and_hash_project_id_copilot(_file_path: &Path) -> String {
    // Copilot path format: ~/.vscode/extensions/github.copilot-chat-*/sessions/{session-id}.json
    // We'll use "copilot-global" as the project identifier since sessions aren't project-specific
    hash_text("copilot-global")
}

fn is_probably_tool_json_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with("[{")) && trimmed.contains("\"tool\"")
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

fn is_copilot_cli_session_file(path: &Path) -> bool {
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

fn flush_copilot_cli_turn(
    entries: &mut Vec<ConversationMessage>,
    current_turn: &mut Option<CopilotCliTurn>,
    turn_index: &mut usize,
    conversation_hash: &str,
    project_hash: &str,
    session_name: Option<&String>,
) {
    let Some(turn) = current_turn.take() else {
        return;
    };

    let user_local_hash = format!("{conversation_hash}-cli-user-{}", *turn_index);
    let user_global_hash = hash_text(&format!(
        "{project_hash}:{conversation_hash}:cli:user:{}:{}",
        *turn_index,
        turn.user_date.to_rfc3339()
    ));

    entries.push(ConversationMessage {
        application: Application::Copilot,
        date: turn.user_date,
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

    if turn.has_assistant_content() {
        let assistant_date = turn.assistant_date.unwrap_or(turn.user_date);
        let assistant_local_hash = format!("{conversation_hash}-cli-assistant-{}", *turn_index);
        let assistant_global_hash = hash_text(&format!(
            "{project_hash}:{conversation_hash}:cli:assistant:{}:{}",
            *turn_index,
            assistant_date.to_rfc3339()
        ));

        let mut assistant_stats = turn.stats;
        let input_text = std::iter::once(turn.user_text.as_str())
            .chain(turn.tool_result_parts.iter().map(String::as_str))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let output_text = turn
            .reasoning_parts
            .iter()
            .chain(turn.tool_request_parts.iter())
            .chain(turn.assistant_text_parts.iter())
            .map(String::as_str)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");

        assistant_stats.input_tokens = count_tokens(&input_text);
        assistant_stats.output_tokens = count_tokens(&output_text);

        entries.push(ConversationMessage {
            application: Application::Copilot,
            date: assistant_date,
            project_hash: project_hash.to_string(),
            conversation_hash: conversation_hash.to_string(),
            local_hash: Some(assistant_local_hash),
            global_hash: assistant_global_hash,
            model: turn.model,
            stats: assistant_stats,
            role: MessageRole::Assistant,
            uuid: None,
            session_name: session_name.cloned(),
        });
    }

    *turn_index += 1;
}

// Helper function to extract model from model_id field
fn extract_model_from_model_id(model_id: &str) -> Option<String> {
    // Model ID format examples:
    // "generic-copilot/litellm/anthropic/claude-haiku-4.5"
    // "LiteLLM/Sonnet 4.5"

    // Try to parse the full path format first
    if let Some(last_part) = model_id.split('/').next_back() {
        return Some(last_part.to_string());
    }

    // Otherwise return the whole string
    Some(model_id.to_string())
}

// Count tool invocations in a response
fn count_tool_calls(response: &[CopilotResponsePart]) -> u32 {
    response
        .iter()
        .filter(|part| {
            matches!(part, CopilotResponsePart::WithKind { kind, .. } if kind == "toolInvocationSerialized")
        })
        .count() as u32
}

// Extract file operation stats from tool call metadata
fn extract_file_operations(metadata: &CopilotMetadata) -> Stats {
    let mut stats = Stats::default();

    // Parse tool call results to extract file operations
    if let Some(tool_call_rounds) = &metadata.tool_call_rounds {
        for round in tool_call_rounds {
            for tool_call in &round.tool_calls {
                // Count different types of tool calls based on the tool name
                match tool_call.name.as_str() {
                    "read_file" => stats.files_read += 1,
                    "replace_string_in_file" | "multi_replace_string_in_file" => {
                        stats.files_edited += 1
                    }
                    "create_file" => stats.files_added += 1,
                    "file_search" => stats.file_searches += 1,
                    "grep_search" | "semantic_search" => stats.file_content_searches += 1,
                    "run_in_terminal" => stats.terminal_commands += 1,
                    _ => {}
                }
            }
        }
    }

    stats
}

// Parse a single Copilot chat session file
pub(crate) fn parse_copilot_session_file(session_file: &Path) -> Result<Vec<ConversationMessage>> {
    let project_hash = extract_and_hash_project_id_copilot(session_file);

    // Read and parse the session file
    let mut session_content = std::fs::read_to_string(session_file)?.into_bytes();
    let session: CopilotChatSession = simd_json::from_slice(&mut session_content)
        .context("Failed to parse Copilot session file")?;

    // Get the conversation hash from the session_id or file name
    let conversation_hash = session
        .session_id
        .as_ref()
        .map(|id| hash_text(id))
        .unwrap_or_else(|| {
            session_file
                .file_stem()
                .and_then(|n| n.to_str())
                .map(hash_text)
                .unwrap_or_else(|| hash_text(&session_file.to_string_lossy()))
        });

    let mut entries = Vec::new();
    let mut fallback_session_name: Option<String> = None;

    // Process each request-response pair
    for (idx, request) in session.requests.iter().enumerate() {
        // Extract model from model_id or result metadata
        let model = request
            .model_id
            .as_ref()
            .and_then(|id| extract_model_from_model_id(id));

        // Estimate input tokens from user message
        let mut input_tokens = count_tokens(&request.message.text);

        // Estimate output tokens from model responses
        let mut output_tokens = 0;

        // Count tokens from tool call rounds (model's thinking + tool requests)
        if let Some(result) = &request.result
            && let Some(metadata) = &result.metadata
        {
            if let Some(tool_call_rounds) = &metadata.tool_call_rounds {
                for round in tool_call_rounds {
                    // The "response" field contains the model's thinking before making tool calls
                    if let Some(response_text) = &round.response {
                        output_tokens += count_tokens(response_text);
                    }

                    // Count tool call requests (name + arguments) as output
                    for tool_call in &round.tool_calls {
                        output_tokens += count_tokens(&tool_call.name);
                        output_tokens += count_tokens(&tool_call.arguments);
                    }
                }
            }

            // Count tool call results as input tokens (these are fed back to the model)
            // Extract actual text content from the nested structure
            if let Some(tool_results) = &metadata.tool_call_results {
                let mut extracted_text = String::new();
                extract_text_from_value(tool_results, &mut extracted_text);
                input_tokens += count_tokens(&extracted_text);
            }
        }

        // Count tokens from the final response shown to the user
        for part in &request.response {
            match part {
                CopilotResponsePart::PlainValue { value, .. } => {
                    // This is actual text output from the model
                    output_tokens += count_tokens(value);
                }
                CopilotResponsePart::WithKind { .. } => {
                    // Don't count tool invocation serialized or other metadata
                    // These are just UI elements, not model output
                    // The actual tool calls are already counted in tool_call_rounds above
                    // Skip - already counted in tool_call_rounds or not model output
                }
            }
        }

        // Capture fallback session name from the first user message text
        if fallback_session_name.is_none() && !request.message.text.is_empty() {
            let text_str = request.message.text.clone();
            if !is_probably_tool_json_text(&text_str) {
                let truncated = if text_str.chars().count() > 50 {
                    let chars: String = text_str.chars().take(50).collect();
                    format!("{}...", chars)
                } else {
                    text_str
                };
                fallback_session_name = Some(truncated);
            }
        }

        // Create user message
        let user_date = DateTime::from_timestamp_millis(request.timestamp).unwrap_or_else(Utc::now);

        let user_local_hash = format!("{}-user-{}", conversation_hash, idx);
        let user_global_hash = hash_text(&format!(
            "{}:{}:user:{}:{}",
            project_hash, conversation_hash, idx, request.timestamp
        ));

        entries.push(ConversationMessage {
            application: Application::Copilot,
            date: user_date,
            project_hash: project_hash.clone(),
            conversation_hash: conversation_hash.clone(),
            local_hash: Some(user_local_hash),
            global_hash: user_global_hash,
            model: None,
            stats: Stats::default(),
            role: MessageRole::User,
            uuid: None,
            session_name: fallback_session_name.clone(),
        });

        // Create assistant message
        let assistant_date = user_date; // Use same timestamp as user message
        let assistant_local_hash = format!("{}-assistant-{}", conversation_hash, idx);
        let assistant_global_hash = hash_text(&format!(
            "{}:{}:assistant:{}:{}",
            project_hash, conversation_hash, idx, request.timestamp
        ));

        // Count tool calls
        let tool_calls = count_tool_calls(&request.response);

        // Extract file operations if metadata is available
        let mut stats = Stats {
            tool_calls,
            input_tokens,
            output_tokens,
            ..Default::default()
        };

        if let Some(result) = &request.result
            && let Some(metadata) = &result.metadata
        {
            let file_ops = extract_file_operations(metadata);
            stats.files_read += file_ops.files_read;
            stats.files_edited += file_ops.files_edited;
            stats.files_added += file_ops.files_added;
            stats.files_deleted += file_ops.files_deleted;
            stats.file_searches += file_ops.file_searches;
            stats.file_content_searches += file_ops.file_content_searches;
            stats.terminal_commands += file_ops.terminal_commands;
        }

        entries.push(ConversationMessage {
            application: Application::Copilot,
            date: assistant_date,
            project_hash: project_hash.clone(),
            conversation_hash: conversation_hash.clone(),
            local_hash: Some(assistant_local_hash),
            global_hash: assistant_global_hash,
            model,
            stats,
            role: MessageRole::Assistant,
            uuid: None,
            session_name: fallback_session_name.clone(),
        });
    }

    Ok(entries)
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
    let mut turn_index = 0usize;
    let mut current_turn: Option<CopilotCliTurn> = None;

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
                    &mut turn_index,
                    &conversation_hash,
                    &project_hash,
                    session_name.as_ref(),
                );

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

                current_turn = Some(CopilotCliTurn::new(
                    user_text,
                    event_timestamp.unwrap_or_else(Utc::now),
                    current_model.clone(),
                ));
            }
            "assistant.message" | "assistant.message.delta" => {
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));
                turn.model = current_model.clone().or_else(|| turn.model.clone());

                if let Some(data) = event_data.as_object() {
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
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));

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
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));
                turn.stats.tool_calls += 1;

                if let Some(data) = event_data.as_object() {
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
                let Some(turn) = current_turn.as_mut() else {
                    continue;
                };

                turn.assistant_date
                    .get_or_insert_with(|| event_timestamp.unwrap_or(turn.user_date));

                if let Some(data) = event_data.as_object()
                    && let Some(result) = data.get("result")
                {
                    let text = extract_text_from_cli_value(result);
                    if !text.trim().is_empty() {
                        turn.tool_result_parts.push(text);
                    }
                }
            }
            "abort" | "session.error" => {
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
        &mut turn_index,
        &conversation_hash,
        &project_hash,
        session_name.as_ref(),
    );

    Ok(entries)
}

#[async_trait]
impl Analyzer for CopilotAnalyzer {
    fn display_name(&self) -> &'static str {
        "GitHub Copilot"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        // VSCode forks that might have Copilot installed: Code, Cursor, Windsurf, VSCodium, Positron, Antigravity
        let vscode_forks = [
            "Code",
            "Cursor",
            "Windsurf",
            "VSCodium",
            "Positron",
            "Code - Insiders",
            "Antigravity",
        ];

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();

            // macOS paths for all VSCode forks
            for fork in &vscode_forks {
                patterns.push(format!("{home_str}/Library/Application Support/{fork}/User/workspaceStorage/*/chatSessions/*.json"));
            }

            for dir_name in COPILOT_CLI_STATE_DIRS {
                patterns.push(format!("{home_str}/.copilot/{dir_name}/*.jsonl"));
                patterns.push(format!("{home_str}/.copilot/{dir_name}/*/events.jsonl"));
            }
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let mut sources: Vec<DataSource> = Self::workspace_storage_dirs()
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).min_depth(3).max_depth(3).into_iter())
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path().extension().is_some_and(|ext| ext == "json")
                    && e.path()
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|name| name == "chatSessions")
            })
            .map(|e| DataSource {
                path: e.into_path(),
            })
            .collect();

        sources.extend(
            Self::cli_session_dirs()
                .into_iter()
                .flat_map(|dir| WalkDir::new(dir).min_depth(1).max_depth(2).into_iter())
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file() && is_copilot_cli_session_file(e.path()))
                .map(|e| DataSource {
                    path: e.into_path(),
                }),
        );

        Ok(sources)
    }

    fn is_available(&self) -> bool {
        Self::workspace_storage_dirs()
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).min_depth(3).max_depth(3).into_iter())
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_type().is_file()
                    && e.path().extension().is_some_and(|ext| ext == "json")
                    && e.path()
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|name| name == "chatSessions")
            })
            || Self::cli_session_dirs()
                .into_iter()
                .flat_map(|dir| WalkDir::new(dir).min_depth(1).max_depth(2).into_iter())
                .filter_map(|e| e.ok())
                .any(|e| e.file_type().is_file() && is_copilot_cli_session_file(e.path()))
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        if is_copilot_cli_session_file(&source.path) {
            parse_copilot_cli_session_file(&source.path)
        } else {
            parse_copilot_session_file(&source.path)
        }
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        let mut dirs = Self::workspace_storage_dirs();
        dirs.extend(Self::cli_session_dirs());
        dirs
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        // Must be a .json file in a "chatSessions" directory
        path.is_file()
            && ((path.extension().is_some_and(|ext| ext == "json")
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|name| name == "chatSessions"))
                || is_copilot_cli_session_file(path))
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_project_hash() {
        let path = PathBuf::from(
            "/home/user/.vscode/extensions/github.copilot-chat-0.22.4/sessions/test-session.json",
        );
        let hash = extract_and_hash_project_id_copilot(&path);
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 hex length
    }

    #[test]
    fn test_extract_model_from_model_id() {
        assert_eq!(
            extract_model_from_model_id("generic-copilot/litellm/anthropic/claude-haiku-4.5"),
            Some("claude-haiku-4.5".to_string())
        );
        assert_eq!(
            extract_model_from_model_id("LiteLLM/Sonnet 4.5"),
            Some("Sonnet 4.5".to_string())
        );
    }

    #[test]
    fn test_count_tool_calls() {
        use simd_json::OwnedValue;

        let response = vec![
            CopilotResponsePart::PlainValue {
                value: "Hello".to_string(),
                other: OwnedValue::Object(Default::default()),
            },
            CopilotResponsePart::WithKind {
                kind: "toolInvocationSerialized".to_string(),
                data: OwnedValue::Object(Default::default()),
            },
            CopilotResponsePart::WithKind {
                kind: "toolInvocationSerialized".to_string(),
                data: OwnedValue::Object(Default::default()),
            },
        ];
        assert_eq!(count_tool_calls(&response), 2);
    }

    #[test]
    fn test_count_tokens() {
        // Test basic token counting
        let text = "Hello, world!";
        let token_count = count_tokens(text);
        // Should be around 3-4 tokens for "Hello, world!"
        assert!(token_count > 0);
        assert!(token_count < 10);

        // Test empty string
        assert_eq!(count_tokens(""), 0);

        // Test longer text
        let long_text = "This is a longer piece of text that should have more tokens.";
        let long_count = count_tokens(long_text);
        assert!(long_count > token_count);
    }

    #[test]
    fn test_extract_text_from_value() {
        use simd_json::OwnedValue;

        // Test extracting text from nested structure using JSON parsing
        let json_str = r#"{
            "text": "Hello world",
            "priority": 100,
            "children": [
                {"text": "Nested text"}
            ]
        }"#;

        let mut bytes = json_str.as_bytes().to_vec();
        let value: OwnedValue = simd_json::from_slice(&mut bytes).unwrap();

        let mut extracted = String::new();
        extract_text_from_value(&value, &mut extracted);

        assert!(extracted.contains("Hello world"));
        assert!(extracted.contains("Nested text"));
    }

    #[test]
    fn test_is_copilot_cli_session_file() {
        let nested_path =
            PathBuf::from("/home/user/.copilot/session-state/12345678-1234/events.jsonl");
        assert!(is_copilot_cli_session_file(&nested_path));

        let flat_path = PathBuf::from("/home/user/.copilot/history-session-state/test.jsonl");
        assert!(is_copilot_cli_session_file(&flat_path));

        let invalid_path =
            PathBuf::from("/home/user/.copilot/session-state/12345678-1234/meta.json");
        assert!(!is_copilot_cli_session_file(&invalid_path));
    }
}
