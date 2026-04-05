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
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use simd_json::OwnedValue;
use simd_json::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct OpenCodeAnalyzer;

impl OpenCodeAnalyzer {
    pub fn new() -> Self {
        Self
    }

    /// `~/.local/share/opencode/storage/message` — legacy JSON message files.
    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".local/share/opencode/storage/message"))
    }

    /// `~/.local/share/opencode/storage` — legacy JSON storage root.
    fn storage_root() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".local/share/opencode/storage"))
    }

    /// `~/.local/share/opencode` — parent directory (for watching the DB).
    fn app_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".local/share/opencode"))
    }

    /// Discover all OpenCode SQLite database files.
    ///
    /// OpenCode stores data in `opencode.db` for the default/latest/beta channel,
    /// and `opencode-{channel}.db` for other channels (e.g. `opencode-canary.db`).
    fn discover_db_files() -> Vec<PathBuf> {
        let Some(app_dir) = Self::app_dir() else {
            return Vec::new();
        };
        if !app_dir.is_dir() {
            return Vec::new();
        }

        let pattern = app_dir.join("opencode*.db");
        let pattern_str = pattern.to_string_lossy().to_string();

        glob(&pattern_str)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|p| {
                // Accept "opencode.db" and "opencode-{channel}.db", but reject
                // WAL/SHM journal files and unrelated matches.
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                name == "opencode.db" || (name.starts_with("opencode-") && name.ends_with(".db"))
            })
            .collect()
    }

    /// Check if any SQLite database file exists on disk.
    fn has_sqlite_db() -> bool {
        !Self::discover_db_files().is_empty()
    }

    /// Check if the legacy JSON message directory has any files.
    fn has_json_messages() -> bool {
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
}

// ---------------------------------------------------------------------------
// Serde types — mirrors the on-disk JSON schema produced by OpenCode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeProjectTime {
    pub(crate) created: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeProject {
    pub(crate) id: String,
    pub(crate) worktree: String,
    #[serde(default)]
    pub(crate) vcs: Option<String>,
    pub(crate) time: OpenCodeProjectTime,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeSessionTime {
    pub(crate) created: i64,
    pub(crate) updated: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeSessionSummary {
    #[serde(default)]
    pub(crate) additions: i64,
    #[serde(default)]
    pub(crate) deletions: i64,
    #[serde(default)]
    pub(crate) files: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeSession {
    pub(crate) id: String,
    #[serde(rename = "projectID")]
    pub(crate) project_id: String,
    pub(crate) directory: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    pub(crate) time: OpenCodeSessionTime,
    #[serde(default)]
    pub(crate) summary: Option<OpenCodeSessionSummary>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeMessageTime {
    #[serde(default)]
    pub(crate) created: Option<i64>,
    #[serde(default)]
    pub(crate) completed: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeMessageSummaryDetails {
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) diffs: Vec<OwnedValue>,
}

/// The `summary` field can be either a boolean flag (`true`) indicating a summary message,
/// or an object containing summary details. This enum handles both cases.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum OpenCodeMessageSummary {
    Flag(bool),
    Details(OpenCodeMessageSummaryDetails),
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeModelRef {
    #[serde(rename = "providerID")]
    pub(crate) provider_id: String,
    #[serde(rename = "modelID")]
    pub(crate) model_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct OpenCodeCacheTokens {
    #[serde(default)]
    pub(crate) read: u64,
    #[serde(default)]
    pub(crate) write: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct OpenCodeTokens {
    #[serde(default)]
    pub(crate) input: u64,
    #[serde(default)]
    pub(crate) output: u64,
    #[serde(default)]
    pub(crate) reasoning: u64,
    #[serde(default)]
    pub(crate) cache: OpenCodeCacheTokens,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeMessagePath {
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) root: Option<String>,
}

/// Represents a single message from the OpenCode on-disk format.
///
/// Works for both the legacy JSON files (where `id`/`sessionID` are in the JSON)
/// and the new SQLite database (where they come from DB columns and the `data`
/// JSON blob omits them).
///
/// Also used by the Kilo CLI analyzer, which shares the identical on-disk format.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub(crate) struct OpenCodeMessage {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(rename = "sessionID")]
    #[serde(default)]
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) role: String,
    #[serde(default)]
    pub(crate) time: OpenCodeMessageTime,
    #[serde(default)]
    pub(crate) summary: Option<OpenCodeMessageSummary>,
    #[serde(default)]
    pub(crate) agent: Option<String>,
    #[serde(rename = "model")]
    #[serde(default)]
    pub(crate) model_ref: Option<OpenCodeModelRef>,
    #[serde(rename = "modelID")]
    #[serde(default)]
    pub(crate) model_id: Option<String>,
    #[serde(rename = "providerID")]
    #[serde(default)]
    pub(crate) provider_id: Option<String>,
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
    #[serde(default)]
    pub(crate) mode: Option<String>,
    #[serde(default)]
    pub(crate) path: Option<OpenCodeMessagePath>,
    #[serde(default)]
    pub(crate) cost: Option<f64>,
    #[serde(default)]
    pub(crate) tokens: Option<OpenCodeTokens>,
    #[serde(default)]
    pub(crate) finish: Option<String>,
}

impl OpenCodeMessage {
    pub(crate) fn model_name(&self) -> Option<String> {
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

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub(crate) fn ms_to_datetime(ms: Option<i64>) -> DateTime<Utc> {
    let ms = ms.unwrap_or(0);
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
}

/// Compute stats from an [`OpenCodeMessage`], optionally merging in pre-loaded
/// tool-call stats (from parts data).
pub(crate) fn compute_message_stats(msg: &OpenCodeMessage, tool_stats: Stats) -> Stats {
    if msg.role != "assistant" {
        return Stats::default();
    }

    let mut s = tool_stats;

    if let Some(tokens) = &msg.tokens {
        s.input_tokens = tokens.input;
        s.output_tokens = tokens.output;
        s.reasoning_tokens = tokens.reasoning;
        s.cache_creation_tokens = tokens.cache.write;
        s.cache_read_tokens = tokens.cache.read;
        s.cached_tokens = tokens.cache.write + tokens.cache.read;

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

    if let Some(cost) = msg.cost
        && cost > 0.0
    {
        s.cost = cost;
    }

    // Ensure tool_calls is at least 1 when a model call happened
    if s.tool_calls == 0
        && let Some(tokens) = &msg.tokens
        && (tokens.input > 0 || tokens.output > 0)
    {
        s.tool_calls = 1;
    }

    s
}

/// Build a [`ConversationMessage`] from an [`OpenCodeMessage`] and pre-computed stats.
///
/// Accepts an [`Application`] variant and `hash_prefix` so that both OpenCode and
/// Kilo CLI can share this logic with their respective identities.
pub(crate) fn build_conversation_message(
    msg: OpenCodeMessage,
    session_title: Option<String>,
    project_worktree: Option<&str>,
    fallback_project_hash: Option<&str>,
    stats: Stats,
    application: Application,
    hash_prefix: &str,
) -> ConversationMessage {
    let project_hash = if let Some(worktree) = project_worktree {
        hash_text(worktree)
    } else if let Some(fallback) = fallback_project_hash {
        hash_text(fallback)
    } else {
        hash_text(&msg.session_id)
    };

    let conversation_hash = hash_text(&msg.session_id);
    let local_hash = Some(msg.id.clone());
    let global_hash = hash_text(&format!("{}_{}_{}", hash_prefix, msg.session_id, msg.id));
    let date = ms_to_datetime(msg.time.created);

    ConversationMessage {
        application,
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
        session_name: session_title,
    }
}

// ---------------------------------------------------------------------------
// Legacy JSON file helpers
// ---------------------------------------------------------------------------

pub(crate) fn load_projects(project_root: &Path) -> HashMap<String, OpenCodeProject> {
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

pub(crate) fn load_sessions(session_root: &Path) -> HashMap<String, OpenCodeSession> {
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

/// Accumulate a single tool-call part into `stats`.
///
/// Shared between the legacy filesystem path ([`extract_tool_stats_from_parts`])
/// and the SQLite path ([`batch_load_tool_stats_from_db`]) so the counting logic
/// stays in one place.
pub(crate) fn accumulate_tool_stat(stats: &mut Stats, tool_name: &str, value: &OwnedValue) {
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

pub(crate) fn extract_tool_stats_from_parts(part_root: &Path, message_id: &str) -> Stats {
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

        accumulate_tool_stat(&mut stats, tool_name, &value);
    }

    stats
}

/// Convert a legacy JSON message to a [`ConversationMessage`], reading parts
/// from the filesystem.
fn json_to_conversation_message(
    msg: OpenCodeMessage,
    sessions: &HashMap<String, OpenCodeSession>,
    projects: &HashMap<String, OpenCodeProject>,
    part_root: &Path,
) -> ConversationMessage {
    let session = sessions.get(&msg.session_id);
    let project = session.and_then(|s| projects.get(&s.project_id));

    let tool_stats = if msg.role == "assistant" {
        extract_tool_stats_from_parts(part_root, &msg.id)
    } else {
        Stats::default()
    };

    let stats = compute_message_stats(&msg, tool_stats);

    let session_title = session.and_then(|s| s.title.clone());
    let worktree = project.map(|p| p.worktree.as_str());
    let fallback = session.map(|s| s.id.as_str());

    build_conversation_message(
        msg,
        session_title,
        worktree,
        fallback,
        stats,
        Application::OpenCode,
        "opencode",
    )
}

// ---------------------------------------------------------------------------
// SQLite database helpers
// ---------------------------------------------------------------------------

/// Open an OpenCode-format SQLite database in read-only mode with WAL support.
pub(crate) fn open_db(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

/// Lightweight project data from the `project` table.
pub(crate) struct DbProject {
    pub(crate) worktree: String,
}

/// Lightweight session data from the `session` table.
pub(crate) struct DbSession {
    pub(crate) project_id: String,
    pub(crate) title: Option<String>,
}

/// Load projects from the SQLite `project` table.
pub(crate) fn load_projects_from_db(conn: &Connection) -> HashMap<String, DbProject> {
    let mut map = HashMap::new();

    let Ok(mut stmt) = conn.prepare("SELECT id, worktree FROM project") else {
        return map;
    };

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                DbProject {
                    worktree: row.get(1)?,
                },
            ))
        })
        .ok();

    if let Some(rows) = rows {
        for row in rows.flatten() {
            map.insert(row.0, row.1);
        }
    }

    map
}

/// Load sessions from the SQLite `session` table.
pub(crate) fn load_sessions_from_db(conn: &Connection) -> HashMap<String, DbSession> {
    let mut map = HashMap::new();

    let Ok(mut stmt) = conn.prepare("SELECT id, project_id, title FROM session") else {
        return map;
    };

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                DbSession {
                    project_id: row.get(1)?,
                    title: row.get::<_, Option<String>>(2)?,
                },
            ))
        })
        .ok();

    if let Some(rows) = rows {
        for row in rows.flatten() {
            map.insert(row.0, row.1);
        }
    }

    map
}

/// Batch-load tool call stats from the SQLite `part` table.
///
/// Returns a map of `message_id → Stats` with tool call counts.
/// Only loads parts that contain tool-type data (uses `json_extract`
/// to avoid deserializing non-tool parts).
pub(crate) fn batch_load_tool_stats_from_db(conn: &Connection) -> HashMap<String, Stats> {
    let mut map: HashMap<String, Stats> = HashMap::new();

    // Use json_extract to pre-filter for tool-type parts — avoids
    // deserializing text, reasoning, step-start, etc. parts which are
    // typically much larger.
    let Ok(mut stmt) = conn
        .prepare("SELECT message_id, data FROM part WHERE json_extract(data, '$.type') = 'tool'")
    else {
        return map;
    };

    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok();

    let Some(rows) = rows else { return map };

    for row in rows.flatten() {
        let (message_id, data) = row;

        let mut bytes = data.into_bytes();
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

        let stats = map.entry(message_id).or_default();
        accumulate_tool_stat(stats, tool_name, &value);
    }

    map
}

/// Batch-load step-finish token/cost data from the SQLite `part` table.
///
/// Returns a map of `message_id → StepFinishAgg` with aggregated token counts
/// and costs across all `step-finish` parts for that message. This is used as a
/// fallback when the message-level `data` blob has zero tokens (which can happen
/// in newer OpenCode versions where per-step accounting is the primary source).
#[derive(Debug, Clone, Default)]
pub(crate) struct StepFinishAgg {
    pub(crate) input: u64,
    pub(crate) output: u64,
    pub(crate) reasoning: u64,
    pub(crate) cache_read: u64,
    pub(crate) cache_write: u64,
    pub(crate) cost: f64,
}

pub(crate) fn batch_load_step_finish_from_db(conn: &Connection) -> HashMap<String, StepFinishAgg> {
    let mut map: HashMap<String, StepFinishAgg> = HashMap::new();

    let Ok(mut stmt) = conn.prepare(
        "SELECT message_id, data FROM part WHERE json_extract(data, '$.type') = 'step-finish'",
    ) else {
        return map;
    };

    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok();

    let Some(rows) = rows else { return map };

    for row in rows.flatten() {
        let (message_id, data) = row;

        let mut bytes = data.into_bytes();
        let Ok(value) = simd_json::from_slice::<OwnedValue>(&mut bytes) else {
            continue;
        };

        let Some(part_type) = value.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        if part_type != "step-finish" {
            continue;
        }

        let agg = map.entry(message_id).or_default();

        if let Some(tokens) = value.get("tokens") {
            agg.input += tokens.get("input").and_then(|v| v.as_u64()).unwrap_or(0);
            agg.output += tokens.get("output").and_then(|v| v.as_u64()).unwrap_or(0);
            agg.reasoning += tokens
                .get("reasoning")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if let Some(cache) = tokens.get("cache") {
                agg.cache_read += cache.get("read").and_then(|v| v.as_u64()).unwrap_or(0);
                agg.cache_write += cache.get("write").and_then(|v| v.as_u64()).unwrap_or(0);
            }
        }

        if let Some(cost) = value.get("cost").and_then(|v| v.as_f64()) {
            agg.cost += cost;
        }
    }

    map
}

/// Parse all messages from an OpenCode-format SQLite database.
///
/// Reads from the `message`, `session`, `project`, and `part` tables.
/// The `message.data` column is a JSON blob matching [`OpenCodeMessage`]
/// (minus `id` and `sessionID`, which come from the row columns).
///
/// Accepts [`Application`] and `hash_prefix` so both OpenCode and Kilo CLI
/// can share this logic with their respective identities.
pub(crate) fn parse_sqlite_messages(
    db_path: &Path,
    application: Application,
    hash_prefix: &str,
) -> Result<Vec<ConversationMessage>> {
    let conn = open_db(db_path)?;

    let db_projects = load_projects_from_db(&conn);
    let db_sessions = load_sessions_from_db(&conn);
    let tool_stats_map = batch_load_tool_stats_from_db(&conn);
    let step_finish_map = batch_load_step_finish_from_db(&conn);

    let mut stmt = conn.prepare("SELECT id, session_id, time_created, data FROM message")?;

    let messages: Vec<ConversationMessage> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .filter_map(|(id, session_id, time_created, data)| {
            // Parse the JSON data blob into an OpenCodeMessage.
            // The `id` and `sessionID` fields are not in the blob — inject
            // them from the DB columns after deserialization.
            let mut bytes = data.into_bytes();
            let mut msg = simd_json::from_slice::<OpenCodeMessage>(&mut bytes).ok()?;

            msg.id = id.clone();
            msg.session_id = session_id.clone();

            // Use DB column timestamp as the canonical creation time, falling
            // back to whatever the JSON blob contained.
            if msg.time.created.is_none() || msg.time.created == Some(0) {
                msg.time.created = Some(time_created);
            }

            // If the message-level tokens are all zero but step-finish parts
            // have token data, use the aggregated step-finish data instead.
            // This handles newer OpenCode versions where per-step accounting
            // is the primary data source.
            if msg.role == "assistant" {
                let msg_has_tokens = msg.tokens.as_ref().is_some_and(|t| {
                    t.input > 0
                        || t.output > 0
                        || t.reasoning > 0
                        || t.cache.read > 0
                        || t.cache.write > 0
                });
                if !msg_has_tokens
                    && let Some(agg) = step_finish_map.get(&id)
                    && (agg.input > 0
                        || agg.output > 0
                        || agg.reasoning > 0
                        || agg.cache_read > 0
                        || agg.cache_write > 0)
                {
                    msg.tokens = Some(OpenCodeTokens {
                        input: agg.input,
                        output: agg.output,
                        reasoning: agg.reasoning,
                        cache: OpenCodeCacheTokens {
                            read: agg.cache_read,
                            write: agg.cache_write,
                        },
                    });
                    if agg.cost > 0.0 && msg.cost.is_none_or(|c| c == 0.0) {
                        msg.cost = Some(agg.cost);
                    }
                }
            }

            // Resolve session/project metadata
            let session = db_sessions.get(&session_id);
            let project = session.and_then(|s| db_projects.get(&s.project_id));

            let tool_stats = tool_stats_map.get(&id).cloned().unwrap_or_default();
            let stats = compute_message_stats(&msg, tool_stats);

            let session_title = session.and_then(|s| s.title.clone());
            let worktree = project.map(|p| p.worktree.as_str());
            // Use session_id as fallback (consistent with the JSON path which
            // uses session.id — NOT project_id — so deduplication produces the
            // same project_hash regardless of which source a message came from).
            let fallback = Some(session_id.as_str());

            Some(build_conversation_message(
                msg,
                session_title,
                worktree,
                fallback,
                stats,
                application.clone(),
                hash_prefix,
            ))
        })
        .collect();

    Ok(messages)
}

// ---------------------------------------------------------------------------
// Analyzer trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Analyzer for OpenCodeAnalyzer {
    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            // Legacy JSON message files.
            patterns.push(format!(
                "{home_str}/.local/share/opencode/storage/message/*/*.json"
            ));
            // Default SQLite database.
            patterns.push(format!("{home_str}/.local/share/opencode/opencode.db"));
            // Channel-specific SQLite databases (e.g. opencode-canary.db).
            patterns.push(format!("{home_str}/.local/share/opencode/opencode-*.db"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let mut sources: Vec<DataSource> = Vec::new();

        // Discover legacy JSON message files.
        if let Some(data_dir) = Self::data_dir()
            && data_dir.is_dir()
        {
            let json_sources = WalkDir::new(data_dir)
                .min_depth(2)
                .max_depth(2)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "json")
                })
                .map(|e| DataSource {
                    path: e.into_path(),
                });
            sources.extend(json_sources);
        }

        // Discover all SQLite databases (default + channel-specific).
        for db in Self::discover_db_files() {
            sources.push(DataSource { path: db });
        }

        Ok(sources)
    }

    fn is_available(&self) -> bool {
        Self::has_sqlite_db() || Self::has_json_messages()
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        // SQLite database — return all messages at once.
        if source.path.extension().is_some_and(|ext| ext == "db") {
            return parse_sqlite_messages(&source.path, Application::OpenCode, "opencode");
        }

        // Legacy JSON message file — load context and parse single file.
        let storage_root = Self::storage_root().context("Could not determine storage root")?;
        let project_root = storage_root.join("project");
        let session_root = storage_root.join("session");
        let part_root = storage_root.join("part");

        let projects = load_projects(&project_root);
        let sessions = load_sessions(&session_root);

        let content = fs::read_to_string(&source.path)?;
        let mut bytes = content.into_bytes();
        let msg = simd_json::from_slice::<OpenCodeMessage>(&mut bytes)?;

        Ok(vec![json_to_conversation_message(
            msg, &sessions, &projects, &part_root,
        )])
    }

    /// Load shared context once, then process all JSON files in parallel.
    /// SQLite sources are handled separately since the DB query is already fast.
    fn parse_sources_parallel_with_paths(
        &self,
        sources: &[DataSource],
    ) -> Vec<(PathBuf, Vec<ConversationMessage>)> {
        // Partition sources into JSON files and DB files.
        let (db_sources, json_sources): (Vec<_>, Vec<_>) = sources
            .iter()
            .partition(|s| s.path.extension().is_some_and(|ext| ext == "db"));

        let mut results: Vec<(PathBuf, Vec<ConversationMessage>)> = Vec::new();

        // --- SQLite sources first: parse each DB ---
        // SQLite records are richer (have tool stats, step-finish tokens, etc.)
        // so they are added first. During deduplication (which keeps the first-
        // seen entry per global_hash), SQLite wins over legacy JSON.
        for source in db_sources {
            match parse_sqlite_messages(&source.path, Application::OpenCode, "opencode") {
                Ok(messages) if !messages.is_empty() => {
                    results.push((source.path.clone(), messages));
                }
                Ok(_) => {} // empty DB
                Err(e) => {
                    eprintln!(
                        "Failed to parse OpenCode SQLite DB {:?}: {}",
                        source.path, e
                    );
                }
            }
        }

        // --- JSON sources: load shared context once, parse in parallel ---
        if !json_sources.is_empty()
            && let Some(storage_root) = Self::storage_root()
        {
            let project_root = storage_root.join("project");
            let session_root = storage_root.join("session");
            let part_root = storage_root.join("part");

            let projects = load_projects(&project_root);
            let sessions = load_sessions(&session_root);

            let json_results: Vec<_> = json_sources
                .par_iter()
                .filter_map(|source| {
                    let content = fs::read_to_string(&source.path).ok()?;
                    let mut bytes = content.into_bytes();
                    let msg = simd_json::from_slice::<OpenCodeMessage>(&mut bytes).ok()?;
                    let conversation_msg =
                        json_to_conversation_message(msg, &sessions, &projects, &part_root);
                    Some((source.path.clone(), vec![conversation_msg]))
                })
                .collect();

            results.extend(json_results);
        }

        results
    }

    /// Parse all sources and deduplicate.
    ///
    /// Deduplication is necessary because messages may exist in both the legacy
    /// JSON files and the SQLite database during the migration period.
    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        let all: Vec<ConversationMessage> = self
            .parse_sources_parallel_with_paths(sources)
            .into_iter()
            .flat_map(|(_, msgs)| msgs)
            .collect();
        crate::utils::deduplicate_by_global_hash(all)
    }

    /// Reuses `parse_sources_parallel` for the shared partition → parse → dedup
    /// pipeline, then aggregates into stats.
    fn get_stats_with_sources(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<crate::types::AgenticCodingToolStats> {
        let messages = self.parse_sources_parallel(&sources);

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
        let mut dirs = Vec::new();

        // Watch the legacy JSON message directory.
        if let Some(data_dir) = Self::data_dir()
            && data_dir.is_dir()
        {
            dirs.push(data_dir);
        }

        // Watch the parent app directory for SQLite DB changes.
        if let Some(app_dir) = Self::app_dir()
            && app_dir.is_dir()
        {
            dirs.push(app_dir);
        }

        dirs
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        // Accept any OpenCode SQLite database file (opencode.db or opencode-{channel}.db).
        if path.is_file() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == "opencode.db" || (name.starts_with("opencode-") && name.ends_with(".db")) {
                return true;
            }
        }

        // Accept legacy JSON message files at depth 2 from the data_dir.
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
            return false;
        }
        if let Some(data_dir) = Self::data_dir()
            && let Ok(relative) = path.strip_prefix(&data_dir)
        {
            return relative.components().count() == 2;
        }
        false
    }

    /// When a SQLite database is present it contains many messages in a single
    /// file, so [`MultiSession`](ContributionStrategy::MultiSession) is the
    /// correct caching strategy. When only legacy JSON files exist, each file
    /// maps to a single message.
    fn contribution_strategy(&self) -> ContributionStrategy {
        if Self::has_sqlite_db() {
            ContributionStrategy::MultiSession
        } else {
            ContributionStrategy::SingleMessage
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- JSON message parsing tests ----

    // The `summary` field in OpenCode messages has 3 valid states observed in real data:
    //
    // 1. Absent - field not present
    // 2. Boolean - `"summary": true` (indicates a summary message; `false` not observed)
    // 3. Object - `"summary": { "title": "...", "body": "...", "diffs": [...] }`
    //
    // See: https://github.com/Piebald-AI/splitrail/issues/82

    #[test]
    fn test_parse_message_with_boolean_summary() {
        let json = r#"{
            "id": "msg_b1377b33f001HK4wL4AFesueYC",
            "sessionID": "ses_4ec88e3ceffeuc6U278whBC1TE",
            "role": "assistant",
            "time": { "created": 1765558170431, "completed": 1765558177390 },
            "modelID": "claude-opus-4-5",
            "providerID": "anthropic",
            "summary": true,
            "tokens": { "input": 9, "output": 101, "reasoning": 0, "cache": { "read": 0, "write": 162060 } },
            "finish": "stop"
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: OpenCodeMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.id, "msg_b1377b33f001HK4wL4AFesueYC");
        assert!(matches!(
            msg.summary,
            Some(OpenCodeMessageSummary::Flag(true))
        ));
    }

    #[test]
    fn test_parse_message_with_object_summary() {
        let json = r#"{
            "id": "msg_b42fdd2ed00115jZW5RSdppbds",
            "sessionID": "ses_4bd022d14ffeIcvK1800hA6gN2",
            "role": "user",
            "time": { "created": 1766355489517 },
            "summary": { "title": "Analyzing OpenCode summary field patterns", "diffs": [] },
            "agent": "general",
            "model": { "providerID": "anthropic", "modelID": "claude-opus-4-5" }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: OpenCodeMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.id, "msg_b42fdd2ed00115jZW5RSdppbds");
        assert!(matches!(
            msg.summary,
            Some(OpenCodeMessageSummary::Details(_))
        ));
    }

    #[test]
    fn test_parse_message_without_summary() {
        let json = r#"{
            "id": "msg_929a16848001TDUN2qM31WbRp6",
            "sessionID": "ses_6d65e97bdffepVt6J7EnV8BZdS",
            "role": "user",
            "time": { "created": 1757340067912 }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: OpenCodeMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.id, "msg_929a16848001TDUN2qM31WbRp6");
        assert!(msg.summary.is_none());
    }

    // ---- SQLite data blob parsing tests ----
    //
    // The `message.data` JSON blob in the SQLite DB omits `id` and `sessionID`
    // (those come from DB columns). These tests verify we can parse such blobs.

    #[test]
    fn test_parse_sqlite_assistant_data_blob() {
        // Data blob as stored in `message.data` (no id/sessionID fields).
        let json = r#"{
            "role": "assistant",
            "time": { "created": 1771083156415, "completed": 1771083174019 },
            "parentID": "msg_parent",
            "modelID": "claude-sonnet-4-20250514",
            "providerID": "anthropic",
            "agent": "coder",
            "cost": 0.0123,
            "tokens": {
                "total": 5000,
                "input": 3000,
                "output": 1500,
                "reasoning": 200,
                "cache": { "read": 2000, "write": 1000 }
            },
            "finish": "stop"
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let mut msg: OpenCodeMessage =
            simd_json::from_slice(&mut bytes).expect("should parse data blob");

        // Inject DB columns.
        msg.id = "msg_test_id".to_string();
        msg.session_id = "ses_test_session".to_string();

        assert_eq!(msg.id, "msg_test_id");
        assert_eq!(msg.session_id, "ses_test_session");
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.model_name().unwrap(), "claude-sonnet-4-20250514");
        assert_eq!(msg.cost, Some(0.0123));

        let tokens = msg.tokens.as_ref().unwrap();
        assert_eq!(tokens.input, 3000);
        assert_eq!(tokens.output, 1500);
        assert_eq!(tokens.reasoning, 200);
        assert_eq!(tokens.cache.read, 2000);
        assert_eq!(tokens.cache.write, 1000);
    }

    #[test]
    fn test_parse_sqlite_user_data_blob() {
        let json = r#"{
            "role": "user",
            "time": { "created": 1771083156409 },
            "agent": "coder",
            "model": { "providerID": "anthropic", "modelID": "claude-sonnet-4-20250514" }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let mut msg: OpenCodeMessage =
            simd_json::from_slice(&mut bytes).expect("should parse user data blob");

        msg.id = "msg_user".to_string();
        msg.session_id = "ses_test".to_string();

        assert_eq!(msg.role, "user");
        assert_eq!(msg.model_name().unwrap(), "claude-sonnet-4-20250514");
        assert!(msg.tokens.is_none());
    }

    #[test]
    fn test_parse_sqlite_minimal_data_blob() {
        // Minimal blob with just the role field.
        let json = r#"{ "role": "user" }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: OpenCodeMessage =
            simd_json::from_slice(&mut bytes).expect("should parse minimal blob");
        assert_eq!(msg.role, "user");
        assert!(msg.id.is_empty());
        assert!(msg.session_id.is_empty());
    }

    // ---- Stats computation tests ----

    #[test]
    fn test_compute_message_stats_assistant_with_cost() {
        let msg = OpenCodeMessage {
            role: "assistant".to_string(),
            cost: Some(0.05),
            tokens: Some(OpenCodeTokens {
                input: 1000,
                output: 500,
                reasoning: 100,
                cache: OpenCodeCacheTokens {
                    read: 2000,
                    write: 500,
                },
            }),
            model_id: Some("claude-sonnet-4-20250514".to_string()),
            ..Default::default()
        };
        let stats = compute_message_stats(&msg, Stats::default());
        assert_eq!(stats.input_tokens, 1000);
        assert_eq!(stats.output_tokens, 500);
        assert_eq!(stats.reasoning_tokens, 100);
        assert_eq!(stats.cache_read_tokens, 2000);
        assert_eq!(stats.cache_creation_tokens, 500);
        assert_eq!(stats.cached_tokens, 2500); // read + write
        // Explicit cost wins.
        assert!((stats.cost - 0.05).abs() < f64::EPSILON);
        assert_eq!(stats.tool_calls, 1); // at least 1 for model call
    }

    #[test]
    fn test_compute_message_stats_user() {
        let msg = OpenCodeMessage {
            role: "user".to_string(),
            ..Default::default()
        };
        let stats = compute_message_stats(&msg, Stats::default());
        assert_eq!(stats.input_tokens, 0);
        assert_eq!(stats.output_tokens, 0);
        assert_eq!(stats.cost, 0.0);
    }

    #[test]
    fn test_compute_message_stats_preserves_tool_stats() {
        let msg = OpenCodeMessage {
            role: "assistant".to_string(),
            tokens: Some(OpenCodeTokens {
                input: 100,
                output: 50,
                ..Default::default()
            }),
            model_id: Some("test-model".to_string()),
            ..Default::default()
        };
        let tool_stats = Stats {
            tool_calls: 5,
            files_read: 3,
            ..Default::default()
        };

        let stats = compute_message_stats(&msg, tool_stats);
        assert_eq!(stats.tool_calls, 5);
        assert_eq!(stats.files_read, 3);
    }

    // ---- build_conversation_message tests ----

    #[test]
    fn test_build_conversation_message_with_project() {
        let msg = OpenCodeMessage {
            id: "msg_123".to_string(),
            session_id: "ses_456".to_string(),
            role: "assistant".to_string(),
            time: OpenCodeMessageTime {
                created: Some(1700000000000),
                ..Default::default()
            },
            model_id: Some("claude-sonnet-4-20250514".to_string()),
            ..Default::default()
        };

        let conv = build_conversation_message(
            msg,
            Some("Test Session".to_string()),
            Some("/home/user/project"),
            None,
            Stats::default(),
            Application::OpenCode,
            "opencode",
        );

        assert_eq!(conv.application, Application::OpenCode);
        assert_eq!(conv.role, MessageRole::Assistant);
        assert_eq!(conv.session_name.as_deref(), Some("Test Session"));
        assert_eq!(conv.project_hash, hash_text("/home/user/project"));
        assert_eq!(conv.conversation_hash, hash_text("ses_456"));
        assert_eq!(conv.global_hash, hash_text("opencode_ses_456_msg_123"));
        assert_eq!(conv.model.as_deref(), Some("claude-sonnet-4-20250514"));
    }

    #[test]
    fn test_build_conversation_message_fallback_project_hash() {
        let msg = OpenCodeMessage {
            id: "msg_a".to_string(),
            session_id: "ses_b".to_string(),
            role: "user".to_string(),
            ..Default::default()
        };

        let conv = build_conversation_message(
            msg,
            None,
            None,
            Some("ses_b"),
            Stats::default(),
            Application::OpenCode,
            "opencode",
        );
        assert_eq!(conv.project_hash, hash_text("ses_b"));
    }

    #[test]
    fn test_build_conversation_message_session_id_fallback() {
        let msg = OpenCodeMessage {
            id: "msg_a".to_string(),
            session_id: "ses_c".to_string(),
            role: "user".to_string(),
            ..Default::default()
        };

        // No worktree, no fallback — uses session_id.
        let conv = build_conversation_message(
            msg,
            None,
            None,
            None,
            Stats::default(),
            Application::OpenCode,
            "opencode",
        );
        assert_eq!(conv.project_hash, hash_text("ses_c"));
    }

    // ---- global_hash consistency test (JSON ↔ SQLite) ----

    #[test]
    fn test_global_hash_matches_json_and_sqlite_paths() {
        // The global_hash for a message must be identical whether parsed from
        // a JSON file or from the SQLite database, so deduplication works.
        let session_id = "ses_3a33896dbffeieFOLryTAxfy7D";
        let message_id = "msg_c5cc84bbf001a3bQs6VdR97IUK";

        let expected = hash_text(&format!("opencode_{session_id}_{message_id}"));

        // Simulate JSON path.
        let json_msg = OpenCodeMessage {
            id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: "assistant".to_string(),
            ..Default::default()
        };
        let json_conv = build_conversation_message(
            json_msg,
            None,
            None,
            None,
            Stats::default(),
            Application::OpenCode,
            "opencode",
        );

        // Simulate SQLite path (id and session_id injected from DB columns).
        let mut sqlite_msg = OpenCodeMessage {
            role: "assistant".to_string(),
            ..Default::default()
        };
        sqlite_msg.id = message_id.to_string();
        sqlite_msg.session_id = session_id.to_string();
        let sqlite_conv = build_conversation_message(
            sqlite_msg,
            None,
            None,
            None,
            Stats::default(),
            Application::OpenCode,
            "opencode",
        );

        assert_eq!(json_conv.global_hash, expected);
        assert_eq!(sqlite_conv.global_hash, expected);
        assert_eq!(json_conv.global_hash, sqlite_conv.global_hash);
    }

    // ---- SQLite helper tests ----

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

    // ---- In-memory SQLite integration tests ----

    /// Create an in-memory DB matching the **initial** OpenCode SQLite schema
    /// (v1.1.53, migration `20260127222353_familiar_lady_ursula`).
    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();

        conn.execute_batch(
            "
            CREATE TABLE project (
                id TEXT PRIMARY KEY,
                worktree TEXT NOT NULL,
                vcs TEXT,
                name TEXT,
                icon_url TEXT,
                icon_color TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                time_initialized INTEGER,
                sandboxes TEXT NOT NULL DEFAULT '[]'
            );

            CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
                parent_id TEXT,
                slug TEXT NOT NULL DEFAULT '',
                directory TEXT NOT NULL,
                title TEXT NOT NULL,
                version TEXT NOT NULL DEFAULT '',
                share_url TEXT,
                summary_additions INTEGER,
                summary_deletions INTEGER,
                summary_files INTEGER,
                summary_diffs TEXT,
                revert TEXT,
                permission TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                time_compacting INTEGER,
                time_archived INTEGER
            );

            CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );

            CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL REFERENCES message(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );

            CREATE INDEX message_session_idx ON message (session_id);
            CREATE INDEX part_message_idx ON part (message_id);
            CREATE INDEX part_session_idx ON part (session_id);
            CREATE INDEX session_project_idx ON session (project_id);
            CREATE INDEX session_parent_idx ON session (parent_id);
            ",
        )
        .unwrap();

        conn
    }

    /// Create an in-memory DB matching the **current** OpenCode SQLite schema
    /// (after all migrations through `20260323234822_events`).
    ///
    /// This includes the `commands` column on `project`, the `workspace_id`
    /// column on `session`, the `workspace` table, and the updated composite
    /// indexes on `message` and `part`.
    fn create_test_db_v2() -> Connection {
        let conn = Connection::open_in_memory().unwrap();

        conn.execute_batch(
            "
            CREATE TABLE project (
                id TEXT PRIMARY KEY,
                worktree TEXT NOT NULL,
                vcs TEXT,
                name TEXT,
                icon_url TEXT,
                icon_color TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                time_initialized INTEGER,
                sandboxes TEXT NOT NULL DEFAULT '[]',
                commands TEXT
            );

            CREATE TABLE workspace (
                id TEXT PRIMARY KEY,
                branch TEXT,
                project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
                type TEXT NOT NULL,
                name TEXT,
                directory TEXT,
                extra TEXT
            );

            CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
                workspace_id TEXT,
                parent_id TEXT,
                slug TEXT NOT NULL DEFAULT '',
                directory TEXT NOT NULL,
                title TEXT NOT NULL,
                version TEXT NOT NULL DEFAULT '',
                share_url TEXT,
                summary_additions INTEGER,
                summary_deletions INTEGER,
                summary_files INTEGER,
                summary_diffs TEXT,
                revert TEXT,
                permission TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                time_compacting INTEGER,
                time_archived INTEGER
            );

            CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );

            CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL REFERENCES message(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );

            CREATE INDEX message_session_time_created_id_idx
                ON message (session_id, time_created, id);
            CREATE INDEX part_message_id_id_idx ON part (message_id, id);
            CREATE INDEX part_session_idx ON part (session_id);
            CREATE INDEX session_project_idx ON session (project_id);
            CREATE INDEX session_workspace_idx ON session (workspace_id);
            CREATE INDEX session_parent_idx ON session (parent_id);
            ",
        )
        .unwrap();

        conn
    }

    #[test]
    fn test_load_projects_from_db() {
        let conn = create_test_db();
        conn.execute(
            "INSERT INTO project (id, worktree, vcs, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["proj_1", "/home/user/myproject", "git", 1700000000000i64, 1700000000000i64],
        )
        .unwrap();

        let projects = load_projects_from_db(&conn);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects["proj_1"].worktree, "/home/user/myproject");
    }

    #[test]
    fn test_load_sessions_from_db() {
        let conn = create_test_db();
        conn.execute(
            "INSERT INTO project (id, worktree, time_created, time_updated) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["proj_1", "/home/user/proj", 1700000000000i64, 1700000000000i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, title, directory, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params!["ses_1", "proj_1", "My Session", "/home/user/proj", 1700000000000i64, 1700000000000i64],
        )
        .unwrap();

        let sessions = load_sessions_from_db(&conn);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions["ses_1"].title.as_deref(), Some("My Session"));
        assert_eq!(sessions["ses_1"].project_id, "proj_1");
    }

    #[test]
    fn test_batch_load_tool_stats_from_db() {
        let conn = create_test_db();

        // Insert project + session + message first (for FK constraints).
        conn.execute(
            "INSERT INTO project (id, worktree, time_created, time_updated) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["proj_1", "/tmp", 0i64, 0i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, title, directory, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params!["ses_1", "proj_1", "s", "/tmp", 0i64, 0i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["msg_1", "ses_1", 0i64, 0i64, r#"{"role":"assistant"}"#],
        )
        .unwrap();

        // Insert tool parts.
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_1", "msg_1", "ses_1", 0i64, 0i64,
                r#"{"type":"tool","tool":"read","callID":"c1","state":{"status":"completed","input":{},"output":"contents","title":"Read file","metadata":{},"time":{"start":0,"end":1}}}"#
            ],
        ).unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_2", "msg_1", "ses_1", 0i64, 0i64,
                r#"{"type":"tool","tool":"glob","callID":"c2","state":{"status":"completed","input":{},"output":"files","title":"Glob","metadata":{"count":5},"time":{"start":0,"end":1}}}"#
            ],
        ).unwrap();
        // Non-tool part (should be ignored).
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_3", "msg_1", "ses_1", 0i64, 0i64,
                r#"{"type":"text","text":"Hello world"}"#
            ],
        ).unwrap();

        let stats = batch_load_tool_stats_from_db(&conn);
        let msg_stats = &stats["msg_1"];
        assert_eq!(msg_stats.tool_calls, 2);
        assert_eq!(msg_stats.files_read, 6); // 1 from read + 5 from glob count
        assert_eq!(msg_stats.file_searches, 1);
    }

    #[test]
    fn test_batch_load_tool_stats_empty_db() {
        let conn = create_test_db();
        let stats = batch_load_tool_stats_from_db(&conn);
        assert!(stats.is_empty());
    }

    #[test]
    fn test_sqlite_end_to_end_in_memory() {
        // Build a full in-memory DB and verify message conversion.
        let conn = create_test_db();

        conn.execute(
            "INSERT INTO project (id, worktree, vcs, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["proj_abc", "/home/dev/myrepo", "git", 1700000000000i64, 1700000000000i64],
        ).unwrap();

        conn.execute(
            "INSERT INTO session (id, project_id, title, directory, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params!["ses_xyz", "proj_abc", "Implement feature", "/home/dev/myrepo", 1700000000000i64, 1700001000000i64],
        ).unwrap();

        // User message (data blob has no id/sessionID).
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "msg_user1", "ses_xyz", 1700000100000i64, 1700000100000i64,
                r#"{"role":"user","time":{"created":1700000100000},"agent":"coder","model":{"providerID":"anthropic","modelID":"claude-sonnet-4-20250514"}}"#
            ],
        ).unwrap();

        // Assistant message with tokens and cost.
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "msg_asst1", "ses_xyz", 1700000200000i64, 1700000210000i64,
                r#"{"role":"assistant","time":{"created":1700000200000,"completed":1700000210000},"modelID":"claude-sonnet-4-20250514","providerID":"anthropic","cost":0.0042,"tokens":{"total":2500,"input":1500,"output":800,"reasoning":200,"cache":{"read":1000,"write":500}},"finish":"stop"}"#
            ],
        ).unwrap();

        // A tool part for the assistant message.
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_tool1", "msg_asst1", "ses_xyz", 1700000201000i64, 1700000202000i64,
                r#"{"type":"tool","tool":"read","callID":"call_1","state":{"status":"completed","input":{"path":"/tmp/foo.rs"},"output":"fn main(){}","title":"Read /tmp/foo.rs","metadata":{},"time":{"start":1700000201000,"end":1700000202000}}}"#
            ],
        ).unwrap();

        // Query and convert using our helpers.
        let db_projects = load_projects_from_db(&conn);
        let db_sessions = load_sessions_from_db(&conn);
        let tool_stats_map = batch_load_tool_stats_from_db(&conn);

        assert_eq!(db_projects.len(), 1);
        assert_eq!(db_sessions.len(), 1);
        assert_eq!(tool_stats_map.len(), 1);

        // Parse messages.
        let mut stmt = conn
            .prepare("SELECT id, session_id, time_created, data FROM message ORDER BY time_created")
            .unwrap();
        let messages: Vec<ConversationMessage> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0).unwrap(),
                    row.get::<_, String>(1).unwrap(),
                    row.get::<_, i64>(2).unwrap(),
                    row.get::<_, String>(3).unwrap(),
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .filter_map(|(id, session_id, time_created, data)| {
                let mut bytes = data.into_bytes();
                let mut msg = simd_json::from_slice::<OpenCodeMessage>(&mut bytes).ok()?;
                msg.id = id.clone();
                msg.session_id = session_id.clone();
                if msg.time.created.is_none() || msg.time.created == Some(0) {
                    msg.time.created = Some(time_created);
                }

                let session = db_sessions.get(&session_id);
                let project = session.and_then(|s| db_projects.get(&s.project_id));
                let tool_stats = tool_stats_map.get(&id).cloned().unwrap_or_default();
                let stats = compute_message_stats(&msg, tool_stats);
                let session_title = session.and_then(|s| s.title.clone());
                let worktree = project.map(|p| p.worktree.as_str());
                let fallback = Some(session_id.as_str());

                Some(build_conversation_message(
                    msg,
                    session_title,
                    worktree,
                    fallback,
                    stats,
                    Application::OpenCode,
                    "opencode",
                ))
            })
            .collect();

        assert_eq!(messages.len(), 2);

        // Verify user message.
        let user_msg = &messages[0];
        assert_eq!(user_msg.role, MessageRole::User);
        assert_eq!(user_msg.application, Application::OpenCode);
        assert_eq!(user_msg.session_name.as_deref(), Some("Implement feature"));
        assert_eq!(user_msg.project_hash, hash_text("/home/dev/myrepo"));
        assert_eq!(user_msg.stats.input_tokens, 0);

        // Verify assistant message.
        let asst_msg = &messages[1];
        assert_eq!(asst_msg.role, MessageRole::Assistant);
        assert_eq!(asst_msg.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(asst_msg.stats.input_tokens, 1500);
        assert_eq!(asst_msg.stats.output_tokens, 800);
        assert_eq!(asst_msg.stats.reasoning_tokens, 200);
        assert_eq!(asst_msg.stats.cache_read_tokens, 1000);
        assert_eq!(asst_msg.stats.cache_creation_tokens, 500);
        assert!((asst_msg.stats.cost - 0.0042).abs() < f64::EPSILON);
        // 1 tool call from the "read" part.
        assert_eq!(asst_msg.stats.tool_calls, 1);
        assert_eq!(asst_msg.stats.files_read, 1);

        // Verify global hash matches the expected format.
        assert_eq!(
            asst_msg.global_hash,
            hash_text("opencode_ses_xyz_msg_asst1")
        );
    }

    // ---- New SQLite schema (v2) tests ----
    //
    // These tests exercise the updated schema with workspace_id,
    // channel-specific databases, step-finish token aggregation,
    // and the "global" project_id fallback.

    #[test]
    fn test_v2_schema_load_projects() {
        let conn = create_test_db_v2();
        conn.execute(
            "INSERT INTO project (id, worktree, vcs, name, icon_color, sandboxes, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                "a1b2c3d4e5",
                "/code/myproject",
                "git",
                "My Project",
                "blue",
                "[]",
                1770000000000i64,
                1770000000000i64,
            ],
        )
        .unwrap();

        let projects = load_projects_from_db(&conn);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects["a1b2c3d4e5"].worktree, "/code/myproject");
    }

    #[test]
    fn test_v2_schema_load_sessions_with_workspace() {
        let conn = create_test_db_v2();

        conn.execute(
            "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["proj_hash", "/code/proj", "[]", 0i64, 0i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspace (id, project_id, type, name) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["ws_1", "proj_hash", "branch", "feat-x"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, workspace_id, slug, directory, title, version, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "ses_ws",
                "proj_hash",
                "ws_1",
                "cool-hawk",
                "/code/proj",
                "Feature work",
                "1.2.20",
                1770000000000i64,
                1770001000000i64,
            ],
        )
        .unwrap();

        let sessions = load_sessions_from_db(&conn);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions["ses_ws"].title.as_deref(), Some("Feature work"));
        assert_eq!(sessions["ses_ws"].project_id, "proj_hash");
    }

    #[test]
    fn test_v2_schema_global_project_id() {
        // Sessions with project_id = "global" should still parse correctly;
        // the project lookup returns None and we fall back to session_id
        // for the project hash.
        let conn = create_test_db_v2();

        // Insert a project that isn't "global" so FK isn't violated.
        conn.execute(
            "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["global", "/", "[]", 0i64, 0i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                "ses_global",
                "global",
                "eager-fox",
                "/code",
                "Greeting",
                "1.2.16",
                1770000000000i64,
                1770000000000i64,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "msg_g1",
                "ses_global",
                1770000100000i64,
                1770000100000i64,
                r#"{"role":"user","time":{"created":1770000100000},"agent":"build","model":{"providerID":"anthropic","modelID":"claude-opus-4-5"}}"#
            ],
        )
        .unwrap();

        let db_projects = load_projects_from_db(&conn);
        let db_sessions = load_sessions_from_db(&conn);

        let session = db_sessions.get("ses_global").unwrap();
        let project = db_projects.get(&session.project_id);
        // The "global" project's worktree is "/" which is a valid but
        // non-meaningful path — we still use it as the project hash source.
        assert_eq!(project.unwrap().worktree, "/");
    }

    #[test]
    fn test_batch_load_step_finish_from_db() {
        let conn = create_test_db_v2();

        // Scaffold project/session/message.
        conn.execute(
            "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["p1", "/tmp", "[]", 0i64, 0i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params!["s1", "p1", "slug", "/tmp", "t", "1.0", 0i64, 0i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["msg_sf", "s1", 0i64, 0i64, r#"{"role":"assistant"}"#],
        )
        .unwrap();

        // Two step-finish parts with token data.
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_sf1",
                "msg_sf",
                "s1",
                0i64,
                0i64,
                r#"{"type":"step-finish","reason":"tool-calls","cost":0.01,"tokens":{"total":5000,"input":1000,"output":500,"reasoning":100,"cache":{"read":3000,"write":400}}}"#
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_sf2",
                "msg_sf",
                "s1",
                0i64,
                0i64,
                r#"{"type":"step-finish","reason":"stop","cost":0.02,"tokens":{"total":3000,"input":800,"output":300,"reasoning":50,"cache":{"read":1500,"write":350}}}"#
            ],
        )
        .unwrap();
        // Non-step-finish part (should be ignored).
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_text",
                "msg_sf",
                "s1",
                0i64,
                0i64,
                r#"{"type":"text","text":"hello"}"#
            ],
        )
        .unwrap();

        let agg = batch_load_step_finish_from_db(&conn);
        let msg_agg = &agg["msg_sf"];
        assert_eq!(msg_agg.input, 1800); // 1000 + 800
        assert_eq!(msg_agg.output, 800); // 500 + 300
        assert_eq!(msg_agg.reasoning, 150); // 100 + 50
        assert_eq!(msg_agg.cache_read, 4500); // 3000 + 1500
        assert_eq!(msg_agg.cache_write, 750); // 400 + 350
        assert!((msg_agg.cost - 0.03).abs() < f64::EPSILON); // 0.01 + 0.02
    }

    #[test]
    fn test_step_finish_fallback_when_message_has_zero_tokens() {
        let conn = create_test_db_v2();

        conn.execute(
            "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["p1", "/code/test", "[]", 0i64, 0i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params!["s1", "p1", "slug", "/code/test", "t", "1.2.20", 0i64, 0i64],
        )
        .unwrap();

        // Assistant message with zero tokens at message level.
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "msg_z",
                "s1",
                1770000100000i64,
                1770000110000i64,
                r#"{"role":"assistant","time":{"created":1770000100000,"completed":1770000110000},"modelID":"claude-opus-4-6","providerID":"anthropic","cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"finish":"stop"}"#
            ],
        )
        .unwrap();

        // Step-finish part with actual token data.
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_sf",
                "msg_z",
                "s1",
                0i64,
                0i64,
                r#"{"type":"step-finish","reason":"stop","cost":0.05,"tokens":{"total":10000,"input":5000,"output":3000,"reasoning":500,"cache":{"read":1000,"write":500}}}"#
            ],
        )
        .unwrap();

        // Query and convert using our helpers — simulating parse_sqlite_messages logic.
        let db_projects = load_projects_from_db(&conn);
        let db_sessions = load_sessions_from_db(&conn);
        let tool_stats_map = batch_load_tool_stats_from_db(&conn);
        let step_finish_map = batch_load_step_finish_from_db(&conn);

        let mut stmt = conn
            .prepare("SELECT id, session_id, time_created, data FROM message")
            .unwrap();

        let messages: Vec<ConversationMessage> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0).unwrap(),
                    row.get::<_, String>(1).unwrap(),
                    row.get::<_, i64>(2).unwrap(),
                    row.get::<_, String>(3).unwrap(),
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .filter_map(|(id, session_id, _time_created, data)| {
                let mut bytes = data.into_bytes();
                let mut msg = simd_json::from_slice::<OpenCodeMessage>(&mut bytes).ok()?;
                msg.id = id.clone();
                msg.session_id = session_id.clone();

                // Apply step-finish fallback (same logic as parse_sqlite_messages).
                if msg.role == "assistant" {
                    let msg_has_tokens = msg.tokens.as_ref().is_some_and(|t| {
                        t.input > 0
                            || t.output > 0
                            || t.reasoning > 0
                            || t.cache.read > 0
                            || t.cache.write > 0
                    });
                    if !msg_has_tokens
                        && let Some(agg) = step_finish_map.get(&id)
                        && (agg.input > 0
                            || agg.output > 0
                            || agg.reasoning > 0
                            || agg.cache_read > 0
                            || agg.cache_write > 0)
                    {
                        msg.tokens = Some(OpenCodeTokens {
                            input: agg.input,
                            output: agg.output,
                            reasoning: agg.reasoning,
                            cache: OpenCodeCacheTokens {
                                read: agg.cache_read,
                                write: agg.cache_write,
                            },
                        });
                        if agg.cost > 0.0 && msg.cost.is_none_or(|c| c == 0.0) {
                            msg.cost = Some(agg.cost);
                        }
                    }
                }

                let session = db_sessions.get(&session_id);
                let project = session.and_then(|s| db_projects.get(&s.project_id));
                let tool_stats = tool_stats_map.get(&id).cloned().unwrap_or_default();
                let stats = compute_message_stats(&msg, tool_stats);
                let session_title = session.and_then(|s| s.title.clone());
                let worktree = project.map(|p| p.worktree.as_str());
                let fallback = Some(session_id.as_str());

                Some(build_conversation_message(
                    msg,
                    session_title,
                    worktree,
                    fallback,
                    stats,
                    Application::OpenCode,
                    "opencode",
                ))
            })
            .collect();

        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        // Tokens should come from step-finish fallback.
        assert_eq!(msg.stats.input_tokens, 5000);
        assert_eq!(msg.stats.output_tokens, 3000);
        assert_eq!(msg.stats.reasoning_tokens, 500);
        assert_eq!(msg.stats.cache_read_tokens, 1000);
        assert_eq!(msg.stats.cache_creation_tokens, 500);
        assert!((msg.stats.cost - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn test_v2_schema_end_to_end() {
        // Full end-to-end test with the v2 schema including workspace,
        // hash-based project IDs, and new-format data blobs.
        let conn = create_test_db_v2();

        // SHA-hash project IDs (as in real OpenCode data).
        conn.execute(
            "INSERT INTO project (id, worktree, vcs, name, icon_color, sandboxes, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                "0b4651dc870efaaf627a2dadd5613224e4343b32",
                "/code/tweakcc",
                "git",
                "tweakcc",
                "purple",
                "[]",
                1764038326711i64,
                1773863312475i64,
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                "ses_real1",
                "0b4651dc870efaaf627a2dadd5613224e4343b32",
                "clever-hawk",
                "/code/tweakcc",
                "Exploring tweakcc codebase",
                "1.2.16",
                1766190800000i64,
                1766191000000i64,
            ],
        )
        .unwrap();

        // User message with the newer format (has model object, tools, variant).
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "msg_u1",
                "ses_real1",
                1766190934946i64,
                1766190934946i64,
                r#"{"role":"user","time":{"created":1766190934946},"summary":{"title":"Exploring tweakcc codebase","diffs":[]},"agent":"explore","model":{"providerID":"anthropic","modelID":"claude-opus-4-5"},"tools":{"todowrite":false,"todoread":false}}"#
            ],
        )
        .unwrap();

        // Assistant message with all new fields (parentID, mode, path, etc.).
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "msg_a1",
                "ses_real1",
                1766190946687i64,
                1766190953244i64,
                r#"{"role":"assistant","time":{"created":1766190946687,"completed":1766190953244},"parentID":"msg_u1","modelID":"claude-opus-4-5","providerID":"anthropic","mode":"explore","agent":"explore","path":{"cwd":"/code/tweakcc","root":"/code/tweakcc"},"cost":0.042,"tokens":{"total":23335,"input":0,"output":232,"reasoning":0,"cache":{"read":23103,"write":27398}},"finish":"tool-calls"}"#
            ],
        )
        .unwrap();

        // Tool part.
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_t1",
                "msg_a1",
                "ses_real1",
                1766190947000i64,
                1766190948000i64,
                r#"{"type":"tool","tool":"read","callID":"toolu_01","state":{"status":"completed","input":{"filePath":"/code/tweakcc/package.json"},"output":"...","title":"Read file","metadata":{},"time":{"start":1766190947000,"end":1766190948000}}}"#
            ],
        )
        .unwrap();

        // Step-finish part (tokens duplicate message-level, which is normal).
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "prt_sf1",
                "msg_a1",
                "ses_real1",
                1766190953000i64,
                1766190953244i64,
                r#"{"type":"step-finish","reason":"tool-calls","cost":0.042,"tokens":{"total":23335,"input":0,"output":232,"reasoning":0,"cache":{"read":23103,"write":27398}}}"#
            ],
        )
        .unwrap();

        // Query using our helpers.
        let db_projects = load_projects_from_db(&conn);
        let db_sessions = load_sessions_from_db(&conn);
        let tool_stats_map = batch_load_tool_stats_from_db(&conn);

        assert_eq!(db_projects.len(), 1);
        assert_eq!(
            db_projects["0b4651dc870efaaf627a2dadd5613224e4343b32"].worktree,
            "/code/tweakcc"
        );
        assert_eq!(db_sessions.len(), 1);
        assert_eq!(
            db_sessions["ses_real1"].title.as_deref(),
            Some("Exploring tweakcc codebase")
        );
        assert_eq!(tool_stats_map["msg_a1"].tool_calls, 1);
        assert_eq!(tool_stats_map["msg_a1"].files_read, 1);
    }

    #[test]
    fn test_parse_message_with_tools_field() {
        // User messages in the newer format include a `tools` object.
        let json = r#"{
            "role": "user",
            "time": { "created": 1770000000000 },
            "agent": "build",
            "model": { "providerID": "anthropic", "modelID": "claude-opus-4-5" },
            "tools": { "todowrite": false, "todoread": false, "task": false, "edit": false, "write": false }
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: OpenCodeMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.model_name().unwrap(), "claude-opus-4-5");
    }

    #[test]
    fn test_parse_message_with_total_tokens_field() {
        // Newer messages include a `total` field in the tokens object.
        let json = r#"{
            "role": "assistant",
            "time": { "created": 1770000000000, "completed": 1770000010000 },
            "modelID": "minimax-m2.5-free",
            "providerID": "opencode",
            "cost": 0,
            "tokens": { "total": 112793, "input": 849, "output": 1896, "reasoning": 0, "cache": { "write": 0, "read": 110048 } },
            "finish": "stop"
        }"#;
        let mut bytes = json.as_bytes().to_vec();
        let msg: OpenCodeMessage = simd_json::from_slice(&mut bytes).expect("should parse");
        assert_eq!(msg.role, "assistant");
        let tokens = msg.tokens.as_ref().unwrap();
        assert_eq!(tokens.input, 849);
        assert_eq!(tokens.output, 1896);
        assert_eq!(tokens.cache.read, 110048);
    }

    #[test]
    fn test_is_valid_data_path_channel_db() {
        let analyzer = OpenCodeAnalyzer::new();

        // Channel-specific DB files should be accepted.
        let tmp = std::env::temp_dir().join("opencode-canary.db");
        std::fs::write(&tmp, "fake").unwrap();
        assert!(analyzer.is_valid_data_path(&tmp));
        std::fs::remove_file(&tmp).unwrap();

        let tmp2 = std::env::temp_dir().join("opencode.db");
        std::fs::write(&tmp2, "fake").unwrap();
        assert!(analyzer.is_valid_data_path(&tmp2));
        std::fs::remove_file(&tmp2).unwrap();

        // Reject non-matching patterns.
        let tmp3 = std::env::temp_dir().join("opencode.db-wal");
        std::fs::write(&tmp3, "fake").unwrap();
        assert!(!analyzer.is_valid_data_path(&tmp3));
        std::fs::remove_file(&tmp3).unwrap();
    }
}
