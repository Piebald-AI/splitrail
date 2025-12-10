//! Piebald analyzer - reads usage data from Piebald's SQLite database.
//!
//! Piebald is a local-first AI chat interface that stores conversations in a SQLite database.
//! This is the first SQLite-based analyzer in splitrail.

use crate::analyzer::{Analyzer, DataSource};
use crate::models::calculate_total_cost;
use crate::types::{AgenticCodingToolStats, Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
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
}

/// Represents a message from Piebald's database.
struct PiebaldMessage {
    id: i64,
    parent_chat_id: i64,
    role: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    created_at: String,
}

/// Query all chats from the database.
fn query_chats(conn: &Connection) -> Result<Vec<PiebaldChat>> {
    let mut stmt = conn.prepare("SELECT id, title, model FROM chats ORDER BY created_at")?;

    let chats = stmt
        .query_map([], |row| {
            Ok(PiebaldChat {
                id: row.get(0)?,
                title: row.get(1)?,
                model: row.get(2)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(chats)
}

/// Query all messages from the database.
fn query_messages(conn: &Connection) -> Result<Vec<PiebaldMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, parent_chat_id, role, input_tokens, output_tokens,
                reasoning_tokens, cache_read_tokens, cache_write_tokens, created_at
         FROM messages
         ORDER BY created_at",
    )?;

    let messages = stmt
        .query_map([], |row| {
            Ok(PiebaldMessage {
                id: row.get(0)?,
                parent_chat_id: row.get(1)?,
                role: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                reasoning_tokens: row.get(5)?,
                cache_read_tokens: row.get(6)?,
                cache_write_tokens: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(messages)
}

/// Parse a timestamp string from Piebald's database.
///
/// Piebald stores timestamps in SQLite's datetime format: "YYYY-MM-DD HH:MM:SS"
fn parse_timestamp(ts: &str) -> DateTime<Utc> {
    // Try RFC3339 first (with timezone)
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return dt.with_timezone(&Utc);
    }

    // Try SQLite's default datetime format
    if let Ok(naive) = NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S") {
        return naive.and_utc();
    }

    // Try with milliseconds
    if let Ok(naive) = NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S%.f") {
        return naive.and_utc();
    }

    // Fallback to now
    Utc::now()
}

/// Convert Piebald messages to splitrail's ConversationMessage format.
fn convert_messages(
    chats: &[PiebaldChat],
    messages: Vec<PiebaldMessage>,
) -> Vec<ConversationMessage> {
    // Build chat lookup map for O(1) access
    let chat_map: HashMap<i64, &PiebaldChat> = chats.iter().map(|c| (c.id, c)).collect();

    // Piebald is global (no project concept) - use constant hash
    let project_hash = hash_text("piebald");

    messages
        .into_iter()
        .filter_map(|msg| {
            let chat = chat_map.get(&msg.parent_chat_id)?;

            let date = parse_timestamp(&msg.created_at);

            // Generate hashes for deduplication
            let conversation_hash = hash_text(&msg.parent_chat_id.to_string());
            let local_hash = hash_text(&msg.id.to_string());
            let global_hash = hash_text(&format!("piebald_{}_{}", msg.parent_chat_id, msg.id));

            // Determine role
            let role = match msg.role.to_lowercase().as_str() {
                "user" => MessageRole::User,
                _ => MessageRole::Assistant,
            };

            // Model is only set for assistant messages
            let model_str = if role == MessageRole::Assistant {
                chat.model.clone()
            } else {
                None
            };

            // Map token stats
            let input_tokens = msg.input_tokens.unwrap_or(0) as u64;
            let output_tokens = msg.output_tokens.unwrap_or(0) as u64;
            let reasoning_tokens = msg.reasoning_tokens.unwrap_or(0) as u64;
            let cache_read_tokens = msg.cache_read_tokens.unwrap_or(0) as u64;
            let cache_creation_tokens = msg.cache_write_tokens.unwrap_or(0) as u64;

            // Calculate cost using splitrail's model pricing
            let cost = if let Some(ref model) = model_str {
                calculate_total_cost(
                    model,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                )
            } else {
                0.0
            };

            let stats = Stats {
                input_tokens,
                output_tokens,
                reasoning_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                cached_tokens: cache_read_tokens + cache_creation_tokens,
                cost,
                ..Default::default()
            };

            Some(ConversationMessage {
                application: Application::Piebald,
                date,
                project_hash: project_hash.clone(),
                conversation_hash,
                local_hash: Some(local_hash),
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
        if let Some(path) = get_piebald_db_path() {
            if path.exists() {
                return Ok(vec![DataSource { path }]);
            }
        }
        Ok(Vec::new())
    }

    async fn parse_conversations(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<Vec<ConversationMessage>> {
        let mut all_messages = Vec::new();

        for source in sources {
            match open_piebald_db(&source.path) {
                Ok(conn) => {
                    let chats = query_chats(&conn)?;
                    let messages = query_messages(&conn)?;
                    let converted = convert_messages(&chats, messages);
                    all_messages.extend(converted);
                }
                Err(e) => {
                    eprintln!("Failed to open Piebald database {:?}: {}", source.path, e);
                }
            }
        }

        // Deduplicate by local hash
        Ok(crate::utils::deduplicate_by_local_hash_parallel(all_messages))
    }

    async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
        let sources = self.discover_data_sources()?;
        let messages = self.parse_conversations(sources).await?;
        let mut daily_stats = crate::utils::aggregate_by_date(&messages);

        // Remove "unknown" dates
        daily_stats.retain(|date, _| date != "unknown");

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

    #[tokio::test]
    async fn test_parse_conversations_empty_sources() {
        let analyzer = PiebaldAnalyzer::new();
        let result = analyzer.parse_conversations(Vec::new()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_parse_timestamp_sqlite_format() {
        let ts = "2025-12-10 14:30:00";
        let dt = parse_timestamp(ts);
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2025-12-10");
    }

    #[test]
    fn test_parse_timestamp_rfc3339() {
        let ts = "2025-12-10T14:30:00Z";
        let dt = parse_timestamp(ts);
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2025-12-10");
    }

    #[test]
    fn test_parse_timestamp_with_millis() {
        let ts = "2025-12-10 14:30:00.123";
        let dt = parse_timestamp(ts);
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2025-12-10");
    }
}
