//! Piebald analyzer - reads usage data from Piebald's SQLite database.
//!
//! <https://piebald.ai>

use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::models::{
    InputTokenSemantics, ServiceTier, calculate_total_cost_for_service_tier_at, get_model_info,
};
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct PiebaldAnalyzer;

impl PiebaldAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

/// Get the path to Piebald's database file.
///
/// Cross-platform paths:
/// - Linux: $XDG_DATA_HOME/piebald/app.db or ~/.local/share/piebald/app.db
/// - macOS: ~/Library/Application Support/piebald/app.db
/// - Windows: %APPDATA%\piebald\app.db
fn get_piebald_db_path() -> Option<PathBuf> {
    dirs::data_dir().map(|data_dir| data_dir.join("piebald").join("app.db"))
}

/// Open Piebald's database in read-only mode.
fn open_piebald_db(path: &PathBuf) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;

    let conn = Connection::open_with_flags(path, flags)?;

    // Set busy timeout to handle locked database (Piebald might be running)
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    Ok(conn)
}

/// Represents a chat from Piebald's database.
struct PiebaldChat {
    id: i64,
    title: Option<String>,
    model: Option<String>,
    project_directory: Option<String>,
}

/// Represents a message from Piebald's database.
struct PiebaldMessage {
    id: i64,
    parent_chat_id: i64,
    role: String,
    model: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    service_tier: Option<String>,
    created_at: String,
    updated_at: String,
}

/// Query all chats from the database.
fn query_chats(conn: &Connection) -> Result<Vec<PiebaldChat>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.title, c.model, p.directory
         FROM chats c
         LEFT JOIN projects p ON p.id = c.project_id
         ORDER BY c.created_at",
    )?;

    let chats = stmt
        .query_map([], |row| {
            Ok(PiebaldChat {
                id: row.get(0)?,
                title: row.get(1)?,
                model: row.get(2)?,
                project_directory: row.get(3)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(chats)
}

/// Query all messages from the database.
fn query_messages(conn: &Connection) -> Result<Vec<PiebaldMessage>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.parent_chat_id, m.role, m.model, m.input_tokens, m.output_tokens,
                m.reasoning_tokens, m.cache_read_tokens, m.cache_write_tokens,
                COALESCE(responses.service_tier, completions.service_tier) AS service_tier,
                m.created_at, m.updated_at
         FROM messages m
         LEFT JOIN override_gen_cfg_data_openai_responses responses
                ON responses.gen_cfg_id = m.config_id
         LEFT JOIN override_gen_cfg_data_openai_completions completions
                ON completions.gen_cfg_id = m.config_id
         ORDER BY m.updated_at",
    )?;

    let messages = stmt
        .query_map([], |row| {
            Ok(PiebaldMessage {
                id: row.get(0)?,
                parent_chat_id: row.get(1)?,
                role: row.get(2)?,
                model: row.get(3)?,
                input_tokens: row.get(4)?,
                output_tokens: row.get(5)?,
                reasoning_tokens: row.get(6)?,
                cache_read_tokens: row.get(7)?,
                cache_write_tokens: row.get(8)?,
                service_tier: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(messages)
}

/// Query tool call counts per message.
///
/// Joins `message_parts` → `message_part_tool_call` to count how many tool calls
/// each message made. Returns a map from message ID to tool call count.
fn query_tool_call_counts(conn: &Connection) -> Result<HashMap<i64, u32>> {
    let mut stmt = conn.prepare(
        "SELECT mp.parent_chat_message_id, COUNT(*) as tool_call_count
         FROM message_parts mp
         JOIN message_part_tool_call tc ON tc.message_part_id = mp.id
         GROUP BY mp.parent_chat_message_id",
    )?;

    let counts = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, u32>(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(counts)
}

/// Parse a timestamp string from Piebald's database.
///
/// Piebald stores timestamps in RFC3339 format with timezone (e.g., "2025-12-10T15:55:48.819321712+00:00").
/// Returns None if the timestamp cannot be parsed.
fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
    // Piebald uses RFC3339 format exclusively
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_service_tier(service_tier: Option<&str>) -> ServiceTier {
    match service_tier
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("priority") => ServiceTier::Priority,
        Some("flex") => ServiceTier::Flex,
        Some("batch") => ServiceTier::Batch,
        _ => ServiceTier::Standard,
    }
}

fn normalize_input_tokens(model: Option<&str>, input_tokens: u64, cache_read_tokens: u64) -> u64 {
    if model
        .and_then(get_model_info)
        .is_some_and(|info| info.input_token_semantics == InputTokenSemantics::IncludesCacheRead)
    {
        input_tokens.saturating_sub(cache_read_tokens)
    } else {
        input_tokens
    }
}

/// Convert Piebald messages to splitrail's ConversationMessage format.
fn convert_messages(
    chats: &[PiebaldChat],
    messages: Vec<PiebaldMessage>,
    tool_call_counts: &HashMap<i64, u32>,
) -> Vec<ConversationMessage> {
    // Build chat lookup map for O(1) access
    let chat_map: HashMap<i64, &PiebaldChat> = chats.iter().map(|c| (c.id, c)).collect();

    messages
        .into_iter()
        .filter_map(|msg| {
            let chat = chat_map.get(&msg.parent_chat_id)?;

            // Parse timestamp - use updated_at so that streaming updates are captured
            // (updated_at changes when tokens are added during streaming)
            let date = parse_timestamp(&msg.updated_at)?;

            // Use project path from Piebald's projects table, falling back to "ungrouped" if not set.
            let project_hash = hash_text(chat.project_directory.as_deref().unwrap_or("ungrouped"));

            // Generate globally unique hash using created_at timestamp + message ID.
            // Use created_at (not updated_at) so the hash stays stable across token updates.
            // The timestamp has nanosecond precision which is unique per installation,
            // and combined with the message ID ensures no collisions across users.
            // NOTE: We cannot use just msg.id because it's a local SQLite autoincrement
            // that starts at 1 for every Piebald installation, causing collisions.
            let conversation_hash = msg.parent_chat_id.to_string();
            let global_hash = hash_text(&format!("piebald_{}_{}", msg.created_at, msg.id));

            // Determine role
            let role = match msg.role.to_lowercase().as_str() {
                "user" => MessageRole::User,
                _ => MessageRole::Assistant,
            };

            // Use per-message model (the model that actually generated this response),
            // falling back to chat-level model for older messages that may lack it.
            // Only set for assistant messages.
            let model_str = if role == MessageRole::Assistant {
                msg.model.clone().or_else(|| chat.model.clone())
            } else {
                None
            };

            // Map token stats
            let raw_input_tokens = msg.input_tokens.unwrap_or(0) as u64;
            let output_tokens = msg.output_tokens.unwrap_or(0) as u64;
            let reasoning_tokens = msg.reasoning_tokens.unwrap_or(0) as u64;
            let cache_read_tokens = msg.cache_read_tokens.unwrap_or(0) as u64;
            let cache_creation_tokens = msg.cache_write_tokens.unwrap_or(0) as u64;
            let input_tokens =
                normalize_input_tokens(model_str.as_deref(), raw_input_tokens, cache_read_tokens);

            let service_tier = parse_service_tier(msg.service_tier.as_deref());

            // Calculate cost using splitrail's model pricing
            let cost = if let Some(ref model) = model_str {
                calculate_total_cost_for_service_tier_at(
                    model,
                    service_tier,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                    Some(date),
                )
            } else {
                0.0
            };

            // Look up tool call count for this message
            let tool_calls = tool_call_counts.get(&msg.id).copied().unwrap_or(0);

            let stats = Stats {
                input_tokens,
                output_tokens,
                reasoning_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                cached_tokens: cache_read_tokens + cache_creation_tokens,
                cost,
                tool_calls,
                ..Default::default()
            };

            Some(ConversationMessage {
                application: Application::Piebald,
                date,
                project_hash,
                conversation_hash,
                local_hash: Some(msg.id.to_string()),
                global_hash,
                model: model_str,
                stats,
                role,
                uuid: Some(msg.id.to_string()),
                session_name: chat.title.clone(),
            })
        })
        .collect()
}

#[async_trait]
impl Analyzer for PiebaldAnalyzer {
    fn display_name(&self) -> &'static str {
        "Piebald"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(path) = get_piebald_db_path() {
            patterns.push(path.to_string_lossy().to_string());
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        if let Some(path) = get_piebald_db_path()
            && path.exists()
        {
            return Ok(vec![DataSource { path }]);
        }
        Ok(Vec::new())
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        let conn = open_piebald_db(&source.path)?;
        let chats = query_chats(&conn)?;
        let messages = query_messages(&conn)?;
        let tool_call_counts = query_tool_call_counts(&conn)?;
        Ok(convert_messages(&chats, messages, &tool_call_counts))
    }

    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        let all_messages: Vec<ConversationMessage> = sources
            .par_iter()
            .flat_map(|source| self.parse_source(source).unwrap_or_default())
            .collect();
        crate::utils::deduplicate_by_local_hash(all_messages)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        dirs::data_dir()
            .map(|data_dir| data_dir.join("piebald"))
            .filter(|d| d.is_dir())
            .into_iter()
            .collect()
    }

    fn is_valid_data_path(&self, path: &std::path::Path) -> bool {
        // Must be the app.db file
        path.is_file() && path.file_name().is_some_and(|n| n == "app.db")
    }

    // Piebald uses SQLite database containing all sessions
    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::MultiSession
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_name() {
        let analyzer = PiebaldAnalyzer::new();
        assert_eq!(analyzer.display_name(), "Piebald");
    }

    #[test]
    fn test_discover_data_sources_no_panic() {
        let analyzer = PiebaldAnalyzer::new();
        let result = analyzer.discover_data_sources();
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_stats_empty_sources() {
        let analyzer = PiebaldAnalyzer::new();
        let result = analyzer.get_stats_with_sources(Vec::new());
        assert!(result.is_ok());
        assert!(result.unwrap().messages.is_empty());
    }

    #[test]
    fn test_parse_service_tier_maps_known_values() {
        assert_eq!(parse_service_tier(Some("priority")), ServiceTier::Priority);
        assert_eq!(parse_service_tier(Some(" flex ")), ServiceTier::Flex);
        assert_eq!(parse_service_tier(Some("BATCH")), ServiceTier::Batch);
    }

    #[test]
    fn test_parse_service_tier_defaults_unknown_values_to_standard() {
        assert_eq!(parse_service_tier(None), ServiceTier::Standard);
        assert_eq!(parse_service_tier(Some("")), ServiceTier::Standard);
        assert_eq!(parse_service_tier(Some("scale")), ServiceTier::Standard);
    }

    #[test]
    fn test_convert_messages_uses_service_tier_pricing() {
        let chats = vec![PiebaldChat {
            id: 1,
            title: Some("Priority chat".to_string()),
            model: Some("gpt-5.4".to_string()),
            project_directory: Some("/tmp/project".to_string()),
        }];
        let messages = vec![PiebaldMessage {
            id: 10,
            parent_chat_id: 1,
            role: "assistant".to_string(),
            model: Some("gpt-5.4".to_string()),
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            reasoning_tokens: Some(0),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
            service_tier: Some("priority".to_string()),
            created_at: "2026-05-01T12:00:00Z".to_string(),
            updated_at: "2026-05-01T12:00:01Z".to_string(),
        }];
        let tool_call_counts = HashMap::new();

        let converted = convert_messages(&chats, messages, &tool_call_counts);

        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].stats.cost, 35.0);
    }

    #[test]
    fn test_normalize_input_tokens_subtracts_openai_cached_reads() {
        assert_eq!(normalize_input_tokens(Some("gpt-5"), 1_000, 300), 700);
    }

    #[test]
    fn test_normalize_input_tokens_subtracts_tiered_openai_cached_reads() {
        assert_eq!(normalize_input_tokens(Some("gpt-5.5"), 1_000, 300), 700);
    }

    #[test]
    fn test_normalize_input_tokens_preserves_anthropic_input() {
        assert_eq!(
            normalize_input_tokens(Some("claude-sonnet-4-20250514"), 700, 300),
            700
        );
    }

    #[test]
    fn test_normalize_input_tokens_saturates_for_openai() {
        assert_eq!(normalize_input_tokens(Some("gpt-5"), 100, 300), 0);
    }

    #[test]
    fn test_parse_timestamp_rfc3339() {
        let ts = "2025-12-10T14:30:00Z";
        let dt = parse_timestamp(ts).expect("should parse RFC3339 format");
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2025-12-10");
    }

    #[test]
    fn test_parse_timestamp_rfc3339_with_nanoseconds() {
        // This is Piebald's actual timestamp format
        let ts = "2025-12-10T15:55:48.819321712+00:00";
        let dt = parse_timestamp(ts).expect("should parse RFC3339 with nanoseconds");
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2025-12-10");
    }

    #[test]
    fn test_parse_timestamp_rfc3339_with_offset() {
        let ts = "2025-12-10T08:30:00-07:00";
        let dt = parse_timestamp(ts).expect("should parse RFC3339 with timezone offset");
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2025-12-10");
    }

    #[test]
    fn test_parse_timestamp_rejects_non_rfc3339() {
        // SQLite format is not supported
        assert!(parse_timestamp("2025-12-10 14:30:00").is_none());
        // Milliseconds without timezone is not supported
        assert!(parse_timestamp("2025-12-10 14:30:00.123").is_none());
        // Invalid format
        assert!(parse_timestamp("invalid-timestamp").is_none());
    }
}
