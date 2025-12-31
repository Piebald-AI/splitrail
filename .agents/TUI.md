# Real-Time Monitoring & TUI

Splitrail provides a terminal UI with live updates when analyzer data files change.

## Components

### FileWatcher (`src/watcher.rs`)

Watches analyzer data directories for changes:

- Uses the `notify` crate for cross-platform file watching
- Triggers incremental re-parsing on file changes
- Updates TUI in real-time via channels

```rust
// Key functions
FileWatcher::new(directories: Vec<PathBuf>) -> Result<Self>
FileWatcher::start(&self, tx: Sender<WatchEvent>) -> Result<()>
```

### RealtimeStatsManager

Coordinates real-time updates:

- Background file watching
- Auto-upload to Splitrail Cloud (if configured)
- Stats updates to TUI via `tokio::sync::watch`

### TUI (`src/tui.rs`, `src/tui/logic.rs`)

The terminal interface using `ratatui`:

- Daily stats view with date navigation
- Session view with lazy message loading
- Real-time stats refresh

## Key Patterns

### Channel-Based Updates

```rust
// Stats updates flow through watch channels
let (tx, rx) = tokio::sync::watch::channel(initial_stats);

// TUI subscribes to updates
while rx.changed().await.is_ok() {
    let stats = rx.borrow().clone();
    // Render updated stats
}
```

### Lazy Message Loading

TUI loads messages on-demand for the session view to avoid memory bloat:

```rust
// Only load messages when user navigates to session view
if view == View::Sessions {
    let messages = analyzer.get_messages_for_session(session_id).await?;
}
```

## Adding Watch Support to an Analyzer

Implement `get_watch_directories()` in your analyzer:

```rust
fn get_watch_directories(&self) -> Vec<PathBuf> {
    Self::data_dir()
        .filter(|d| d.is_dir())
        .into_iter()
        .collect()
}
```
