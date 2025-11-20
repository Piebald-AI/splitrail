use crate::types::{AgenticCodingToolStats, MultiAnalyzerStats, Stats};
use crate::utils::{NumberFormatOptions, format_date_for_display, format_number};
use crate::watcher::{FileWatcher, RealtimeStatsManager, WatcherEvent};
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::style::{Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Tabs};
use ratatui::{Frame, Terminal};
use std::collections::BTreeMap;
use std::io::{Write, stdout};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone)]
pub enum UploadStatus {
    None,
    Uploading {
        current: usize,
        total: usize,
        dots: usize,
    },
    Uploaded,
    Failed(String), // Include error message
    MissingApiToken,
    MissingServerUrl,
    MissingConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatsViewMode {
    Daily,
    Session,
}

struct UiState<'a> {
    table_states: &'a mut [TableState],
    _scroll_offset: usize,
    selected_tab: usize,
    stats_view_mode: StatsViewMode,
    session_window_offsets: &'a mut [usize],
}

#[derive(Debug, Clone)]
struct SessionAggregate {
    session_id: String,
    first_timestamp: DateTime<Utc>,
    analyzer_name: String,
    stats: Stats,
    models: Vec<String>,
    session_name: Option<String>,
}

fn accumulate_stats(dst: &mut Stats, src: &Stats) {
    // Token and cost stats
    dst.input_tokens += src.input_tokens;
    dst.output_tokens += src.output_tokens;
    dst.reasoning_tokens += src.reasoning_tokens;
    dst.cache_creation_tokens += src.cache_creation_tokens;
    dst.cache_read_tokens += src.cache_read_tokens;
    dst.cached_tokens += src.cached_tokens;
    dst.cost += src.cost;
    dst.tool_calls += src.tool_calls;

    // File operation stats
    dst.terminal_commands += src.terminal_commands;
    dst.file_searches += src.file_searches;
    dst.file_content_searches += src.file_content_searches;
    dst.files_read += src.files_read;
    dst.files_added += src.files_added;
    dst.files_edited += src.files_edited;
    dst.files_deleted += src.files_deleted;
    dst.lines_read += src.lines_read;
    dst.lines_added += src.lines_added;
    dst.lines_edited += src.lines_edited;
    dst.lines_deleted += src.lines_deleted;
    dst.bytes_read += src.bytes_read;
    dst.bytes_added += src.bytes_added;
    dst.bytes_edited += src.bytes_edited;
    dst.bytes_deleted += src.bytes_deleted;

    // Todo stats
    dst.todos_created += src.todos_created;
    dst.todos_completed += src.todos_completed;
    dst.todos_in_progress += src.todos_in_progress;
    dst.todo_writes += src.todo_writes;
    dst.todo_reads += src.todo_reads;

    // Composition stats
    dst.code_lines += src.code_lines;
    dst.docs_lines += src.docs_lines;
    dst.data_lines += src.data_lines;
    dst.media_lines += src.media_lines;
    dst.config_lines += src.config_lines;
    dst.other_lines += src.other_lines;
}

fn aggregate_sessions_for_tool(stats: &AgenticCodingToolStats) -> Vec<SessionAggregate> {
    let mut sessions: BTreeMap<String, SessionAggregate> = BTreeMap::new();

    for msg in &stats.messages {
        let session_key = msg.conversation_hash.clone();
        let entry = sessions
            .entry(session_key.clone())
            .or_insert_with(|| SessionAggregate {
                session_id: session_key.clone(),
                first_timestamp: msg.date,
                analyzer_name: stats.analyzer_name.clone(),
                stats: Stats::default(),
                models: Vec::new(),
                session_name: None,
            });

        if msg.date < entry.first_timestamp {
            entry.first_timestamp = msg.date;
        }

        // Only aggregate stats for assistant/model messages and track models
        if let Some(model) = &msg.model {
            if !entry.models.iter().any(|m| m == model) {
                entry.models.push(model.clone());
            }
            accumulate_stats(&mut entry.stats, &msg.stats);
        }

        // Capture session name if available (last one wins, or first one, doesn't matter much as they should be consistent per file/session)
        if let Some(name) = &msg.session_name {
            entry.session_name = Some(name.clone());
        }
    }

    let mut result: Vec<SessionAggregate> = sessions.into_values().collect();

    // Sort oldest sessions first so newest appear at the bottom (like per-day view)
    result.sort_by_key(|s| s.first_timestamp);

    result
}

fn aggregate_sessions_for_all_tools(
    filtered_stats: &[&AgenticCodingToolStats],
) -> Vec<Vec<SessionAggregate>> {
    filtered_stats
        .iter()
        .map(|stats| aggregate_sessions_for_tool(stats))
        .collect()
}

fn aggregate_sessions_for_all_tools_owned(
    stats: &[AgenticCodingToolStats],
) -> Vec<Vec<SessionAggregate>> {
    stats
        .iter()
        .map(|stats| aggregate_sessions_for_tool(stats))
        .collect()
}
#[derive(Debug, Clone)]
struct SessionTableCache {
    sessions: Vec<SessionAggregate>,
    best_cost_i: Option<usize>,
    best_total_tokens_i: Option<usize>,
    total_cost: f64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cached_tokens: u64,
    total_tool_calls: u64,
    all_models_text: String,
}

fn build_session_table_cache(sessions: Vec<SessionAggregate>) -> SessionTableCache {
    let mut best_cost = None;
    let mut best_cost_i = None;
    let mut best_total_tokens = None;
    let mut best_total_tokens_i = None;

    let mut total_cost = 0.0;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_cached_tokens = 0u64;
    let mut total_tool_calls = 0u64;

    for (i, session) in sessions.iter().enumerate() {
        if best_cost.map_or(true, |best| session.stats.cost > best) {
            best_cost = Some(session.stats.cost);
            best_cost_i = Some(i);
        }

        let total_tokens =
            session.stats.input_tokens + session.stats.output_tokens + session.stats.cached_tokens;
        if best_total_tokens
            .map_or(true, |best| total_tokens > best)
        {
            best_total_tokens = Some(total_tokens);
            best_total_tokens_i = Some(i);
        }

        total_cost += session.stats.cost;
        total_input_tokens += session.stats.input_tokens;
        total_output_tokens += session.stats.output_tokens;
        total_cached_tokens += session.stats.cached_tokens;
        total_tool_calls += session.stats.tool_calls as u64;
    }

    let mut all_models = std::collections::HashSet::new();
    for session in &sessions {
        for model in &session.models {
            all_models.insert(model.clone());
        }
    }
    let mut all_models_vec = all_models.into_iter().collect::<Vec<_>>();
    all_models_vec.sort();
    let all_models_text = all_models_vec.join(", ");

    SessionTableCache {
        sessions,
        best_cost_i,
        best_total_tokens_i,
        total_cost,
        total_input_tokens,
        total_output_tokens,
        total_cached_tokens,
        total_tool_calls,
        all_models_text,
    }
}

fn has_data(stats: &AgenticCodingToolStats) -> bool {
    stats.num_conversations > 0
        || stats.daily_stats.values().any(|day| {
            day.stats.cost > 0.0
                || day.stats.input_tokens > 0
                || day.stats.output_tokens > 0
                || day.stats.reasoning_tokens > 0
                || day.stats.tool_calls > 0
        })
}

pub fn run_tui(
    stats_receiver: watch::Receiver<MultiAnalyzerStats>,
    format_options: &NumberFormatOptions,
    upload_status: Arc<Mutex<UploadStatus>>,
    file_watcher: FileWatcher,
    mut stats_manager: RealtimeStatsManager,
) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut selected_tab = 0;
    let mut scroll_offset = 0;
    let mut stats_view_mode = StatsViewMode::Daily;

    let (watcher_tx, mut watcher_rx) = mpsc::unbounded_channel::<WatcherEvent>();

    tokio::spawn(async move {
        while let Some(event) = watcher_rx.recv().await {
            if let Err(e) = stats_manager.handle_watcher_event(event).await {
                eprintln!("Error handling watcher event: {e}");
            }
        }
    });

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(run_app(
            &mut terminal,
            stats_receiver,
            format_options,
            &mut selected_tab,
            &mut scroll_offset,
            &mut stats_view_mode,
            upload_status,
            file_watcher,
            watcher_tx,
        ))
    });

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut stats_receiver: watch::Receiver<MultiAnalyzerStats>,
    format_options: &NumberFormatOptions,
    selected_tab: &mut usize,
    scroll_offset: &mut usize,
    stats_view_mode: &mut StatsViewMode,
    upload_status: Arc<Mutex<UploadStatus>>,
    file_watcher: FileWatcher,
    watcher_tx: mpsc::UnboundedSender<WatcherEvent>,
) -> Result<()> {
    let mut table_states: Vec<TableState> = Vec::new();
    let mut session_window_offsets: Vec<usize> = Vec::new();
    let mut current_stats = stats_receiver.borrow().clone();

    // Initialize table states for current stats
    update_table_states(&mut table_states, &current_stats, selected_tab);
    update_window_offsets(&mut session_window_offsets, &table_states.len());

    let mut needs_redraw = true;
    let mut last_upload_status = {
        let status = upload_status.lock().unwrap();
        format!("{:?}", *status)
    };
    let mut dots_counter = 0; // Counter for dots animation (advance every 5 frames = 500ms)

    // Filter analyzer stats to only include those with data - calculate once and update when stats change
    let mut filtered_stats: Vec<&AgenticCodingToolStats> = current_stats
        .analyzer_stats
        .iter()
        .filter(|stats| has_data(stats))
        .collect();

    let mut session_stats_per_tool = aggregate_sessions_for_all_tools(&filtered_stats);
    let mut session_table_cache: Vec<SessionTableCache> = session_stats_per_tool
        .iter()
        .cloned()
        .map(build_session_table_cache)
        .collect();
    let mut recompute_version: u64 = 0;
    let mut pending_session_recompute:
        Option<tokio::task::JoinHandle<(u64, Vec<Vec<SessionAggregate>>, Vec<SessionTableCache>)>> =
        None;

    loop {
        // Check for stats updates
        if stats_receiver.has_changed()? {
            current_stats = stats_receiver.borrow_and_update().clone();
            // Recalculate filtered stats only when stats change
            filtered_stats = current_stats
                .analyzer_stats
                .iter()
                .filter(|stats| has_data(stats))
                .collect();
            update_table_states(&mut table_states, &current_stats, selected_tab);
            update_window_offsets(&mut session_window_offsets, &table_states.len());
            recompute_version = recompute_version.wrapping_add(1);
            let version = recompute_version;
            if let Some(handle) = pending_session_recompute.take() {
                handle.abort();
            }
            let stats_for_recompute: Vec<AgenticCodingToolStats> =
                filtered_stats.iter().map(|s| (*s).clone()).collect();
            pending_session_recompute = Some(tokio::task::spawn_blocking(move || {
                let session_stats = aggregate_sessions_for_all_tools_owned(&stats_for_recompute);
                let caches = session_stats
                    .iter()
                    .cloned()
                    .map(build_session_table_cache)
                    .collect();
                (version, session_stats, caches)
            }));
            needs_redraw = true;
        }

        // Check for file watcher events; hand off processing so UI thread stays responsive
        while let Some(watcher_event) = file_watcher.try_recv() {
            let _ = watcher_tx.send(watcher_event);
        }

        // Check if upload status has changed or advance dots animation
        let current_upload_status = {
            let mut status = upload_status.lock().unwrap();
            // Advance dots animation for uploading status every 500ms (5 frames at 100ms)
            if let UploadStatus::Uploading {
                current: _,
                total: _,
                dots,
            } = &mut *status
            {
                // Always animate dots during upload
                dots_counter += 1;
                if dots_counter >= 5 {
                    *dots = (*dots + 1) % 4;
                    dots_counter = 0;
                    needs_redraw = true;
                }
            } else {
                // Reset counter when not uploading
                dots_counter = 0;
            }
            format!("{:?}", *status)
        };
        if current_upload_status != last_upload_status {
            last_upload_status = current_upload_status;
            needs_redraw = true;
        }

        // Only redraw if something has changed
        if needs_redraw {
            terminal.draw(|frame| {
                let mut ui_state = UiState {
                    table_states: &mut table_states,
                    _scroll_offset: *scroll_offset,
                    selected_tab: *selected_tab,
                    stats_view_mode: *stats_view_mode,
                    session_window_offsets: &mut session_window_offsets,
                };
                draw_ui(
                    frame,
                    &filtered_stats,
                    format_options,
                    &mut ui_state,
                    upload_status.clone(),
                    &session_table_cache,
                );
            })?;
            needs_redraw = false;
        }

        if let Some(handle) = pending_session_recompute.as_mut() {
            if handle.is_finished() {
                if let Ok((version, new_sessions, new_cache)) = handle.await {
                    if version == recompute_version {
                        session_stats_per_tool = new_sessions;
                        session_table_cache = new_cache;
                        needs_redraw = true;
                    }
                }
                pending_session_recompute = None;
            }
        }

        // Use a timeout to allow periodic refreshes for upload status updates
        if let Ok(event_available) = event::poll(Duration::from_millis(100)) {
            if !event_available {
                continue;
            }

            // Handle different event types
            let key = match event::read()? {
                Event::Key(key) if key.is_press() => key,
                Event::Resize(_, _) => {
                    // Terminal was resized, trigger redraw
                    needs_redraw = true;
                    continue;
                }
                _ => continue,
            };

            // Handle quitting.
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                break;
            }

            // Only handle navigation keys if we have data (`filtered_stats` is non-empty).
            if filtered_stats.is_empty() {
                continue;
            }

            match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    if *selected_tab > 0 {
                        *selected_tab -= 1;

                        if let StatsViewMode::Session = *stats_view_mode
                            && let Some(session_rows) = session_stats_per_tool.get(*selected_tab)
                            && let Some(table_state) = table_states.get_mut(*selected_tab)
                            && !session_rows.is_empty()
                        {
                            table_state.select(Some(session_rows.len().saturating_sub(1)));
                        }

                        needs_redraw = true;
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if *selected_tab < filtered_stats.len().saturating_sub(1) {
                        *selected_tab += 1;

                        if let StatsViewMode::Session = *stats_view_mode
                            && let Some(session_rows) = session_stats_per_tool.get(*selected_tab)
                            && let Some(table_state) = table_states.get_mut(*selected_tab)
                            && !session_rows.is_empty()
                        {
                            table_state.select(Some(session_rows.len().saturating_sub(1)));
                        }

                        needs_redraw = true;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(table_state) = table_states.get_mut(*selected_tab)
                        && let Some(selected) = table_state.selected()
                    {
                        match *stats_view_mode {
                            StatsViewMode::Daily => {
                                if let Some(current_stats) = filtered_stats.get(*selected_tab) {
                                    let total_rows = current_stats.daily_stats.len();
                                    if selected < total_rows.saturating_add(1) {
                                        table_state.select(Some(
                                            if selected == total_rows.saturating_sub(1) {
                                                selected + 2
                                            } else {
                                                selected + 1
                                            },
                                        ));
                                        needs_redraw = true;
                                    }
                                }
                            }
                            StatsViewMode::Session => {
                                if let Some(session_rows) =
                                    session_stats_per_tool.get(*selected_tab)
                                {
                                    let total_rows = session_rows.len();
                                    if total_rows > 0 && selected < total_rows.saturating_add(1) {
                                        // sessions: 0..total_rows-1
                                        // separator: total_rows
                                        // totals: total_rows + 1
                                        table_state.select(Some(
                                            if selected == total_rows.saturating_sub(1) {
                                                // Jump from last session row to totals (skip separator)
                                                selected + 2
                                            } else {
                                                // Move down one row (may land on separator or totals)
                                                selected + 1
                                            },
                                        ));
                                        needs_redraw = true;
                                    }
                                }
                            }
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(table_state) = table_states.get_mut(*selected_tab)
                        && let Some(selected) = table_state.selected()
                        && selected > 0
                    {
                        match *stats_view_mode {
                            StatsViewMode::Daily => {
                                if let Some(current_stats) = filtered_stats.get(*selected_tab) {
                                    table_state.select(Some(selected.saturating_sub(
                                        if selected == current_stats.daily_stats.len() + 1 {
                                            2
                                        } else {
                                            1
                                        },
                                    )));
                                    needs_redraw = true;
                                }
                            }
                            StatsViewMode::Session => {
                                if let Some(session_rows) =
                                    session_stats_per_tool.get(*selected_tab)
                                {
                                    // sessions: 0..len-1, separator: len, totals: len+1
                                    table_state.select(Some(selected.saturating_sub(
                                        if selected == session_rows.len() + 1 {
                                            // Jump from totals back to last session (skip separator)
                                            2
                                        } else {
                                            1
                                        },
                                    )));
                                    needs_redraw = true;
                                }
                            }
                        }
                    }
                }
                KeyCode::Home => {
                    if let Some(table_state) = table_states.get_mut(*selected_tab) {
                        table_state.select(Some(0));
                        needs_redraw = true;
                    }
                }
                KeyCode::End => {
                    if let Some(table_state) = table_states.get_mut(*selected_tab) {
                        match *stats_view_mode {
                            StatsViewMode::Daily => {
                                if let Some(current_stats) = filtered_stats.get(*selected_tab) {
                                    let total_rows = current_stats.daily_stats.len() + 2;
                                    table_state.select(Some(total_rows.saturating_sub(1)));
                                    needs_redraw = true;
                                }
                            }
                            StatsViewMode::Session => {
                                if let Some(session_rows) =
                                    session_stats_per_tool.get(*selected_tab)
                                    && !session_rows.is_empty()
                                {
                                    // sessions + separator + totals
                                    let total_rows = session_rows.len() + 2;
                                    // Move to totals row (last row)
                                    table_state.select(Some(total_rows.saturating_sub(1)));
                                    needs_redraw = true;
                                }
                            }
                        }
                    }
                }
                KeyCode::PageDown => {
                    if let Some(table_state) = table_states.get_mut(*selected_tab)
                        && let Some(selected) = table_state.selected()
                    {
                        match *stats_view_mode {
                            StatsViewMode::Daily => {
                                if let Some(current_stats) = filtered_stats.get(*selected_tab) {
                                    let total_rows = current_stats.daily_stats.len() + 2;
                                    let new_selected =
                                        (selected + 10).min(total_rows.saturating_sub(1));
                                    table_state.select(Some(new_selected));
                                    needs_redraw = true;
                                }
                            }
                            StatsViewMode::Session => {
                                if let Some(session_rows) =
                                    session_stats_per_tool.get(*selected_tab)
                                    && !session_rows.is_empty()
                                {
                                    // sessions + separator + totals
                                    let total_rows = session_rows.len() + 2;
                                    let new_selected =
                                        (selected + 10).min(total_rows.saturating_sub(1));
                                    table_state.select(Some(new_selected));
                                    needs_redraw = true;
                                }
                            }
                        }
                    }
                }
                KeyCode::PageUp => {
                    if let Some(table_state) = table_states.get_mut(*selected_tab)
                        && let Some(selected) = table_state.selected()
                    {
                        let new_selected = selected.saturating_sub(10);
                        table_state.select(Some(new_selected));
                        needs_redraw = true;
                    }
                }
                KeyCode::Char('t') => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        *stats_view_mode = match *stats_view_mode {
                            StatsViewMode::Daily => StatsViewMode::Session,
                            StatsViewMode::Session => StatsViewMode::Daily,
                        };

                        if let StatsViewMode::Session = *stats_view_mode
                            && let Some(session_rows) = session_stats_per_tool.get(*selected_tab)
                            && let Some(table_state) = table_states.get_mut(*selected_tab)
                            && !session_rows.is_empty()
                        {
                            table_state.select(Some(session_rows.len().saturating_sub(1)));
                        }

                        needs_redraw = true;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn draw_ui(
    frame: &mut Frame,
    filtered_stats: &[&AgenticCodingToolStats],
    format_options: &NumberFormatOptions,
    ui_state: &mut UiState,
    upload_status: Arc<Mutex<UploadStatus>>,
    session_table_cache: &[SessionTableCache],
) {
    // Since we're already working with filtered stats, has_data is simply whether we have any stats
    let has_data = !filtered_stats.is_empty();

    // Check if we have an error to determine help area height
    let has_error = if let Ok(status) = upload_status.lock() {
        matches!(*status, UploadStatus::Failed(_))
    } else {
        false
    };

    // Adjust layout based on whether we have data or not
    let chunks = if has_data {
        Layout::vertical([
            Constraint::Length(3),                             // Header
            Constraint::Length(1),                             // Tabs
            Constraint::Min(3),                                // Main table
            Constraint::Length(9),                             // Summary stats
            Constraint::Length(if has_error { 3 } else { 1 }), // Help text
        ])
        .split(frame.area())
    } else {
        Layout::vertical([
            Constraint::Length(3), // Header
            Constraint::Min(3),    // No-data message
            Constraint::Length(1), // Help text
        ])
        .split(frame.area())
    };

    // Header
    let header = Paragraph::new(Text::from(vec![
        Line::styled(
            "AGENTIC DEVELOPMENT TOOL ACTIVITY ANALYSIS",
            Style::new().cyan().bold(),
        ),
        Line::styled(
            "==========================================",
            Style::new().cyan().bold(),
        ),
    ]));
    frame.render_widget(header, chunks[0]);

    if has_data {
        // Tabs
        let tab_titles: Vec<Line> = filtered_stats
            .iter()
            .map(|stats| {
                Line::from(format!(
                    " {} ({}) ",
                    stats.analyzer_name, stats.num_conversations
                ))
            })
            .collect();

        let tabs = Tabs::new(tab_titles)
            .select(ui_state.selected_tab)
            // .style(Style::default().add_modifier(Modifier::DIM))
            .highlight_style(Style::new().black().on_light_green())
            .padding("", "")
            .divider(" | ");

        frame.render_widget(tabs, chunks[1]);

        // Get current analyzer stats
        if let Some(current_stats) = filtered_stats.get(ui_state.selected_tab)
            && let Some(current_table_state) = ui_state.table_states.get_mut(ui_state.selected_tab)
        {
            // Main table
            match ui_state.stats_view_mode {
                StatsViewMode::Daily => {
                    draw_daily_stats_table(
                        frame,
                        chunks[2],
                        current_stats,
                        format_options,
                        current_table_state,
                    );
                }
                StatsViewMode::Session => {
                    if let Some(cache) = session_table_cache.get(ui_state.selected_tab) {
                        draw_session_stats_table(
                            frame,
                            chunks[2],
                            cache,
                            format_options,
                            current_table_state,
                            &mut ui_state.session_window_offsets[ui_state.selected_tab],
                        );
                    }
                }
            }

            // Summary stats - pass all filtered stats for aggregation
            draw_summary_stats(frame, chunks[3], filtered_stats, format_options);
        }

        // Help text for data view with upload status
        let help_area = chunks[4];

        // Split help area horizontally: help text on left, upload status on right
        let help_chunks = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Min(20), // Allow flexible space for error messages
        ])
        .split(help_area);

        let help_text = match ui_state.stats_view_mode {
            StatsViewMode::Daily => {
                "Use ←/→ or h/l to switch tabs, ↑/↓ or j/k to navigate, Ctrl+T for per-session view, q/Esc to quit"
            }
            StatsViewMode::Session => {
                "Use ←/→ or h/l to switch tabs, ↑/↓ or j/k to navigate, Ctrl+T for per-day view, q/Esc to quit"
            }
        };
        let help = Paragraph::new(help_text).style(Style::default().add_modifier(Modifier::DIM));
        frame.render_widget(help, help_chunks[0]);

        // Upload status in bottom-right
        if let Ok(status) = upload_status.lock() {
            let (status_text, status_style) = match &*status {
                UploadStatus::None => (String::new(), Style::default()),
                UploadStatus::Uploading {
                    current,
                    total,
                    dots,
                } => {
                    // Always show animated dots - ignore is_counting
                    let dots_str = match dots % 4 {
                        0 => "   ",
                        1 => ".  ",
                        2 => ".. ",
                        _ => "...",
                    };
                    (
                        format!(
                            "Uploading {}/{} messages{}",
                            format_number(*current as u64, format_options),
                            format_number(*total as u64, format_options),
                            dots_str
                        ),
                        Style::default().add_modifier(Modifier::DIM),
                    )
                }
                UploadStatus::Uploaded => (
                    "✓ Uploaded successfully".to_string(),
                    Style::default().fg(Color::Green),
                ),
                UploadStatus::Failed(error) => {
                    // Show full error message - let the widget handle wrapping/display
                    (format!("✕ {error}"), Style::default().fg(Color::Red))
                }
                UploadStatus::MissingApiToken => (
                    "No API token for uploading".to_string(),
                    Style::default().fg(Color::Yellow),
                ),
                UploadStatus::MissingServerUrl => (
                    "No server URL for uploading".to_string(),
                    Style::default().fg(Color::Yellow),
                ),
                UploadStatus::MissingConfig => (
                    "Upload config incomplete".to_string(),
                    Style::default().fg(Color::Yellow),
                ),
            };

            if !status_text.is_empty() {
                let status_widget = Paragraph::new(status_text)
                    .style(status_style)
                    .alignment(ratatui::layout::Alignment::Right)
                    .wrap(ratatui::widgets::Wrap { trim: true });
                frame.render_widget(status_widget, help_chunks[1]);
            }
        }
    } else {
        // No data message
        let no_data_message = Paragraph::new(Text::styled(
            "You don't have any agentic development tool data.  Once you start using Claude Code, Gemini CLI, or Codex CLI, you'll see some data here.",
            Style::default().add_modifier(Modifier::DIM),
        ));
        frame.render_widget(no_data_message, chunks[1]);

        // Help text for no-data view
        let help = Paragraph::new("Press q/Esc to quit")
            .style(Style::default().add_modifier(Modifier::DIM));
        frame.render_widget(help, chunks[2]);
    }
}

fn draw_daily_stats_table(
    frame: &mut Frame,
    area: Rect,
    stats: &AgenticCodingToolStats,
    format_options: &NumberFormatOptions,
    table_state: &mut TableState,
) -> usize {
    let header = Row::new(vec![
        Cell::new(""),
        Cell::new("Date"),
        Cell::new(Text::from("Cost").right_aligned()),
        Cell::new(Text::from("Cached Tks").right_aligned()),
        Cell::new(Text::from("Inp Tks").right_aligned()),
        Cell::new(Text::from("Outp Tks").right_aligned()),
        Cell::new(Text::from("Reason Tks").right_aligned()),
        Cell::new(Text::from("Convs").right_aligned()),
        Cell::new(Text::from("Tools").right_aligned()),
        // Cell::new(Text::from("Lines").right_aligned()),
        Cell::new("Models"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .height(1);

    // Find best values for highlighting
    // TODO: Let's refactor this.

    let mut best_cost = 0.0;
    let mut best_cost_i = 0;
    let mut best_cached_tokens = 0;
    let mut best_cached_tokens_i = 0;
    let mut best_input_tokens = 0;
    let mut best_input_tokens_i = 0;
    let mut best_output_tokens = 0;
    let mut best_output_tokens_i = 0;
    let mut best_reasoning_tokens = 0;
    let mut best_reasoning_tokens_i = 0;
    let mut best_conversations = 0;
    let mut best_conversations_i = 0;
    let mut best_tool_calls = 0;
    let mut best_tool_calls_i = 0;

    for (i, day_stats) in stats.daily_stats.values().enumerate() {
        if day_stats.stats.cost > best_cost {
            best_cost = day_stats.stats.cost;
            best_cost_i = i;
        }
        if day_stats.stats.cached_tokens > best_cached_tokens {
            best_cached_tokens = day_stats.stats.cached_tokens;
            best_cached_tokens_i = i;
        }
        if day_stats.stats.input_tokens > best_input_tokens {
            best_input_tokens = day_stats.stats.input_tokens;
            best_input_tokens_i = i;
        }
        if day_stats.stats.output_tokens > best_output_tokens {
            best_output_tokens = day_stats.stats.output_tokens;
            best_output_tokens_i = i;
        }
        if day_stats.stats.reasoning_tokens > best_reasoning_tokens {
            best_reasoning_tokens = day_stats.stats.reasoning_tokens;
            best_reasoning_tokens_i = i;
        }
        if day_stats.conversations > best_conversations {
            best_conversations = day_stats.conversations;
            best_conversations_i = i;
        }
        if day_stats.stats.tool_calls > best_tool_calls {
            best_tool_calls = day_stats.stats.tool_calls;
            best_tool_calls_i = i;
        }
    }

    let mut rows = Vec::new();
    let mut total_cost = 0.0;
    let mut total_cached = 0;
    let mut total_input = 0;
    let mut total_output = 0;
    let mut total_reasoning = 0;
    let mut total_tool_calls = 0;

    for (i, (date, day_stats)) in stats.daily_stats.iter().enumerate() {
        total_cost += day_stats.stats.cost;
        total_cached += day_stats.stats.cached_tokens;
        total_input += day_stats.stats.input_tokens;
        total_output += day_stats.stats.output_tokens;
        total_reasoning += day_stats.stats.reasoning_tokens;
        total_tool_calls += day_stats.stats.tool_calls;

        let mut models_vec = day_stats.models.keys().cloned().collect::<Vec<String>>();
        models_vec.sort();
        let models = models_vec.join(", ");

        let lines_summary = format!(
            "{}/{}/{}",
            format_number(day_stats.stats.lines_read, format_options),
            format_number(day_stats.stats.lines_edited, format_options),
            format_number(day_stats.stats.lines_added, format_options)
        );

        // Check if this is an empty row
        let is_empty_row = day_stats.stats.cost == 0.0
            && day_stats.stats.cached_tokens == 0
            && day_stats.stats.input_tokens == 0
            && day_stats.stats.output_tokens == 0
            && day_stats.stats.reasoning_tokens == 0
            && day_stats.conversations == 0
            && day_stats.user_messages == 0
            && day_stats.ai_messages == 0
            && day_stats.stats.tool_calls == 0;

        // Create styled cells with colors matching original implementation
        let date_cell = if is_empty_row {
            Line::from(Span::styled(
                format_date_for_display(date),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else {
            Line::from(Span::raw(format_date_for_display(date)))
        };

        let cost_cell = if is_empty_row {
            Line::from(Span::styled(
                format!("${:.2}", day_stats.stats.cost),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_cost_i {
            Line::from(Span::styled(
                format!("${:.2}", day_stats.stats.cost),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::styled(
                format!("${:.2}", day_stats.stats.cost),
                Style::default().fg(Color::Yellow),
            ))
        }
        .right_aligned();

        let cached_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.stats.cached_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_cached_tokens_i {
            Line::from(Span::styled(
                format_number(day_stats.stats.cached_tokens, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::styled(
                format_number(day_stats.stats.cached_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        }
        .right_aligned();

        let input_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.stats.input_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_input_tokens_i {
            Line::from(Span::styled(
                format_number(day_stats.stats.input_tokens, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::raw(format_number(
                day_stats.stats.input_tokens,
                format_options,
            )))
        }
        .right_aligned();

        let output_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.stats.output_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_output_tokens_i {
            Line::from(Span::styled(
                format_number(day_stats.stats.output_tokens, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::raw(format_number(
                day_stats.stats.output_tokens,
                format_options,
            )))
        }
        .right_aligned();

        let reasoning_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.stats.reasoning_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_reasoning_tokens_i {
            Line::from(Span::styled(
                format_number(day_stats.stats.reasoning_tokens, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::raw(format_number(
                day_stats.stats.reasoning_tokens,
                format_options,
            )))
        }
        .right_aligned();

        let conv_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.conversations as u64, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_conversations_i {
            Line::from(Span::styled(
                format_number(day_stats.conversations as u64, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::raw(format_number(
                day_stats.conversations as u64,
                format_options,
            )))
        }
        .right_aligned();

        let tool_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.stats.tool_calls as u64, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_tool_calls_i {
            Line::from(Span::styled(
                format_number(day_stats.stats.tool_calls as u64, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::styled(
                format_number(day_stats.stats.tool_calls as u64, format_options),
                Style::default().fg(Color::Green),
            ))
        }
        .right_aligned();

        let _lines_cell = if is_empty_row {
            Line::from(Span::styled(
                lines_summary,
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else {
            Line::from(Span::styled(
                lines_summary,
                Style::default().fg(Color::Blue),
            ))
        }
        .right_aligned();

        let models_cell = Line::from(Span::styled(
            models,
            Style::default().add_modifier(Modifier::DIM),
        ));

        // Create arrow indicator for currently selected row
        let arrow_cell = if table_state.selected() == Some(i) {
            Line::from(Span::styled(
                "→",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(Span::raw(""))
        };

        let row = Row::new(vec![
            arrow_cell,
            date_cell,
            cost_cell,
            cached_cell,
            input_cell,
            output_cell,
            reasoning_cell,
            conv_cell,
            tool_cell,
            // lines_cell,
            models_cell,
        ]);

        rows.push(row);
    }

    // Collect all unique models for the totals row
    let mut all_models = std::collections::HashSet::new();
    for day_stats in stats.daily_stats.values() {
        for model in day_stats.models.keys() {
            all_models.insert(model);
        }
    }
    let mut all_models_vec = all_models
        .iter()
        .map(|k| k.to_string())
        .collect::<Vec<String>>();
    all_models_vec.sort();
    let all_models_text = all_models_vec.join(", ");

    // Add separator row before totals
    let separator_row = Row::new(vec![
        Line::from(Span::styled(
            "",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "───────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "──────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "────────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "─────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "──────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "──────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "──────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        /*
        Line::from(Span::styled(
            "───────────────────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        */
        Line::from(Span::styled(
            "─".repeat(all_models_text.len().max(18)),
            Style::default().add_modifier(Modifier::DIM),
        )),
    ]);
    rows.push(separator_row);

    // Add totals row
    let _total_lines_r = stats
        .daily_stats
        .values()
        .map(|s| s.stats.lines_read)
        .sum::<u64>();
    let _total_lines_e = stats
        .daily_stats
        .values()
        .map(|s| s.stats.lines_edited)
        .sum::<u64>();
    let _total_lines_a = stats
        .daily_stats
        .values()
        .map(|s| s.stats.lines_added)
        .sum::<u64>();

    let totals_row = Row::new(vec![
        // Arrow indicator for totals row when selected
        if table_state.selected() == Some(rows.len()) {
            Line::from(Span::styled(
                "→",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(Span::raw(""))
        },
        Line::from(Span::styled(
            format!("Total ({}d)", stats.daily_stats.len()),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("${total_cost:.2}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        Line::from(Span::styled(
            format_number(total_cached, format_options),
            Style::default()
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        Line::from(Span::styled(
            format_number(total_input, format_options),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        Line::from(Span::styled(
            format_number(total_output, format_options),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        Line::from(Span::styled(
            format_number(total_reasoning, format_options),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        Line::from(Span::styled(
            format_number(stats.num_conversations, format_options),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        Line::from(Span::styled(
            format_number(total_tool_calls as u64, format_options),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        /*
        Line::from(Span::styled(
            format!(
                "{}/{}/{}",
                format_number(total_lines_r, format_options),
                format_number(total_lines_e, format_options),
                format_number(total_lines_a, format_options)
            ),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        */
        Line::from(Span::styled(
            all_models_text,
            Style::default().add_modifier(Modifier::DIM),
        )),
    ]);

    rows.push(totals_row);

    // Save the row count before moving rows into the table
    let total_rows = rows.len();

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),  // Arrow
            Constraint::Length(11), // Date
            Constraint::Length(10), // Cost
            Constraint::Length(12), // Cached
            Constraint::Length(8),  // Input
            Constraint::Length(9),  // Output
            Constraint::Length(11), // Reasoning
            Constraint::Length(6),  // Convs
            Constraint::Length(6),  // Tools
            // Constraint::Length(23), // Lines
            Constraint::Min(10), // Models
        ],
    )
    .header(header)
    .block(Block::default().title(""))
    .row_highlight_style(Style::new().blue())
    .column_spacing(2);

    frame.render_stateful_widget(table, area, table_state);

    // Return the total number of rows in the table
    total_rows
}

fn draw_session_stats_table(
    frame: &mut Frame,
    area: Rect,
    cache: &SessionTableCache,
    format_options: &NumberFormatOptions,
    table_state: &mut TableState,
    window_offset: &mut usize,
) {
    let header = Row::new(vec![
        Cell::new(""),
        Cell::new("Session"),
        Cell::new("Started"),
        Cell::new("Tool"),
        Cell::new(Text::from("Cost").right_aligned()),
        Cell::new(Text::from("Inp Tks").right_aligned()),
        Cell::new(Text::from("Outp Tks").right_aligned()),
        Cell::new(Text::from("Total Tks").right_aligned()),
        Cell::new(Text::from("Tools").right_aligned()),
        Cell::new("Models"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .height(1);

    let total_session_rows = cache.sessions.len();
    // Total rows in the table body: sessions + optional separator + totals row
    let total_rows = if total_session_rows > 0 {
        total_session_rows + 2
    } else {
        1 // Only totals row when there are no sessions
    };

    let selected_global = table_state
        .selected()
        .unwrap_or(0)
        .min(total_rows.saturating_sub(1));

    // Estimate how many rows fit: header takes 1 row, keep the rest for body.
    let max_body_rows = area
        .height
        .saturating_sub(1)
        .max(1) as usize;

    // Render only a window that keeps the selection visible; maintain offset unless we hit edges.
    let mut window_start = if total_rows > 0 {
        (*window_offset).min(total_rows.saturating_sub(1))
    } else {
        0
    };

    if total_rows > max_body_rows {
        if selected_global < window_start {
            window_start = selected_global;
        } else if selected_global >= window_start + max_body_rows {
            window_start = selected_global + 1 - max_body_rows;
        }
    } else {
        window_start = 0;
    }

    *window_offset = window_start;
    let window_end = (window_start + max_body_rows).min(total_rows);

    let mut rows = Vec::new();

    for i in window_start..window_end {
        if i < total_session_rows {
            let session = &cache.sessions[i];

            let session_display_name = session
                .session_name
                .clone()
                .unwrap_or_else(|| session.session_id.clone());

            // Truncate by characters, not bytes, to avoid panicking on multi-byte UTF-8
            let short_id = if session_display_name.chars().count() > 30 {
                let truncated: String = session_display_name.chars().take(30).collect();
                format!("{truncated}…")
            } else {
                session_display_name
            };

            let local_ts = session.first_timestamp.with_timezone(&Local);
            let ts_str = local_ts.format("%Y-%m-%d %H:%M").to_string();

            let session_cell = Line::from(Span::styled(
                short_id,
                Style::default().add_modifier(Modifier::DIM),
            ));

            let started_cell = Line::from(Span::raw(ts_str));

            let tool_cell = Line::from(Span::styled(
                session.analyzer_name.clone(),
                Style::default().fg(Color::Cyan),
            ));

            let cost_cell = if cache.best_cost_i == Some(i) {
                Line::from(Span::styled(
                    format!("${:.2}", session.stats.cost),
                    Style::default().fg(Color::Red),
                ))
            } else {
                Line::from(Span::styled(
                    format!("${:.2}", session.stats.cost),
                    Style::default().fg(Color::Yellow),
                ))
            }
            .right_aligned();

            let input_cell = Line::from(Span::raw(format_number(
                session.stats.input_tokens,
                format_options,
            )))
            .right_aligned();

            let output_cell = Line::from(Span::raw(format_number(
                session.stats.output_tokens,
                format_options,
            )))
            .right_aligned();

            let total_tokens = session.stats.input_tokens
                + session.stats.output_tokens
                + session.stats.cached_tokens;

            let total_cell = if cache.best_total_tokens_i == Some(i) {
                Line::from(Span::styled(
                    format_number(total_tokens, format_options),
                    Style::default().fg(Color::Green),
                ))
            } else {
                Line::from(Span::raw(format_number(total_tokens, format_options)))
            }
            .right_aligned();

            let tools_cell = Line::from(Span::styled(
                format_number(session.stats.tool_calls as u64, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
            .right_aligned();

            // Per-session models column: sorted, deduplicated list of models used in this session
            let mut models_vec = session.models.clone();
            models_vec.sort();
            models_vec.dedup();
            let models_text = models_vec.join(", ");

            let models_cell = Line::from(Span::styled(
                models_text,
                Style::default().add_modifier(Modifier::DIM),
            ));

            let row = Row::new(vec![
                Line::from(Span::raw("")),
                session_cell,
                started_cell,
                tool_cell,
                cost_cell,
                input_cell,
                output_cell,
                total_cell,
                tools_cell,
                models_cell,
            ]);

            rows.push(row);
        } else if i == total_session_rows && total_session_rows > 0 {
            // Separator row
            let separator_row = Row::new(vec![
                Line::from(Span::styled(
                    "",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "────────────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "─────────────────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "──────────────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "──────────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "────────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "─────────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "────────────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "──────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(Span::styled(
                    "────────────",
                    Style::default().add_modifier(Modifier::DIM),
                )),
            ]);
            rows.push(separator_row);
        } else {
            // Totals row
            let totals_row = Row::new(vec![
                Line::from(Span::raw("")),
                Line::from(Span::styled(
                    format!("Total ({} sessions)", total_session_rows),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::raw("")),
                Line::from(Span::raw("")),
                Line::from(Span::styled(
                    format!("${:.2}", cache.total_cost),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(cache.total_input_tokens, format_options),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(cache.total_output_tokens, format_options),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(
                        cache.total_input_tokens
                            + cache.total_output_tokens
                            + cache.total_cached_tokens,
                        format_options,
                    ),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(cache.total_tool_calls, format_options),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    cache.all_models_text.clone(),
                    Style::default().add_modifier(Modifier::DIM),
                )),
            ]);
            rows.push(totals_row);
        }
    }

    let mut render_state = TableState::default();
    render_state.select(Some(selected_global.saturating_sub(window_start)));

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),  // Arrow / highlight symbol space
            Constraint::Length(32), // Session (increased width for name)
            Constraint::Length(17), // Started
            Constraint::Length(14), // Tool
            Constraint::Length(10), // Cost
            Constraint::Length(8),  // Input
            Constraint::Length(9),  // Output
            Constraint::Length(11), // Total tokens
            Constraint::Length(6),  // Tools
            Constraint::Min(10),    // Models
        ],
    )
    .header(header)
    .block(Block::default().title(""))
    .highlight_symbol("→")
    .row_highlight_style(Style::new().blue())
    .column_spacing(2);

    frame.render_stateful_widget(table, area, &mut render_state);
}

fn draw_summary_stats(
    frame: &mut Frame,
    area: Rect,
    filtered_stats: &[&AgenticCodingToolStats],
    format_options: &NumberFormatOptions,
) {
    // Aggregate stats from all tools
    let mut total_cost: f64 = 0.0;
    let mut total_cached: u64 = 0;
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_reasoning: u64 = 0;
    let mut total_tool_calls: u64 = 0;
    let mut all_days = std::collections::HashSet::new();

    for stats in filtered_stats {
        total_cost += stats
            .daily_stats
            .values()
            .map(|s| s.stats.cost)
            .sum::<f64>();
        total_cached += stats
            .daily_stats
            .values()
            .map(|s| s.stats.cached_tokens)
            .sum::<u64>();
        total_input += stats
            .daily_stats
            .values()
            .map(|s| s.stats.input_tokens)
            .sum::<u64>();
        total_output += stats
            .daily_stats
            .values()
            .map(|s| s.stats.output_tokens)
            .sum::<u64>();
        total_reasoning += stats
            .daily_stats
            .values()
            .map(|s| s.stats.reasoning_tokens)
            .sum::<u64>();
        total_tool_calls += stats
            .daily_stats
            .values()
            .map(|s| s.stats.tool_calls as u64)
            .sum::<u64>();

        // Collect unique days across all tools that have actual data
        for (day, day_stats) in &stats.daily_stats {
            if day_stats.stats.cost > 0.0
                || day_stats.stats.input_tokens > 0
                || day_stats.stats.output_tokens > 0
                || day_stats.stats.reasoning_tokens > 0
                || day_stats.stats.cached_tokens > 0
                || day_stats.stats.tool_calls > 0
                || day_stats.ai_messages > 0
                || day_stats.conversations > 0
            {
                all_days.insert(day);
            }
        }
    }

    let total_tokens = total_cached + total_input + total_output;
    let tools_count = filtered_stats.len();

    // Define summary rows with labels and values
    let summary_rows = vec![
        ("Tools:", format!("{tools_count} tracked"), Color::Cyan),
        (
            "Tokens:",
            format_number(total_tokens, format_options),
            Color::LightBlue,
        ),
        (
            "Reasoning:",
            format_number(total_reasoning, format_options),
            Color::Red,
        ),
        (
            "Tool Calls:",
            format_number(total_tool_calls, format_options),
            Color::LightGreen,
        ),
        ("Cost:", format!("${total_cost:.2}"), Color::LightYellow),
        ("Days tracked:", all_days.len().to_string(), Color::White),
    ];

    // Find the maximum label width for alignment
    let max_label_width = summary_rows
        .iter()
        .map(|(label, _, _)| label.len())
        .max()
        .unwrap_or(0);

    // Create lines with consistent spacing
    let mut summary_lines: Vec<Line> = summary_rows
        .into_iter()
        .map(|(label, value, color)| {
            Line::from(vec![
                Span::raw(format!("{label:<max_label_width$}")),
                Span::raw("      "), // 6 spaces between label and value
                Span::styled(value, Style::new().fg(color).bold()),
            ])
        })
        .collect();

    summary_lines.insert(
        0,
        Line::from(vec![Span::styled(
            "-----------------------------",
            Style::default().dim(),
        )]),
    );
    summary_lines.insert(
        0,
        Line::from(vec![Span::styled("Totals", Style::default().bold().dim())]),
    );

    let summary_widget =
        Paragraph::new(Text::from(summary_lines)).block(Block::default().title(""));
    frame.render_widget(summary_widget, area);
}

fn update_table_states(
    table_states: &mut Vec<TableState>,
    current_stats: &MultiAnalyzerStats,
    selected_tab: &mut usize,
) {
    let filtered_count = current_stats
        .analyzer_stats
        .iter()
        .filter(|stats| has_data(stats))
        .count();

    // Preserve existing table states when resizing
    let old_states = table_states.clone();
    table_states.clear();

    for i in 0..filtered_count {
        let state = if i < old_states.len() {
            // Preserve existing state if available
            old_states[i].clone()
        } else {
            // Create new state for new analyzers
            let mut new_state = TableState::default();
            new_state.select(Some(0));
            new_state
        };
        table_states.push(state);
    }

    // Ensure selected tab is within bounds
    if *selected_tab >= filtered_count && filtered_count > 0 {
        *selected_tab = filtered_count - 1;
    }
}

fn update_window_offsets(window_offsets: &mut Vec<usize>, filtered_count: &usize) {
    let old = window_offsets.clone();
    window_offsets.clear();

    for i in 0..*filtered_count {
        if i < old.len() {
            window_offsets.push(old[i]);
        } else {
            window_offsets.push(0);
        }
    }
}

pub fn create_upload_progress_callback(
    format_options: &NumberFormatOptions,
) -> impl Fn(usize, usize) + '_ {
    static LAST_CURRENT: AtomicUsize = AtomicUsize::new(0);
    static DOTS: AtomicUsize = AtomicUsize::new(0);
    static LAST_DOTS_UPDATE: AtomicU64 = AtomicU64::new(0);

    move |current: usize, total: usize| {
        let last = LAST_CURRENT.load(Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let last_update = LAST_DOTS_UPDATE.load(Ordering::Relaxed);

        let mut should_update = false;

        if current != last {
            // Progress changed - update current but keep dots timing
            LAST_CURRENT.store(current, Ordering::Relaxed);
            should_update = true;
        }

        if now - last_update >= 500 {
            // 500ms between dot updates
            // Enough time passed - advance dots animation
            let dots = DOTS.load(Ordering::Relaxed);
            DOTS.store((dots + 1) % 4, Ordering::Relaxed);
            LAST_DOTS_UPDATE.store(now, Ordering::Relaxed);
            should_update = true;
        }

        if should_update {
            let current_dots = DOTS.load(Ordering::Relaxed);
            let dots_str = ".".repeat(current_dots);
            print!(
                "\r\x1b[KUploading {}/{} messages{}",
                format_number(current as u64, format_options),
                format_number(total as u64, format_options),
                dots_str
            );
            let _ = Write::flush(&mut stdout());
        }
    }
}

pub fn show_upload_success(total: usize, format_options: &NumberFormatOptions) {
    let _ = execute!(
        stdout(),
        Print("\r"),
        SetForegroundColor(crossterm::style::Color::DarkGreen),
        Print(format!(
            "✓ Successfully uploaded {} messages\n",
            format_number(total as u64, format_options)
        )),
        ResetColor
    );
}

pub fn show_upload_error(error: &str) {
    let _ = execute!(
        stdout(),
        Print("\r"),
        SetForegroundColor(crossterm::style::Color::DarkRed),
        Print(format!("✕ {error}\n")),
        ResetColor
    );
}
