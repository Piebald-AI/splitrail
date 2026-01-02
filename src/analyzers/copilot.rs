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
        simd_json::OwnedValue::String(s) => {
            // Only accumulate if it's a "text" field value, not metadata like URIs
            if !s.starts_with("vscode-")
                && !s.starts_with("file://")
                && !s.starts_with("ssh-remote")
            {
                accumulated_text.push_str(s);
                accumulated_text.push(' ');
            }
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
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = Self::workspace_storage_dirs()
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
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        parse_copilot_session_file(&source.path)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Self::workspace_storage_dirs()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        // Must be a .json file in a "chatSessions" directory
        path.is_file()
            && path.extension().is_some_and(|ext| ext == "json")
            && path
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|name| name == "chatSessions")
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
}
