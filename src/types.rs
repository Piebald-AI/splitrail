use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tinyvec::TinyVec;

use crate::cache::ModelKey;
use crate::tui::logic::aggregate_sessions_from_messages;

// Re-export interning functions for convenience
pub use crate::cache::{intern_model, resolve_model};

// ============================================================================
// CompactDate - Compact date representation (4 bytes, no heap allocation)
// ============================================================================

/// Compact representation of a date in "YYYY-MM-DD" format.
/// Stored as year (u16) + month (u8) + day (u8) = 4 bytes total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CompactDate {
    year: u16,
    month: u8,
    day: u8,
}

impl CompactDate {
    /// Create a CompactDate directly from a DateTime (in local timezone).
    #[inline]
    pub fn from_local<Tz: chrono::TimeZone>(dt: &DateTime<Tz>) -> Self {
        use chrono::{Datelike, Local};
        let local = dt.with_timezone(&Local);
        Self {
            year: local.year() as u16,
            month: local.month() as u8,
            day: local.day() as u8,
        }
    }

    /// Create a CompactDate from individual components.
    #[inline]
    pub fn from_parts(year: u16, month: u8, day: u8) -> Self {
        Self { year, month, day }
    }

    /// Get the year component.
    #[inline]
    pub fn year(&self) -> u16 {
        self.year
    }

    /// Get the month component (1-12).
    #[inline]
    pub fn month(&self) -> u8 {
        self.month
    }

    /// Get the day component (1-31).
    #[inline]
    pub fn day(&self) -> u8 {
        self.day
    }

    /// Create a CompactDate from a "YYYY-MM-DD" string.
    #[inline]
    pub fn from_str(s: &str) -> Option<Self> {
        Self::parse(s).map(|(year, month, day)| Self { year, month, day })
    }

    /// Parse a date string, returning None if invalid format.
    #[inline]
    fn parse(s: &str) -> Option<(u16, u8, u8)> {
        if s.len() != 10 {
            return None;
        }
        let bytes = s.as_bytes();
        if bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let year = (bytes[0].wrapping_sub(b'0') as u16)
            .checked_mul(1000)?
            .checked_add((bytes[1].wrapping_sub(b'0') as u16).checked_mul(100)?)?
            .checked_add((bytes[2].wrapping_sub(b'0') as u16).checked_mul(10)?)?
            .checked_add(bytes[3].wrapping_sub(b'0') as u16)?;
        let month = (bytes[5].wrapping_sub(b'0'))
            .checked_mul(10)?
            .checked_add(bytes[6].wrapping_sub(b'0'))?;
        let day = (bytes[8].wrapping_sub(b'0'))
            .checked_mul(10)?
            .checked_add(bytes[9].wrapping_sub(b'0'))?;
        Some((year, month, day))
    }
}

impl Serialize for CompactDate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CompactDate {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).ok_or_else(|| serde::de::Error::custom("invalid date format"))
    }
}

impl Ord for CompactDate {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.year, self.month, self.day).cmp(&(other.year, other.month, other.day))
    }
}

impl PartialOrd for CompactDate {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for CompactDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

// ============================================================================
// ModelCounts - Compact reference-counted model tracking
// ============================================================================

/// Compact model reference counts with inline storage for up to 3 models.
/// Provides a map-like interface over a TinyVec for memory efficiency.
/// Spills to heap if more than 3 models are added.
#[derive(Debug, Clone, Default)]
pub struct ModelCounts(TinyVec<[(ModelKey, u32); 3]>);

impl ModelCounts {
    /// Create an empty ModelCounts.
    #[inline]
    pub fn new() -> Self {
        Self(TinyVec::new())
    }

    /// Increment the count for a model, inserting with count if not present.
    #[inline]
    pub fn increment(&mut self, key: ModelKey, count: u32) {
        if let Some((_, c)) = self.0.iter_mut().find(|(k, _)| *k == key) {
            *c += count;
        } else {
            self.0.push((key, count));
        }
    }

    /// Decrement the count for a model, removing it if count reaches zero.
    #[inline]
    pub fn decrement(&mut self, key: ModelKey, count: u32) {
        if let Some((_, c)) = self.0.iter_mut().find(|(k, _)| *k == key) {
            *c = c.saturating_sub(count);
        }
        self.0.retain(|(_, c)| *c > 0);
    }

    /// Get the count for a model, returning None if not present.
    #[inline]
    pub fn get(&self, key: ModelKey) -> Option<u32> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, c)| *c)
    }

    /// Iterate over (model, count) pairs.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &(ModelKey, u32)> {
        self.0.iter()
    }

    /// Create with a single model entry.
    #[inline]
    pub fn from_single(key: ModelKey, count: u32) -> Self {
        let mut s = Self::new();
        s.0.push((key, count));
        s
    }
}

// ============================================================================
// SessionAggregate
// ============================================================================

/// Pre-computed session aggregate for TUI display.
/// Contains aggregated stats per conversation session.
/// Note: Not serialized - view-only type for TUI. Uses `Arc<str>` for memory efficiency.
#[derive(Debug, Clone)]
pub struct SessionAggregate {
    pub session_id: String,
    pub first_timestamp: DateTime<Utc>,
    /// Shared across all sessions from the same analyzer (Arc clone is cheap)
    pub analyzer_name: Arc<str>,
    pub stats: TuiStats,
    /// Reference-counted model names for correct incremental update subtraction.
    /// Inline storage for up to 3 models; interned keys are 4 bytes each.
    pub models: ModelCounts,
    pub session_name: Option<String>,
    pub date: CompactDate,
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

/// Daily statistics for TUI display.
/// Note: This struct only contains fields displayed in the TUI. File operation stats
/// (files_read, files_edited, etc.) are not included here - they are computed on-demand
/// from raw messages when needed (e.g., in the MCP server).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyStats {
    pub date: CompactDate,
    pub user_messages: u32,
    pub ai_messages: u32,
    pub conversations: u32,
    /// Reference-counted model occurrences for correct incremental update subtraction.
    pub models: BTreeMap<String, u32>,
    pub stats: TuiStats,
}

impl std::ops::AddAssign<&DailyStats> for DailyStats {
    fn add_assign(&mut self, rhs: &DailyStats) {
        self.user_messages += rhs.user_messages;
        self.ai_messages += rhs.ai_messages;
        self.conversations += rhs.conversations;
        for (model, count) in &rhs.models {
            *self.models.entry(model.clone()).or_insert(0) += count;
        }
        self.stats += rhs.stats;
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
        self.stats -= rhs.stats;
    }
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

/// Lightweight stats for TUI display only (24 bytes vs 320 bytes for full Stats).
/// Contains only fields actually rendered in the UI.
/// Uses u32 for memory efficiency - sufficient for per-session and per-day values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiStats {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub reasoning_tokens: u32,
    pub cached_tokens: u32,
    pub cost_cents: u32, // Store as cents to avoid f32 precision issues
    pub tool_calls: u32,
}

impl TuiStats {
    /// Get cost as f64 dollars for display
    #[inline]
    pub fn cost(&self) -> f64 {
        self.cost_cents as f64 / 100.0
    }

    /// Set cost from f64 dollars
    #[inline]
    pub fn set_cost(&mut self, dollars: f64) {
        self.cost_cents = (dollars * 100.0).round() as u32;
    }

    /// Add cost from f64 dollars
    #[inline]
    pub fn add_cost(&mut self, dollars: f64) {
        self.cost_cents = self
            .cost_cents
            .saturating_add((dollars * 100.0).round() as u32);
    }
}

impl From<&Stats> for TuiStats {
    fn from(s: &Stats) -> Self {
        TuiStats {
            input_tokens: s.input_tokens as u32,
            output_tokens: s.output_tokens as u32,
            reasoning_tokens: s.reasoning_tokens as u32,
            cached_tokens: s.cached_tokens as u32,
            cost_cents: (s.cost * 100.0).round() as u32,
            tool_calls: s.tool_calls,
        }
    }
}

impl std::ops::AddAssign for TuiStats {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens = self.input_tokens.saturating_add(rhs.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(rhs.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(rhs.reasoning_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(rhs.cached_tokens);
        self.cost_cents = self.cost_cents.saturating_add(rhs.cost_cents);
        self.tool_calls = self.tool_calls.saturating_add(rhs.tool_calls);
    }
}

impl std::ops::SubAssign for TuiStats {
    fn sub_assign(&mut self, rhs: Self) {
        self.input_tokens = self.input_tokens.saturating_sub(rhs.input_tokens);
        self.output_tokens = self.output_tokens.saturating_sub(rhs.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_sub(rhs.reasoning_tokens);
        self.cached_tokens = self.cached_tokens.saturating_sub(rhs.cached_tokens);
        self.cost_cents = self.cost_cents.saturating_sub(rhs.cost_cents);
        self.tool_calls = self.tool_calls.saturating_sub(rhs.tool_calls);
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
/// Saves a lot of memory by not storing each message.
/// Note: Not serialized - view-only type for TUI. Uses `Arc<str>` for memory efficiency.
#[derive(Debug, Clone)]
pub struct AnalyzerStatsView {
    pub daily_stats: BTreeMap<String, DailyStats>,
    pub session_aggregates: Vec<SessionAggregate>,
    pub num_conversations: u64,
    /// Shared analyzer name - same Arc used by all SessionAggregates
    pub analyzer_name: Arc<str>,
}

/// Shared view type - Arc<RwLock<...>> allows mutation without cloning.
pub type SharedAnalyzerView = Arc<RwLock<AnalyzerStatsView>>;

/// Container for TUI display - view-only stats without messages.
/// Uses Arc<RwLock<...>> to share AnalyzerStatsView across caches and channels.
/// RwLock enables in-place mutation without cloning during incremental updates.
#[derive(Debug, Clone)]
pub struct MultiAnalyzerStatsView {
    pub analyzer_stats: Vec<SharedAnalyzerView>,
}

impl AgenticCodingToolStats {
    /// Convert full stats to lightweight view, consuming self.
    /// Messages are dropped, session_aggregates are pre-computed.
    /// Returns SharedAnalyzerView for efficient sharing and in-place mutation.
    pub fn into_view(self) -> SharedAnalyzerView {
        // Convert analyzer_name to Arc<str> once, shared across all sessions
        let analyzer_name: Arc<str> = Arc::from(self.analyzer_name);
        let session_aggregates =
            aggregate_sessions_from_messages(&self.messages, Arc::clone(&analyzer_name));
        Arc::new(RwLock::new(AnalyzerStatsView {
            daily_stats: self.daily_stats,
            session_aggregates,
            num_conversations: self.num_conversations,
            analyzer_name,
        }))
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
        let v = view.read();

        assert_eq!(&*v.analyzer_name, "Test");
        assert_eq!(v.num_conversations, 2);
        assert_eq!(v.session_aggregates.len(), 2);
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
        assert_eq!(&*view.analyzer_stats[0].read().analyzer_name, "Analyzer1");
    }

    // ========================================================================
    // Model reference counting tests
    // ========================================================================

    #[test]
    fn daily_stats_model_ref_counting() {
        let mut stats = DailyStats::default();
        let day1 = DailyStats {
            models: [("gpt-4".into(), 2)].into_iter().collect(),
            ..Default::default()
        };
        let day2 = DailyStats {
            models: [("gpt-4".into(), 1), ("claude".into(), 1)]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        stats += &day1;
        assert_eq!(stats.models.get("gpt-4"), Some(&2));

        stats += &day2;
        assert_eq!(stats.models.get("gpt-4"), Some(&3));
        assert_eq!(stats.models.get("claude"), Some(&1));

        stats -= &day1;
        assert_eq!(stats.models.get("gpt-4"), Some(&1));
        assert_eq!(stats.models.get("claude"), Some(&1));

        stats -= &day2;
        assert_eq!(stats.models.get("gpt-4"), None); // removed at 0
        assert_eq!(stats.models.get("claude"), None);
    }

    fn make_session_contrib(
        session_id: &str,
        model: &str,
        count: u32,
    ) -> crate::contribution_cache::MultiSessionContribution {
        crate::contribution_cache::MultiSessionContribution {
            session_aggregates: vec![SessionAggregate {
                session_id: session_id.into(),
                first_timestamp: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
                analyzer_name: Arc::from("Test"),
                stats: TuiStats::default(),
                models: ModelCounts::from_single(intern_model(model), count),
                session_name: None,
                date: CompactDate::default(),
            }],
            ..Default::default()
        }
    }

    fn empty_view() -> AnalyzerStatsView {
        AnalyzerStatsView {
            daily_stats: BTreeMap::new(),
            session_aggregates: Vec::new(),
            num_conversations: 0,
            analyzer_name: Arc::from("Test"),
        }
    }

    /// Helper to get model count
    fn get_model_count(models: &ModelCounts, key: ModelKey) -> Option<u32> {
        models.get(key)
    }

    #[test]
    fn session_model_ref_counting() {
        let mut view = empty_view();
        let model_key = intern_model("claude-3-5-sonnet");

        view.add_multi_session_contribution(&make_session_contrib("s1", "claude-3-5-sonnet", 1));
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, model_key),
            Some(1)
        );

        view.add_multi_session_contribution(&make_session_contrib("s1", "claude-3-5-sonnet", 2));
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, model_key),
            Some(3)
        );

        view.subtract_multi_session_contribution(&make_session_contrib(
            "s1",
            "claude-3-5-sonnet",
            1,
        ));
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, model_key),
            Some(2)
        );

        view.subtract_multi_session_contribution(&make_session_contrib(
            "s1",
            "claude-3-5-sonnet",
            2,
        ));
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, model_key),
            None
        );
    }

    #[test]
    fn session_model_persists_after_partial_subtraction() {
        // Scenario: two files contribute same model to same session
        let mut view = empty_view();
        let model_key = intern_model("gpt-4");

        view.add_multi_session_contribution(&make_session_contrib("s1", "gpt-4", 1)); // file1
        view.add_multi_session_contribution(&make_session_contrib("s1", "gpt-4", 1)); // file2
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, model_key),
            Some(2)
        );

        view.subtract_multi_session_contribution(&make_session_contrib("s1", "gpt-4", 1)); // remove file1
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, model_key),
            Some(1)
        );
        // file2 remains
    }

    #[test]
    fn session_multiple_models() {
        let mut view = empty_view();
        let mut contrib = make_session_contrib("s1", "gpt-4", 1);
        contrib.session_aggregates[0]
            .models
            .increment(intern_model("claude"), 2);

        view.add_multi_session_contribution(&contrib);
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, intern_model("gpt-4")),
            Some(1)
        );
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, intern_model("claude")),
            Some(2)
        );

        view.subtract_multi_session_contribution(&make_session_contrib("s1", "gpt-4", 1));
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, intern_model("gpt-4")),
            None
        );
        assert_eq!(
            get_model_count(&view.session_aggregates[0].models, intern_model("claude")),
            Some(2)
        );
    }
}
