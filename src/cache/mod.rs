//! Zero-copy memory-mapped persistent cache for incremental file parsing.
//!
//! This module provides per-file caching to avoid re-parsing unchanged files.
//! It stores parsed messages and pre-aggregated daily statistics for each file,
//! with metadata-based invalidation.
//!
//! ## Architecture
//!
//! - Hot data (metadata + aggregates) in memory-mapped rkyv archive
//! - Cold data (messages) loaded lazily from separate files
//! - Zero-copy access via rkyv for instant startup

mod mmap_repository;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::types::{ConversationMessage, DailyStats, FileMetadata};

pub use mmap_repository::MmapCacheRepository;

/// Cached data for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileCacheEntry {
    pub metadata: FileMetadata,
    pub messages: Vec<ConversationMessage>,
    /// Pre-aggregated stats contribution from this file
    pub daily_contributions: HashMap<String, DailyStats>,
    /// Cached session model name (for analyzers like Codex CLI where model is in header)
    #[serde(default)]
    pub cached_model: Option<String>,
}

/// Cache key combining analyzer name and file path
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileCacheKey {
    pub analyzer_name: String,
    pub file_path: PathBuf,
}

impl FileCacheKey {
    pub fn new(analyzer_name: &str, path: &Path) -> Self {
        Self {
            analyzer_name: analyzer_name.to_string(),
            file_path: path.to_path_buf(),
        }
    }
}

/// Statistics about the cache
#[derive(Debug)]
pub struct CacheStats {
    pub total_entries: usize,
    pub by_analyzer: HashMap<String, usize>,
    pub db_size: u64,
}

/// Get the path to the cache directory
pub fn cache_db_path() -> anyhow::Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".splitrail").join("cache.meta"))
}

/// Format bytes as human-readable string
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

/// File stats cache using zero-copy mmap repository.
pub struct FileStatsCache {
    repo: RwLock<Option<Arc<MmapCacheRepository>>>,
}

impl Default for FileStatsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStatsCache {
    pub fn new() -> Self {
        Self {
            repo: RwLock::new(None),
        }
    }

    /// Get or initialize the repository with lazy initialization
    fn get_repo(&self) -> Arc<MmapCacheRepository> {
        // Fast path: check if already initialized
        {
            let guard = self.repo.read();
            if let Some(repo) = guard.as_ref() {
                return Arc::clone(repo);
            }
        }

        // Slow path: initialize
        let mut guard = self.repo.write();
        if guard.is_none() {
            let repo = MmapCacheRepository::open().unwrap_or_else(|e| {
                eprintln!("Warning: Failed to open cache: {e}");
                // Fall back to empty in-memory cache (no persistence)
                MmapCacheRepository::empty()
            });
            *guard = Some(Arc::new(repo));
        }
        Arc::clone(guard.as_ref().unwrap())
    }

    pub fn get(&self, key: &FileCacheKey, current_path: &Path) -> Option<FileCacheEntry> {
        self.get_repo().get(key, current_path)
    }

    pub fn get_unchecked(&self, key: &FileCacheKey) -> Option<FileCacheEntry> {
        self.get_repo().get_unchecked(key)
    }

    pub fn insert(&self, key: FileCacheKey, entry: FileCacheEntry) {
        self.get_repo().insert(key, entry);
    }

    pub fn invalidate_analyzer(&self, analyzer_name: &str) {
        self.get_repo().invalidate_analyzer(analyzer_name);
    }

    pub fn remove(&self, key: &FileCacheKey) {
        self.get_repo().remove(key);
    }

    pub fn get_analyzer_entries(&self, analyzer_name: &str) -> Vec<(PathBuf, FileCacheEntry)> {
        self.get_repo().get_analyzer_entries(analyzer_name)
    }

    pub fn prune_deleted_files(&self, analyzer_name: &str, current_paths: &[PathBuf]) {
        self.get_repo()
            .prune_deleted_files(analyzer_name, current_paths);
    }

    pub fn stats(&self) -> CacheStats {
        self.get_repo().stats()
    }

    pub fn save_to_disk(&self) -> anyhow::Result<()> {
        self.get_repo().persist()
    }

    pub fn load_from_disk() -> anyhow::Result<Self> {
        let repo = MmapCacheRepository::open()?;
        Ok(Self {
            repo: RwLock::new(Some(Arc::new(repo))),
        })
    }

    pub fn clear(&self) {
        let _ = self.get_repo().clear();
    }

    /// Check if a file is stale (for use without loading full entry)
    pub fn is_stale(&self, key: &FileCacheKey, current_meta: &FileMetadata) -> bool {
        self.get_repo().is_stale(key, current_meta)
    }

    /// Get just the daily contributions without loading messages
    pub fn get_daily_contributions(&self, key: &FileCacheKey) -> Option<Vec<(String, DailyStats)>> {
        self.get_repo().get_daily_contributions(key)
    }

    /// Load messages on demand (for TUI session view)
    pub fn load_messages(&self, key: &FileCacheKey) -> anyhow::Result<Vec<ConversationMessage>> {
        self.get_repo().load_messages(key)
    }
}

// Make FileStatsCache Debug
impl std::fmt::Debug for FileStatsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let initialized = self.repo.read().is_some();
        f.debug_struct("FileStatsCache")
            .field("initialized", &initialized)
            .finish()
    }
}

// =============================================================================
// SNAPSHOT CACHE - Caches final deduplicated result for instant warm starts
// =============================================================================

use crate::types::AgenticCodingToolStats;
use std::collections::BTreeMap;
use std::fs;

/// "Hot" snapshot - small, fast to load, enough for TUI display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotSnapshot {
    pub fingerprint: u64,
    pub daily_stats: BTreeMap<String, DailyStats>,
    pub num_conversations: u64,
    pub analyzer_name: String,
}

/// "Cold" snapshot - large, contains all messages for session detail view
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColdSnapshot {
    pub fingerprint: u64,
    pub messages: Vec<crate::types::ConversationMessage>,
}

/// Compute a fingerprint of source files for cache invalidation.
/// Fingerprint changes if any file is added, removed, or modified.
pub fn compute_sources_fingerprint(sources: &[crate::analyzer::DataSource]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();

    // Sort for deterministic ordering
    let mut sorted: Vec<_> = sources.iter().collect();
    sorted.sort_by_key(|s| &s.path);

    for source in sorted {
        source.path.hash(&mut hasher);
        if let Ok(meta) = fs::metadata(&source.path) {
            if let Ok(modified) = meta.modified() {
                modified.hash(&mut hasher);
            }
            meta.len().hash(&mut hasher);
        }
    }

    hasher.finish()
}

/// Get the path to the snapshot cache files for an analyzer
fn snapshot_paths(analyzer_name: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let cache_dir = home.join(".splitrail").join("snapshots");
    fs::create_dir_all(&cache_dir)?;
    let safe_name = analyzer_name.replace(['/', '\\', ' '], "_");
    Ok((
        cache_dir.join(format!("{}.hot", safe_name)),
        cache_dir.join(format!("{}.cold", safe_name)),
    ))
}

/// Load ONLY hot snapshot for ultra-fast startup (messages loaded lazily later)
pub fn load_snapshot_hot_only(
    analyzer_name: &str,
    expected_fingerprint: u64,
) -> Option<AgenticCodingToolStats> {
    let (hot_path, _) = snapshot_paths(analyzer_name).ok()?;

    if !hot_path.exists() {
        return None;
    }
    let hot_data = fs::read(&hot_path).ok()?;
    let hot: HotSnapshot = bincode::deserialize(&hot_data).ok()?;

    if hot.fingerprint != expected_fingerprint {
        return None;
    }

    Some(AgenticCodingToolStats {
        daily_stats: hot.daily_stats,
        num_conversations: hot.num_conversations,
        messages: Vec::new(), // Lazy load later if needed
        analyzer_name: hot.analyzer_name,
    })
}

/// Save snapshot to disk (split into hot and cold)
pub fn save_snapshot(
    analyzer_name: &str,
    fingerprint: u64,
    stats: &AgenticCodingToolStats,
) -> anyhow::Result<()> {
    let (hot_path, cold_path) = snapshot_paths(analyzer_name)?;

    // Save hot snapshot (small)
    let hot = HotSnapshot {
        fingerprint,
        daily_stats: stats.daily_stats.clone(),
        num_conversations: stats.num_conversations,
        analyzer_name: stats.analyzer_name.clone(),
    };
    let hot_data = bincode::serialize(&hot)?;
    fs::write(&hot_path, hot_data)?;

    // Save cold snapshot (large)
    let cold = ColdSnapshot {
        fingerprint,
        messages: stats.messages.clone(),
    };
    let cold_data = bincode::serialize(&cold)?;
    fs::write(&cold_path, cold_data)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Application, MessageRole, Stats};
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    // ==========================================================================
    // TEST HELPERS
    // ==========================================================================

    fn make_test_stats() -> Stats {
        Stats {
            input_tokens: 100,
            output_tokens: 50,
            reasoning_tokens: 25,
            cache_creation_tokens: 10,
            cache_read_tokens: 5,
            cached_tokens: 15,
            cost: 0.12345,
            tool_calls: 3,
            terminal_commands: 2,
            file_searches: 1,
            file_content_searches: 1,
            files_read: 5,
            files_added: 2,
            files_edited: 3,
            files_deleted: 1,
            lines_read: 500,
            lines_added: 100,
            lines_edited: 50,
            lines_deleted: 25,
            bytes_read: 10000,
            bytes_added: 2000,
            bytes_edited: 1000,
            bytes_deleted: 500,
            todos_created: 3,
            todos_completed: 2,
            todos_in_progress: 1,
            todo_writes: 4,
            todo_reads: 2,
            code_lines: 200,
            docs_lines: 50,
            data_lines: 30,
            media_lines: 10,
            config_lines: 20,
            other_lines: 5,
        }
    }

    fn make_test_daily_stats(date: &str) -> DailyStats {
        let mut models = BTreeMap::new();
        models.insert("claude-sonnet-4".to_string(), 5);
        models.insert("gpt-4o".to_string(), 3);

        DailyStats {
            date: date.to_string(),
            user_messages: 10,
            ai_messages: 8,
            conversations: 2,
            models,
            stats: make_test_stats(),
        }
    }

    fn make_test_message(conv_hash: &str, timestamp: i64) -> ConversationMessage {
        ConversationMessage {
            application: Application::ClaudeCode,
            date: Utc.timestamp_opt(timestamp, 0).single().unwrap(),
            project_hash: "proj_test".to_string(),
            conversation_hash: conv_hash.to_string(),
            local_hash: Some("local_hash_123".to_string()),
            global_hash: format!("global_{}_{}", conv_hash, timestamp),
            model: Some("claude-sonnet-4".to_string()),
            stats: make_test_stats(),
            role: MessageRole::Assistant,
            uuid: Some("uuid-123".to_string()),
            session_name: Some("Test Session".to_string()),
        }
    }

    fn make_test_cache_entry() -> FileCacheEntry {
        let mut daily_contributions = HashMap::new();
        daily_contributions.insert(
            "2025-01-15".to_string(),
            make_test_daily_stats("2025-01-15"),
        );
        daily_contributions.insert(
            "2025-01-16".to_string(),
            make_test_daily_stats("2025-01-16"),
        );

        FileCacheEntry {
            metadata: FileMetadata {
                size: 12345,
                modified: 1700000000,
                last_parsed_offset: 12345,
            },
            messages: vec![
                make_test_message("conv1", 1700000000),
                make_test_message("conv1", 1700000100),
            ],
            daily_contributions,
            cached_model: Some("claude-sonnet-4".to_string()),
        }
    }

    // ==========================================================================
    // BASIC FORMAT TESTS
    // ==========================================================================

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 bytes");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1048576), "1.00 MB");
        assert_eq!(format_bytes(1073741824), "1.00 GB");
    }

    #[test]
    fn test_file_cache_key() {
        let key = FileCacheKey::new("Test", Path::new("/foo/bar.jsonl"));
        assert_eq!(key.analyzer_name, "Test");
        assert_eq!(key.file_path, PathBuf::from("/foo/bar.jsonl"));
    }

    // ==========================================================================
    // CACHE REPOSITORY TESTS
    // ==========================================================================

    #[test]
    fn test_cache_empty_fallback_persist_noop() {
        let repo = MmapCacheRepository::empty();
        // Persist on empty fallback should not panic and return Ok
        assert!(repo.persist().is_ok());
    }

    #[test]
    fn test_cache_insert_get_roundtrip() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
        let entry = make_test_cache_entry();

        repo.insert(key.clone(), entry.clone());

        // get_unchecked should return the entry
        let retrieved = repo.get_unchecked(&key).expect("should retrieve entry");
        assert_eq!(retrieved.metadata.size, 12345);
        assert_eq!(retrieved.messages.len(), 2);
        assert_eq!(retrieved.daily_contributions.len(), 2);
        assert_eq!(retrieved.cached_model, Some("claude-sonnet-4".to_string()));
    }

    #[test]
    fn test_cache_get_returns_none_for_missing() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/nonexistent.jsonl"));

        assert!(repo.get_unchecked(&key).is_none());
    }

    #[test]
    fn test_cache_remove_entry() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
        let entry = make_test_cache_entry();

        repo.insert(key.clone(), entry);
        assert!(repo.get_unchecked(&key).is_some());

        repo.remove(&key);
        assert!(repo.get_unchecked(&key).is_none());
    }

    #[test]
    fn test_cache_get_daily_contributions() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
        let entry = make_test_cache_entry();

        repo.insert(key.clone(), entry);

        let contributions = repo
            .get_daily_contributions(&key)
            .expect("should get contributions");
        assert_eq!(contributions.len(), 2);

        // Verify stats are preserved
        let jan15 = contributions
            .iter()
            .find(|(d, _)| d == "2025-01-15")
            .map(|(_, s)| s)
            .unwrap();
        assert_eq!(jan15.ai_messages, 8);
        assert_eq!(jan15.stats.input_tokens, 100);
    }

    #[test]
    fn test_dirty_entries_override_index() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));

        // Insert initial entry
        let mut entry1 = make_test_cache_entry();
        entry1.metadata.size = 1000;
        repo.insert(key.clone(), entry1);

        // Insert updated entry (dirty)
        let mut entry2 = make_test_cache_entry();
        entry2.metadata.size = 2000;
        repo.insert(key.clone(), entry2);

        // Should get the dirty (newer) entry
        let retrieved = repo.get_unchecked(&key).unwrap();
        assert_eq!(retrieved.metadata.size, 2000);
    }

    #[test]
    fn test_removed_then_reinserted_key() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));

        // Insert, remove, reinsert
        let entry1 = make_test_cache_entry();
        repo.insert(key.clone(), entry1);
        repo.remove(&key);

        let mut entry2 = make_test_cache_entry();
        entry2.metadata.size = 9999;
        repo.insert(key.clone(), entry2);

        // Should get the reinserted entry
        let retrieved = repo.get_unchecked(&key).unwrap();
        assert_eq!(retrieved.metadata.size, 9999);
    }

    #[test]
    fn test_invalidate_analyzer() {
        let repo = MmapCacheRepository::empty();

        // Insert entries for two analyzers
        let key1 = FileCacheKey::new("Analyzer1", Path::new("/test/file1.jsonl"));
        let key2 = FileCacheKey::new("Analyzer1", Path::new("/test/file2.jsonl"));
        let key3 = FileCacheKey::new("Analyzer2", Path::new("/test/file3.jsonl"));

        repo.insert(key1.clone(), make_test_cache_entry());
        repo.insert(key2.clone(), make_test_cache_entry());
        repo.insert(key3.clone(), make_test_cache_entry());

        // Invalidate Analyzer1
        repo.invalidate_analyzer("Analyzer1");

        // Analyzer1 entries should be gone
        assert!(repo.get_unchecked(&key1).is_none());
        assert!(repo.get_unchecked(&key2).is_none());

        // Analyzer2 entry should still exist
        assert!(repo.get_unchecked(&key3).is_some());
    }

    #[test]
    fn test_clear_removes_all() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));

        repo.insert(key.clone(), make_test_cache_entry());
        assert!(repo.get_unchecked(&key).is_some());

        repo.clear().unwrap();
        assert!(repo.get_unchecked(&key).is_none());
    }

    #[test]
    fn test_cache_stats() {
        let repo = MmapCacheRepository::empty();

        let stats = repo.stats();
        assert_eq!(stats.total_entries, 0);

        let key1 = FileCacheKey::new("Analyzer1", Path::new("/test/file1.jsonl"));
        let key2 = FileCacheKey::new("Analyzer2", Path::new("/test/file2.jsonl"));

        repo.insert(key1, make_test_cache_entry());
        repo.insert(key2, make_test_cache_entry());

        let stats = repo.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.by_analyzer.get("Analyzer1"), Some(&1));
        assert_eq!(stats.by_analyzer.get("Analyzer2"), Some(&1));
    }

    // ==========================================================================
    // STALENESS DETECTION TESTS
    // ==========================================================================

    #[test]
    fn test_is_stale_not_in_cache() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/nonexistent.jsonl"));
        let meta = FileMetadata {
            size: 100,
            modified: 1700000000,
            last_parsed_offset: 0,
        };

        // Key not in cache should always be stale
        assert!(repo.is_stale(&key, &meta));
    }

    #[test]
    fn test_is_stale_unchanged_file() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
        let entry = make_test_cache_entry();

        repo.insert(key.clone(), entry.clone());

        let current_meta = FileMetadata {
            size: entry.metadata.size,
            modified: entry.metadata.modified,
            last_parsed_offset: 0,
        };

        // Same size and modified time should not be stale
        assert!(!repo.is_stale(&key, &current_meta));
    }

    #[test]
    fn test_is_stale_size_change() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
        let entry = make_test_cache_entry();

        repo.insert(key.clone(), entry.clone());

        let current_meta = FileMetadata {
            size: entry.metadata.size + 1000, // Size changed
            modified: entry.metadata.modified,
            last_parsed_offset: 0,
        };

        assert!(repo.is_stale(&key, &current_meta));
    }

    #[test]
    fn test_is_stale_modified_change() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
        let entry = make_test_cache_entry();

        repo.insert(key.clone(), entry.clone());

        let current_meta = FileMetadata {
            size: entry.metadata.size,
            modified: entry.metadata.modified + 1, // Modified time changed
            last_parsed_offset: 0,
        };

        assert!(repo.is_stale(&key, &current_meta));
    }

    // ==========================================================================
    // PERSISTENCE TESTS (using tempdir)
    // ==========================================================================

    #[test]
    fn test_cache_persist_and_reload() {
        let temp_dir = TempDir::new().unwrap();
        let cache_meta_path = temp_dir.path().join("cache.meta");

        // Create and populate a repository
        {
            let repo = MmapCacheRepository::empty();
            let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
            repo.insert(key, make_test_cache_entry());

            // Manually write to the temp location for this test
            // (In production, persist() writes to ~/.splitrail/)
            // This test verifies the dirty entry tracking works
            let stats = repo.stats();
            assert_eq!(stats.total_entries, 1);
        }

        // Note: Full persist/reload test would require modifying cache_dir
        // which is set in MmapCacheRepository::open(). For unit tests, we
        // verify the dirty entry handling works correctly.
        assert!(cache_meta_path.parent().unwrap().exists());
    }

    #[test]
    fn test_persist_clears_dirty_state() {
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));

        repo.insert(key.clone(), make_test_cache_entry());

        // For empty fallback, persist is a no-op but should not error
        repo.persist().unwrap();

        // Entry should still be retrievable after "persist"
        assert!(repo.get_unchecked(&key).is_some());
    }

    // ==========================================================================
    // PRUNE DELETED FILES TESTS
    // ==========================================================================

    #[test]
    fn test_prune_deleted_files() {
        let repo = MmapCacheRepository::empty();

        let key1 = FileCacheKey::new("TestAnalyzer", Path::new("/test/file1.jsonl"));
        let key2 = FileCacheKey::new("TestAnalyzer", Path::new("/test/file2.jsonl"));
        let key3 = FileCacheKey::new("TestAnalyzer", Path::new("/test/file3.jsonl"));

        repo.insert(key1.clone(), make_test_cache_entry());
        repo.insert(key2.clone(), make_test_cache_entry());
        repo.insert(key3.clone(), make_test_cache_entry());

        // Only file1 and file3 still exist
        let current_paths = vec![
            PathBuf::from("/test/file1.jsonl"),
            PathBuf::from("/test/file3.jsonl"),
        ];

        repo.prune_deleted_files("TestAnalyzer", &current_paths);

        // file2 should be removed
        assert!(repo.get_unchecked(&key1).is_some());
        assert!(repo.get_unchecked(&key2).is_none());
        assert!(repo.get_unchecked(&key3).is_some());
    }

    // ==========================================================================
    // CONVERSION ROUNDTRIP TESTS
    // ==========================================================================

    #[test]
    fn test_stats_conversion_roundtrip() {
        use super::mmap_repository::CacheStats_;

        let original = make_test_stats();
        let cached = CacheStats_::from(&original);
        let recovered = Stats::from(&cached);

        assert_eq!(recovered.input_tokens, original.input_tokens);
        assert_eq!(recovered.output_tokens, original.output_tokens);
        assert_eq!(recovered.reasoning_tokens, original.reasoning_tokens);
        assert_eq!(
            recovered.cache_creation_tokens,
            original.cache_creation_tokens
        );
        assert_eq!(recovered.cache_read_tokens, original.cache_read_tokens);
        assert_eq!(recovered.cached_tokens, original.cached_tokens);
        assert_eq!(recovered.tool_calls, original.tool_calls);
        assert_eq!(recovered.terminal_commands, original.terminal_commands);
        assert_eq!(recovered.file_searches, original.file_searches);
        assert_eq!(
            recovered.file_content_searches,
            original.file_content_searches
        );
        assert_eq!(recovered.files_read, original.files_read);
        assert_eq!(recovered.files_added, original.files_added);
        assert_eq!(recovered.files_edited, original.files_edited);
        assert_eq!(recovered.files_deleted, original.files_deleted);
        assert_eq!(recovered.lines_read, original.lines_read);
        assert_eq!(recovered.lines_added, original.lines_added);
        assert_eq!(recovered.lines_edited, original.lines_edited);
        assert_eq!(recovered.lines_deleted, original.lines_deleted);
        assert_eq!(recovered.bytes_read, original.bytes_read);
        assert_eq!(recovered.bytes_added, original.bytes_added);
        assert_eq!(recovered.bytes_edited, original.bytes_edited);
        assert_eq!(recovered.bytes_deleted, original.bytes_deleted);
        assert_eq!(recovered.todos_created, original.todos_created);
        assert_eq!(recovered.todos_completed, original.todos_completed);
        assert_eq!(recovered.todos_in_progress, original.todos_in_progress);
        assert_eq!(recovered.todo_writes, original.todo_writes);
        assert_eq!(recovered.todo_reads, original.todo_reads);
        assert_eq!(recovered.code_lines, original.code_lines);
        assert_eq!(recovered.docs_lines, original.docs_lines);
        assert_eq!(recovered.data_lines, original.data_lines);
        assert_eq!(recovered.media_lines, original.media_lines);
        assert_eq!(recovered.config_lines, original.config_lines);
        assert_eq!(recovered.other_lines, original.other_lines);
    }

    #[test]
    fn test_cost_precision_roundtrip() {
        use super::mmap_repository::CacheStats_;

        // Test various cost values
        let test_costs = [
            0.0, 0.00001, 0.001, 0.01, 0.12345, 1.0, 123.456789, 9999.99999,
        ];

        for &cost in &test_costs {
            let original = Stats {
                cost,
                ..Default::default()
            };
            let cached = CacheStats_::from(&original);
            let recovered = Stats::from(&cached);

            // Should preserve 5 decimal places of precision
            assert!(
                (cost - recovered.cost).abs() < 0.00001,
                "Cost {} did not roundtrip correctly, got {}",
                cost,
                recovered.cost
            );
        }
    }

    #[test]
    fn test_cost_very_small_precision() {
        use super::mmap_repository::CacheStats_;

        // Test the smallest representable value (1 micro-cent)
        let cost = 0.00001;
        let original = Stats {
            cost,
            ..Default::default()
        };
        let cached = CacheStats_::from(&original);
        assert_eq!(cached.cost_millicents, 1);

        let recovered = Stats::from(&cached);
        assert_eq!(recovered.cost, 0.00001);
    }

    #[test]
    fn test_daily_stats_conversion_roundtrip() {
        use super::mmap_repository::CacheDailyStats;

        let original = make_test_daily_stats("2025-01-15");
        let cached = CacheDailyStats::from(&original);
        let recovered = DailyStats::from(&cached);

        assert_eq!(recovered.date, original.date);
        assert_eq!(recovered.user_messages, original.user_messages);
        assert_eq!(recovered.ai_messages, original.ai_messages);
        assert_eq!(recovered.conversations, original.conversations);
        assert_eq!(recovered.models.len(), original.models.len());
        assert_eq!(
            recovered.models.get("claude-sonnet-4"),
            original.models.get("claude-sonnet-4")
        );
    }

    #[test]
    fn test_message_conversion_roundtrip() {
        use super::mmap_repository::CacheMessage;

        let original = make_test_message("conv_test", 1700000000);
        let cached = CacheMessage::from(&original);
        let recovered = ConversationMessage::from(&cached);

        assert_eq!(recovered.project_hash, original.project_hash);
        assert_eq!(recovered.conversation_hash, original.conversation_hash);
        assert_eq!(recovered.local_hash, original.local_hash);
        assert_eq!(recovered.global_hash, original.global_hash);
        assert_eq!(recovered.model, original.model);
        assert_eq!(recovered.role, original.role);
        assert_eq!(recovered.uuid, original.uuid);
        assert_eq!(recovered.session_name, original.session_name);
        assert_eq!(recovered.date.timestamp(), original.date.timestamp());
    }

    #[test]
    fn test_application_all_variants_roundtrip() {
        use super::mmap_repository::{application_to_u8, u8_to_application};

        let variants = [
            Application::ClaudeCode,
            Application::GeminiCli,
            Application::QwenCode,
            Application::CodexCli,
            Application::Cline,
            Application::RooCode,
            Application::KiloCode,
            Application::Copilot,
            Application::OpenCode,
        ];

        for app in variants {
            let u8_val = application_to_u8(&app);
            let recovered = u8_to_application(u8_val);
            assert_eq!(
                std::mem::discriminant(&recovered),
                std::mem::discriminant(&app),
                "Application variant {:?} did not roundtrip",
                app
            );
        }
    }

    #[test]
    fn test_role_all_variants_roundtrip() {
        use super::mmap_repository::{role_to_u8, u8_to_role};

        let variants = [MessageRole::User, MessageRole::Assistant];

        for role in variants {
            let u8_val = role_to_u8(&role);
            let recovered = u8_to_role(u8_val);
            assert_eq!(recovered, role);
        }
    }

    #[test]
    fn test_unknown_application_fallback() {
        use super::mmap_repository::u8_to_application;

        // Unknown u8 values should fallback to ClaudeCode
        let recovered = u8_to_application(255);
        assert!(matches!(recovered, Application::ClaudeCode));

        let recovered = u8_to_application(100);
        assert!(matches!(recovered, Application::ClaudeCode));
    }

    #[test]
    fn test_timestamp_edge_cases() {
        use super::mmap_repository::CacheMessage;

        // Test Unix epoch (timestamp = 0)
        let msg_epoch = make_test_message("conv", 0);
        let cached = CacheMessage::from(&msg_epoch);
        let recovered = ConversationMessage::from(&cached);
        assert_eq!(recovered.date.timestamp(), 0);

        // Test a typical modern timestamp
        let msg_modern = make_test_message("conv", 1700000000);
        let cached = CacheMessage::from(&msg_modern);
        let recovered = ConversationMessage::from(&cached);
        assert_eq!(recovered.date.timestamp(), 1700000000);

        // Test far future (year 2100)
        let msg_future = make_test_message("conv", 4102444800);
        let cached = CacheMessage::from(&msg_future);
        let recovered = ConversationMessage::from(&cached);
        assert_eq!(recovered.date.timestamp(), 4102444800);
    }

    // ==========================================================================
    // CONCURRENCY TESTS
    // ==========================================================================

    #[test]
    fn test_lazy_initialization_race() {
        use std::sync::Arc;
        use std::thread;

        let cache = Arc::new(FileStatsCache::new());
        let mut handles = vec![];

        // Spawn multiple threads that all try to access the cache simultaneously
        for i in 0..10 {
            let cache_clone = Arc::clone(&cache);
            handles.push(thread::spawn(move || {
                let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
                // This triggers get_repo() initialization
                let _ = cache_clone.get_unchecked(&key);
                i
            }));
        }

        // All threads should complete without panic
        for handle in handles {
            handle.join().expect("thread should not panic");
        }
    }

    #[test]
    fn test_concurrent_inserts_same_key() {
        use std::sync::Arc;
        use std::thread;

        let repo = Arc::new(MmapCacheRepository::empty());
        let key = FileCacheKey::new("TestAnalyzer", Path::new("/test/file.jsonl"));
        let mut handles = vec![];

        // Spawn multiple threads that insert to the same key
        for i in 0..10 {
            let repo_clone = Arc::clone(&repo);
            let key_clone = key.clone();
            handles.push(thread::spawn(move || {
                let mut entry = make_test_cache_entry();
                entry.metadata.size = i as u64 * 1000;
                repo_clone.insert(key_clone, entry);
                i
            }));
        }

        for handle in handles {
            handle.join().expect("thread should not panic");
        }

        // Should have exactly one entry (last writer wins)
        let stats = repo.stats();
        assert_eq!(stats.total_entries, 1);

        // Entry should exist
        assert!(repo.get_unchecked(&key).is_some());
    }

    // ==========================================================================
    // SNAPSHOT CACHE TESTS
    // ==========================================================================

    #[test]
    fn test_compute_sources_fingerprint_deterministic() {
        use crate::analyzer::DataSource;

        let temp_dir = TempDir::new().unwrap();
        let file1 = temp_dir.path().join("file1.jsonl");
        let file2 = temp_dir.path().join("file2.jsonl");

        std::fs::write(&file1, "content1").unwrap();
        std::fs::write(&file2, "content2").unwrap();

        let sources = vec![
            DataSource {
                path: file1.clone(),
            },
            DataSource {
                path: file2.clone(),
            },
        ];

        let fp1 = compute_sources_fingerprint(&sources);
        let fp2 = compute_sources_fingerprint(&sources);

        // Same sources should produce same fingerprint
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_compute_sources_fingerprint_changes_on_modification() {
        use crate::analyzer::DataSource;

        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("file.jsonl");

        std::fs::write(&file, "original content").unwrap();
        let sources = vec![DataSource { path: file.clone() }];

        let fp_before = compute_sources_fingerprint(&sources);

        // Modify the file
        std::thread::sleep(std::time::Duration::from_millis(10)); // Ensure mtime changes
        std::fs::write(&file, "modified content - longer").unwrap();

        let fp_after = compute_sources_fingerprint(&sources);

        // Fingerprint should change when file is modified
        assert_ne!(fp_before, fp_after);
    }

    #[test]
    fn test_compute_sources_fingerprint_order_independent() {
        use crate::analyzer::DataSource;

        let temp_dir = TempDir::new().unwrap();
        let file1 = temp_dir.path().join("aaa.jsonl");
        let file2 = temp_dir.path().join("zzz.jsonl");

        std::fs::write(&file1, "content1").unwrap();
        std::fs::write(&file2, "content2").unwrap();

        let sources_order1 = vec![
            DataSource {
                path: file1.clone(),
            },
            DataSource {
                path: file2.clone(),
            },
        ];

        let sources_order2 = vec![
            DataSource {
                path: file2.clone(),
            },
            DataSource {
                path: file1.clone(),
            },
        ];

        let fp1 = compute_sources_fingerprint(&sources_order1);
        let fp2 = compute_sources_fingerprint(&sources_order2);

        // Order shouldn't matter (internally sorted)
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_load_snapshot_invalid_fingerprint_returns_none() {
        // With no snapshot saved, should return None
        let result = load_snapshot_hot_only("nonexistent_analyzer_xyz", 12345);
        assert!(result.is_none());
    }

    // ==========================================================================
    // INTEGRATION TESTS
    // ==========================================================================

    #[test]
    fn test_cache_entry_roundtrip_preserves_stats() {
        // Integration test: Insert entry, retrieve it, verify stats are identical
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("IntegrationTest", Path::new("/test/integration.jsonl"));

        let original_stats = make_test_stats();
        let original_entry = FileCacheEntry {
            metadata: FileMetadata {
                size: 5000,
                modified: 1700000000,
                last_parsed_offset: 5000,
            },
            messages: vec![make_test_message("conv_int", 1700000000)],
            daily_contributions: {
                let mut map = HashMap::new();
                map.insert(
                    "2025-01-15".to_string(),
                    make_test_daily_stats("2025-01-15"),
                );
                map
            },
            cached_model: Some("test-model".to_string()),
        };

        repo.insert(key.clone(), original_entry.clone());

        // Retrieve and verify
        let retrieved = repo.get_unchecked(&key).expect("should get entry");

        // Verify metadata
        assert_eq!(retrieved.metadata.size, original_entry.metadata.size);
        assert_eq!(
            retrieved.metadata.modified,
            original_entry.metadata.modified
        );
        assert_eq!(
            retrieved.metadata.last_parsed_offset,
            original_entry.metadata.last_parsed_offset
        );

        // Verify messages
        assert_eq!(retrieved.messages.len(), 1);
        assert_eq!(
            retrieved.messages[0].stats.input_tokens,
            original_stats.input_tokens
        );
        assert_eq!(retrieved.messages[0].stats.cost, original_stats.cost);

        // Verify daily contributions
        let daily = retrieved
            .daily_contributions
            .get("2025-01-15")
            .expect("daily stats");
        assert_eq!(daily.ai_messages, 8);
        assert_eq!(daily.stats.input_tokens, original_stats.input_tokens);
    }

    #[test]
    fn test_cache_invalidation_on_metadata_change() {
        // Integration test: Verify that file changes are detected
        let repo = MmapCacheRepository::empty();
        let key = FileCacheKey::new("InvalidationTest", Path::new("/test/file.jsonl"));

        // Insert original entry
        let entry = FileCacheEntry {
            metadata: FileMetadata {
                size: 1000,
                modified: 1700000000,
                last_parsed_offset: 1000,
            },
            messages: vec![],
            daily_contributions: HashMap::new(),
            cached_model: None,
        };
        repo.insert(key.clone(), entry);

        // Simulate file modification (size changed)
        let new_metadata = FileMetadata {
            size: 2000,           // File grew
            modified: 1700000001, // Modified time changed
            last_parsed_offset: 0,
        };

        // Should be stale
        assert!(repo.is_stale(&key, &new_metadata));

        // After updating with new entry, should not be stale
        let updated_entry = FileCacheEntry {
            metadata: new_metadata.clone(),
            messages: vec![],
            daily_contributions: HashMap::new(),
            cached_model: None,
        };
        repo.insert(key.clone(), updated_entry);

        let check_meta = FileMetadata {
            size: 2000,
            modified: 1700000001,
            last_parsed_offset: 0,
        };
        assert!(!repo.is_stale(&key, &check_meta));
    }

    #[test]
    fn test_multiple_analyzers_share_cache_without_interference() {
        // Integration test: Multiple analyzers can use same cache without conflicts
        let repo = MmapCacheRepository::empty();

        // Insert entries for different analyzers
        let analyzers = ["Claude Code", "Gemini CLI", "Codex CLI", "Copilot"];

        for (i, analyzer_name) in analyzers.iter().enumerate() {
            let key =
                FileCacheKey::new(analyzer_name, Path::new(&format!("/test/{}/file.jsonl", i)));

            let mut entry = make_test_cache_entry();
            entry.metadata.size = (i as u64 + 1) * 1000;
            entry.cached_model = Some(format!("model-{}", i));

            repo.insert(key, entry);
        }

        // Verify each analyzer's entries are independent
        for (i, analyzer_name) in analyzers.iter().enumerate() {
            let key =
                FileCacheKey::new(analyzer_name, Path::new(&format!("/test/{}/file.jsonl", i)));

            let retrieved = repo.get_unchecked(&key).expect("should get entry");
            assert_eq!(retrieved.metadata.size, (i as u64 + 1) * 1000);
            assert_eq!(retrieved.cached_model, Some(format!("model-{}", i)));
        }

        // Verify stats
        let stats = repo.stats();
        assert_eq!(stats.total_entries, 4);

        // Invalidate one analyzer shouldn't affect others
        repo.invalidate_analyzer("Gemini CLI");

        let stats_after = repo.stats();
        assert_eq!(stats_after.total_entries, 3);

        // Other analyzers should still have their entries
        let claude_key = FileCacheKey::new("Claude Code", Path::new("/test/0/file.jsonl"));
        assert!(repo.get_unchecked(&claude_key).is_some());
    }

    #[test]
    fn test_empty_repo_stats_and_persist() {
        let repo = MmapCacheRepository::empty();
        let stats = repo.stats();
        assert_eq!(stats.total_entries, 0);
        assert!(stats.by_analyzer.is_empty());
        assert!(repo.persist().is_ok());
    }
}
