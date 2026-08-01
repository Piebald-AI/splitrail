use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::models::calculate_total_cost_for_context_at;
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::Deserialize;
use simd_json::prelude::*;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use super::copilot::count_tokens;

pub struct GrokAnalyzer;

impl GrokAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|home| home.join(".grok").join("sessions"))
    }
}

#[derive(Debug, Deserialize)]
struct GrokToolCall {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct GrokChatRecord {
    #[serde(rename = "type", default)]
    record_type: String,
    #[serde(default)]
    content: Option<simd_json::OwnedValue>,
    #[serde(default)]
    synthetic_reason: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<GrokToolCall>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cached_read_tokens: u64,
    #[serde(default)]
    cache_creation_tokens: u64,
    #[serde(default)]
    reasoning_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct GrokUpdateRecord {
    #[serde(default)]
    params: Option<GrokUpdateParams>,
}

#[derive(Debug, Deserialize)]
struct GrokUpdateParams {
    #[serde(default)]
    update: Option<GrokSessionUpdate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokSessionUpdate {
    #[serde(default)]
    session_update: Option<String>,
    #[serde(default)]
    usage: Option<GrokUsage>,
}

#[derive(Debug, Deserialize)]
struct GrokSessionSummary {
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
}

fn is_grok_chat_path(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .is_some_and(|name| name == "chat_history.jsonl")
        && path
            .parent()
            .and_then(Path::parent)
            .is_some_and(|project_dir| project_dir.parent().is_some())
}

fn truncate_session_name(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let truncated: String = text.chars().take(50).collect();
    Some(if truncated.chars().count() < text.chars().count() {
        format!("{truncated}...")
    } else {
        truncated
    })
}

fn user_text_for_session_name(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(start) = text.find("<user_query>") {
        let query = &text[start + "<user_query>".len()..];
        return query
            .split_once("</user_query>")
            .map(|(query, _)| query.trim().to_string());
    }

    if text.starts_with("<user_info>") || text.starts_with("<system-reminder>") {
        return None;
    }

    Some(text.to_string())
}

fn text_from_content(content: &simd_json::OwnedValue) -> Option<String> {
    match content {
        simd_json::OwnedValue::String(text) => Some(text.to_string()),
        simd_json::OwnedValue::Array(items) => items.iter().find_map(|item| {
            item.get("text")
                .and_then(|text| text.as_str())
                .map(ToOwned::to_owned)
        }),
        simd_json::OwnedValue::Object(object) => object
            .get("text")
            .and_then(|text| text.as_str())
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn extract_tool_stats(tool_calls: &[GrokToolCall]) -> Stats {
    let mut stats = Stats {
        tool_calls: tool_calls.len() as u32,
        ..Default::default()
    };

    for tool_call in tool_calls {
        match tool_call.name.as_str() {
            "read_file" | "list_dir" | "list_directory" => stats.files_read += 1,
            "grep" | "search" | "rg" => stats.file_content_searches += 1,
            "run_terminal_command" | "run_terminal" => stats.terminal_commands += 1,
            "write_file" | "create_file" => stats.files_added += 1,
            "apply_patch" | "edit_file" => stats.files_edited += 1,
            _ => {}
        }
    }

    stats
}

fn parse_turn_usages(chat_history_path: &Path) -> Vec<GrokUsage> {
    let Some(session_dir) = chat_history_path.parent() else {
        return Vec::new();
    };
    let updates_path = session_dir.join("updates.jsonl");
    let Ok(content) = std::fs::read_to_string(updates_path) else {
        return Vec::new();
    };

    content
        .lines()
        .filter_map(|line| {
            let mut bytes = line.as_bytes().to_vec();
            let record = simd_json::from_slice::<GrokUpdateRecord>(&mut bytes).ok()?;
            let update = record.params?.update?;
            if update.session_update.as_deref() != Some("turn_completed") {
                return None;
            }
            Some(update.usage.unwrap_or_default())
        })
        .collect()
}

fn distribute_u64(total: u64, indices: &[usize], weights: &[u64]) -> Vec<u64> {
    if indices.is_empty() {
        return Vec::new();
    }

    let total_weight: u128 = indices
        .iter()
        .map(|index| u128::from(weights[*index].max(1)))
        .sum();
    let mut distributed = 0_u64;

    indices
        .iter()
        .enumerate()
        .map(|(position, index)| {
            if position + 1 == indices.len() {
                return total.saturating_sub(distributed);
            }

            let weight = u128::from(weights[*index].max(1));
            let share = ((u128::from(total) * weight) / total_weight) as u64;
            distributed = distributed.saturating_add(share);
            share
        })
        .collect()
}

fn distribute_f64(total: f64, indices: &[usize], weights: &[u64]) -> Vec<f64> {
    if indices.is_empty() {
        return Vec::new();
    }

    let total_weight: f64 = indices
        .iter()
        .map(|index| weights[*index].max(1) as f64)
        .sum();
    let mut distributed = 0.0;

    indices
        .iter()
        .enumerate()
        .map(|(position, index)| {
            if position + 1 == indices.len() {
                return total - distributed;
            }

            let weight = weights[*index].max(1) as f64;
            let share = total * weight / total_weight;
            distributed += share;
            share
        })
        .collect()
}

fn apply_turn_usage(
    messages: &mut [ConversationMessage],
    assistant_groups: &[Vec<usize>],
    message_weights: &[u64],
    usages: &[GrokUsage],
) {
    for (group, usage) in assistant_groups.iter().zip(usages) {
        if group.is_empty() {
            continue;
        }

        let input_tokens = distribute_u64(usage.input_tokens, group, message_weights);
        let output_tokens = distribute_u64(usage.output_tokens, group, message_weights);
        let cached_read_tokens = distribute_u64(usage.cached_read_tokens, group, message_weights);
        let cache_creation_tokens =
            distribute_u64(usage.cache_creation_tokens, group, message_weights);
        let reasoning_tokens = distribute_u64(usage.reasoning_tokens, group, message_weights);
        // Grok's local CLI may report a discounted or subscription cost in
        // costUsdTicks. Splitrail uses the public API standard price table so
        // the value is comparable with the other API-based analyzers.
        let estimated_costs = group.first().and_then(|index| {
            messages[*index].model.as_deref().map(|model| {
                let context_tokens = usage
                    .input_tokens
                    .saturating_add(usage.cached_read_tokens)
                    .saturating_add(usage.cache_creation_tokens);
                let total_cost = calculate_total_cost_for_context_at(
                    model,
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_creation_tokens,
                    usage.cached_read_tokens,
                    context_tokens,
                    Some(messages[*index].date),
                );
                distribute_f64(total_cost, group, message_weights)
            })
        });

        for (position, index) in group.iter().enumerate() {
            let stats = &mut messages[*index].stats;
            stats.input_tokens = input_tokens[position];
            stats.output_tokens = output_tokens[position];
            stats.cache_read_tokens = cached_read_tokens[position];
            stats.cache_creation_tokens = cache_creation_tokens[position];
            stats.cached_tokens = stats.cache_read_tokens + stats.cache_creation_tokens;
            stats.reasoning_tokens = reasoning_tokens[position];
            if let Some(estimated_costs) = &estimated_costs {
                stats.cost = estimated_costs[position];
            }
        }
    }
}

fn session_metadata(path: &Path) -> (DateTime<Utc>, Option<String>) {
    let summary_path = path
        .parent()
        .map(|session_dir| session_dir.join("summary.json"));

    if let Some(summary_path) = summary_path
        && let Ok(mut bytes) = std::fs::read(summary_path)
        && let Ok(summary) = simd_json::from_slice::<GrokSessionSummary>(&mut bytes)
    {
        let date = summary.created_at.unwrap_or_else(Utc::now);
        return (date, None);
    }

    let date = path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());
    (date, None)
}

pub fn parse_chat_history_file(path: &Path) -> Result<Vec<ConversationMessage>> {
    let project_dir = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let project_hash = hash_text(&project_dir);
    let conversation_hash = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| hash_text(&name.to_string_lossy()))
        .unwrap_or_else(|| hash_text(&path.to_string_lossy()));
    let file_path = path.to_string_lossy();
    let (date, mut session_name) = session_metadata(path);
    let content = std::fs::read_to_string(path)?;
    let mut messages = Vec::new();
    let mut assistant_groups: Vec<Vec<usize>> = Vec::new();
    let mut message_weights = Vec::new();

    for (line_index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let mut bytes = line.as_bytes().to_vec();
        let record = match simd_json::from_slice::<GrokChatRecord>(&mut bytes) {
            Ok(record) => record,
            Err(_) => continue,
        };

        if record.synthetic_reason.is_some() {
            continue;
        }

        let (role, model, stats, weight) = match record.record_type.as_str() {
            "user" => {
                let user_text = record
                    .content
                    .as_ref()
                    .and_then(text_from_content)
                    .and_then(|text| user_text_for_session_name(&text));
                if user_text.is_none() {
                    continue;
                }
                if session_name.is_none() {
                    session_name = user_text.and_then(|text| truncate_session_name(&text));
                }
                assistant_groups.push(Vec::new());
                (MessageRole::User, None, Stats::default(), 0)
            }
            "assistant" => {
                let tool_calls = record.tool_calls.unwrap_or_default();
                let output_text = record
                    .content
                    .as_ref()
                    .and_then(text_from_content)
                    .unwrap_or_default();
                let weight = count_tokens(&output_text)
                    .saturating_add(tool_calls.len() as u64)
                    .max(1);
                (
                    MessageRole::Assistant,
                    record.model_id,
                    extract_tool_stats(&tool_calls),
                    weight,
                )
            }
            _ => continue,
        };

        let message_index = messages.len();
        if role == MessageRole::Assistant
            && let Some(group) = assistant_groups.last_mut()
        {
            group.push(message_index);
        }
        message_weights.push(weight);

        messages.push(ConversationMessage {
            application: Application::Grok,
            date,
            project_hash: project_hash.clone(),
            conversation_hash: conversation_hash.clone(),
            local_hash: Some(format!("{conversation_hash}:{line_index}")),
            global_hash: hash_text(&format!("{file_path}:{line_index}")),
            model,
            stats,
            role,
            uuid: None,
            session_name: session_name.clone(),
        });
    }

    // `updates.jsonl` contains authoritative per-turn usage. Chat history does
    // not carry token counts, so distribute each turn total across its model
    // calls using the existing tokenizer as a stable weight.
    let usages = parse_turn_usages(path);
    apply_turn_usage(&mut messages, &assistant_groups, &message_weights, &usages);

    Ok(messages)
}

#[async_trait]
impl Analyzer for GrokAnalyzer {
    fn display_name(&self) -> &'static str {
        "Grok"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        Self::data_dir()
            .map(|dir| format!("{}/*/*/chat_history.jsonl", dir.to_string_lossy()))
            .into_iter()
            .collect()
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = Self::data_dir()
            .filter(|dir| dir.is_dir())
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).into_iter())
            .filter_map(|entry| entry.ok())
            .filter(|entry| is_grok_chat_path(entry.path()))
            .map(|entry| DataSource {
                path: entry.into_path(),
            })
            .collect();

        Ok(sources)
    }

    fn is_available(&self) -> bool {
        Self::data_dir()
            .filter(|dir| dir.is_dir())
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).into_iter())
            .filter_map(|entry| entry.ok())
            .any(|entry| is_grok_chat_path(entry.path()))
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        parse_chat_history_file(&source.path)
    }

    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        let messages: Vec<_> = sources
            .par_iter()
            .flat_map(|source| self.parse_source(source).unwrap_or_default())
            .collect();
        crate::utils::deduplicate_by_global_hash(messages)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Self::data_dir()
            .filter(|dir| dir.is_dir())
            .into_iter()
            .collect()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        is_grok_chat_path(path)
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }

    fn requires_full_reload_for_source_change(&self) -> bool {
        // Usage is stored beside chat_history.jsonl in updates.jsonl, so a
        // change to either file can change the contribution for the session.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_real_messages_and_skips_synthetic_context() {
        let dir = tempdir().expect("temporary directory should be created");
        let project_dir = dir.path().join("project");
        let session_dir = project_dir.join("session");
        std::fs::create_dir_all(&session_dir).expect("session directory should be created");
        std::fs::write(
            session_dir.join("summary.json"),
            r#"{"created_at":"2026-08-01T12:00:00Z"}"#,
        )
        .expect("summary should be written");
        std::fs::write(
            session_dir.join("chat_history.jsonl"),
            concat!(
                r#"{"type":"user","synthetic_reason":"project_instructions","content":[{"type":"text","text":"ignored"}]}"#, "\n",
                r#"{"type":"user","content":[{"type":"text","text":"Implement Grok support"}]}"#, "\n",
                r#"{"type":"assistant","model_id":"grok-4.5","tool_calls":[{"name":"read_file"},{"name":"run_terminal_command"}]}"#, "\n",
            ),
        )
        .expect("chat history should be written");
        std::fs::write(
            session_dir.join("updates.jsonl"),
            r#"{"params":{"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":100,"outputTokens":20,"cachedReadTokens":30,"cacheCreationTokens":4,"reasoningTokens":5,"costUsdTicks":10000000000}}}}"#,
        )
        .expect("updates should be written");

        let messages = parse_chat_history_file(&session_dir.join("chat_history.jsonl"))
            .expect("chat history should parse");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert_eq!(messages[1].model.as_deref(), Some("grok-4.5"));
        assert_eq!(messages[1].stats.tool_calls, 2);
        assert_eq!(messages[1].stats.files_read, 1);
        assert_eq!(messages[1].stats.terminal_commands, 1);
        assert_eq!(messages[1].stats.input_tokens, 100);
        assert_eq!(messages[1].stats.output_tokens, 20);
        assert_eq!(messages[1].stats.cache_read_tokens, 30);
        assert_eq!(messages[1].stats.cache_creation_tokens, 4);
        assert_eq!(messages[1].stats.reasoning_tokens, 5);
        assert!((messages[1].stats.cost - 0.000329).abs() < f64::EPSILON);
        assert_eq!(
            messages[0].session_name.as_deref(),
            Some("Implement Grok support")
        );
    }
}
