use crate::analyzer::{Analyzer, DataSource};
use crate::cache::FileCacheEntry;
use crate::models::calculate_total_cost;
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, FileMetadata, MessageRole, Stats,
};
use crate::utils::hash_text;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use glob::glob;
use jwalk::WalkDir;
use rayon::prelude::*;
use serde::Deserialize;
use simd_json::OwnedValue;
use simd_json::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub struct OpenCodeAnalyzer;

impl OpenCodeAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenCodeProjectTime {
    created: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenCodeProject {
    id: String,
    worktree: String,
    vcs: String,
    time: OpenCodeProjectTime,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenCodeSessionTime {
    created: i64,
    updated: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenCodeSessionSummary {
    #[serde(default)]
    additions: i64,
    #[serde(default)]
    deletions: i64,
    #[serde(default)]
    files: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenCodeSession {
    id: String,
    #[serde(rename = "projectID")]
    project_id: String,
    directory: String,
    title: String,
    time: OpenCodeSessionTime,
    summary: OpenCodeSessionSummary,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct OpenCodeMessageTime {
    #[serde(default)]
    created: Option<i64>,
    #[serde(default)]
    completed: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct OpenCodeMessageSummary {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    diffs: Vec<OwnedValue>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct OpenCodeModelRef {
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct OpenCodeCacheTokens {
    #[serde(default)]
    read: u64,
    #[serde(default)]
    write: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct OpenCodeTokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    reasoning: u64,
    #[serde(default)]
    cache: OpenCodeCacheTokens,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct OpenCodeMessagePath {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    root: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct OpenCodeMessage {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    role: String,
    #[serde(default)]
    time: OpenCodeMessageTime,
    #[serde(default)]
    summary: Option<OpenCodeMessageSummary>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(rename = "model")]
    #[serde(default)]
    model_ref: Option<OpenCodeModelRef>,
    #[serde(rename = "modelID")]
    #[serde(default)]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    path: Option<OpenCodeMessagePath>,
    #[serde(default)]
    cost: Option<f64>,
    #[serde(default)]
    tokens: Option<OpenCodeTokens>,
    #[serde(default)]
    finish: Option<String>,
}

impl OpenCodeMessage {
    fn model_name(&self) -> Option<String> {
        if let Some(model_id) = &self.model_id
            && !model_id.is_empty()
        {
            return Some(model_id.clone());
        }
        if let Some(model_ref) = &self.model_ref
            && !model_ref.model_id.is_empty()
        {
            return Some(model_ref.model_id.clone());
        }
        None
    }
}

fn ms_to_datetime(ms: Option<i64>) -> DateTime<Utc> {
    let ms = ms.unwrap_or(0);
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
}

fn load_projects(project_root: &Path) -> HashMap<String, OpenCodeProject> {
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
                    if let Ok(project) = simd_json::from_slice::<OpenCodeProject>(&mut bytes) {
                        projects.insert(project.id.clone(), project);
                    }
                }
                Err(_) => continue,
            }
        }
    }

    projects
}

fn load_sessions(session_root: &Path) -> HashMap<String, OpenCodeSession> {
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
                        if let Ok(session) = simd_json::from_slice::<OpenCodeSession>(&mut bytes) {
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
    msg: OpenCodeMessage,
    sessions: &HashMap<String, OpenCodeSession>,
    projects: &HashMap<String, OpenCodeProject>,
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
    let global_hash = hash_text(&format!("opencode_{}_{}", msg.session_id, msg.id));

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
            // Prefer explicit cost from OpenCode if present
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

    ConversationMessage {
        application: Application::OpenCode,
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
        session_name: session.map(|s| s.title.clone()),
    }
}

#[async_trait]
impl Analyzer for OpenCodeAnalyzer {
    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            // Message JSON files – presence of at least one indicates OpenCode usage.
            patterns.push(format!(
                "{home_str}/.local/share/opencode/storage/message/*/*.json"
            ));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let mut sources = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let message_dir = home_dir.join(".local/share/opencode/storage/message");

            if message_dir.is_dir() {
                // Pattern: ~/.local/share/opencode/storage/message/*/*.json
                // jwalk walks directories in parallel
                for entry in WalkDir::new(&message_dir)
                    .min_depth(2) // */*.json
                    .max_depth(2)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_type().is_file()
                            && e.path().extension().is_some_and(|ext| ext == "json")
                    })
                {
                    sources.push(DataSource { path: entry.path() });
                }
            }
        }

        Ok(sources)
    }

    async fn parse_conversations(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<Vec<ConversationMessage>> {
        let home_dir = dirs::home_dir().context("Could not find home directory")?;
        let storage_root = home_dir.join(".local/share/opencode/storage");
        let project_root = storage_root.join("project");
        let session_root = storage_root.join("session");
        let part_root = storage_root.join("part");

        let projects = load_projects(&project_root);
        let sessions = load_sessions(&session_root);

        let messages: Vec<ConversationMessage> = sources
            .into_par_iter()
            .filter_map(|source| {
                let path = source.path;
                let content = match fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Failed to read OpenCode message {}: {e}", path.display());
                        return None;
                    }
                };

                let mut bytes = content.into_bytes();
                let msg = match simd_json::from_slice::<OpenCodeMessage>(&mut bytes) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("Failed to parse OpenCode message {}: {e}", path.display());
                        return None;
                    }
                };

                Some(to_conversation_message(
                    msg, &sessions, &projects, &part_root,
                ))
            })
            .collect();

        Ok(messages)
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

    fn supports_caching(&self) -> bool {
        true
    }

    fn parse_single_file(&self, source: &DataSource) -> Result<FileCacheEntry> {
        let metadata = FileMetadata::from_path(&source.path)?;

        // Load project and session mappings (needed for message parsing)
        let home_dir = dirs::home_dir().context("Could not find home directory")?;
        let storage_root = home_dir.join(".local/share/opencode/storage");
        let project_root = storage_root.join("project");
        let session_root = storage_root.join("session");
        let part_root = storage_root.join("part");

        let projects = load_projects(&project_root);
        let sessions = load_sessions(&session_root);

        // Parse the single message file
        let content = fs::read_to_string(&source.path)?;
        let mut bytes = content.into_bytes();
        let msg: OpenCodeMessage = simd_json::from_slice(&mut bytes)?;
        let conv_msg = to_conversation_message(msg, &sessions, &projects, &part_root);

        let messages = vec![conv_msg];
        let daily_contributions = crate::utils::aggregate_by_date_simple(&messages);

        Ok(FileCacheEntry {
            metadata,
            messages,
            daily_contributions,
            cached_model: None,
        })
    }
}
