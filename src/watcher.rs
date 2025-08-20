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
use crate::types::MultiAnalyzerStats;
use crate::upload;

#[derive(Debug, Clone)]
pub enum WatcherEvent {
    DataChanged(String), // analyzer name
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
    // Only care about create, write, and remove events
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
            for path in &event.paths {
                // Find which analyzer owns this file by checking which watched directory contains it
                if let Some(analyzer_name) = find_analyzer_for_path(path, dir_to_analyzer) {
                    let _ = tx.send(WatcherEvent::DataChanged(analyzer_name));
                    break; // Only send one event per filesystem event
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
    current_stats: MultiAnalyzerStats,
    update_tx: watch::Sender<MultiAnalyzerStats>,
    update_rx: watch::Receiver<MultiAnalyzerStats>,
    last_upload_time: Option<Instant>,
    upload_debounce: Duration,
    upload_status: Option<Arc<Mutex<UploadStatus>>>,
    upload_in_progress: Arc<Mutex<bool>>,
    pending_upload: Arc<Mutex<bool>>,
}

impl RealtimeStatsManager {
    pub async fn new(registry: AnalyzerRegistry) -> Result<Self> {
        // Initial stats load using registry method
        let initial_stats = registry.load_all_stats().await?;
        let (update_tx, update_rx) = watch::channel(initial_stats.clone());

        Ok(Self {
            registry,
            current_stats: initial_stats,
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

    pub fn get_stats_receiver(&self) -> watch::Receiver<MultiAnalyzerStats> {
        self.update_rx.clone()
    }

    pub async fn handle_watcher_event(&mut self, event: WatcherEvent) -> Result<()> {
        match event {
            WatcherEvent::DataChanged(analyzer_name) => {
                // Reload data for the specific analyzer
                if let Some(analyzer) = self.registry.get_analyzer_by_display_name(&analyzer_name) {
                    match analyzer.get_stats().await {
                        Ok(new_stats) => {
                            // Update the stats for this analyzer
                            let mut updated_analyzer_stats =
                                self.current_stats.analyzer_stats.clone();

                            // Find and replace the stats for this analyzer
                            if let Some(pos) = updated_analyzer_stats
                                .iter()
                                .position(|s| s.analyzer_name == analyzer_name)
                            {
                                updated_analyzer_stats[pos] = new_stats;
                            } else {
                                // New analyzer data
                                updated_analyzer_stats.push(new_stats);
                            }

                            self.current_stats = MultiAnalyzerStats {
                                analyzer_stats: updated_analyzer_stats,
                            };

                            // Send the update
                            let _ = self.update_tx.send(self.current_stats.clone());

                            // Trigger auto-upload if enabled and debounce time has passed
                            self.trigger_auto_upload_if_enabled().await;
                        }
                        Err(e) => {
                            eprintln!("Error reloading {analyzer_name} stats: {e}");
                        }
                    }
                }
            }
            WatcherEvent::Error(err) => {
                eprintln!("File watcher error: {err}");
            }
        }
        Ok(())
    }

    async fn trigger_auto_upload_if_enabled(&mut self) {
        // Check if auto-upload is enabled
        let _config = match Config::load() {
            Ok(Some(cfg)) if cfg.upload.auto_upload && cfg.is_configured() => cfg,
            _ => return, // Auto-upload not enabled or config not available
        };

        // Check if an upload is already in progress
        if let Ok(in_progress) = self.upload_in_progress.lock() {
            if *in_progress {
                // Mark that we have pending changes to upload
                if let Ok(mut pending) = self.pending_upload.lock() {
                    *pending = true;
                }
                return;
            }
        }

        // Check debounce timing
        let now = Instant::now();
        if let Some(last_time) = self.last_upload_time {
            if now.duration_since(last_time) < self.upload_debounce {
                // Schedule a delayed upload
                let remaining_wait = self.upload_debounce - now.duration_since(last_time);
                let stats = self.current_stats.clone();
                let upload_status = self.upload_status.clone();
                let upload_in_progress = self.upload_in_progress.clone();
                let pending_upload = self.pending_upload.clone();
                
                tokio::spawn(async move {
                    tokio::time::sleep(remaining_wait).await;
                    
                    // Check if we should still upload
                    let should_upload = if let Ok(mut pending) = pending_upload.lock() {
                        let was_pending = *pending;
                        *pending = false;
                        was_pending
                    } else {
                        true
                    };
                    
                    if should_upload {
                        // Mark upload as in progress
                        if let Ok(mut in_progress) = upload_in_progress.lock() {
                            *in_progress = true;
                        }
                        
                        upload::perform_background_upload(stats, upload_status, None).await;
                        
                        // Mark upload as complete
                        if let Ok(mut in_progress) = upload_in_progress.lock() {
                            *in_progress = false;
                        }
                    }
                });
                
                // Mark that we have a pending upload scheduled
                if let Ok(mut pending) = self.pending_upload.lock() {
                    *pending = true;
                }
                return;
            }
        }

        self.last_upload_time = Some(now);

        // Mark upload as in progress
        if let Ok(mut in_progress) = self.upload_in_progress.lock() {
            *in_progress = true;
        }

        // Clone necessary data for the async upload task
        let stats = self.current_stats.clone();
        let upload_status = self.upload_status.clone();
        let upload_in_progress = self.upload_in_progress.clone();
        let pending_upload = self.pending_upload.clone();

        // Spawn background upload task
        tokio::spawn(async move {
            upload::perform_background_upload(stats.clone(), upload_status.clone(), None).await;
            
            // Mark upload as complete
            if let Ok(mut in_progress) = upload_in_progress.lock() {
                *in_progress = false;
            }
            
            // Check if we need to upload again due to changes during the upload
            let should_upload_again = if let Ok(mut pending) = pending_upload.lock() {
                let was_pending = *pending;
                *pending = false;
                was_pending
            } else {
                false
            };
            
            if should_upload_again {
                // Wait a short time before uploading again
                tokio::time::sleep(Duration::from_secs(1)).await;
                upload::perform_background_upload(stats, upload_status, None).await;
            }
        });
    }
}
