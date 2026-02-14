use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::models::calculate_total_cost;
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use glob::glob;
use rayon::prelude::*;
use serde::Deserialize;
use simd_json::OwnedValue;
use simd_json::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Analyzer for Kilo Code CLI — a terminal-based AI coding agent forked from
/// OpenCode.  Data lives under `~/.local/share/kilo/storage/` with the same
/// message-per-file layout as OpenCode: one JSON file per message, organised
/// into per-session directories, with companion session/project/part metadata.
pub struct KiloCliAnalyzer;

impl KiloCliAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".local/share/kilo/storage/message"))
    }

    fn storage_root() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".local/share/kilo/storage"))
    }
}

// ---------------------------------------------------------------------------
// Serde types – mirrors the on-disk JSON schema produced by the Kilo CLI.
// Structurally identical to OpenCode but kept separate so the two analysers
// can evolve independently if the formats ever diverge.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct KiloCliProjectTime {
    created: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct KiloCliProject {
    id: String,
    worktree: String,
    #[serde(default)]
    vcs: Option<String>,
    time: KiloCliProjectTime,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct KiloCliSessionTime {
    created: i64,
    updated: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct KiloCliSessionSummary {
    #[serde(default)]
    additions: i64,
    #[serde(default)]
    deletions: i64,
    #[serde(default)]
    files: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct KiloCliSession {
    id: String,
    #[serde(rename = "projectID")]
    project_id: String,
    directory: String,
    #[serde(default)]
    title: Option<String>,
    time: KiloCliSessionTime,
    #[serde(default)]
    summary: Option<KiloCliSessionSummary>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct KiloCliMessageTime {
    #[serde(default)]
    created: Option<i64>,
    #[serde(default)]
    completed: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct KiloCliMessageSummaryDetails {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    diffs: Vec<OwnedValue>,
}

/// The `summary` field can be either a boolean flag (`true`) indicating a
/// summary/compaction message, or an object with details.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum KiloCliMessageSummary {
    Flag(bool),
    Details(KiloCliMessageSummaryDetails),
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct KiloCliModelRef {
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct KiloCliCacheTokens {
    #[serde(default)]
    read: u64,
    #[serde(default)]
    write: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct KiloCliTokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    reasoning: u64,
    #[serde(default)]
    cache: KiloCliCacheTokens,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct KiloCliMessagePath {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    root: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct KiloCliMessage {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    role: String,
    #[serde(default)]
    time: KiloCliMessageTime,
    #[serde(default)]
    summary: Option<KiloCliMessageSummary>,
    #[serde(default)]
    agent: Option<String>,
    /// User messages carry the model ref inside a nested `model` object.
    #[serde(rename = "model")]
    #[serde(default)]
    model_ref: Option<KiloCliModelRef>,
    /// Assistant messages carry the model ID at the top level.
    #[serde(rename = "modelID")]
    #[serde(default)]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(rename = "parentID")]
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    path: Option<KiloCliMessagePath>,
    #[serde(default)]
    cost: Option<f64>,
    #[serde(default)]
    tokens: Option<KiloCliTokens>,
    #[serde(default)]
    finish: Option<String>,
}

impl KiloCliMessage {
    fn model_name(&self) -> Option<String> {
        // Prefer top-level modelID (set on assistant messages)
        if let Some(model_id) = &self.model_id
            && !model_id.is_empty()
        {
            return Some(model_id.clone());
        }
        // Fall back to nested model ref (set on user messages)
        if let Some(model_ref) = &self.model_ref
            && !model_ref.model_id.is_empty()
        {
            return Some(model_ref.model_id.clone());
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ms_to_datetime(ms: Option<i64>) -> DateTime<Utc> {
    let ms = ms.unwrap_or(0);
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
}

fn load_projects(project_root: &Path) -> HashMap<String, KiloCliProject> {
    let mut projects = HashMap::new();

    if !project_root.is_dir() {
        return projects;
    }

    let pattern = project_root.join("*.json");
    let pattern_str = pattern.to_string_lossy().to_string();

    if let Ok(paths) = glob(&pattern_str) {
        for entry in paths {
            let Ok(path) = entry else { continue };
            match fs::read_to_string(&path) {
                Ok(content) => {
                    let mut bytes = content.into_bytes();
                    if let Ok(project) = simd_json::from_slice::<KiloCliProject>(&mut bytes) {
                        projects.insert(project.id.clone(), project);
                    }
                }
                Err(_) => continue,
            }
        }
    }

    projects
}

fn load_sessions(session_root: &Path) -> HashMap<String, KiloCliSession> {
    let mut sessions = HashMap::new();

    if !session_root.is_dir() {
        return sessions;
    }

    let entries = match fs::read_dir(session_root) {
        Ok(entries) => entries,
        Err(_) => return sessions,
    };

    for project_dir in entries.flatten() {
        let path = project_dir.path();
        if !path.is_dir() {
            continue;
        }

        let pattern = path.join("*.json");
        let pattern_str = pattern.to_string_lossy().to_string();

        if let Ok(paths) = glob(&pattern_str) {
            for entry in paths {
                let Ok(session_path) = entry else { continue };
                match fs::read_to_string(&session_path) {
                    Ok(content) => {
                        let mut bytes = content.into_bytes();
                        if let Ok(session) = simd_json::from_slice::<KiloCliSession>(&mut bytes) {
                            sessions.insert(session.id.clone(), session);
                        }
                    }
                    Err(_) => continue,
                }
            }
        }
    }

    sessions
}

fn extract_tool_stats_from_parts(part_root: &Path, message_id: &str) -> Stats {
    let mut stats = Stats::default();

    let dir = part_root.join(message_id);
    if !dir.is_dir() {
        return stats;
    }

    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return stats,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut bytes = content.into_bytes();
        let Ok(value) = simd_json::from_slice::<OwnedValue>(&mut bytes) else {
            continue;
        };

        let Some(part_type) = value.get("type").and_then(|v| v.as_str()) else {
            continue;
        };

        if part_type != "tool" {
            continue;
        }

        let Some(tool_name) = value.get("tool").and_then(|v| v.as_str()) else {
            continue;
        };

        stats.tool_calls += 1;

        match tool_name {
            "read" => {
                stats.files_read += 1;
            }
            "glob" => {
                stats.file_searches += 1;
                if let Some(count) = value
                    .get("state")
                    .and_then(|s| s.get("metadata"))
                    .and_then(|m| m.get("count"))
                    .and_then(|c| c.as_u64())
                {
                    stats.files_read += count;
                }
            }
            _ => {}
        }
    }

    stats
}

fn to_conversation_message(
    msg: KiloCliMessage,
    sessions: &HashMap<String, KiloCliSession>,
    projects: &HashMap<String, KiloCliProject>,
    part_root: &Path,
) -> ConversationMessage {
    let session = sessions.get(&msg.session_id);
    let project = session.and_then(|s| projects.get(&s.project_id));

    let project_hash = if let Some(project) = project {
        hash_text(&project.worktree)
    } else if let Some(session) = session {
        hash_text(&session.id)
    } else {
        hash_text(&msg.session_id)
    };

    let conversation_hash = hash_text(&msg.session_id);

    let local_hash = Some(msg.id.clone());
    let global_hash = hash_text(&format!("kilo_cli_{}_{}", msg.session_id, msg.id));

    let date = ms_to_datetime(msg.time.created);

    let mut stats = if msg.role == "assistant" {
        let mut s = extract_tool_stats_from_parts(part_root, &msg.id);

        if let Some(tokens) = &msg.tokens {
            s.input_tokens = tokens.input;
            s.output_tokens = tokens.output;
            s.reasoning_tokens = tokens.reasoning;
            s.cache_creation_tokens = tokens.cache.write;
            s.cache_read_tokens = tokens.cache.read;
            s.cached_tokens = tokens.cache.read;

            if let Some(model_name) = msg.model_name() {
                s.cost = calculate_total_cost(
                    &model_name,
                    s.input_tokens,
                    s.output_tokens,
                    s.cache_creation_tokens,
                    s.cache_read_tokens,
                );
            }
        }

        if let Some(cost) = msg.cost {
            // Prefer explicit cost from Kilo CLI if present
            if cost > 0.0 {
                s.cost = cost;
            }
        }

        s
    } else {
        Stats::default()
    };

    if msg.role == "assistant"
        && stats.tool_calls == 0
        && let Some(tokens) = &msg.tokens
        && (tokens.input > 0 || tokens.output > 0)
    {
        // Ensure tool_calls is at least 1 when we had a model call
        stats.tool_calls = 1;
    }

    let session_name = session.and_then(|s| s.title.clone());

    ConversationMessage {
        application: Application::KiloCli,
        date,
        project_hash,
        conversation_hash,
        local_hash,
        global_hash,
        model: msg.model_name(),
        stats,
        role: if msg.role == "user" {
            MessageRole::User
        } else {
            MessageRole::Assistant
        },
        uuid: None,
        session_name,
    }
}

// ---------------------------------------------------------------------------
// Analyzer implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Analyzer for KiloCliAnalyzer {
    fn display_name(&self) -> &'static str {
        "Kilo CLI"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            // Message JSON files — presence of at least one indicates Kilo CLI usage.
            patterns.push(format!(
                "{home_str}/.local/share/kilo/storage/message/*/*.json"
            ));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|message_dir| {
                WalkDir::new(message_dir)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
            })
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "json")
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
            .flat_map(|message_dir| {
                WalkDir::new(message_dir)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
            })
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "json")
            })
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        let storage_root =
            Self::storage_root().context("Could not determine Kilo CLI storage root")?;
        let project_root = storage_root.join("project");
        let session_root = storage_root.join("session");
        let part_root = storage_root.join("part");

        let projects = load_projects(&project_root);
        let sessions = load_sessions(&session_root);

        let content = fs::read_to_string(&source.path)?;
        let mut bytes = content.into_bytes();
        let msg = simd_json::from_slice::<KiloCliMessage>(&mut bytes)?;

        Ok(vec![to_conversation_message(
            msg, &sessions, &projects, &part_root,
        )])
    }

    // Load shared context once, then process all files in parallel.
    fn parse_sources_parallel_with_paths(
        &self,
        sources: &[DataSource],
    ) -> Vec<(PathBuf, Vec<ConversationMessage>)> {
        let Some(storage_root) = Self::storage_root() else {
            eprintln!("Could not determine Kilo CLI storage root");
            return Vec::new();
        };

        let project_root = storage_root.join("project");
        let session_root = storage_root.join("session");
        let part_root = storage_root.join("part");

        let projects = load_projects(&project_root);
        let sessions = load_sessions(&session_root);

        sources
            .par_iter()
            .filter_map(|source| {
                let content = fs::read_to_string(&source.path).ok()?;
                let mut bytes = content.into_bytes();
                let msg = simd_json::from_slice::<KiloCliMessage>(&mut bytes).ok()?;
                let conversation_msg =
                    to_conversation_message(msg, &sessions, &projects, &part_root);
                Some((source.path.clone(), vec![conversation_msg]))
            })
            .collect()
    }

    // Each message file is unique — no deduplication needed.
    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        self.parse_sources_parallel_with_paths(sources)
            .into_iter()
            .flat_map(|(_, msgs)| msgs)
            .collect()
    }

    fn get_stats_with_sources(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<crate::types::AgenticCodingToolStats> {
        let storage_root =
            Self::storage_root().context("Could not determine Kilo CLI storage root")?;
        let project_root = storage_root.join("project");
        let session_root = storage_root.join("session");
        let part_root = storage_root.join("part");

        let projects = load_projects(&project_root);
        let sessions = load_sessions(&session_root);

        let messages: Vec<ConversationMessage> = sources
            .par_iter()
            .filter_map(|source| {
                let content = fs::read_to_string(&source.path).ok()?;
                let mut bytes = content.into_bytes();
                let msg = simd_json::from_slice::<KiloCliMessage>(&mut bytes).ok()?;
                Some(to_conversation_message(
                    msg, &sessions, &projects, &part_root,
                ))
            })
            .collect();

        let mut daily_stats = crate::utils::aggregate_by_date(&messages);
        daily_stats.retain(|date, _| date != "unknown");
        let num_conversations = daily_stats
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        Ok(crate::types::AgenticCodingToolStats {
            daily_stats,
            num_conversations,
            messages,
            analyzer_name: self.display_name().to_string(),
        })
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .collect()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        // Must be a file with .json extension
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
            return false;
        }
        // Must be at depth 2 from data_dir (session_id/message_id.json)
        if let Some(data_dir) = Self::data_dir()
            && let Ok(relative) = path.strip_prefix(&data_dir)
        {
            return relative.components().count() == 2;
        }
        false
    }

    // Each Kilo CLI message file contains exactly one message.
    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleMessage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_assistant_message() {
        let json = r#"{
            "id": "msg_c5cc84bbf001a3bQs6VdR97IUK",
            "sessionID": "ses_3a33896dbffeieFOLryTAxfy7D",
            "role": "assistant",
            "time": { "created": 1771083156415, "completed": 1771083174019 },
            "parentID": "msg_c5cc84bb4001bKJ6xO4CN6ri8O",
            "modelID": "z-ai/glm-5:free",
            "providerID": "kilo",
            "mode": "code",
            "agent": "code",
            "cost": 0.017154,
            "tokens": {
                "total": 57407,
                "input": 1207,
                "output": 1569,
                "reasoning": 281,
                "cache": { "read": 54631, "write": 0 }
            },
            "finish": "stop"
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.id, "msg_c5cc84bbf001a3bQs6VdR97IUK");
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.model_name().unwrap(), "z-ai/glm-5:free");
        assert_eq!(msg.cost, Some(0.017154));

        let tokens = msg.tokens.unwrap();
        assert_eq!(tokens.input, 1207);
        assert_eq!(tokens.output, 1569);
        assert_eq!(tokens.reasoning, 281);
        assert_eq!(tokens.cache.read, 54631);
        assert_eq!(tokens.cache.write, 0);
    }

    #[test]
    fn test_parse_user_message_with_nested_model() {
        let json = r#"{
            "id": "msg_c5cc84bb4001bKJ6xO4CN6ri8O",
            "sessionID": "ses_3a33896dbffeieFOLryTAxfy7D",
            "role": "user",
            "time": { "created": 1771083156409 },
            "summary": { "title": "Count correction: only 2 items", "diffs": [] },
            "agent": "code",
            "model": { "providerID": "kilo", "modelID": "z-ai/glm-5:free" }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.id, "msg_c5cc84bb4001bKJ6xO4CN6ri8O");
        assert_eq!(msg.role, "user");
        // User messages carry the model in a nested `model` object
        assert_eq!(msg.model_name().unwrap(), "z-ai/glm-5:free");
        assert!(msg.tokens.is_none());
    }

    #[test]
    fn test_parse_message_with_boolean_summary() {
        let json = r#"{
            "id": "msg_test_bool_summary",
            "sessionID": "ses_test",
            "role": "assistant",
            "time": { "created": 1771083156415, "completed": 1771083174019 },
            "modelID": "claude-sonnet-4-20250514",
            "providerID": "anthropic",
            "summary": true,
            "tokens": { "input": 9, "output": 101, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
            "finish": "stop"
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert!(matches!(
            msg.summary,
            Some(KiloCliMessageSummary::Flag(true))
        ));
    }

    #[test]
    fn test_parse_message_with_object_summary() {
        let json = r#"{
            "id": "msg_test_obj_summary",
            "sessionID": "ses_test",
            "role": "user",
            "time": { "created": 1771083156409 },
            "summary": { "title": "Codebase explanation request", "diffs": [] },
            "agent": "code",
            "model": { "providerID": "kilo", "modelID": "openrouter/aurora-alpha" }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert!(matches!(
            msg.summary,
            Some(KiloCliMessageSummary::Details(_))
        ));
    }

    #[test]
    fn test_parse_message_without_summary() {
        let json = r#"{
            "id": "msg_test_no_summary",
            "sessionID": "ses_test",
            "role": "user",
            "time": { "created": 1771083156409 }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert!(msg.summary.is_none());
    }

    #[test]
    fn test_parse_error_message() {
        // Messages with errors should still parse (the error field is untyped/ignored)
        let json = r#"{
            "id": "msg_c5cc15675001svpGaPEh0EfefL",
            "sessionID": "ses_3a33ea9afffeaFys5zprxBZ3gy",
            "role": "assistant",
            "time": { "created": 1771082700405, "completed": 1771082701087 },
            "error": {
                "name": "APIError",
                "data": { "message": "Not Found", "statusCode": 404 }
            },
            "parentID": "msg_c5cc15656001VZPrwMOKTGs07p",
            "modelID": "anthropic/claude-opus-4.6:slackbot",
            "providerID": "kilo",
            "mode": "code",
            "agent": "code",
            "cost": 0,
            "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.id, "msg_c5cc15675001svpGaPEh0EfefL");
        assert_eq!(
            msg.model_name().unwrap(),
            "anthropic/claude-opus-4.6:slackbot"
        );
    }

    #[test]
    fn test_parse_tool_calls_finish_message() {
        // Messages with finish="tool-calls" indicate multi-step tool use
        let json = r#"{
            "id": "msg_test_tool_calls",
            "sessionID": "ses_test",
            "role": "assistant",
            "time": { "created": 1771083098429, "completed": 1771083101385 },
            "modelID": "z-ai/glm-5:free",
            "providerID": "kilo",
            "mode": "code",
            "agent": "code",
            "cost": 0.0130238,
            "tokens": {
                "total": 13215,
                "input": 12691,
                "output": 76,
                "reasoning": 28,
                "cache": { "read": 448, "write": 0 }
            },
            "finish": "tool-calls"
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.finish, Some("tool-calls".to_string()));
    }

    #[test]
    fn test_model_name_prefers_top_level() {
        // When both modelID and model ref are present, top-level modelID wins
        let json = r#"{
            "id": "msg_both",
            "sessionID": "ses_test",
            "role": "assistant",
            "time": { "created": 1771083156415 },
            "modelID": "top-level-model",
            "model": { "providerID": "kilo", "modelID": "nested-model" }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.model_name().unwrap(), "top-level-model");
    }

    #[test]
    fn test_model_name_falls_back_to_nested() {
        // When modelID is absent, fall back to nested model ref
        let json = r#"{
            "id": "msg_nested",
            "sessionID": "ses_test",
            "role": "user",
            "time": { "created": 1771083156409 },
            "model": { "providerID": "kilo", "modelID": "nested-only" }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloCliMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.model_name().unwrap(), "nested-only");
    }

    #[test]
    fn test_ms_to_datetime_valid() {
        let dt = ms_to_datetime(Some(1771083156415));
        assert_eq!(dt.timestamp_millis(), 1771083156415);
    }

    #[test]
    fn test_ms_to_datetime_none() {
        let dt = ms_to_datetime(None);
        assert_eq!(dt.timestamp(), 0);
    }

    #[test]
    fn test_load_projects_nonexistent_dir() {
        let projects = load_projects(Path::new("/nonexistent/path"));
        assert!(projects.is_empty());
    }

    #[test]
    fn test_load_sessions_nonexistent_dir() {
        let sessions = load_sessions(Path::new("/nonexistent/path"));
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_extract_tool_stats_nonexistent_dir() {
        let stats = extract_tool_stats_from_parts(Path::new("/nonexistent"), "msg_fake");
        assert_eq!(stats.tool_calls, 0);
        assert_eq!(stats.files_read, 0);
    }
}
