pub mod logic;
#[cfg(test)]
mod tests;

use crate::models::is_model_estimated;
use crate::types::{AnalyzerStatsView, MultiAnalyzerStatsView, SharedAnalyzerView};
use crate::utils::{NumberFormatOptions, format_date_for_display, format_number};
use crate::watcher::{FileWatcher, RealtimeStatsManager, WatcherEvent};
use anyhow::Result;
use chrono::Local;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::style::{Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, execute};
use logic::{SessionAggregate, date_matches_buffer, has_data_shared};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Tabs};
use ratatui::{Frame, Terminal};
use std::collections::HashSet;
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

/// A simple Either type for iterators to avoid allocation when reversing.
enum EitherIter<L, R> {
    Left(L),
    Right(R),
}

impl<L, R, T> Iterator for EitherIter<L, R>
where
    L: Iterator<Item = T>,
    R: Iterator<Item = T>,
{
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            EitherIter::Left(l) => l.next(),
            EitherIter::Right(r) => r.next(),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            EitherIter::Left(l) => l.size_hint(),
            EitherIter::Right(r) => r.size_hint(),
        }
    }
}

struct UiState<'a> {
    table_states: &'a mut [TableState],
    _scroll_offset: usize,
    selected_tab: usize,
    stats_view_mode: StatsViewMode,
    session_window_offsets: &'a mut [usize],
    session_day_filters: &'a mut [Option<String>],
    date_jump_active: bool,
    date_jump_buffer: &'a str,
    sort_reversed: bool,
    show_totals: bool,
}

pub fn run_tui(
    stats_receiver: watch::Receiver<MultiAnalyzerStatsView>,
    format_options: &NumberFormatOptions,
    upload_status: Arc<Mutex<UploadStatus>>,
    update_status: Arc<Mutex<crate::version_check::UpdateStatus>>,
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
        // Persist cache when TUI exits
        stats_manager.persist_cache();
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
            update_status,
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
    mut stats_receiver: watch::Receiver<MultiAnalyzerStatsView>,
    format_options: &NumberFormatOptions,
    selected_tab: &mut usize,
    scroll_offset: &mut usize,
    stats_view_mode: &mut StatsViewMode,
    upload_status: Arc<Mutex<UploadStatus>>,
    update_status: Arc<Mutex<crate::version_check::UpdateStatus>>,
    file_watcher: FileWatcher,
    watcher_tx: mpsc::UnboundedSender<WatcherEvent>,
) -> Result<()> {
    let mut table_states: Vec<TableState> = Vec::new();
    let mut session_window_offsets: Vec<usize> = Vec::new();
    let mut session_day_filters: Vec<Option<String>> = Vec::new();
    let mut date_jump_active = false;
    let mut date_jump_buffer = String::new();
    let mut sort_reversed = false;
    let mut show_totals = true;
    let mut current_stats = stats_receiver.borrow().clone();

    // Initialize table states for current stats
    update_table_states(&mut table_states, &current_stats, selected_tab);
    update_window_offsets(&mut session_window_offsets, &table_states.len());
    update_day_filters(&mut session_day_filters, &table_states.len());

    let mut needs_redraw = true;
    let mut last_upload_status = {
        let status = upload_status.lock().unwrap();
        format!("{:?}", *status)
    };
    let mut last_update_status = {
        let status = update_status.lock().unwrap();
        format!("{:?}", *status)
    };
    let mut dots_counter = 0; // Counter for dots animation (advance every 5 frames = 500ms)

    // Filter analyzer stats to only include those with data - calculate once and update when stats change
    // SharedAnalyzerView = Arc<RwLock<AnalyzerStatsView>> - clone is cheap (just Arc pointer)
    let mut filtered_stats: Vec<SharedAnalyzerView> = current_stats
        .analyzer_stats
        .iter()
        .filter(|stats| has_data_shared(stats))
        .cloned()
        .collect();

    loop {
        // Check for update status changes
        let current_update_status = {
            let status = update_status.lock().unwrap();
            format!("{:?}", *status)
        };
        if current_update_status != last_update_status {
            last_update_status = current_update_status;
            needs_redraw = true;
        }

        // Check for stats updates
        if stats_receiver.has_changed()? {
            current_stats = stats_receiver.borrow_and_update().clone();
            // Recalculate filtered stats only when stats change
            filtered_stats = current_stats
                .analyzer_stats
                .iter()
                .filter(|stats| has_data_shared(stats))
                .cloned()
                .collect();
            update_table_states(&mut table_states, &current_stats, selected_tab);
            update_window_offsets(&mut session_window_offsets, &table_states.len());
            update_day_filters(&mut session_day_filters, &table_states.len());

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
                    session_day_filters: &mut session_day_filters,
                    date_jump_active,
                    date_jump_buffer: &date_jump_buffer,
                    sort_reversed,
                    show_totals,
                };
                draw_ui(
                    frame,
                    &filtered_stats,
                    format_options,
                    &mut ui_state,
                    upload_status.clone(),
                    update_status.clone(),
                );
            })?;
            needs_redraw = false;
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

            // Handle update notification dismissal
            if matches!(key.code, KeyCode::Char('u'))
                && let Ok(mut status) = update_status.lock()
                && matches!(
                    *status,
                    crate::version_check::UpdateStatus::Available { .. }
                )
            {
                *status = crate::version_check::UpdateStatus::Dismissed;
                needs_redraw = true;
            }

            // Only handle navigation keys if we have data (`filtered_stats` is non-empty).
            if filtered_stats.is_empty() {
                continue;
            }

            if date_jump_active {
                match key.code {
                    KeyCode::Char(c) if c.is_ascii_alphanumeric() || c == '-' || c == '/' => {
                        date_jump_buffer.push(c);
                        // Auto-jump to first matching date
                        if let Some(current_stats) = filtered_stats.get(*selected_tab)
                            && let Some(table_state) = table_states.get_mut(*selected_tab)
                        {
                            let stats = current_stats.read();
                            if let Some((index, _)) =
                                stats.daily_stats.iter().enumerate().find(|(_, (day, _))| {
                                    date_matches_buffer(day, &date_jump_buffer)
                                })
                            {
                                table_state.select(Some(index));
                            }
                        }
                        needs_redraw = true;
                    }
                    KeyCode::Backspace => {
                        date_jump_buffer.pop();
                        // Re-evaluate match after backspace
                        if let Some(current_stats) = filtered_stats.get(*selected_tab)
                            && let Some(table_state) = table_states.get_mut(*selected_tab)
                        {
                            let stats = current_stats.read();
                            if let Some((index, _)) =
                                stats.daily_stats.iter().enumerate().find(|(_, (day, _))| {
                                    date_matches_buffer(day, &date_jump_buffer)
                                })
                            {
                                table_state.select(Some(index));
                            }
                        }
                        needs_redraw = true;
                    }
                    KeyCode::Enter | KeyCode::Esc => {
                        date_jump_active = false;
                        date_jump_buffer.clear();
                        needs_redraw = true;
                    }
                    _ => {}
                }
                continue;
            }

            match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    if *selected_tab > 0 {
                        *selected_tab -= 1;

                        if let StatsViewMode::Session = *stats_view_mode
                            && let Some(table_state) = table_states.get_mut(*selected_tab)
                            && let Some(view) = filtered_stats.get(*selected_tab)
                        {
                            let view = view.read();
                            let target_len = match session_day_filters
                                .get(*selected_tab)
                                .and_then(|f| f.as_ref())
                            {
                                Some(day) => view
                                    .session_aggregates
                                    .iter()
                                    .filter(|s| &s.day_key == day)
                                    .count(),
                                None => view.session_aggregates.len(),
                            };
                            if target_len > 0 {
                                table_state.select(Some(target_len.saturating_sub(1)));
                            }
                        }

                        needs_redraw = true;
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if *selected_tab < filtered_stats.len().saturating_sub(1) {
                        *selected_tab += 1;

                        if let StatsViewMode::Session = *stats_view_mode
                            && let Some(table_state) = table_states.get_mut(*selected_tab)
                            && let Some(view) = filtered_stats.get(*selected_tab)
                        {
                            let view = view.read();
                            let target_len = match session_day_filters
                                .get(*selected_tab)
                                .and_then(|f| f.as_ref())
                            {
                                Some(day) => view
                                    .session_aggregates
                                    .iter()
                                    .filter(|s| &s.day_key == day)
                                    .count(),
                                None => view.session_aggregates.len(),
                            };
                            if target_len > 0 {
                                table_state.select(Some(target_len.saturating_sub(1)));
                            }
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
                                    let view = current_stats.read();
                                    let total_rows = view.daily_stats.len();
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
                                let filtered_len = filtered_stats
                                    .get(*selected_tab)
                                    .map(|view| {
                                        let v = view.read();
                                        session_day_filters
                                            .get(*selected_tab)
                                            .and_then(|f| f.as_ref())
                                            .map(|day| {
                                                v.session_aggregates
                                                    .iter()
                                                    .filter(|s| &s.day_key == day)
                                                    .count()
                                            })
                                            .unwrap_or_else(|| v.session_aggregates.len())
                                    })
                                    .unwrap_or(0);

                                if filtered_len > 0 && selected < filtered_len.saturating_add(1) {
                                    // sessions: 0..len-1, separator: len, totals: len+1
                                    table_state.select(Some(
                                        if selected == filtered_len.saturating_sub(1) {
                                            selected + 2
                                        } else {
                                            selected + 1
                                        },
                                    ));
                                    needs_redraw = true;
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
                                    let view = current_stats.read();
                                    table_state.select(Some(selected.saturating_sub(
                                        if selected == view.daily_stats.len() + 1 {
                                            2
                                        } else {
                                            1
                                        },
                                    )));
                                    needs_redraw = true;
                                }
                            }
                            StatsViewMode::Session => {
                                let filtered_len = filtered_stats
                                    .get(*selected_tab)
                                    .map(|view| {
                                        let v = view.read();
                                        session_day_filters
                                            .get(*selected_tab)
                                            .and_then(|f| f.as_ref())
                                            .map(|day| {
                                                v.session_aggregates
                                                    .iter()
                                                    .filter(|s| &s.day_key == day)
                                                    .count()
                                            })
                                            .unwrap_or_else(|| v.session_aggregates.len())
                                    })
                                    .unwrap_or(0);

                                // sessions: 0..len-1, separator: len, totals: len+1
                                table_state.select(Some(selected.saturating_sub(
                                    if selected == filtered_len.saturating_add(1) {
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
                                    let view = current_stats.read();
                                    let total_rows = view.daily_stats.len() + 2;
                                    table_state.select(Some(total_rows.saturating_sub(1)));
                                    needs_redraw = true;
                                }
                            }
                            StatsViewMode::Session => {
                                let filtered_len = filtered_stats
                                    .get(*selected_tab)
                                    .map(|view| {
                                        let v = view.read();
                                        session_day_filters
                                            .get(*selected_tab)
                                            .and_then(|f| f.as_ref())
                                            .map(|day| {
                                                v.session_aggregates
                                                    .iter()
                                                    .filter(|s| &s.day_key == day)
                                                    .count()
                                            })
                                            .unwrap_or_else(|| v.session_aggregates.len())
                                    })
                                    .unwrap_or(0);

                                if filtered_len > 0 {
                                    let total_rows = filtered_len + 2;
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
                                    let view = current_stats.read();
                                    let total_rows = view.daily_stats.len() + 2;
                                    let new_selected =
                                        (selected + 10).min(total_rows.saturating_sub(1));
                                    table_state.select(Some(new_selected));
                                    needs_redraw = true;
                                }
                            }
                            StatsViewMode::Session => {
                                let filtered_len = filtered_stats
                                    .get(*selected_tab)
                                    .map(|view| {
                                        let v = view.read();
                                        session_day_filters
                                            .get(*selected_tab)
                                            .and_then(|f| f.as_ref())
                                            .map(|day| {
                                                v.session_aggregates
                                                    .iter()
                                                    .filter(|s| &s.day_key == day)
                                                    .count()
                                            })
                                            .unwrap_or_else(|| v.session_aggregates.len())
                                    })
                                    .unwrap_or(0);

                                if filtered_len > 0 {
                                    let total_rows = filtered_len + 2;
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
                KeyCode::Char('/') => {
                    if let StatsViewMode::Daily = *stats_view_mode {
                        date_jump_active = true;
                        date_jump_buffer.clear();
                        needs_redraw = true;
                    }
                }
                KeyCode::Char('t') => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        *stats_view_mode = match *stats_view_mode {
                            StatsViewMode::Daily => {
                                session_day_filters[*selected_tab] = None;
                                StatsViewMode::Session
                            }
                            StatsViewMode::Session => StatsViewMode::Daily,
                        };

                        date_jump_active = false;
                        date_jump_buffer.clear();

                        if let StatsViewMode::Session = *stats_view_mode
                            && let Some(table_state) = table_states.get_mut(*selected_tab)
                            && let Some(view) = filtered_stats.get(*selected_tab)
                        {
                            let v = view.read();
                            if !v.session_aggregates.is_empty() {
                                let target_len = session_day_filters
                                    .get(*selected_tab)
                                    .and_then(|f| f.as_ref())
                                    .map(|day| {
                                        v.session_aggregates
                                            .iter()
                                            .filter(|s| &s.day_key == day)
                                            .count()
                                    })
                                    .unwrap_or_else(|| v.session_aggregates.len());
                                if target_len > 0 {
                                    table_state.select(Some(target_len.saturating_sub(1)));
                                }
                            }
                        }

                        needs_redraw = true;
                    }
                }
                KeyCode::Enter => {
                    if let StatsViewMode::Daily = *stats_view_mode
                        && let Some(current_stats) = filtered_stats.get(*selected_tab)
                        && let Some(table_state) = table_states.get_mut(*selected_tab)
                        && let Some(selected_idx) = table_state.selected()
                    {
                        let view = current_stats.read();
                        if selected_idx < view.daily_stats.len() {
                            let day_key = if sort_reversed {
                                view.daily_stats.iter().rev().nth(selected_idx)
                            } else {
                                view.daily_stats.iter().nth(selected_idx)
                            }
                            .map(|(k, _)| k.clone());
                            if let Some(day_key) = day_key {
                                session_day_filters[*selected_tab] = Some(day_key);
                                *stats_view_mode = StatsViewMode::Session;
                                session_window_offsets[*selected_tab] = 0;
                                table_state.select(Some(0));
                                needs_redraw = true;
                            }
                        }
                    }
                }
                KeyCode::Char('r') => {
                    sort_reversed = !sort_reversed;
                    needs_redraw = true;
                }
                KeyCode::Char('s') => {
                    show_totals = !show_totals;
                    needs_redraw = true;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn draw_ui(
    frame: &mut Frame,
    filtered_stats: &[SharedAnalyzerView],
    format_options: &NumberFormatOptions,
    ui_state: &mut UiState,
    upload_status: Arc<Mutex<UploadStatus>>,
    update_status: Arc<Mutex<crate::version_check::UpdateStatus>>,
) {
    // Since we're already working with filtered stats, has_data is simply whether we have any stats
    let has_data = !filtered_stats.is_empty();

    // Check if we have an error to determine help area height
    let has_error = if let Ok(status) = upload_status.lock() {
        matches!(*status, UploadStatus::Failed(_))
    } else {
        false
    };

    // Check if update is available
    let show_update_banner = if let Ok(status) = update_status.lock() {
        matches!(
            *status,
            crate::version_check::UpdateStatus::Available { .. }
        )
    } else {
        false
    };

    // Adjust layout based on whether we have data and update banner
    let (chunks, chunk_offset) = if has_data {
        if show_update_banner {
            let mut constraints = vec![
                Constraint::Length(3), // Header
                Constraint::Length(1), // Update banner
                Constraint::Length(1), // Tabs
                Constraint::Min(3),    // Main table
            ];
            if ui_state.show_totals {
                constraints.push(Constraint::Length(9)); // Summary stats
            }
            constraints.push(Constraint::Length(if has_error { 4 } else { 2 })); // Help text
            (
                Layout::vertical(constraints).split(frame.area()),
                1, // Offset for banner
            )
        } else {
            let mut constraints = vec![
                Constraint::Length(3), // Header
                Constraint::Length(1), // Tabs
                Constraint::Min(3),    // Main table
            ];
            if ui_state.show_totals {
                constraints.push(Constraint::Length(9)); // Summary stats
            }
            constraints.push(Constraint::Length(if has_error { 4 } else { 2 })); // Help text
            (
                Layout::vertical(constraints).split(frame.area()),
                0, // No offset
            )
        }
    } else {
        (
            Layout::vertical([
                Constraint::Length(3), // Header
                Constraint::Min(3),    // No-data message
                Constraint::Length(1), // Help text
            ])
            .split(frame.area()),
            0, // No offset (no banner in no-data view)
        )
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

    // Update banner (if showing)
    if show_update_banner
        && let Ok(status) = update_status.lock()
        && let crate::version_check::UpdateStatus::Available { latest, current } = &*status
    {
        let banner = Paragraph::new(format!(
            " New version available: {} -> {} (press 'u' to dismiss)",
            current, latest
        ))
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
        frame.render_widget(banner, chunks[1]);
    }

    if has_data {
        // Tabs
        let tab_titles: Vec<Line> = filtered_stats
            .iter()
            .map(|stats| {
                let s = stats.read();
                Line::from(format!(" {} ({}) ", s.analyzer_name, s.num_conversations))
            })
            .collect();

        let tabs = Tabs::new(tab_titles)
            .select(ui_state.selected_tab)
            // .style(Style::default().add_modifier(Modifier::DIM))
            .highlight_style(Style::new().black().on_light_green())
            .padding("", "")
            .divider(" | ");

        frame.render_widget(tabs, chunks[1 + chunk_offset]);

        // Get current analyzer stats
        if let Some(current_stats) = filtered_stats.get(ui_state.selected_tab)
            && let Some(current_table_state) = ui_state.table_states.get_mut(ui_state.selected_tab)
        {
            // Draw main table - hold read lock only for this scope
            let has_estimated_models = {
                let view = current_stats.read();
                match ui_state.stats_view_mode {
                    StatsViewMode::Daily => {
                        let (_, has_estimated) = draw_daily_stats_table(
                            frame,
                            chunks[2 + chunk_offset],
                            &view,
                            format_options,
                            current_table_state,
                            if ui_state.date_jump_active {
                                ui_state.date_jump_buffer
                            } else {
                                ""
                            },
                            ui_state.sort_reversed,
                        );
                        has_estimated
                    }
                    StatsViewMode::Session => {
                        draw_session_stats_table(
                            frame,
                            chunks[2 + chunk_offset],
                            &view.session_aggregates,
                            format_options,
                            current_table_state,
                            &mut ui_state.session_window_offsets[ui_state.selected_tab],
                            ui_state.session_day_filters[ui_state.selected_tab].as_ref(),
                            ui_state.sort_reversed,
                        );
                        false // Session view doesn't track estimated models yet
                    }
                }
            }; // Read lock on current_stats released here BEFORE draw_summary_stats

            // Summary stats - pass all filtered stats for aggregation (only if visible)
            // When in Session mode with a day filter, only show totals for that day
            // NOTE: This acquires its own read locks, so we must not hold any above
            let help_chunk_offset = if ui_state.show_totals {
                let day_filter = match ui_state.stats_view_mode {
                    StatsViewMode::Session => ui_state
                        .session_day_filters
                        .get(ui_state.selected_tab)
                        .and_then(|f| f.as_ref()),
                    StatsViewMode::Daily => None,
                };
                draw_summary_stats(
                    frame,
                    chunks[3 + chunk_offset],
                    filtered_stats,
                    format_options,
                    day_filter,
                );
                4 + chunk_offset
            } else {
                3 + chunk_offset
            };

            // Help text for data view with upload status
            let help_area = chunks[help_chunk_offset];

            // Split help area horizontally: help text on left, upload status on right
            let help_chunks = Layout::horizontal([
                Constraint::Fill(1),    // Help text takes remaining space
                Constraint::Length(30), // Fixed space for upload status
            ])
            .split(help_area);

            let base_help_text = match ui_state.stats_view_mode {
                StatsViewMode::Daily => {
                    "Use ←/→ or h/l to switch tabs • ↑/↓ or j/k to navigate • r to reverse sort • s to toggle summary • / for date jump • Enter to drill into day • Ctrl+T for per-session view • q/Esc to quit"
                }
                StatsViewMode::Session => {
                    "Use ←/→ or h/l to switch tabs • ↑/↓ or j/k to navigate • r to reverse sort • s to toggle summary • Ctrl+T for per-day view • q/Esc to quit"
                }
            };

            let help_text = if has_estimated_models {
                format!("{} • * = estimated pricing", base_help_text)
            } else {
                base_help_text.to_string()
            };

            let help = Paragraph::new(help_text)
                .style(Style::default().add_modifier(Modifier::DIM))
                .wrap(ratatui::widgets::Wrap { trim: true });
            frame.render_widget(help, help_chunks[0]);

            // Upload status on right side
            if let Ok(status) = upload_status.lock() {
                let (status_text, status_style) = match &*status {
                    UploadStatus::None => (String::new(), Style::default()),
                    UploadStatus::Uploading {
                        current,
                        total,
                        dots,
                    } => {
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
        }
    } else {
        // No data message
        let no_data_message = Paragraph::new(Text::styled(
            "You don't have any agentic development tool data.  Once you start using Claude Code / Codex CLI / Gemini CLI / Qwen Code / Cline / Roo Code / Kilo Code / GitHub Copilot / OpenCode / Pi Agent, you'll see some data here.",
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
    stats: &AnalyzerStatsView,
    format_options: &NumberFormatOptions,
    table_state: &mut TableState,
    date_filter: &str,
    sort_reversed: bool,
) -> (usize, bool) {
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

    let mut best_cost_cents: u32 = 0;
    let mut best_cost_i = 0;
    let mut best_cached_tokens: u32 = 0;
    let mut best_cached_tokens_i = 0;
    let mut best_input_tokens: u32 = 0;
    let mut best_input_tokens_i = 0;
    let mut best_output_tokens: u32 = 0;
    let mut best_output_tokens_i = 0;
    let mut best_reasoning_tokens: u32 = 0;
    let mut best_reasoning_tokens_i = 0;
    let mut best_conversations = 0;
    let mut best_conversations_i = 0;
    let mut best_tool_calls: u32 = 0;
    let mut best_tool_calls_i = 0;

    for (i, day_stats) in stats.daily_stats.values().enumerate() {
        if day_stats.stats.cost_cents > best_cost_cents {
            best_cost_cents = day_stats.stats.cost_cents;
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
    let mut total_cost_cents: u64 = 0;
    let mut total_cached: u64 = 0;
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_reasoning: u64 = 0;
    let mut total_tool_calls: u64 = 0;
    let mut total_conversations: u64 = 0;

    // Use EitherIter to avoid allocation when reversing
    let items_to_render = if sort_reversed {
        EitherIter::Right(stats.daily_stats.iter().rev())
    } else {
        EitherIter::Left(stats.daily_stats.iter())
    };

    for (i, (date, day_stats)) in items_to_render.enumerate() {
        // Filter rows based on date search
        if !date_filter.is_empty() && !date_matches_buffer(date, date_filter) {
            continue;
        }

        total_cost_cents += day_stats.stats.cost_cents as u64;
        total_cached += day_stats.stats.cached_tokens as u64;
        total_input += day_stats.stats.input_tokens as u64;
        total_output += day_stats.stats.output_tokens as u64;
        total_reasoning += day_stats.stats.reasoning_tokens as u64;
        total_tool_calls += day_stats.stats.tool_calls as u64;
        total_conversations += day_stats.conversations as u64;

        let mut models_vec: Vec<String> = day_stats
            .models
            .keys()
            .map(|m| {
                if is_model_estimated(m) {
                    format!("{}*", m)
                } else {
                    m.clone()
                }
            })
            .collect();
        models_vec.sort();
        let models = models_vec.join(", ");

        // Check if this is an empty row
        let is_empty_row = day_stats.stats.cost_cents == 0
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
                format!("${:.2}", day_stats.stats.cost()),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_cost_i {
            Line::from(Span::styled(
                format!("${:.2}", day_stats.stats.cost()),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::styled(
                format!("${:.2}", day_stats.stats.cost()),
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
    let mut all_models = HashSet::new();
    let mut has_estimated_models = false;
    for day_stats in stats.daily_stats.values() {
        for model in day_stats.models.keys() {
            all_models.insert(model);
            if is_model_estimated(model) {
                has_estimated_models = true;
            }
        }
    }
    let mut all_models_vec: Vec<String> = all_models
        .iter()
        .map(|k| {
            if is_model_estimated(k) {
                format!("{}*", k)
            } else {
                k.to_string()
            }
        })
        .collect();
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
    let total_cost = total_cost_cents as f64 / 100.0;

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
            format_number(total_conversations, format_options),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
        Line::from(Span::styled(
            format_number(total_tool_calls, format_options),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ))
        .right_aligned(),
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

    // Return the total number of rows in the table and whether there are estimated models
    (total_rows, has_estimated_models)
}

#[allow(clippy::too_many_arguments)]
fn draw_session_stats_table(
    frame: &mut Frame,
    area: Rect,
    sessions: &[SessionAggregate],
    format_options: &NumberFormatOptions,
    table_state: &mut TableState,
    window_offset: &mut usize,
    day_filter: Option<&String>,
    sort_reversed: bool,
) {
    let header = Row::new(vec![
        Cell::new(""),
        Cell::new("Session"),
        Cell::new("Started"),
        Cell::new(Text::from("Cost").right_aligned()),
        Cell::new(Text::from("Cached Tks").right_aligned()),
        Cell::new(Text::from("Inp Tks").right_aligned()),
        Cell::new(Text::from("Outp Tks").right_aligned()),
        Cell::new(Text::from("Reason Tks").right_aligned()),
        Cell::new(Text::from("Tools").right_aligned()),
        Cell::new("Models"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .height(1);

    let filtered_sessions: Vec<&SessionAggregate> = {
        let mut sessions: Vec<_> = match day_filter {
            Some(day) => sessions.iter().filter(|s| &s.day_key == day).collect(),
            None => sessions.iter().collect(),
        };
        if sort_reversed {
            sessions.reverse();
        }
        sessions
    };

    let total_session_rows = filtered_sessions.len();
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
    let max_body_rows = area.height.saturating_sub(1).max(1) as usize;

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

    // Recompute bests/totals for the filtered subset so highlighting and totals stay accurate.
    let mut best_cost_i: Option<usize> = None;
    let mut best_cached_tokens_i: Option<usize> = None;
    let mut best_input_tokens_i: Option<usize> = None;
    let mut best_output_tokens_i: Option<usize> = None;
    let mut best_reasoning_tokens_i: Option<usize> = None;
    let mut best_tool_calls_i: Option<usize> = None;

    let mut total_cost_cents: u64 = 0;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_cached_tokens: u64 = 0;
    let mut total_reasoning_tokens: u64 = 0;
    let mut total_tool_calls: u64 = 0;
    let mut all_models = HashSet::new();

    for (idx, session) in filtered_sessions.iter().enumerate() {
        if best_cost_i
            .map(|best_idx| session.stats.cost_cents > filtered_sessions[best_idx].stats.cost_cents)
            .unwrap_or(true)
        {
            best_cost_i = Some(idx);
        }

        if best_cached_tokens_i
            .map(|best_idx| {
                session.stats.cached_tokens > filtered_sessions[best_idx].stats.cached_tokens
            })
            .unwrap_or(true)
        {
            best_cached_tokens_i = Some(idx);
        }

        if best_input_tokens_i
            .map(|best_idx| {
                session.stats.input_tokens > filtered_sessions[best_idx].stats.input_tokens
            })
            .unwrap_or(true)
        {
            best_input_tokens_i = Some(idx);
        }

        if best_output_tokens_i
            .map(|best_idx| {
                session.stats.output_tokens > filtered_sessions[best_idx].stats.output_tokens
            })
            .unwrap_or(true)
        {
            best_output_tokens_i = Some(idx);
        }

        if best_reasoning_tokens_i
            .map(|best_idx| {
                session.stats.reasoning_tokens > filtered_sessions[best_idx].stats.reasoning_tokens
            })
            .unwrap_or(true)
        {
            best_reasoning_tokens_i = Some(idx);
        }

        if best_tool_calls_i
            .map(|best_idx| session.stats.tool_calls > filtered_sessions[best_idx].stats.tool_calls)
            .unwrap_or(true)
        {
            best_tool_calls_i = Some(idx);
        }

        total_cost_cents += session.stats.cost_cents as u64;
        total_input_tokens += session.stats.input_tokens as u64;
        total_output_tokens += session.stats.output_tokens as u64;
        total_cached_tokens += session.stats.cached_tokens as u64;
        total_reasoning_tokens += session.stats.reasoning_tokens as u64;
        total_tool_calls += session.stats.tool_calls as u64;

        for model in &session.models {
            all_models.insert(model.clone());
        }
    }

    let mut all_models_vec = all_models.into_iter().collect::<Vec<_>>();
    all_models_vec.sort();
    let all_models_text = all_models_vec.join(", ");

    for (i, session) in filtered_sessions
        .iter()
        .enumerate()
        .take(window_end)
        .skip(window_start)
    {
        if i < total_session_rows {
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

            let cost_cell = if best_cost_i == Some(i) {
                Line::from(Span::styled(
                    format!("${:.2}", session.stats.cost()),
                    Style::default().fg(Color::Red),
                ))
            } else {
                Line::from(Span::styled(
                    format!("${:.2}", session.stats.cost()),
                    Style::default().fg(Color::Yellow),
                ))
            }
            .right_aligned();

            let cached_cell = if best_cached_tokens_i == Some(i) {
                Line::from(Span::styled(
                    format_number(session.stats.cached_tokens, format_options),
                    Style::default().fg(Color::Red),
                ))
            } else {
                Line::from(Span::styled(
                    format_number(session.stats.cached_tokens, format_options),
                    Style::default().add_modifier(Modifier::DIM),
                ))
            }
            .right_aligned();

            let input_cell = if best_input_tokens_i == Some(i) {
                Line::from(Span::styled(
                    format_number(session.stats.input_tokens, format_options),
                    Style::default().fg(Color::Red),
                ))
            } else {
                Line::from(Span::raw(format_number(
                    session.stats.input_tokens,
                    format_options,
                )))
            }
            .right_aligned();

            let output_cell = if best_output_tokens_i == Some(i) {
                Line::from(Span::styled(
                    format_number(session.stats.output_tokens, format_options),
                    Style::default().fg(Color::Red),
                ))
            } else {
                Line::from(Span::raw(format_number(
                    session.stats.output_tokens,
                    format_options,
                )))
            }
            .right_aligned();

            let reasoning_cell = if best_reasoning_tokens_i == Some(i) {
                Line::from(Span::styled(
                    format_number(session.stats.reasoning_tokens, format_options),
                    Style::default().fg(Color::Red),
                ))
            } else {
                Line::from(Span::raw(format_number(
                    session.stats.reasoning_tokens,
                    format_options,
                )))
            }
            .right_aligned();

            let tools_cell = if best_tool_calls_i == Some(i) {
                Line::from(Span::styled(
                    format_number(session.stats.tool_calls, format_options),
                    Style::default().fg(Color::Red),
                ))
            } else {
                Line::from(Span::styled(
                    format_number(session.stats.tool_calls, format_options),
                    Style::default().add_modifier(Modifier::DIM),
                ))
            }
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
                cost_cell,
                cached_cell,
                input_cell,
                output_cell,
                reasoning_cell,
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
                    "──────────",
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
                    "───────────",
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
            let total_cost = total_cost_cents as f64 / 100.0;
            let totals_row = Row::new(vec![
                Line::from(Span::raw("")),
                Line::from(Span::styled(
                    format!("Total ({} sessions)", total_session_rows),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::raw("")),
                Line::from(Span::styled(
                    format!("${total_cost:.2}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(total_cached_tokens, format_options),
                    Style::default()
                        .add_modifier(Modifier::DIM)
                        .add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(total_input_tokens, format_options),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(total_output_tokens, format_options),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(total_reasoning_tokens, format_options),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    format_number(total_tool_calls, format_options),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))
                .right_aligned(),
                Line::from(Span::styled(
                    all_models_text.clone(),
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
            Constraint::Length(10), // Cost
            Constraint::Length(10), // Cached Tks
            Constraint::Length(8),  // Input
            Constraint::Length(9),  // Output
            Constraint::Length(11), // Reason Tks
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
    filtered_stats: &[SharedAnalyzerView],
    format_options: &NumberFormatOptions,
    day_filter: Option<&String>,
) {
    // Aggregate stats from all tools, optionally filtered to a single day
    let mut total_cost_cents: u64 = 0;
    let mut total_cached: u64 = 0;
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_reasoning: u64 = 0;
    let mut total_tool_calls: u64 = 0;
    let mut all_days = HashSet::new();

    for stats_arc in filtered_stats {
        let stats = stats_arc.read();
        // Iterate directly - filter inline if day_filter is set
        for (day, day_stats) in stats.daily_stats.iter() {
            // Skip if day doesn't match filter
            if let Some(filter_day) = day_filter
                && day != filter_day
            {
                continue;
            }

            total_cost_cents += day_stats.stats.cost_cents as u64;
            total_cached += day_stats.stats.cached_tokens as u64;
            total_input += day_stats.stats.input_tokens as u64;
            total_output += day_stats.stats.output_tokens as u64;
            total_reasoning += day_stats.stats.reasoning_tokens as u64;
            total_tool_calls += day_stats.stats.tool_calls as u64;

            // Collect unique days across all tools that have actual data
            if day_stats.stats.cost_cents > 0
                || day_stats.stats.input_tokens > 0
                || day_stats.stats.output_tokens > 0
                || day_stats.stats.reasoning_tokens > 0
                || day_stats.stats.cached_tokens > 0
                || day_stats.stats.tool_calls > 0
                || day_stats.ai_messages > 0
                || day_stats.conversations > 0
            {
                all_days.insert(day.clone());
            }
        }
    }

    let total_tokens = total_cached + total_input + total_output;
    let total_cost = total_cost_cents as f64 / 100.0;
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

    // Show "Totals" or "Totals for <date>" depending on filter
    let title = if let Some(day) = day_filter {
        format!("Totals for {}", crate::utils::format_date_for_display(day))
    } else {
        "Totals".to_string()
    };
    summary_lines.insert(
        0,
        Line::from(vec![Span::styled(title, Style::default().bold().dim())]),
    );

    let summary_widget =
        Paragraph::new(Text::from(summary_lines)).block(Block::default().title(""));
    frame.render_widget(summary_widget, area);
}

fn update_table_states(
    table_states: &mut Vec<TableState>,
    current_stats: &MultiAnalyzerStatsView,
    selected_tab: &mut usize,
) {
    let filtered_count = current_stats
        .analyzer_stats
        .iter()
        .filter(|stats| has_data_shared(stats))
        .count();

    // Preserve existing table states when resizing
    let old_states = table_states.clone();
    table_states.clear();

    for i in 0..filtered_count {
        let state = if i < old_states.len() {
            // Preserve existing state if available
            old_states[i]
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

fn update_day_filters(filters: &mut Vec<Option<String>>, filtered_count: &usize) {
    let old = filters.clone();
    filters.clear();

    for i in 0..*filtered_count {
        if i < old.len() {
            filters.push(old[i].clone());
        } else {
            filters.push(None);
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
