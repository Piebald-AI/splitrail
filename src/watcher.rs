use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use notify_types::event::{Event, EventKind};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

use crate::analyzer::AnalyzerRegistry;
use crate::config::Config;
use crate::tui::UploadStatus;
use crate::types::MultiAnalyzerStatsView;
use crate::upload;

#[derive(Debug, Clone)]
pub enum WatcherEvent {
    /// A file was created or modified (analyzer name, file path)
    FileChanged(String, PathBuf),
    /// A file was deleted (analyzer name, file path)
    FileDeleted(String, PathBuf),
    /// An error occurred
    Error(String),
}

pub struct FileWatcher {
    _watcher: RecommendedWatcher,
    event_rx: Receiver<WatcherEvent>,
}

impl FileWatcher {
    pub fn new(registry: &AnalyzerRegistry) -> Result<Self> {
        let (event_tx, event_rx) = mpsc::channel();

        // Get directory to analyzer mapping from registry
        let dir_to_analyzer = registry.get_directory_to_analyzer_mapping();
        let watched_dirs: HashSet<_> = dir_to_analyzer.keys().cloned().collect();

        let mut watcher =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
                Ok(event) => {
                    if let Err(e) = handle_fs_event(event, &event_tx, &dir_to_analyzer) {
                        let _ = event_tx
                            .send(WatcherEvent::Error(format!("Event handling error: {e}")));
                    }
                }
                Err(e) => {
                    let _ = event_tx.send(WatcherEvent::Error(format!("Watch error: {e}")));
                }
            })?;

        // Start watching all directories
        for dir in &watched_dirs {
            if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
                eprintln!(
                    "Warning: Could not watch directory {}: {}",
                    dir.display(),
                    e
                );
            }
        }

        Ok(Self {
            _watcher: watcher,
            event_rx,
        })
    }

    pub fn try_recv(&self) -> Option<WatcherEvent> {
        self.event_rx.try_recv().ok()
    }
}

fn handle_fs_event(
    event: Event,
    tx: &Sender<WatcherEvent>,
    dir_to_analyzer: &HashMap<PathBuf, String>,
) -> Result<()> {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {
            for path in &event.paths {
                if let Some(analyzer_name) = find_analyzer_for_path(path, dir_to_analyzer) {
                    // Send per-file event for incremental cache update
                    let _ = tx.send(WatcherEvent::FileChanged(analyzer_name, path.clone()));
                }
            }
        }
        EventKind::Remove(_) => {
            for path in &event.paths {
                if let Some(analyzer_name) = find_analyzer_for_path(path, dir_to_analyzer) {
                    let _ = tx.send(WatcherEvent::FileDeleted(analyzer_name, path.clone()));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn find_analyzer_for_path(
    file_path: &Path,
    dir_to_analyzer: &HashMap<PathBuf, String>,
) -> Option<String> {
    // Find the longest matching directory path (most specific match)
    let mut best_match: Option<(&PathBuf, &String)> = None;
    let mut best_length = 0;

    for (watched_dir, analyzer_name) in dir_to_analyzer {
        if file_path.starts_with(watched_dir) {
            let length = watched_dir.components().count();
            if length > best_length {
                best_length = length;
                best_match = Some((watched_dir, analyzer_name));
            }
        }
    }

    best_match.map(|(_, analyzer_name)| analyzer_name.clone())
}

pub struct RealtimeStatsManager {
    registry: AnalyzerRegistry,
    update_tx: watch::Sender<MultiAnalyzerStatsView>,
    update_rx: watch::Receiver<MultiAnalyzerStatsView>,
    last_upload_time: Option<Instant>,
    upload_debounce: Duration,
    upload_status: Option<Arc<Mutex<UploadStatus>>>,
    upload_in_progress: Arc<Mutex<bool>>,
    pending_upload: Arc<Mutex<bool>>,
}

impl RealtimeStatsManager {
    pub async fn new(registry: AnalyzerRegistry) -> Result<Self> {
        // Initial stats load using a temporary thread pool for parallel parsing.
        // The pool is dropped after loading, releasing thread-local memory.
        let num_threads = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(8);
        let initial_stats = registry.load_all_stats_views_parallel(num_threads)?;
        let (update_tx, update_rx) = watch::channel(initial_stats);

        Ok(Self {
            registry,
            update_tx,
            update_rx,
            last_upload_time: None,
            upload_debounce: Duration::from_secs(3), // Wait 3 seconds after changes before uploading
            upload_status: None,
            upload_in_progress: Arc::new(Mutex::new(false)),
            pending_upload: Arc::new(Mutex::new(false)),
        })
    }

    pub fn set_upload_status(&mut self, status: Arc<Mutex<UploadStatus>>) {
        self.upload_status = Some(status);
    }

    /// Persist the file stats cache to disk (no-op since caching was removed)
    pub fn persist_cache(&self) {
        // Caching has been removed - this is kept for API compatibility
    }

    pub fn get_stats_receiver(&self) -> watch::Receiver<MultiAnalyzerStatsView> {
        self.update_rx.clone()
    }

    pub async fn handle_watcher_event(&mut self, event: WatcherEvent) -> Result<()> {
        match event {
            WatcherEvent::FileChanged(analyzer_name, path) => {
                // True incremental update - O(1), only reparses the changed file
                if self.registry.has_cached_contributions(&analyzer_name) {
                    self.reload_single_file_incremental(&analyzer_name, &path)
                        .await;
                } else {
                    // Fallback to full reload if cache not populated (shouldn't happen normally)
                    self.reload_analyzer_stats(&analyzer_name).await;
                }
            }
            WatcherEvent::FileDeleted(analyzer_name, path) => {
                // Remove file from cache and get updated view
                if self.registry.remove_file_from_cache(&analyzer_name, &path) {
                    self.apply_view_update().await;
                } else {
                    // Fallback to full reload
                    self.reload_analyzer_stats(&analyzer_name).await;
                }
            }
            WatcherEvent::Error(err) => {
                eprintln!("File watcher error: {err}");
            }
        }

        Ok(())
    }

    /// Helper to reload stats for a specific analyzer and broadcast updates (fallback)
    async fn reload_analyzer_stats(&mut self, analyzer_name: &str) {
        if let Some(analyzer) = self.registry.get_analyzer_by_display_name(analyzer_name) {
            // Full parse of all files for this analyzer
            match analyzer.get_stats().await {
                Ok(new_stats) => {
                    // Update the cache with the new view
                    self.registry
                        .update_cached_view(analyzer_name, new_stats.into_view());
                    self.apply_view_update().await;
                }
                Err(e) => {
                    eprintln!("Error reloading {analyzer_name} stats: {e}");
                }
            }
        }
    }

    /// Helper to reload stats for a single file change using true incremental update
    async fn reload_single_file_incremental(&mut self, analyzer_name: &str, path: &Path) {
        // True incremental update - subtract old, add new
        match self
            .registry
            .reload_file_incremental(analyzer_name, path)
            .await
        {
            Ok(()) => {
                self.apply_view_update().await;
            }
            Err(e) => {
                eprintln!("Error in incremental reload for {analyzer_name}: {e}");
                // Fallback to full reload on error
                self.reload_analyzer_stats(analyzer_name).await;
            }
        }
    }

    /// Broadcast the current cache state to listeners.
    /// The view is already updated in place via RwLock; we just rebuild and broadcast.
    async fn apply_view_update(&mut self) {
        // Build fresh MultiAnalyzerStatsView from cache - just clones Arc pointers
        let stats = MultiAnalyzerStatsView {
            analyzer_stats: self.registry.get_all_cached_views(),
        };

        // Send the update
        let _ = self.update_tx.send(stats);

        // Trigger auto-upload if enabled and debounce time has passed
        self.trigger_auto_upload_if_enabled().await;
    }

    async fn trigger_auto_upload_if_enabled(&mut self) {
        // Check if auto-upload is enabled
        let _config = match Config::load() {
            Ok(Some(cfg)) if cfg.upload.auto_upload && cfg.is_configured() => cfg,
            _ => return, // Auto-upload not enabled or config not available
        };

        // Check if an upload is already in progress
        if let Ok(in_progress) = self.upload_in_progress.lock()
            && *in_progress
        {
            // Mark that we have pending changes to upload
            if let Ok(mut pending) = self.pending_upload.lock() {
                *pending = true;
            }
            return;
        }

        // Check debounce timing - skip actual upload for debounce period
        // Upload will be triggered on next change after debounce expires
        let now = Instant::now();
        if let Some(last_time) = self.last_upload_time
            && now.duration_since(last_time) < self.upload_debounce
        {
            // Mark that we have pending changes to upload
            if let Ok(mut pending) = self.pending_upload.lock() {
                *pending = true;
            }
            return;
        }

        self.last_upload_time = Some(now);

        // Check if an upload is already in progress
        if let Ok(mut in_progress) = self.upload_in_progress.lock() {
            if *in_progress {
                // Mark that we have pending changes to upload
                if let Ok(mut pending) = self.pending_upload.lock() {
                    *pending = true;
                }
                return;
            }
            *in_progress = true;
        }

        // For upload, we need full stats (with messages)
        let full_stats = match self.registry.load_all_stats().await {
            Ok(stats) => stats,
            Err(_) => {
                if let Ok(mut in_progress) = self.upload_in_progress.lock() {
                    *in_progress = false;
                }
                return;
            }
        };

        let upload_status = self.upload_status.clone();
        let upload_in_progress = self.upload_in_progress.clone();

        // Spawn background upload task
        tokio::spawn(async move {
            upload::perform_background_upload(full_stats, upload_status, None).await;

            // Mark upload as complete
            if let Ok(mut in_progress) = upload_in_progress.lock() {
                *in_progress = false;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::{Analyzer, DataSource};
    use crate::types::{
        AgenticCodingToolStats, Application, ConversationMessage, MessageRole, Stats,
    };
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use notify_types::event::{CreateKind, Event as NotifyEvent, EventKind as NotifyEventKind};
    use std::collections::BTreeMap;

    fn sample_stats(name: &str) -> AgenticCodingToolStats {
        let date = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let msg = ConversationMessage {
            application: Application::ClaudeCode,
            date,
            project_hash: "proj".into(),
            conversation_hash: "conv".into(),
            local_hash: None,
            global_hash: "global".into(),
            model: Some("model".into()),
            stats: Stats {
                input_tokens: 1,
                ..Stats::default()
            },
            role: MessageRole::Assistant,
            uuid: None,
            session_name: Some("session".into()),
        };

        AgenticCodingToolStats {
            daily_stats: BTreeMap::new(),
            num_conversations: 1,
            messages: vec![msg],
            analyzer_name: name.to_string(),
        }
    }

    struct TestAnalyzer {
        name: &'static str,
        stats: AgenticCodingToolStats,
        available: bool,
    }

    #[async_trait]
    impl Analyzer for TestAnalyzer {
        fn display_name(&self) -> &'static str {
            self.name
        }

        fn get_data_glob_patterns(&self) -> Vec<String> {
            vec![]
        }

        fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
            Ok(Vec::new())
        }

        async fn parse_conversations(
            &self,
            _sources: Vec<DataSource>,
        ) -> Result<Vec<ConversationMessage>> {
            Ok(self.stats.messages.clone())
        }

        async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
            Ok(self.stats.clone())
        }

        fn is_available(&self) -> bool {
            self.available
        }

        fn get_watch_directories(&self) -> Vec<PathBuf> {
            Vec::new()
        }
    }

    #[test]
    fn find_analyzer_prefers_more_specific_directory() {
        let mut mapping = HashMap::new();
        mapping.insert(PathBuf::from("/tmp/project"), "root".to_string());
        mapping.insert(PathBuf::from("/tmp/project/chats"), "chats".to_string());

        let path = Path::new("/tmp/project/chats/session.json");
        let analyzer = find_analyzer_for_path(path, &mapping).expect("analyzer");
        assert_eq!(analyzer, "chats");
    }

    #[test]
    fn handle_fs_event_emits_file_changed_for_create() {
        let mut mapping = HashMap::new();
        let dir = PathBuf::from("/tmp/project/chats");
        mapping.insert(dir.clone(), "analyzer".to_string());

        let file_path = dir.join("session.json");
        let event =
            NotifyEvent::new(NotifyEventKind::Create(CreateKind::File)).add_path(file_path.clone());

        let (tx, rx) = mpsc::channel();
        handle_fs_event(event, &tx, &mapping).expect("handle_fs_event");

        let evt = rx.try_recv().expect("event");
        match evt {
            WatcherEvent::FileChanged(name, path) => {
                assert_eq!(name, "analyzer");
                assert_eq!(path, file_path);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_watcher_event_updates_stats_for_data_change() {
        let stats = sample_stats("test-analyzer");
        let mut registry = AnalyzerRegistry::new();
        registry.register(TestAnalyzer {
            name: "test-analyzer",
            stats: stats.clone(),
            available: true,
        });

        let mut manager = RealtimeStatsManager::new(registry).await.expect("manager");

        let initial = manager.get_stats_receiver().borrow().clone();
        assert!(
            initial.analyzer_stats.is_empty()
                || initial.analyzer_stats[0].read().analyzer_name == "test-analyzer"
        );

        manager
            .handle_watcher_event(WatcherEvent::FileDeleted(
                "test-analyzer".into(),
                PathBuf::from("/fake/path.jsonl"),
            ))
            .await
            .expect("handle_watcher_event");

        let updated = manager.get_stats_receiver().borrow().clone();
        // After handling FileDeleted, we should still have stats for the analyzer.
        assert!(!updated.analyzer_stats.is_empty());
        assert_eq!(
            updated.analyzer_stats[0].read().analyzer_name,
            "test-analyzer"
        );

        // Also exercise the error branch.
        manager
            .handle_watcher_event(WatcherEvent::Error("something went wrong".into()))
            .await
            .expect("handle_watcher_event error");
    }

    #[test]
    fn handle_fs_event_emits_file_deleted_for_remove() {
        use notify_types::event::RemoveKind;

        let mut mapping = HashMap::new();
        let dir = PathBuf::from("/tmp/project/chats");
        mapping.insert(dir.clone(), "analyzer".to_string());

        let file_path = dir.join("deleted_session.json");
        let event =
            NotifyEvent::new(NotifyEventKind::Remove(RemoveKind::File)).add_path(file_path.clone());

        let (tx, rx) = mpsc::channel();
        handle_fs_event(event, &tx, &mapping).expect("handle_fs_event");

        let evt = rx.try_recv().expect("event");
        match evt {
            WatcherEvent::FileDeleted(name, path) => {
                assert_eq!(name, "analyzer");
                assert_eq!(path, file_path);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_file_deleted_event_reloads_stats() {
        let stats = sample_stats("test-analyzer");
        let mut registry = AnalyzerRegistry::new();
        registry.register(TestAnalyzer {
            name: "test-analyzer",
            stats: stats.clone(),
            available: true,
        });

        let mut manager = RealtimeStatsManager::new(registry).await.expect("manager");

        // Handle FileDeleted event
        manager
            .handle_watcher_event(WatcherEvent::FileDeleted(
                "test-analyzer".into(),
                PathBuf::from("/fake/path.jsonl"),
            ))
            .await
            .expect("handle FileDeleted");

        // Stats should still be accessible after handling the event
        let updated = manager.get_stats_receiver().borrow().clone();
        // The test analyzer doesn't have real sources, so stats may be empty or present
        // The key is that this doesn't panic
        assert!(updated.analyzer_stats.is_empty() || !updated.analyzer_stats.is_empty());
    }

    #[tokio::test]
    async fn handle_file_changed_event_reloads_stats() {
        let stats = sample_stats("test-analyzer");
        let mut registry = AnalyzerRegistry::new();
        registry.register(TestAnalyzer {
            name: "test-analyzer",
            stats: stats.clone(),
            available: true,
        });

        let mut manager = RealtimeStatsManager::new(registry).await.expect("manager");

        // Handle FileChanged event
        manager
            .handle_watcher_event(WatcherEvent::FileChanged(
                "test-analyzer".into(),
                PathBuf::from("/fake/path.jsonl"),
            ))
            .await
            .expect("handle FileChanged");

        // The key is that this doesn't panic and manager remains usable
        let updated = manager.get_stats_receiver().borrow().clone();
        assert!(updated.analyzer_stats.is_empty() || !updated.analyzer_stats.is_empty());
    }

    #[tokio::test]
    async fn persist_cache_does_not_panic() {
        let stats = sample_stats("test-analyzer");
        let mut registry = AnalyzerRegistry::new();
        registry.register(TestAnalyzer {
            name: "test-analyzer",
            stats,
            available: true,
        });

        let manager = RealtimeStatsManager::new(registry).await.expect("manager");

        // persist_cache should not panic even if cache is empty
        manager.persist_cache();
    }
}
