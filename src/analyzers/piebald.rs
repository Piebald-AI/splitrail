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
///
/// Piebald's primary keys were integers until its KSUID migration rewrote them
/// as TEXT. Both are read as strings so one code path serves every layout.
struct PiebaldChat {
    /// Native primary key, used only to join rows within this database.
    id: String,
    /// Identity exposed to Splitrail. See [`PiebaldLayout::stable_id_sql`].
    stable_id: String,
    title: Option<String>,
    model: Option<String>,
    project_directory: Option<String>,
}

/// Represents a message from Piebald's database.
struct PiebaldMessage {
    /// Native primary key, used only to join tool counts.
    id: String,
    /// Identity exposed to Splitrail. See [`PiebaldLayout::stable_id_sql`].
    stable_id: String,
    /// Native key of the owning chat.
    parent_chat_id: String,
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
fn query_chats(conn: &Connection, layout: PiebaldLayout) -> Result<Vec<PiebaldChat>> {
    let (stable_id, legacy_join) = layout.stable_id_sql("chats", "c");
    let sql = format!(
        "SELECT CAST(c.id AS TEXT), {stable_id}, c.title, c.model, p.directory
         FROM chats c
         LEFT JOIN projects p ON p.id = c.project_id
         {legacy_join}
         ORDER BY c.created_at"
    );
    let mut stmt = conn.prepare(&sql)?;

    // Row errors propagate instead of being filtered out. Every value read here
    // is either TEXT-cast or nullable, so a conversion failure means the schema
    // changed under us. Skipping such rows one at a time is how a whole schema
    // migration once turned all Piebald history into a silent zero.
    let chats = stmt
        .query_map([], |row| {
            Ok(PiebaldChat {
                id: row.get(0)?,
                stable_id: row.get(1)?,
                title: row.get(2)?,
                model: row.get(3)?,
                project_directory: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(chats)
}

/// Where tool calls and per-message usage live: before or after Piebald's
/// typed-message-parts migration.
#[derive(Clone, Copy)]
enum PiebaldSchema {
    Legacy,
    TypedParts,
}

/// Where a message's OpenAI service tier can be recovered from.
#[derive(Clone, Copy)]
enum TierSource {
    /// Each message or generation pins a `generation_configs` row, whose
    /// per-engine override tables record the tier used for that turn.
    GenerationConfig,
    /// Piebald's generation-config fold dropped those configs and the per-turn
    /// `config_id`. Settings now live on profiles. A chat with its own settings
    /// points at a hidden profile it owns, so the chat's profile carries its
    /// effective tier. `message_generations.profile_id` is deliberately not
    /// used: it records the user-visible source profile, not that hidden clone,
    /// and it would miss every chat-specific tier. Per-turn tier history no longer
    /// exists in the database, so a chat's turns are all priced at its current tier.
    /// That is deliberate, with two consequences: changing a profile's tier reprices
    /// earlier turns of every chat on it, including already-uploaded messages
    /// (same `global_hash`, new cost), and a chat that switched profiles is priced
    /// entirely at the one it uses now.
    ProfileSettings,
}

/// Every independent schema feature the analyzer must adapt to. Piebald
/// migrates these in separate steps, so they are detected separately rather
/// than inferred from one another or from an application version.
#[derive(Clone, Copy)]
struct PiebaldLayout {
    parts: PiebaldSchema,
    tier_source: TierSource,
    /// Whether `_ksuid_map` exists. Piebald's KSUID migration rewrites integer
    /// primary keys as TEXT KSUIDs and retains this old-to-new map afterwards.
    has_ksuid_map: bool,
}

impl PiebaldLayout {
    /// Detect finalized storage rather than an application version. Piebald commits
    /// the additive schema before backfilling tools, so the generation table can
    /// coexist with authoritative legacy calls and an empty execution-context table.
    /// Keep legacy reads until finalization drops the old tool table atomically with
    /// the old message columns. This also covers a failed or in-progress backfill.
    /// SQL failures still propagate rather than being mistaken for an older schema.
    ///
    /// The tier source keys on the *new* tables existing, not the old ones being
    /// absent, so a database too old to have either keeps its previous behavior.
    /// Both `profile_settings_openai_*` tables are created by the same migration
    /// transaction, so checking one implies the other.
    fn detect(conn: &Connection) -> Result<Self> {
        let (typed_parts, profile_settings, has_ksuid_map): (bool, bool, bool) = conn.query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM sqlite_master
                        WHERE type = 'table' AND name = 'message_generations')
                 AND NOT EXISTS(SELECT 1 FROM sqlite_master
                        WHERE type = 'table' AND name = 'message_part_tool_call'),
                 EXISTS(SELECT 1 FROM sqlite_master
                        WHERE type = 'table' AND name = 'profile_settings_openai_responses'),
                 EXISTS(SELECT 1 FROM sqlite_master
                        WHERE type = 'table' AND name = '_ksuid_map')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(Self {
            parts: if typed_parts {
                PiebaldSchema::TypedParts
            } else {
                PiebaldSchema::Legacy
            },
            tier_source: if profile_settings {
                TierSource::ProfileSettings
            } else {
                TierSource::GenerationConfig
            },
            has_ksuid_map,
        })
    }

    /// Return a SQL expression for the identity Splitrail exposes for `alias`,
    /// plus the join that expression needs (empty when none is needed).
    ///
    /// Rows created before Piebald's KSUID migration keep their old integer ID,
    /// recovered through `_ksuid_map`. That ID feeds `global_hash`, the key Splitrail
    /// Cloud deduplicates uploads by. With the KSUID instead, every historical
    /// message would get a new hash and be uploaded and counted a second time. It
    /// also keeps local hashes and session grouping continuous across the flip.
    /// Rows created after the flip have no mapping and use their KSUID. A KSUID
    /// never collides with an old ID, and `created_at` is hashed alongside anyway.
    fn stable_id_sql(self, table: &str, alias: &str) -> (String, String) {
        let native = format!("CAST({alias}.id AS TEXT)");
        if !self.has_ksuid_map {
            return (native, String::new());
        }
        // `table` and `alias` are fixed literals from this module, never input.
        let map = format!("{alias}_ksuid");
        (
            format!("COALESCE(CAST({map}.old_id AS TEXT), {native})"),
            format!(
                "LEFT JOIN _ksuid_map {map}
                        ON {map}.table_name = '{table}' AND {map}.new_id = {alias}.id"
            ),
        )
    }
}

/// Query messages using the matching layout, retaining user messages without
/// generation rows while excluding non-conversational context containers.
fn query_messages(conn: &Connection, layout: PiebaldLayout) -> Result<Vec<PiebaldMessage>> {
    // Generation metadata is one-to-one with assistant messages, not user
    // messages. Keep the outer join and the original message timestamps so
    // migration does not change deduplication identities or streaming dates.
    let (usage, usage_join, kind_filter) = match layout.parts {
        PiebaldSchema::TypedParts => (
            "g",
            "LEFT JOIN message_generations g ON g.message_id = m.id",
            "AND m.message_kind = 'normal'",
        ),
        PiebaldSchema::Legacy => ("m", "", ""),
    };
    // Responses settings take precedence over Completions, as they always have.
    let tier_joins = match layout.tier_source {
        TierSource::GenerationConfig => format!(
            "LEFT JOIN override_gen_cfg_data_openai_responses responses
                    ON responses.gen_cfg_id = {usage}.config_id
             LEFT JOIN override_gen_cfg_data_openai_completions completions
                    ON completions.gen_cfg_id = {usage}.config_id"
        ),
        TierSource::ProfileSettings => {
            "LEFT JOIN chats tier_chat ON tier_chat.id = m.parent_chat_id
             LEFT JOIN profile_settings_openai_responses responses
                    ON responses.profile_id = tier_chat.profile_id
             LEFT JOIN profile_settings_openai_completions completions
                    ON completions.profile_id = tier_chat.profile_id"
                .to_string()
        }
    };
    let (stable_id, legacy_join) = layout.stable_id_sql("messages", "m");
    // Unparented messages are chat or Launchpad drafts, not conversation turns.
    // They could never join a chat below, and excluding them here keeps their
    // NULL parent from failing the now-strict row conversion.
    let sql = format!(
        "SELECT CAST(m.id AS TEXT), {stable_id}, CAST(m.parent_chat_id AS TEXT), m.role,
                {usage}.model, {usage}.input_tokens, {usage}.output_tokens,
                {usage}.reasoning_tokens, {usage}.cache_read_tokens, {usage}.cache_write_tokens,
                COALESCE(responses.service_tier, completions.service_tier) AS service_tier,
                m.created_at, m.updated_at
         FROM messages m
         {usage_join}
         {tier_joins}
         {legacy_join}
         WHERE m.parent_chat_id IS NOT NULL {kind_filter}
         ORDER BY m.updated_at"
    );
    let mut stmt = conn.prepare(&sql)?;

    // Strict, as in `query_chats`. IDs are TEXT-cast and usage columns are nullable;
    // `role`, `created_at`, and `updated_at` have been `NOT NULL` since Piebald's
    // initial schema. A failed conversion is therefore a schema change to surface.
    let messages = stmt
        .query_map([], |row| {
            Ok(PiebaldMessage {
                id: row.get(0)?,
                stable_id: row.get(1)?,
                parent_chat_id: row.get(2)?,
                role: row.get(3)?,
                model: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                reasoning_tokens: row.get(7)?,
                cache_read_tokens: row.get(8)?,
                cache_write_tokens: row.get(9)?,
                service_tier: row.get(10)?,
                created_at: row.get(11)?,
                updated_at: row.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(messages)
}

/// Query tool call counts per message.
///
/// Returns counts by the owning message's native ID. Typed tools share one
/// execution-context row per part; counting that row avoids enumerating subtype
/// tables and includes MCP, legacy, invalid, and still-streaming calls without
/// double counting results.
fn query_tool_call_counts(
    conn: &Connection,
    schema: PiebaldSchema,
) -> Result<HashMap<String, u32>> {
    let sql = match schema {
        PiebaldSchema::Legacy => {
            "SELECT CAST(mp.parent_chat_message_id AS TEXT), COUNT(*) as tool_call_count
             FROM message_parts mp
             JOIN message_part_tool_call tc ON tc.message_part_id = mp.id
             GROUP BY mp.parent_chat_message_id"
        }
        PiebaldSchema::TypedParts => {
            "SELECT CAST(mp.parent_chat_message_id AS TEXT), COUNT(*) as tool_call_count
             FROM message_parts mp
             JOIN tool_execution_context tc ON tc.message_part_id = mp.id
             WHERE mp.part_type = 'tool'
             GROUP BY mp.parent_chat_message_id"
        }
    };
    let mut stmt = conn.prepare(sql)?;

    let counts = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

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
        // OpenAI names its premium low-latency tier `fast`, while older
        // records and Splitrail's provider-neutral enum use `priority`.
        // Treat both spellings as the same billing class so current Astra
        // traffic is not silently charged at the Standard rate.
        Some("fast" | "priority") => ServiceTier::Priority,
        Some("flex") => ServiceTier::Flex,
        Some("batch") => ServiceTier::Batch,
        _ => ServiceTier::Standard,
    }
}

fn model_uses_openai_token_semantics(model: Option<&str>) -> bool {
    model
        .and_then(get_model_info)
        .is_some_and(|info| info.input_token_semantics == InputTokenSemantics::IncludesCacheRead)
}

fn normalize_input_tokens(
    model: Option<&str>,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: Option<u64>,
) -> u64 {
    if !model_uses_openai_token_semantics(model) {
        return input_tokens;
    }

    let Some(cache_write_tokens) = cache_write_tokens else {
        return input_tokens.saturating_sub(cache_read_tokens);
    };

    // OpenAI may report overlapping reads and writes. Remove their union from
    // ordinary input so no token is billed at all three input rates.
    input_tokens.saturating_sub(cache_read_tokens.max(cache_write_tokens))
}

fn billable_output_tokens(model: Option<&str>, output_tokens: u64, reasoning_tokens: u64) -> u64 {
    if model_uses_openai_token_semantics(model) {
        output_tokens.saturating_add(reasoning_tokens)
    } else {
        output_tokens
    }
}

/// Convert Piebald messages to splitrail's ConversationMessage format.
fn convert_messages(
    chats: &[PiebaldChat],
    messages: Vec<PiebaldMessage>,
    tool_call_counts: &HashMap<String, u32>,
) -> Vec<ConversationMessage> {
    // Build chat lookup map for O(1) access, keyed like `parent_chat_id`: by native ID.
    let chat_map: HashMap<&str, &PiebaldChat> = chats.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut unparseable_timestamps = 0usize;

    let converted: Vec<ConversationMessage> = messages
        .into_iter()
        .filter_map(|msg| {
            // `messages.parent_chat_id` cascades on chat deletion and both queries
            // share one snapshot, so a miss here is not expected in practice.
            let chat = chat_map.get(msg.parent_chat_id.as_str())?;

            // Parse timestamp - use updated_at so that streaming updates are captured
            // (updated_at changes when tokens are added during streaming).
            // Unparseable rows are skipped but counted, so a future timestamp-format
            // change produces a warning rather than another silent drop to zero.
            let Some(date) = parse_timestamp(&msg.updated_at) else {
                unparseable_timestamps += 1;
                return None;
            };

            // Use project path from Piebald's projects table, falling back to "ungrouped" if not set.
            let project_hash = hash_text(chat.project_directory.as_deref().unwrap_or("ungrouped"));

            // Generate globally unique hash using created_at timestamp + message ID.
            // Use created_at (not updated_at) so the hash stays stable across token updates.
            // The timestamp has nanosecond precision which is unique per installation,
            // and combined with the message ID ensures no collisions across users.
            // NOTE: We cannot use just the ID because pre-KSUID IDs were local SQLite
            // autoincrements that start at 1 for every Piebald installation.
            // Stable IDs keep this formula's output unchanged for pre-KSUID rows.
            let conversation_hash = chat.stable_id.clone();
            let global_hash = hash_text(&format!("piebald_{}_{}", msg.created_at, msg.stable_id));

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
            let cache_write_tokens = msg.cache_write_tokens.map(|tokens| tokens as u64);
            let cache_creation_tokens = cache_write_tokens.unwrap_or(0);
            let input_tokens = normalize_input_tokens(
                model_str.as_deref(),
                raw_input_tokens,
                cache_read_tokens,
                cache_write_tokens,
            );

            let service_tier = parse_service_tier(msg.service_tier.as_deref());

            // Calculate cost using splitrail's model pricing. OpenAI-style reasoning tokens are
            // billed at the output rate, while remaining separate in display stats.
            let billable_output =
                billable_output_tokens(model_str.as_deref(), output_tokens, reasoning_tokens);
            let cost = if let Some(ref model) = model_str {
                calculate_total_cost_for_service_tier_at(
                    model,
                    service_tier,
                    input_tokens,
                    billable_output,
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
                project_path: chat.project_directory.clone(),
                conversation_hash,
                local_hash: Some(msg.stable_id.clone()),
                global_hash,
                model: model_str,
                stats,
                role,
                uuid: Some(msg.stable_id),
                session_name: chat.title.clone(),
            })
        })
        .collect();

    if unparseable_timestamps > 0 {
        eprintln!(
            "WARNING: skipped {unparseable_timestamps} Piebald message(s) with unparseable updated_at timestamps"
        );
    }
    converted
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
        let mut conn = open_piebald_db(&source.path)?;
        // Pin schema detection and all reads to one snapshot. Piebald may migrate
        // or stream new usage while Splitrail is running; mixing snapshots could
        // select a dropped table or combine usage and tool counts from different turns.
        // This is a deferred read transaction on a read-only connection, never a write.
        let tx = conn.transaction()?;
        let layout = PiebaldLayout::detect(&tx)?;
        let chats = query_chats(&tx, layout)?;
        let messages = query_messages(&tx, layout)?;
        let tool_call_counts = query_tool_call_counts(&tx, layout.parts)?;
        tx.commit()?;
        Ok(convert_messages(&chats, messages, &tool_call_counts))
    }

    fn parse_sources_parallel(&self, sources: &[DataSource]) -> Vec<ConversationMessage> {
        // The shared helper reports a source that fails to parse. Swallowing that
        // error would make an unsupported Piebald schema indistinguishable from
        // having no Piebald usage at all.
        let all_messages: Vec<ConversationMessage> = self
            .parse_sources_parallel_with_paths(sources)
            .into_iter()
            .flat_map(|(_, messages)| messages)
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
#[path = "piebald_schema_tests.rs"]
mod schema_tests;

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
        assert_eq!(parse_service_tier(Some(" FAST ")), ServiceTier::Priority);
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
            id: "1".to_string(),
            stable_id: "1".to_string(),
            title: Some("Priority chat".to_string()),
            model: Some("gpt-5.4".to_string()),
            project_directory: Some("/tmp/project".to_string()),
        }];
        let messages = vec![PiebaldMessage {
            id: "10".to_string(),
            stable_id: "10".to_string(),
            parent_chat_id: "1".to_string(),
            role: "assistant".to_string(),
            model: Some("gpt-5.4".to_string()),
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            reasoning_tokens: Some(100_000),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
            service_tier: Some("priority".to_string()),
            created_at: "2026-05-01T12:00:00Z".to_string(),
            updated_at: "2026-05-01T12:00:01Z".to_string(),
        }];
        let tool_call_counts = HashMap::new();

        let converted = convert_messages(&chats, messages, &tool_call_counts);

        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].project_path.as_deref(), Some("/tmp/project"));
        assert_eq!(converted[0].stats.output_tokens, 1_000_000);
        assert_eq!(converted[0].stats.reasoning_tokens, 100_000);
        assert_eq!(converted[0].stats.cost, 38.0);
    }

    #[test]
    fn test_billable_output_tokens_includes_openai_reasoning() {
        assert_eq!(billable_output_tokens(Some("gpt-5.5"), 1_000, 300), 1_300);
    }

    #[test]
    fn test_billable_output_tokens_preserves_anthropic_output() {
        assert_eq!(
            billable_output_tokens(Some("claude-sonnet-4-20250514"), 1_000, 300),
            1_000
        );
    }

    #[test]
    fn test_normalize_input_tokens_subtracts_openai_cached_reads() {
        assert_eq!(normalize_input_tokens(Some("gpt-5"), 1_000, 300, None), 700);
    }

    #[test]
    fn test_normalize_input_tokens_subtracts_tiered_openai_cached_reads() {
        assert_eq!(
            normalize_input_tokens(Some("gpt-5.5"), 1_000, 300, None),
            700
        );
    }

    #[test]
    fn test_normalize_input_tokens_subtracts_openai_cache_write_union() {
        assert_eq!(
            normalize_input_tokens(Some("gpt-5.6-sol"), 4_583, 3_945, Some(4_580)),
            3
        );
    }

    #[test]
    fn test_normalize_input_tokens_subtracts_larger_openai_cache_read() {
        assert_eq!(
            normalize_input_tokens(Some("gpt-5.6-sol"), 5_000, 4_000, Some(3_000)),
            1_000
        );
    }

    #[test]
    fn test_convert_messages_counts_cache_writes_toward_astra_context() {
        let chats = vec![PiebaldChat {
            id: "1".to_string(),
            stable_id: "1".to_string(),
            title: Some("Long cached Astra chat".to_string()),
            model: Some("gpt-6-astra".to_string()),
            project_directory: Some("/tmp/project".to_string()),
        }];
        let messages = vec![PiebaldMessage {
            id: "10".to_string(),
            stable_id: "10".to_string(),
            parent_chat_id: "1".to_string(),
            role: "assistant".to_string(),
            model: Some("gpt-6-astra".to_string()),
            input_tokens: Some(400_000),
            output_tokens: Some(10_000),
            reasoning_tokens: Some(0),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(300_000),
            service_tier: None,
            created_at: "2026-09-03T12:00:00Z".to_string(),
            updated_at: "2026-09-03T12:00:01Z".to_string(),
        }];

        let converted = convert_messages(&chats, messages, &HashMap::new());
        let stats = &converted[0].stats;

        // Piebald's raw 400K input includes the 300K cache write, leaving
        // 100K ordinary input for billing. Reconstructing the prompt with the
        // cache write crosses Astra's 272K boundary, so the full request uses
        // $20/M input, $75/M output, and $25/M cache-write rates.
        assert_eq!(stats.input_tokens, 100_000);
        assert_eq!(stats.cache_creation_tokens, 300_000);
        assert!((stats.cost - 10.25).abs() < 1e-9);
    }

    #[test]
    fn test_convert_messages_uses_cache_write_rate_without_double_charging() {
        let chats = vec![PiebaldChat {
            id: "1".to_string(),
            stable_id: "1".to_string(),
            title: Some("Cached chat".to_string()),
            model: Some("gpt-5.6-sol".to_string()),
            project_directory: Some("/tmp/project".to_string()),
        }];
        let messages = vec![PiebaldMessage {
            id: "10".to_string(),
            stable_id: "10".to_string(),
            parent_chat_id: "1".to_string(),
            role: "assistant".to_string(),
            model: Some("gpt-5.6-sol".to_string()),
            input_tokens: Some(4_583),
            output_tokens: Some(0),
            reasoning_tokens: Some(0),
            cache_read_tokens: Some(3_945),
            cache_write_tokens: Some(4_580),
            service_tier: None,
            created_at: "2026-07-10T12:00:00Z".to_string(),
            updated_at: "2026-07-10T12:00:01Z".to_string(),
        }];

        let converted = convert_messages(&chats, messages, &HashMap::new());
        let stats = &converted[0].stats;

        assert_eq!(stats.input_tokens, 3);
        assert_eq!(stats.cache_read_tokens, 3_945);
        assert_eq!(stats.cache_creation_tokens, 4_580);
        assert!((stats.cost - 0.030_612_5).abs() < 1e-9);
    }

    #[test]
    fn test_normalize_input_tokens_preserves_anthropic_input() {
        assert_eq!(
            normalize_input_tokens(Some("claude-sonnet-4-20250514"), 700, 300, Some(200)),
            700
        );
    }

    #[test]
    fn test_normalize_input_tokens_saturates_for_openai() {
        assert_eq!(normalize_input_tokens(Some("gpt-5"), 100, 300, None), 0);
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
