use anyhow::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher, EventKind};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::watch;

use crate::analyzer::AnalyzerRegistry;
use crate::types::MultiAnalyzerStats;

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

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    if let Err(e) = handle_fs_event(event, &event_tx, &dir_to_analyzer) {
                        let _ = event_tx.send(WatcherEvent::Error(format!("Event handling error: {}", e)));
                    }
                }
                Err(e) => {
                    let _ = event_tx.send(WatcherEvent::Error(format!("Watch error: {}", e)));
                }
            }
        })?;

        // Start watching all directories
        for dir in &watched_dirs {
            if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
                eprintln!("Warning: Could not watch directory {}: {}", dir.display(), e);
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
    dir_to_analyzer: &HashMap<PathBuf, String>
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

fn find_analyzer_for_path(file_path: &PathBuf, dir_to_analyzer: &HashMap<PathBuf, String>) -> Option<String> {
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
        })
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
                            let mut updated_analyzer_stats = self.current_stats.analyzer_stats.clone();
                            
                            // Find and replace the stats for this analyzer
                            if let Some(pos) = updated_analyzer_stats.iter().position(|s| s.analyzer_name == analyzer_name) {
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
                        }
                        Err(e) => {
                            eprintln!("Error reloading {} stats: {}", analyzer_name, e);
                        }
                    }
                }
            }
            WatcherEvent::Error(err) => {
                eprintln!("File watcher error: {}", err);
            }
        }
        Ok(())
    }

    pub async fn refresh_all(&mut self) -> Result<()> {
        // Use registry method instead of duplicating logic
        self.current_stats = self.registry.load_all_stats().await?;
        let _ = self.update_tx.send(self.current_stats.clone());
        Ok(())
    }
}