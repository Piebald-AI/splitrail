use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::tui::logic::aggregate_sessions_from_messages;
use crate::utils::aggregate_by_date;

/// Pre-computed session aggregate for TUI display.
/// Contains aggregated stats per conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAggregate {
    pub session_id: String,
    pub first_timestamp: DateTime<Utc>,
    pub analyzer_name: String,
    pub stats: Stats,
    pub models: Vec<String>,
    pub session_name: Option<String>,
    pub day_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Application {
    ClaudeCode,
    GeminiCli,
    QwenCode,
    CodexCli,
    Cline,
    RooCode,
    KiloCode,
    Copilot,
    OpenCode,
    PiAgent,
    Piebald,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub application: Application,
    #[serde(rename = "date")]
    pub date: DateTime<Utc>,
    pub project_hash: String,
    pub conversation_hash: String,
    /// The hash of this message, local to the application that we're gathering data from.  E.g.,
    /// in the Claude Code analyzer, this will be set to the message's hash within Claude Code.
    /// This is an Option because sometimes there's no way to generate, and so no need for,
    /// a local hash.
    pub local_hash: Option<String>,
    /// The hash of this message, global to the Splitrail instance.  This is used on the server to
    /// ensure that, in the event that messages that have been previously uploaded to the server
    /// are reuploaded, they are not redundantly inserted into the database and cause incorrectly
    /// inflated pre-aggregated metrics.
    pub global_hash: String,
    pub model: Option<String>, // None for user messages
    pub stats: Stats,
    pub role: MessageRole,
    pub uuid: Option<String>,
    pub session_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyStats {
    pub date: String,
    pub user_messages: u32,
    pub ai_messages: u32,
    pub conversations: u32,
    pub models: BTreeMap<String, u32>,
    pub stats: Stats,
}

impl std::ops::AddAssign<&DailyStats> for DailyStats {
    fn add_assign(&mut self, rhs: &DailyStats) {
        self.user_messages += rhs.user_messages;
        self.ai_messages += rhs.ai_messages;
        self.conversations += rhs.conversations;
        for (model, count) in &rhs.models {
            *self.models.entry(model.clone()).or_insert(0) += count;
        }
        self.stats += rhs.stats.clone();
    }
}

impl std::ops::SubAssign<&DailyStats> for DailyStats {
    fn sub_assign(&mut self, rhs: &DailyStats) {
        self.user_messages = self.user_messages.saturating_sub(rhs.user_messages);
        self.ai_messages = self.ai_messages.saturating_sub(rhs.ai_messages);
        self.conversations = self.conversations.saturating_sub(rhs.conversations);
        for (model, count) in &rhs.models {
            if let Some(existing) = self.models.get_mut(model) {
                *existing = existing.saturating_sub(*count);
                if *existing == 0 {
                    self.models.remove(model);
                }
            }
        }
        self.stats -= rhs.stats.clone();
    }
}

/// Cached contribution from a single file for incremental updates.
/// Stores pre-computed aggregates so we can subtract old and add new
/// without reparsing all files.
#[derive(Debug, Clone, Default)]
pub struct FileContribution {
    /// Session aggregates from this file (usually 1 per file)
    pub session_aggregates: Vec<SessionAggregate>,
    /// Daily stats from this file keyed by date
    pub daily_stats: BTreeMap<String, DailyStats>,
    /// Number of conversations in this file
    pub conversation_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    // Token and cost stats
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub tool_calls: u32,

    // File operation stats
    pub terminal_commands: u64,
    pub file_searches: u64,
    pub file_content_searches: u64,
    pub files_read: u64,
    pub files_added: u64,
    pub files_edited: u64,
    pub files_deleted: u64,
    pub lines_read: u64,
    pub lines_added: u64,
    pub lines_edited: u64,
    pub lines_deleted: u64,
    pub bytes_read: u64,
    pub bytes_added: u64,
    pub bytes_edited: u64,
    pub bytes_deleted: u64,

    // Todo stats
    pub todos_created: u64,
    pub todos_completed: u64,
    pub todos_in_progress: u64,
    pub todo_writes: u64,
    pub todo_reads: u64,

    // Composition stats
    pub code_lines: u64,
    pub docs_lines: u64,
    pub data_lines: u64,
    pub media_lines: u64,
    pub config_lines: u64,
    pub other_lines: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum FileCategory {
    SourceCode,
    Data,
    Documentation,
    Media,
    Config,
    Other,
}

impl std::ops::AddAssign for Stats {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.reasoning_tokens += rhs.reasoning_tokens;
        self.cache_creation_tokens += rhs.cache_creation_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
        self.cached_tokens += rhs.cached_tokens;
        self.cost += rhs.cost;
        self.tool_calls += rhs.tool_calls;
        self.terminal_commands += rhs.terminal_commands;
        self.file_searches += rhs.file_searches;
        self.file_content_searches += rhs.file_content_searches;
        self.files_read += rhs.files_read;
        self.files_added += rhs.files_added;
        self.files_edited += rhs.files_edited;
        self.files_deleted += rhs.files_deleted;
        self.lines_read += rhs.lines_read;
        self.lines_added += rhs.lines_added;
        self.lines_edited += rhs.lines_edited;
        self.lines_deleted += rhs.lines_deleted;
        self.bytes_read += rhs.bytes_read;
        self.bytes_added += rhs.bytes_added;
        self.bytes_edited += rhs.bytes_edited;
        self.bytes_deleted += rhs.bytes_deleted;
        self.todos_created += rhs.todos_created;
        self.todos_completed += rhs.todos_completed;
        self.todos_in_progress += rhs.todos_in_progress;
        self.todo_writes += rhs.todo_writes;
        self.todo_reads += rhs.todo_reads;
        self.code_lines += rhs.code_lines;
        self.docs_lines += rhs.docs_lines;
        self.data_lines += rhs.data_lines;
        self.media_lines += rhs.media_lines;
        self.config_lines += rhs.config_lines;
        self.other_lines += rhs.other_lines;
    }
}

impl std::ops::SubAssign for Stats {
    fn sub_assign(&mut self, rhs: Self) {
        self.input_tokens = self.input_tokens.saturating_sub(rhs.input_tokens);
        self.output_tokens = self.output_tokens.saturating_sub(rhs.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_sub(rhs.reasoning_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_sub(rhs.cache_creation_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_sub(rhs.cache_read_tokens);
        self.cached_tokens = self.cached_tokens.saturating_sub(rhs.cached_tokens);
        self.cost -= rhs.cost;
        self.tool_calls = self.tool_calls.saturating_sub(rhs.tool_calls);
        self.terminal_commands = self.terminal_commands.saturating_sub(rhs.terminal_commands);
        self.file_searches = self.file_searches.saturating_sub(rhs.file_searches);
        self.file_content_searches = self
            .file_content_searches
            .saturating_sub(rhs.file_content_searches);
        self.files_read = self.files_read.saturating_sub(rhs.files_read);
        self.files_added = self.files_added.saturating_sub(rhs.files_added);
        self.files_edited = self.files_edited.saturating_sub(rhs.files_edited);
        self.files_deleted = self.files_deleted.saturating_sub(rhs.files_deleted);
        self.lines_read = self.lines_read.saturating_sub(rhs.lines_read);
        self.lines_added = self.lines_added.saturating_sub(rhs.lines_added);
        self.lines_edited = self.lines_edited.saturating_sub(rhs.lines_edited);
        self.lines_deleted = self.lines_deleted.saturating_sub(rhs.lines_deleted);
        self.bytes_read = self.bytes_read.saturating_sub(rhs.bytes_read);
        self.bytes_added = self.bytes_added.saturating_sub(rhs.bytes_added);
        self.bytes_edited = self.bytes_edited.saturating_sub(rhs.bytes_edited);
        self.bytes_deleted = self.bytes_deleted.saturating_sub(rhs.bytes_deleted);
        self.todos_created = self.todos_created.saturating_sub(rhs.todos_created);
        self.todos_completed = self.todos_completed.saturating_sub(rhs.todos_completed);
        self.todos_in_progress = self.todos_in_progress.saturating_sub(rhs.todos_in_progress);
        self.todo_writes = self.todo_writes.saturating_sub(rhs.todo_writes);
        self.todo_reads = self.todo_reads.saturating_sub(rhs.todo_reads);
        self.code_lines = self.code_lines.saturating_sub(rhs.code_lines);
        self.docs_lines = self.docs_lines.saturating_sub(rhs.docs_lines);
        self.data_lines = self.data_lines.saturating_sub(rhs.data_lines);
        self.media_lines = self.media_lines.saturating_sub(rhs.media_lines);
        self.config_lines = self.config_lines.saturating_sub(rhs.config_lines);
        self.other_lines = self.other_lines.saturating_sub(rhs.other_lines);
    }
}

impl FileCategory {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "java" | "cpp" | "c" | "h" | "hpp"
            | "cs" | "go" | "php" | "rb" | "swift" | "kt" | "scala" | "clj" | "hs" | "ml"
            | "fs" | "elm" | "dart" | "lua" | "r" | "jl" | "nim" | "zig" | "v" | "odin" => {
                FileCategory::SourceCode
            }
            "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "csv" | "tsv" | "sql" | "db"
            | "sqlite" | "sqlite3" => FileCategory::Data,
            "md" | "txt" | "rst" | "adoc" | "tex" | "rtf" | "doc" | "docx" | "pdf" | "html"
            | "htm" => FileCategory::Documentation,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "ico" | "webp" | "tiff" | "mp3"
            | "wav" | "mp4" | "avi" | "mkv" | "mov" | "wmv" | "flv" | "webm" => FileCategory::Media,
            "config" | "conf" | "cfg" | "env" | "properties" | "plist" | "reg" | "desktop"
            | "service" => FileCategory::Config,
            _ => FileCategory::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticCodingToolStats {
    pub daily_stats: BTreeMap<String, DailyStats>,
    pub num_conversations: u64,
    pub messages: Vec<ConversationMessage>,
    pub analyzer_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAnalyzerStats {
    pub analyzer_stats: Vec<AgenticCodingToolStats>,
}

/// Lightweight view for TUI - NO raw messages, only pre-computed aggregates.
/// Reduces memory from ~3.5MB to ~70KB per analyzer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerStatsView {
    pub daily_stats: BTreeMap<String, DailyStats>,
    pub session_aggregates: Vec<SessionAggregate>,
    pub num_conversations: u64,
    pub analyzer_name: String,
}

/// Container for TUI display - view-only stats without messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAnalyzerStatsView {
    pub analyzer_stats: Vec<AnalyzerStatsView>,
}

impl AgenticCodingToolStats {
    /// Convert full stats to lightweight view, consuming self.
    /// Messages are dropped, session_aggregates are pre-computed.
    pub fn into_view(self) -> AnalyzerStatsView {
        let session_aggregates =
            aggregate_sessions_from_messages(&self.messages, &self.analyzer_name);
        AnalyzerStatsView {
            daily_stats: self.daily_stats,
            session_aggregates,
            num_conversations: self.num_conversations,
            analyzer_name: self.analyzer_name,
        }
    }
}

impl FileContribution {
    /// Compute a FileContribution from parsed messages.
    pub fn from_messages(messages: &[ConversationMessage], analyzer_name: &str) -> Self {
        let session_aggregates = aggregate_sessions_from_messages(messages, analyzer_name);
        let mut daily_stats = aggregate_by_date(messages);
        daily_stats.retain(|date, _| date != "unknown");

        // Count unique conversations
        let conversation_count = session_aggregates.len() as u64;

        Self {
            session_aggregates,
            daily_stats,
            conversation_count,
        }
    }
}

impl AnalyzerStatsView {
    /// Add a file's contribution to this view (for incremental updates).
    pub fn add_contribution(&mut self, contrib: &FileContribution) {
        // Add daily stats
        for (date, day_stats) in &contrib.daily_stats {
            *self
                .daily_stats
                .entry(date.clone())
                .or_insert_with(|| DailyStats {
                    date: date.clone(),
                    ..Default::default()
                }) += day_stats;
        }

        // Add session aggregates - merge if same session_id exists, otherwise append
        for new_session in &contrib.session_aggregates {
            if let Some(existing) = self
                .session_aggregates
                .iter_mut()
                .find(|s| s.session_id == new_session.session_id)
            {
                // Merge into existing session
                existing.stats += new_session.stats.clone();
                for model in &new_session.models {
                    if !existing.models.contains(model) {
                        existing.models.push(model.clone());
                    }
                }
                if new_session.first_timestamp < existing.first_timestamp {
                    existing.first_timestamp = new_session.first_timestamp;
                    existing.day_key = new_session.day_key.clone();
                }
                if existing.session_name.is_none() {
                    existing.session_name = new_session.session_name.clone();
                }
            } else {
                // New session
                self.session_aggregates.push(new_session.clone());
            }
        }

        self.num_conversations += contrib.conversation_count;

        // Keep sessions sorted by timestamp
        self.session_aggregates.sort_by_key(|s| s.first_timestamp);
    }

    /// Subtract a file's contribution from this view (for incremental updates).
    pub fn subtract_contribution(&mut self, contrib: &FileContribution) {
        // Subtract daily stats
        for (date, day_stats) in &contrib.daily_stats {
            if let Some(existing) = self.daily_stats.get_mut(date) {
                *existing -= day_stats;
                // Remove if empty
                if existing.user_messages == 0
                    && existing.ai_messages == 0
                    && existing.conversations == 0
                {
                    self.daily_stats.remove(date);
                }
            }
        }

        // Subtract session stats (arithmetic, not removal) to handle partial updates correctly
        for old_session in &contrib.session_aggregates {
            if let Some(existing) = self
                .session_aggregates
                .iter_mut()
                .find(|s| s.session_id == old_session.session_id)
            {
                existing.stats -= old_session.stats.clone();
                // Remove models that were in the old session
                for model in &old_session.models {
                    existing.models.retain(|m| m != model);
                }
            }
        }

        self.num_conversations = self
            .num_conversations
            .saturating_sub(contrib.conversation_count);
    }
}

impl MultiAnalyzerStats {
    /// Convert to view type, consuming self and dropping all messages.
    pub fn into_view(self) -> MultiAnalyzerStatsView {
        MultiAnalyzerStatsView {
            analyzer_stats: self
                .analyzer_stats
                .into_iter()
                .map(|s| s.into_view())
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UploadResponse {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn file_category_classifies_extensions() {
        assert!(matches!(
            FileCategory::from_extension("rs"),
            FileCategory::SourceCode
        ));
        assert!(matches!(
            FileCategory::from_extension("JSON"),
            FileCategory::Data
        ));
        assert!(matches!(
            FileCategory::from_extension("md"),
            FileCategory::Documentation
        ));
        assert!(matches!(
            FileCategory::from_extension("png"),
            FileCategory::Media
        ));
        assert!(matches!(
            FileCategory::from_extension("config"),
            FileCategory::Config
        ));
        assert!(matches!(
            FileCategory::from_extension("unknown-ext"),
            FileCategory::Other
        ));
    }

    #[test]
    fn stats_default_is_zeroed() {
        let stats = Stats::default();
        assert_eq!(stats.input_tokens, 0);
        assert_eq!(stats.output_tokens, 0);
        assert_eq!(stats.tool_calls, 0);
        assert_eq!(stats.code_lines, 0);
    }

    fn sample_message(date_str: &str, conv_hash: &str) -> ConversationMessage {
        let date = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        ConversationMessage {
            application: Application::ClaudeCode,
            date: Utc.from_utc_datetime(&date),
            project_hash: "proj".into(),
            conversation_hash: conv_hash.into(),
            local_hash: None,
            global_hash: format!("global_{}", conv_hash),
            model: Some("claude-3-5-sonnet".into()),
            stats: Stats {
                input_tokens: 100,
                output_tokens: 50,
                cost: 0.01,
                ..Stats::default()
            },
            role: MessageRole::Assistant,
            uuid: None,
            session_name: Some("Test Session".into()),
        }
    }

    #[test]
    fn into_view_converts_stats_correctly() {
        let stats = AgenticCodingToolStats {
            daily_stats: BTreeMap::new(),
            num_conversations: 2,
            messages: vec![
                sample_message("2025-01-01", "conv1"),
                sample_message("2025-01-02", "conv2"),
            ],
            analyzer_name: "Test".into(),
        };

        let view = stats.into_view();

        assert_eq!(view.analyzer_name, "Test");
        assert_eq!(view.num_conversations, 2);
        assert_eq!(view.session_aggregates.len(), 2);
    }

    #[test]
    fn multi_analyzer_stats_into_view() {
        let multi = MultiAnalyzerStats {
            analyzer_stats: vec![AgenticCodingToolStats {
                daily_stats: BTreeMap::new(),
                num_conversations: 1,
                messages: vec![sample_message("2025-01-01", "conv1")],
                analyzer_name: "Analyzer1".into(),
            }],
        };

        let view = multi.into_view();

        assert_eq!(view.analyzer_stats.len(), 1);
        assert_eq!(view.analyzer_stats[0].analyzer_name, "Analyzer1");
    }
}
