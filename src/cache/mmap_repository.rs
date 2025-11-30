//! High-performance memory-mapped cache repository using rkyv.
//!
//! This module provides a high-performance cache implementation that:
//! - Uses rkyv for fast serialization/deserialization (10-100x faster than JSON)
//! - Stores messages separately for lazy loading (cold data)
//! - Memory-maps message files for efficient access

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use parking_lot::RwLock;
use rkyv::rancor::BoxedError;
use rkyv::{Archive, Deserialize, Serialize};

use crate::types::{
    Application, ConversationMessage, DailyStats, FileMetadata, MessageRole, Stats,
};

use super::{CacheStats, FileCacheEntry, FileCacheKey};

// =============================================================================
// Archived Types (for rkyv zero-copy)
// =============================================================================

/// Cache index - the "hot" data kept in memory-mapped file
#[derive(Archive, Serialize, Deserialize, Debug)]
#[rkyv(derive(Debug))]
pub struct CacheIndex {
    pub version: u32,
    pub entries: Vec<CacheMetaEntry>,
}

/// Metadata + pre-aggregated stats for a single file
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct CacheMetaEntry {
    pub analyzer_name: String,
    pub file_path: String,
    pub file_size: u64,
    pub file_modified: i64,
    /// Byte offset of last successfully parsed position (for delta parsing)
    pub last_parsed_offset: u64,
    /// Pre-aggregated daily contributions from this file
    pub daily_contributions: Vec<(String, CacheDailyStats)>,
    /// Number of messages (for stats display)
    pub message_count: u32,
    /// Cached session model name (for delta parsing context)
    pub cached_model: Option<String>,
}

/// Archived version of DailyStats (uses primitive types only)
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Default)]
#[rkyv(derive(Debug))]
pub struct CacheDailyStats {
    pub date: String,
    pub user_messages: u32,
    pub ai_messages: u32,
    pub conversations: u32,
    pub models: Vec<(String, u32)>, // BTreeMap as Vec for rkyv
    pub stats: CacheStats_,
}

/// Archived version of Stats (all primitive types)
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Default)]
#[rkyv(derive(Debug))]
pub struct CacheStats_ {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cached_tokens: u64,
    pub cost_millicents: i64, // Store as integer to avoid float issues
    pub tool_calls: u32,
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
    pub todos_created: u64,
    pub todos_completed: u64,
    pub todos_in_progress: u64,
    pub todo_writes: u64,
    pub todo_reads: u64,
    pub code_lines: u64,
    pub docs_lines: u64,
    pub data_lines: u64,
    pub media_lines: u64,
    pub config_lines: u64,
    pub other_lines: u64,
}

/// Archived message for cold storage
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct CacheMessage {
    pub application: u8, // Application enum as u8
    pub date_timestamp: i64,
    pub project_hash: String,
    pub conversation_hash: String,
    pub local_hash: Option<String>,
    pub global_hash: String,
    pub model: Option<String>,
    pub stats: CacheStats_,
    pub role: u8, // MessageRole enum as u8
    pub uuid: Option<String>,
    pub session_name: Option<String>,
}

/// Container for messages (cold data file)
#[derive(Archive, Serialize, Deserialize, Debug)]
#[rkyv(derive(Debug))]
pub struct CacheMessages {
    pub messages: Vec<CacheMessage>,
}

// =============================================================================
// Conversion Functions
// =============================================================================

impl From<&Stats> for CacheStats_ {
    fn from(s: &Stats) -> Self {
        Self {
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            reasoning_tokens: s.reasoning_tokens,
            cache_creation_tokens: s.cache_creation_tokens,
            cache_read_tokens: s.cache_read_tokens,
            cached_tokens: s.cached_tokens,
            // Store cost as integer with 0.00001 precision (micro-cents, not millicents)
            // This gives us 5 decimal places of precision for dollar amounts
            cost_millicents: (s.cost * 100_000.0) as i64,
            tool_calls: s.tool_calls,
            terminal_commands: s.terminal_commands,
            file_searches: s.file_searches,
            file_content_searches: s.file_content_searches,
            files_read: s.files_read,
            files_added: s.files_added,
            files_edited: s.files_edited,
            files_deleted: s.files_deleted,
            lines_read: s.lines_read,
            lines_added: s.lines_added,
            lines_edited: s.lines_edited,
            lines_deleted: s.lines_deleted,
            bytes_read: s.bytes_read,
            bytes_added: s.bytes_added,
            bytes_edited: s.bytes_edited,
            bytes_deleted: s.bytes_deleted,
            todos_created: s.todos_created,
            todos_completed: s.todos_completed,
            todos_in_progress: s.todos_in_progress,
            todo_writes: s.todo_writes,
            todo_reads: s.todo_reads,
            code_lines: s.code_lines,
            docs_lines: s.docs_lines,
            data_lines: s.data_lines,
            media_lines: s.media_lines,
            config_lines: s.config_lines,
            other_lines: s.other_lines,
        }
    }
}

impl From<&CacheStats_> for Stats {
    fn from(s: &CacheStats_) -> Self {
        Self {
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            reasoning_tokens: s.reasoning_tokens,
            cache_creation_tokens: s.cache_creation_tokens,
            cache_read_tokens: s.cache_read_tokens,
            cached_tokens: s.cached_tokens,
            cost: s.cost_millicents as f64 / 100_000.0,
            tool_calls: s.tool_calls,
            terminal_commands: s.terminal_commands,
            file_searches: s.file_searches,
            file_content_searches: s.file_content_searches,
            files_read: s.files_read,
            files_added: s.files_added,
            files_edited: s.files_edited,
            files_deleted: s.files_deleted,
            lines_read: s.lines_read,
            lines_added: s.lines_added,
            lines_edited: s.lines_edited,
            lines_deleted: s.lines_deleted,
            bytes_read: s.bytes_read,
            bytes_added: s.bytes_added,
            bytes_edited: s.bytes_edited,
            bytes_deleted: s.bytes_deleted,
            todos_created: s.todos_created,
            todos_completed: s.todos_completed,
            todos_in_progress: s.todos_in_progress,
            todo_writes: s.todo_writes,
            todo_reads: s.todo_reads,
            code_lines: s.code_lines,
            docs_lines: s.docs_lines,
            data_lines: s.data_lines,
            media_lines: s.media_lines,
            config_lines: s.config_lines,
            other_lines: s.other_lines,
        }
    }
}

impl From<&ArchivedCacheStats_> for Stats {
    fn from(s: &ArchivedCacheStats_) -> Self {
        Self {
            input_tokens: s.input_tokens.into(),
            output_tokens: s.output_tokens.into(),
            reasoning_tokens: s.reasoning_tokens.into(),
            cache_creation_tokens: s.cache_creation_tokens.into(),
            cache_read_tokens: s.cache_read_tokens.into(),
            cached_tokens: s.cached_tokens.into(),
            cost: i64::from(s.cost_millicents) as f64 / 100_000.0,
            tool_calls: s.tool_calls.into(),
            terminal_commands: s.terminal_commands.into(),
            file_searches: s.file_searches.into(),
            file_content_searches: s.file_content_searches.into(),
            files_read: s.files_read.into(),
            files_added: s.files_added.into(),
            files_edited: s.files_edited.into(),
            files_deleted: s.files_deleted.into(),
            lines_read: s.lines_read.into(),
            lines_added: s.lines_added.into(),
            lines_edited: s.lines_edited.into(),
            lines_deleted: s.lines_deleted.into(),
            bytes_read: s.bytes_read.into(),
            bytes_added: s.bytes_added.into(),
            bytes_edited: s.bytes_edited.into(),
            bytes_deleted: s.bytes_deleted.into(),
            todos_created: s.todos_created.into(),
            todos_completed: s.todos_completed.into(),
            todos_in_progress: s.todos_in_progress.into(),
            todo_writes: s.todo_writes.into(),
            todo_reads: s.todo_reads.into(),
            code_lines: s.code_lines.into(),
            docs_lines: s.docs_lines.into(),
            data_lines: s.data_lines.into(),
            media_lines: s.media_lines.into(),
            config_lines: s.config_lines.into(),
            other_lines: s.other_lines.into(),
        }
    }
}

/// Convert Application enum to u8 for serialization
pub(crate) fn application_to_u8(app: &Application) -> u8 {
    match app {
        Application::ClaudeCode => 0,
        Application::GeminiCli => 1,
        Application::QwenCode => 2,
        Application::CodexCli => 3,
        Application::Cline => 4,
        Application::RooCode => 5,
        Application::KiloCode => 6,
        Application::Copilot => 7,
        Application::OpenCode => 8,
    }
}

/// Convert u8 back to Application enum, with ClaudeCode as fallback
pub(crate) fn u8_to_application(v: u8) -> Application {
    match v {
        0 => Application::ClaudeCode,
        1 => Application::GeminiCli,
        2 => Application::QwenCode,
        3 => Application::CodexCli,
        4 => Application::Cline,
        5 => Application::RooCode,
        6 => Application::KiloCode,
        7 => Application::Copilot,
        8 => Application::OpenCode,
        _ => Application::ClaudeCode, // Fallback for unknown values
    }
}

/// Convert MessageRole enum to u8 for serialization
pub(crate) fn role_to_u8(role: &MessageRole) -> u8 {
    match role {
        MessageRole::User => 0,
        MessageRole::Assistant => 1,
    }
}

/// Convert u8 back to MessageRole enum, with Assistant as fallback
pub(crate) fn u8_to_role(v: u8) -> MessageRole {
    match v {
        0 => MessageRole::User,
        _ => MessageRole::Assistant,
    }
}

impl From<&DailyStats> for CacheDailyStats {
    fn from(d: &DailyStats) -> Self {
        Self {
            date: d.date.clone(),
            user_messages: d.user_messages,
            ai_messages: d.ai_messages,
            conversations: d.conversations,
            models: d.models.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            stats: CacheStats_::from(&d.stats),
        }
    }
}

impl From<&CacheDailyStats> for DailyStats {
    fn from(d: &CacheDailyStats) -> Self {
        Self {
            date: d.date.clone(),
            user_messages: d.user_messages,
            ai_messages: d.ai_messages,
            conversations: d.conversations,
            models: d.models.iter().cloned().collect(),
            stats: Stats::from(&d.stats),
        }
    }
}

impl From<&ArchivedCacheDailyStats> for DailyStats {
    fn from(d: &ArchivedCacheDailyStats) -> Self {
        Self {
            date: d.date.to_string(),
            user_messages: d.user_messages.into(),
            ai_messages: d.ai_messages.into(),
            conversations: d.conversations.into(),
            models: d
                .models
                .iter()
                .map(|entry| (entry.0.to_string(), u32::from(entry.1)))
                .collect(),
            stats: Stats::from(&d.stats),
        }
    }
}

impl From<&ConversationMessage> for CacheMessage {
    fn from(m: &ConversationMessage) -> Self {
        Self {
            application: application_to_u8(&m.application),
            date_timestamp: m.date.timestamp(),
            project_hash: m.project_hash.clone(),
            conversation_hash: m.conversation_hash.clone(),
            local_hash: m.local_hash.clone(),
            global_hash: m.global_hash.clone(),
            model: m.model.clone(),
            stats: CacheStats_::from(&m.stats),
            role: role_to_u8(&m.role),
            uuid: m.uuid.clone(),
            session_name: m.session_name.clone(),
        }
    }
}

impl From<&CacheMessage> for ConversationMessage {
    fn from(m: &CacheMessage) -> Self {
        use chrono::TimeZone;
        Self {
            application: u8_to_application(m.application),
            // Use single() to handle invalid timestamps gracefully (falls back to Unix epoch)
            date: chrono::Utc
                .timestamp_opt(m.date_timestamp, 0)
                .single()
                .unwrap_or_else(|| chrono::Utc.timestamp_opt(0, 0).single().unwrap()),
            project_hash: m.project_hash.clone(),
            conversation_hash: m.conversation_hash.clone(),
            local_hash: m.local_hash.clone(),
            global_hash: m.global_hash.clone(),
            model: m.model.clone(),
            stats: Stats::from(&m.stats),
            role: u8_to_role(m.role),
            uuid: m.uuid.clone(),
            session_name: m.session_name.clone(),
        }
    }
}

impl From<&ArchivedCacheMessage> for ConversationMessage {
    fn from(m: &ArchivedCacheMessage) -> Self {
        use chrono::TimeZone;
        Self {
            application: u8_to_application(m.application),
            // Use single() to handle invalid timestamps gracefully (falls back to Unix epoch)
            date: chrono::Utc
                .timestamp_opt(i64::from(m.date_timestamp), 0)
                .single()
                .unwrap_or_else(|| chrono::Utc.timestamp_opt(0, 0).single().unwrap()),
            project_hash: m.project_hash.to_string(),
            conversation_hash: m.conversation_hash.to_string(),
            local_hash: m.local_hash.as_ref().map(|s| s.to_string()),
            global_hash: m.global_hash.to_string(),
            model: m.model.as_ref().map(|s| s.to_string()),
            stats: Stats::from(&m.stats),
            role: u8_to_role(m.role),
            uuid: m.uuid.as_ref().map(|s| s.to_string()),
            session_name: m.session_name.as_ref().map(|s| s.to_string()),
        }
    }
}

// =============================================================================
// Repository Implementation
// =============================================================================

const CACHE_VERSION: u32 = 2;

/// High-performance cache repository using rkyv serialization
pub struct MmapCacheRepository {
    /// Deserialized index (hot data) - loaded once at startup
    index: RwLock<CacheIndex>,
    /// Fast O(1) lookup index - maps keys to entries
    index_map: RwLock<HashMap<FileCacheKey, CacheMetaEntry>>,
    /// In-memory dirty entries awaiting persist
    dirty_entries: RwLock<HashMap<FileCacheKey, CacheMetaEntry>>,
    /// In-memory dirty messages awaiting persist
    dirty_messages: RwLock<HashMap<FileCacheKey, Vec<CacheMessage>>>,
    /// Entries marked for removal
    removed_keys: RwLock<Vec<FileCacheKey>>,
    /// Path to cache directory
    cache_dir: PathBuf,
}

impl MmapCacheRepository {
    /// Create an empty in-memory cache (no persistence)
    /// Used as fallback when disk cache cannot be opened
    pub fn empty() -> Self {
        Self {
            index: RwLock::new(CacheIndex {
                version: CACHE_VERSION,
                entries: Vec::new(),
            }),
            index_map: RwLock::new(HashMap::new()),
            dirty_entries: RwLock::new(HashMap::new()),
            dirty_messages: RwLock::new(HashMap::new()),
            removed_keys: RwLock::new(Vec::new()),
            cache_dir: PathBuf::new(), // Empty path - persist() will be a no-op
        }
    }

    /// Open or create the cache repository
    pub fn open() -> Result<Self> {
        let cache_dir = Self::cache_dir()?;
        fs::create_dir_all(&cache_dir)?;

        let index_path = cache_dir.join("cache.meta");
        let index = if index_path.exists() {
            // Load and deserialize index (fast - rkyv is 10-100x faster than JSON)
            let data = fs::read(&index_path)?;
            match rkyv::from_bytes::<CacheIndex, BoxedError>(&data) {
                Ok(loaded_index) => {
                    if loaded_index.version != CACHE_VERSION {
                        // Version mismatch - clear old cache and start fresh
                        // This handles migration from v1 (without last_parsed_offset) to v2
                        eprintln!(
                            "Cache version {} -> {}: clearing old cache",
                            loaded_index.version, CACHE_VERSION
                        );
                        CacheIndex {
                            version: CACHE_VERSION,
                            entries: Vec::new(),
                        }
                    } else {
                        loaded_index
                    }
                }
                Err(_) => {
                    // Corrupted or incompatible cache - start fresh
                    CacheIndex {
                        version: CACHE_VERSION,
                        entries: Vec::new(),
                    }
                }
            }
        } else {
            CacheIndex {
                version: CACHE_VERSION,
                entries: Vec::new(),
            }
        };

        // Build O(1) lookup HashMap from Vec for fast find_entry
        let index_map: HashMap<FileCacheKey, CacheMetaEntry> = index
            .entries
            .iter()
            .map(|e| {
                let key = FileCacheKey {
                    analyzer_name: e.analyzer_name.clone(),
                    file_path: PathBuf::from(&e.file_path),
                };
                (key, e.clone())
            })
            .collect();

        Ok(Self {
            index: RwLock::new(index),
            index_map: RwLock::new(index_map),
            dirty_entries: RwLock::new(HashMap::new()),
            dirty_messages: RwLock::new(HashMap::new()),
            removed_keys: RwLock::new(Vec::new()),
            cache_dir,
        })
    }

    fn cache_dir() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        Ok(home.join(".splitrail"))
    }

    fn index_path(&self) -> PathBuf {
        self.cache_dir.join("cache.meta")
    }

    fn messages_dir(&self) -> PathBuf {
        self.cache_dir.join("messages")
    }

    fn message_file_path(&self, key: &FileCacheKey) -> PathBuf {
        let hash = crate::utils::fast_hash(&format!(
            "{}:{}",
            key.analyzer_name,
            key.file_path.display()
        ));
        self.messages_dir().join(format!("{}.msg", hash))
    }

    /// Find entry in index - O(1) HashMap lookup
    fn find_entry(&self, key: &FileCacheKey) -> Option<CacheMetaEntry> {
        self.index_map.read().get(key).cloned()
    }

    /// Check if a file's cache entry is stale
    pub fn is_stale(&self, key: &FileCacheKey, current_meta: &FileMetadata) -> bool {
        // Check dirty entries first
        if let Some(entry) = self.dirty_entries.read().get(key) {
            return entry.file_size != current_meta.size
                || entry.file_modified != current_meta.modified;
        }

        // Check index
        if let Some(entry) = self.find_entry(key) {
            return entry.file_size != current_meta.size
                || entry.file_modified != current_meta.modified;
        }

        true // Not in cache = stale
    }

    /// Get pre-aggregated daily contributions for a file
    pub fn get_daily_contributions(&self, key: &FileCacheKey) -> Option<Vec<(String, DailyStats)>> {
        // Check dirty entries first
        if let Some(entry) = self.dirty_entries.read().get(key) {
            return Some(
                entry
                    .daily_contributions
                    .iter()
                    .map(|(date, stats)| (date.clone(), DailyStats::from(stats)))
                    .collect(),
            );
        }

        // Check index_map (O(1) lookup)
        self.find_entry(key).map(|entry| {
            entry
                .daily_contributions
                .iter()
                .map(|(date, stats)| (date.clone(), DailyStats::from(stats)))
                .collect()
        })
    }

    /// Lazy load messages from cold storage
    pub fn load_messages(&self, key: &FileCacheKey) -> Result<Vec<ConversationMessage>> {
        // Check dirty messages first
        if let Some(messages) = self.dirty_messages.read().get(key) {
            return Ok(messages.iter().map(ConversationMessage::from).collect());
        }

        // Load from disk using rkyv deserialization
        let msg_path = self.message_file_path(key);
        if !msg_path.exists() {
            return Ok(Vec::new());
        }

        let data = fs::read(&msg_path)?;
        let container: CacheMessages = rkyv::from_bytes::<CacheMessages, BoxedError>(&data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize messages: {}", e))?;
        Ok(container
            .messages
            .iter()
            .map(ConversationMessage::from)
            .collect())
    }

    /// Insert or update a cache entry
    pub fn insert(&self, key: FileCacheKey, entry: FileCacheEntry) {
        // Convert to cache format
        let meta_entry = CacheMetaEntry {
            analyzer_name: key.analyzer_name.clone(),
            file_path: key.file_path.to_string_lossy().to_string(),
            file_size: entry.metadata.size,
            file_modified: entry.metadata.modified,
            last_parsed_offset: entry.metadata.last_parsed_offset,
            daily_contributions: entry
                .daily_contributions
                .iter()
                .map(|(date, stats)| (date.clone(), CacheDailyStats::from(stats)))
                .collect(),
            message_count: entry.messages.len() as u32,
            cached_model: entry.cached_model.clone(),
        };

        let cache_messages: Vec<CacheMessage> =
            entry.messages.iter().map(CacheMessage::from).collect();

        // Store in dirty maps
        self.dirty_entries.write().insert(key.clone(), meta_entry);
        self.dirty_messages.write().insert(key, cache_messages);
    }

    /// Remove an entry from the cache
    pub fn remove(&self, key: &FileCacheKey) {
        self.dirty_entries.write().remove(key);
        self.dirty_messages.write().remove(key);
        self.removed_keys.write().push(key.clone());
    }

    /// Get a cache entry if valid (metadata matches)
    pub fn get(&self, key: &FileCacheKey, current_path: &Path) -> Option<FileCacheEntry> {
        let current_meta = FileMetadata::from_path(current_path).ok()?;

        if self.is_stale(key, &current_meta) {
            return None;
        }

        // Load full entry
        let daily_contributions = self.get_daily_contributions(key)?;
        let messages = self.load_messages(key).ok()?;

        // Get cached last_parsed_offset and cached_model (current_meta has 0 from from_path())
        let (last_parsed_offset, cached_model) =
            if let Some(entry) = self.dirty_entries.read().get(key) {
                (entry.last_parsed_offset, entry.cached_model.clone())
            } else if let Some(entry) = self.find_entry(key) {
                (entry.last_parsed_offset, entry.cached_model.clone())
            } else {
                (current_meta.size, None) // Fallback: assume fully parsed
            };

        Some(FileCacheEntry {
            metadata: FileMetadata {
                size: current_meta.size,
                modified: current_meta.modified,
                last_parsed_offset,
            },
            messages,
            daily_contributions: daily_contributions.into_iter().collect(),
            cached_model,
        })
    }

    /// Get cache entry without validation
    pub fn get_unchecked(&self, key: &FileCacheKey) -> Option<FileCacheEntry> {
        let daily_contributions = self.get_daily_contributions(key)?;
        let messages = self.load_messages(key).ok()?;

        // Get metadata from cache
        let (size, modified, last_parsed_offset, cached_model) =
            if let Some(entry) = self.dirty_entries.read().get(key) {
                (
                    entry.file_size,
                    entry.file_modified,
                    entry.last_parsed_offset,
                    entry.cached_model.clone(),
                )
            } else if let Some(entry) = self.find_entry(key) {
                (
                    entry.file_size,
                    entry.file_modified,
                    entry.last_parsed_offset,
                    entry.cached_model.clone(),
                )
            } else {
                return None;
            };

        Some(FileCacheEntry {
            metadata: FileMetadata {
                size,
                modified,
                last_parsed_offset,
            },
            messages,
            daily_contributions: daily_contributions.into_iter().collect(),
            cached_model,
        })
    }

    /// Get all entries for an analyzer
    pub fn get_analyzer_entries(&self, analyzer_name: &str) -> Vec<(PathBuf, FileCacheEntry)> {
        let mut entries = Vec::new();

        // From dirty entries
        for (key, _) in self.dirty_entries.read().iter() {
            if key.analyzer_name == analyzer_name
                && let Some(entry) = self.get_unchecked(key)
            {
                entries.push((key.file_path.clone(), entry));
            }
        }

        // From index
        let index = self.index.read();
        for cache_entry in index.entries.iter() {
            if cache_entry.analyzer_name == analyzer_name {
                let key = FileCacheKey {
                    analyzer_name: analyzer_name.to_string(),
                    file_path: PathBuf::from(&cache_entry.file_path),
                };
                // Skip if already in dirty entries
                if !self.dirty_entries.read().contains_key(&key)
                    && let Some(entry) = self.get_unchecked(&key)
                {
                    entries.push((key.file_path, entry));
                }
            }
        }

        entries
    }

    /// Remove entries for deleted files
    pub fn prune_deleted_files(&self, analyzer_name: &str, current_paths: &[PathBuf]) {
        let current_set: std::collections::HashSet<_> = current_paths.iter().collect();

        // Find keys to remove
        let mut keys_to_remove = Vec::new();

        // Check dirty entries
        for key in self.dirty_entries.read().keys() {
            if key.analyzer_name == analyzer_name && !current_set.contains(&key.file_path) {
                keys_to_remove.push(key.clone());
            }
        }

        // Check index
        let index = self.index.read();
        for entry in index.entries.iter() {
            if entry.analyzer_name == analyzer_name {
                let path = PathBuf::from(&entry.file_path);
                if !current_set.contains(&path) {
                    keys_to_remove.push(FileCacheKey {
                        analyzer_name: analyzer_name.to_string(),
                        file_path: path,
                    });
                }
            }
        }
        drop(index);

        // Remove
        for key in keys_to_remove {
            self.remove(&key);
        }
    }

    /// Invalidate all entries for an analyzer
    pub fn invalidate_analyzer(&self, analyzer_name: &str) {
        // Remove from dirty entries
        self.dirty_entries
            .write()
            .retain(|k, _| k.analyzer_name != analyzer_name);
        self.dirty_messages
            .write()
            .retain(|k, _| k.analyzer_name != analyzer_name);

        // Mark index entries for removal
        let index = self.index.read();
        for entry in index.entries.iter() {
            if entry.analyzer_name == analyzer_name {
                self.removed_keys.write().push(FileCacheKey {
                    analyzer_name: analyzer_name.to_string(),
                    file_path: PathBuf::from(&entry.file_path),
                });
            }
        }
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        let mut total_entries = 0;
        let mut by_analyzer: HashMap<String, usize> = HashMap::new();

        // Count dirty entries
        for key in self.dirty_entries.read().keys() {
            total_entries += 1;
            *by_analyzer.entry(key.analyzer_name.clone()).or_default() += 1;
        }

        // Count index entries (excluding those in dirty)
        let index = self.index.read();
        let dirty = self.dirty_entries.read();
        for entry in index.entries.iter() {
            let key = FileCacheKey {
                analyzer_name: entry.analyzer_name.clone(),
                file_path: PathBuf::from(&entry.file_path),
            };
            if !dirty.contains_key(&key) {
                total_entries += 1;
                *by_analyzer.entry(entry.analyzer_name.clone()).or_default() += 1;
            }
        }

        let db_size = fs::metadata(self.index_path())
            .map(|m| m.len())
            .unwrap_or(0);

        CacheStats {
            total_entries,
            by_analyzer,
            db_size,
        }
    }

    /// Persist all dirty data to disk
    pub fn persist(&self) -> Result<()> {
        // Skip persistence for in-memory fallback cache
        if self.cache_dir.as_os_str().is_empty() {
            return Ok(());
        }

        let dirty_entries = self.dirty_entries.read();
        let dirty_messages = self.dirty_messages.read();
        let removed_keys = self.removed_keys.read();

        if dirty_entries.is_empty() && dirty_messages.is_empty() && removed_keys.is_empty() {
            return Ok(());
        }

        // Build new index from existing + dirty - removed
        let mut new_entries: Vec<CacheMetaEntry> = Vec::new();

        // Add existing entries (not in dirty or removed)
        let current_index = self.index.read();
        for entry in current_index.entries.iter() {
            let key = FileCacheKey {
                analyzer_name: entry.analyzer_name.clone(),
                file_path: PathBuf::from(&entry.file_path),
            };
            if !dirty_entries.contains_key(&key) && !removed_keys.contains(&key) {
                new_entries.push(entry.clone());
            }
        }
        drop(current_index);

        // Add dirty entries
        for (_, entry) in dirty_entries.iter() {
            new_entries.push(entry.clone());
        }

        // Write new index
        let new_index = CacheIndex {
            version: CACHE_VERSION,
            entries: new_entries.clone(),
        };

        let bytes = rkyv::to_bytes::<BoxedError>(&new_index)
            .map_err(|e| anyhow::anyhow!("Failed to serialize cache index: {}", e))?;

        // Atomic write via temp file + rename
        let temp_path = self.index_path().with_extension("tmp");
        let mut file = File::create(&temp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp_path, self.index_path())?;

        // Write dirty messages to separate files
        fs::create_dir_all(self.messages_dir())?;
        for (key, messages) in dirty_messages.iter() {
            let msg_container = CacheMessages {
                messages: messages.clone(),
            };
            let bytes = rkyv::to_bytes::<BoxedError>(&msg_container)
                .map_err(|e| anyhow::anyhow!("Failed to serialize messages: {}", e))?;

            let msg_path = self.message_file_path(key);
            let temp_path = msg_path.with_extension("tmp");
            let mut file = File::create(&temp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp_path, &msg_path)?;
        }

        // Delete message files for removed entries
        for key in removed_keys.iter() {
            let msg_path = self.message_file_path(key);
            let _ = fs::remove_file(msg_path);
        }

        // Update in-memory index
        drop(dirty_entries);
        drop(dirty_messages);
        drop(removed_keys);

        // Rebuild index_map from new_entries
        let new_index_map: HashMap<FileCacheKey, CacheMetaEntry> = new_entries
            .iter()
            .map(|e| {
                let key = FileCacheKey {
                    analyzer_name: e.analyzer_name.clone(),
                    file_path: PathBuf::from(&e.file_path),
                };
                (key, e.clone())
            })
            .collect();

        *self.index.write() = CacheIndex {
            version: CACHE_VERSION,
            entries: new_entries,
        };
        *self.index_map.write() = new_index_map;

        // Clear dirty state
        self.dirty_entries.write().clear();
        self.dirty_messages.write().clear();
        self.removed_keys.write().clear();

        Ok(())
    }

    /// Clear all cache data
    pub fn clear(&self) -> Result<()> {
        self.dirty_entries.write().clear();
        self.dirty_messages.write().clear();
        self.removed_keys.write().clear();
        *self.index.write() = CacheIndex {
            version: CACHE_VERSION,
            entries: Vec::new(),
        };
        self.index_map.write().clear();

        // Delete files
        let _ = fs::remove_file(self.index_path());
        let _ = fs::remove_dir_all(self.messages_dir());

        // Also delete snapshots (hot/cold caches)
        let snapshots_dir = self.cache_dir.join("snapshots");
        if snapshots_dir.exists() {
            let _ = fs::remove_dir_all(&snapshots_dir);
        }

        Ok(())
    }
}
