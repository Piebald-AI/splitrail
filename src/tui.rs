use crate::types::{AgenticCodingToolStats, MultiAnalyzerStats};
use crate::utils::{NumberFormatOptions, format_date_for_display, format_number};
use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Tabs};
use ratatui::{Frame, Terminal};
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum UploadStatus {
    None,
    Uploading,
    Uploaded,
    Failed(String), // Include error message
    MissingApiToken,
    MissingServerUrl,
    MissingConfig,
}

fn has_data(stats: &AgenticCodingToolStats) -> bool {
    stats.num_conversations > 0
        || stats.daily_stats.values().any(|day| {
        day.cost > 0.0 || day.input_tokens > 0 || day.output_tokens > 0 || day.tool_calls > 0
    })
}

pub fn run_tui(
    multi_stats: &MultiAnalyzerStats,
    format_options: &NumberFormatOptions,
    upload_status: Arc<Mutex<UploadStatus>>,
) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    // Filter analyzer stats to only include those with data
    let filtered_stats: Vec<&AgenticCodingToolStats> = multi_stats
        .analyzer_stats
        .iter()
        .filter(|stats| has_data(stats))
        .collect();

    let mut table_states: Vec<TableState> = filtered_stats
        .iter()
        .map(|_| {
            let mut state = TableState::default();
            state.select(Some(0));
            state
        })
        .collect();
    let mut scroll_offset = 0;
    let mut selected_tab = 0;

    let result = run_app(
        &mut terminal,
        &filtered_stats,
        format_options,
        &mut table_states,
        &mut scroll_offset,
        &mut selected_tab,
        upload_status,
    );

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    filtered_stats: &[&AgenticCodingToolStats],
    format_options: &NumberFormatOptions,
    table_states: &mut Vec<TableState>,
    scroll_offset: &mut usize,
    selected_tab: &mut usize,
    upload_status: Arc<Mutex<UploadStatus>>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            draw_ui(
                frame,
                filtered_stats,
                format_options,
                table_states,
                *scroll_offset,
                *selected_tab,
                upload_status.clone(),
            );
        })?;

        // Use a timeout to allow periodic refreshes for upload status updates
        if let Ok(event_available) = event::poll(Duration::from_millis(100)) {
            if !event_available {
                continue;
            }

            // Read a key event.  No other event types for now.
            let key = match event::read()? {
                Event::Key(key) if key.is_press() => key,
                _ => continue
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
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if *selected_tab < filtered_stats.len() - 1 {
                        *selected_tab += 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(current_stats) = filtered_stats.get(*selected_tab) {
                        let total_rows = current_stats.daily_stats.len();
                        if let Some(table_state) =
                            table_states.get_mut(*selected_tab)
                        {
                            if let Some(selected) = table_state.selected() {
                                if selected < total_rows.saturating_add(1) {
                                    table_state.select(Some(
                                        if selected == total_rows.saturating_sub(1)
                                        {
                                            selected + 2
                                        } else {
                                            selected + 1
                                        },
                                    ));
                                }
                            }
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(current_stats) = filtered_stats.get(*selected_tab) {
                        if let Some(table_state) =
                            table_states.get_mut(*selected_tab)
                        {
                            if let Some(selected) = table_state.selected() {
                                if selected > 0 {
                                    table_state.select(Some(
                                        selected.saturating_sub(
                                            if selected
                                                == current_stats.daily_stats.len()
                                                + 1
                                            {
                                                2
                                            } else {
                                                1
                                            },
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
                KeyCode::Home => {
                    if let Some(table_state) = table_states.get_mut(*selected_tab) {
                        table_state.select(Some(0));
                    }
                }
                KeyCode::End => {
                    if let Some(current_stats) = filtered_stats.get(*selected_tab) {
                        let total_rows = current_stats.daily_stats.len() + 2;
                        if let Some(table_state) =
                            table_states.get_mut(*selected_tab)
                        {
                            table_state.select(Some(total_rows.saturating_sub(1)));
                        }
                    }
                }
                KeyCode::PageDown => {
                    if let Some(current_stats) = filtered_stats.get(*selected_tab) {
                        let total_rows = current_stats.daily_stats.len() + 2;
                        if let Some(table_state) =
                            table_states.get_mut(*selected_tab)
                        {
                            if let Some(selected) = table_state.selected() {
                                let new_selected = (selected + 10)
                                    .min(total_rows.saturating_sub(1));
                                table_state.select(Some(new_selected));
                            }
                        }
                    }
                }
                KeyCode::PageUp => {
                    if let Some(table_state) = table_states.get_mut(*selected_tab) {
                        if let Some(selected) = table_state.selected() {
                            let new_selected = selected.saturating_sub(10);
                            table_state.select(Some(new_selected));
                        }
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
    table_states: &mut Vec<TableState>,
    _scroll_offset: usize,
    selected_tab: usize,
    upload_status: Arc<Mutex<UploadStatus>>,
) {
    // Since we're already working with filtered stats, has_data is simply whether we have any stats
    let has_data = !filtered_stats.is_empty();

    // Adjust layout based on whether we have data or not
    let chunks = if has_data {
        Layout::vertical([
            Constraint::Length(3), // Header
            Constraint::Length(1), // Tabs
            Constraint::Length(4), // Models info
            Constraint::Min(3),    // Main table
            Constraint::Length(6), // Summary stats
            Constraint::Length(2), // Help text
        ])
            .split(frame.area())
    } else {
        Layout::vertical([
            Constraint::Length(3), // Header
            Constraint::Min(3),    // No-data message
            Constraint::Length(2), // Help text
        ])
            .split(frame.area())
    };

    // Header
    let header = Paragraph::new(Text::from(vec![
        Line::styled(
            "AGENTIC CODING TOOL ACTIVITY ANALYSIS",
            Style::new().cyan().bold(),
        ),
        Line::styled(
            "=====================================",
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
            .select(selected_tab)
            // .style(Style::default().add_modifier(Modifier::DIM))
            .highlight_style(Style::new().black().on_light_green())
            .padding("", "")
            .divider(" | ");

        frame.render_widget(tabs, chunks[1]);

        // Get current analyzer stats
        if let Some(current_stats) = filtered_stats.get(selected_tab) {
            if let Some(current_table_state) = table_states.get_mut(selected_tab) {
                // Models info
                let mut model_lines = vec![Line::styled(
                    "Models:",
                    Style::default().add_modifier(Modifier::DIM),
                )];
                for (k, v) in &current_stats.model_abbrs.abbr_to_desc {
                    // Only show the abbreviation if this model is used
                    for day_stats in current_stats.daily_stats.values() {
                        if day_stats
                            .models
                            .contains_key(&current_stats.model_abbrs.abbr_to_model[k])
                        {
                            model_lines.push(Line::from(Span::styled(
                                format!("  {}: {}", k, v),
                                Style::default().add_modifier(Modifier::DIM),
                            )));
                            break;
                        }
                    }
                }
                let models_widget =
                    Paragraph::new(Text::from(model_lines)).block(Block::default().title(""));
                frame.render_widget(models_widget, chunks[2]);

                // Main table
                draw_daily_stats_table(
                    frame,
                    chunks[3],
                    current_stats,
                    format_options,
                    current_table_state,
                );

                // Summary stats
                draw_summary_stats(frame, chunks[4], current_stats, format_options);
            }
        }

        // Help text for data view with upload status
        let help_area = chunks[5];

        // Split help area horizontally: help text on left, upload status on right
        let help_chunks = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(50), // Increased space for longer error messages
        ]).split(help_area);

        let help =
            Paragraph::new("Use ←/→ or h/l to switch tabs, ↑/↓ or j/k to navigate, q/Esc to quit")
                .style(Style::default().add_modifier(Modifier::DIM));
        frame.render_widget(help, help_chunks[0]);

        // Upload status in bottom-right
        if let Ok(status) = upload_status.lock() {
            let (status_text, status_style) = match &*status {
                UploadStatus::None => (String::new(), Style::default()),
                UploadStatus::Uploading => ("Uploading...".to_string(), Style::default().add_modifier(Modifier::DIM)),
                UploadStatus::Uploaded => ("✓ Uploaded successfully".to_string(), Style::default().fg(Color::Green)),
                UploadStatus::Failed(error) => {
                    // Show error message directly, truncating only if necessary
                    let error_text = if error.len() <= 47 {
                        format!("✕ {}", error)
                    } else {
                        format!("✕ {:.44}...", error)
                    };
                    (error_text, Style::default().fg(Color::Red))
                }
                UploadStatus::MissingApiToken => ("No API token for uploading".to_string(), Style::default().fg(Color::Yellow)),
                UploadStatus::MissingServerUrl => ("No server URL for uploading".to_string(), Style::default().fg(Color::Yellow)),
                UploadStatus::MissingConfig => ("Upload config incomplete".to_string(), Style::default().fg(Color::Yellow)),
            };

            if !status_text.is_empty() {
                let status_widget = Paragraph::new(status_text)
                    .style(status_style)
                    .alignment(ratatui::layout::Alignment::Right);
                frame.render_widget(status_widget, help_chunks[1]);
            }
        }
    } else {
        // No data message
        let no_data_message = Paragraph::new(Text::styled(
            "You don't have any agentic coding data.  Once you start using Claude Code or Codex, you'll see some data here.",
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
        Cell::new(Text::from("Convs").right_aligned()),
        Cell::new(Text::from("Tools").right_aligned()),
        Cell::new(Text::from("Lines").right_aligned()),
        Cell::new(Line::from(vec![
            Span::from("Models "),
            Span::from("(see above)").dim(),
        ])),
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
    let mut best_conversations = 0;
    let mut best_conversations_i = 0;
    let mut best_tool_calls = 0;
    let mut best_tool_calls_i = 0;

    for (i, day_stats) in stats.daily_stats.values().enumerate() {
        if day_stats.cost > best_cost {
            best_cost = day_stats.cost;
            best_cost_i = i;
        }
        if day_stats.cached_tokens > best_cached_tokens {
            best_cached_tokens = day_stats.cached_tokens;
            best_cached_tokens_i = i;
        }
        if day_stats.input_tokens > best_input_tokens {
            best_input_tokens = day_stats.input_tokens;
            best_input_tokens_i = i;
        }
        if day_stats.output_tokens > best_output_tokens {
            best_output_tokens = day_stats.output_tokens;
            best_output_tokens_i = i;
        }
        if day_stats.conversations > best_conversations {
            best_conversations = day_stats.conversations;
            best_conversations_i = i;
        }
        if day_stats.tool_calls > best_tool_calls {
            best_tool_calls = day_stats.tool_calls;
            best_tool_calls_i = i;
        }
    }

    let mut rows = Vec::new();
    let mut total_cost = 0.0;
    let mut total_cached = 0;
    let mut total_input = 0;
    let mut total_output = 0;
    let mut total_tool_calls = 0;

    for (i, (date, day_stats)) in stats.daily_stats.iter().enumerate() {
        total_cost += day_stats.cost;
        total_cached += day_stats.cached_tokens;
        total_input += day_stats.input_tokens;
        total_output += day_stats.output_tokens;
        total_tool_calls += day_stats.tool_calls;

        let models = day_stats
            .models
            .keys()
            .map(|k| {
                stats
                    .model_abbrs
                    .model_to_abbr
                    .get(k)
                    .unwrap_or(&k.clone())
                    .clone()
            })
            .collect::<Vec<String>>()
            .join(", ");

        let lines_summary = format!(
            "{}/{}/{}",
            format_number(day_stats.file_operations.lines_read, format_options),
            format_number(day_stats.file_operations.lines_edited, format_options),
            format_number(day_stats.file_operations.lines_written, format_options)
        );

        // Check if this is an empty row
        let is_empty_row = day_stats.cost == 0.0
            && day_stats.cached_tokens == 0
            && day_stats.input_tokens == 0
            && day_stats.output_tokens == 0
            && day_stats.conversations == 0
            && day_stats.user_messages == 0
            && day_stats.ai_messages == 0
            && day_stats.tool_calls == 0;

        // Create styled cells with colors matching original implementation
        let date_cell = if is_empty_row {
            Line::from(Span::styled(
                format_date_for_display(&date),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else {
            Line::from(Span::raw(format_date_for_display(&date)))
        };

        let cost_cell = if is_empty_row {
            Line::from(Span::styled(
                format!("${:.2}", day_stats.cost),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_cost_i {
            Line::from(Span::styled(
                format!("${:.2}", day_stats.cost),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::styled(
                format!("${:.2}", day_stats.cost),
                Style::default().fg(Color::Yellow),
            ))
        }
            .right_aligned();

        let cached_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.cached_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_cached_tokens_i {
            Line::from(Span::styled(
                format_number(day_stats.cached_tokens, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::styled(
                format_number(day_stats.cached_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        }
            .right_aligned();

        let input_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.input_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_input_tokens_i {
            Line::from(Span::styled(
                format_number(day_stats.input_tokens, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::raw(format_number(
                day_stats.input_tokens,
                format_options,
            )))
        }
            .right_aligned();

        let output_cell = if is_empty_row {
            Line::from(Span::styled(
                format_number(day_stats.output_tokens, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_output_tokens_i {
            Line::from(Span::styled(
                format_number(day_stats.output_tokens, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::raw(format_number(
                day_stats.output_tokens,
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
                format_number(day_stats.tool_calls as u64, format_options),
                Style::default().add_modifier(Modifier::DIM),
            ))
        } else if i == best_tool_calls_i {
            Line::from(Span::styled(
                format_number(day_stats.tool_calls as u64, format_options),
                Style::default().fg(Color::Red),
            ))
        } else {
            Line::from(Span::styled(
                format_number(day_stats.tool_calls as u64, format_options),
                Style::default().fg(Color::Green),
            ))
        }
            .right_aligned();

        let lines_cell = if is_empty_row {
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
            conv_cell,
            tool_cell,
            lines_cell,
            models_cell,
        ]);

        rows.push(row);
    }

    // Add separator row before totals
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
            "────────",
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
            "──────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "──────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "───────────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "──────────",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ]);
    rows.push(separator_row);

    // Add totals row
    let total_lines_r = stats
        .daily_stats
        .values()
        .map(|s| s.file_operations.lines_read)
        .sum::<u64>();
    let total_lines_e = stats
        .daily_stats
        .values()
        .map(|s| s.file_operations.lines_edited)
        .sum::<u64>();
    let total_lines_w = stats
        .daily_stats
        .values()
        .map(|s| s.file_operations.lines_written)
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
            format!("${:.2}", total_cost),
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
        Line::from(Span::styled(
            format!(
                "{}/{}/{}",
                format_number(total_lines_r, format_options),
                format_number(total_lines_e, format_options),
                format_number(total_lines_w, format_options)
            ),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ))
            .right_aligned(),
        Line::from(Span::raw("")),
    ]);

    rows.push(totals_row);

    // Save the row count before moving rows into the table
    let total_rows = rows.len();

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),  // Arrow
            Constraint::Length(12), // Date
            Constraint::Length(8),  // Cost
            Constraint::Length(12), // Cached
            Constraint::Length(8),  // Input
            Constraint::Length(9),  // Output
            Constraint::Length(6),  // Convs
            Constraint::Length(6),  // Tools
            Constraint::Length(15), // Lines
            Constraint::Min(10),    // Models
        ],
    )
        .header(header)
        .block(Block::default().title(""))
        .row_highlight_style(Style::new().blue())
        .column_spacing(4);

    frame.render_stateful_widget(table, area, table_state);

    // Return the total number of rows in the table
    total_rows
}

fn draw_summary_stats(
    frame: &mut Frame,
    area: Rect,
    stats: &AgenticCodingToolStats,
    format_options: &NumberFormatOptions,
) {
    let total_cost: f64 = stats.daily_stats.values().map(|s| s.cost).sum();
    let total_cached: u64 = stats.daily_stats.values().map(|s| s.cached_tokens).sum();
    let total_input: u64 = stats.daily_stats.values().map(|s| s.input_tokens).sum();
    let total_output: u64 = stats.daily_stats.values().map(|s| s.output_tokens).sum();
    let total_tool_calls: u64 = stats
        .daily_stats
        .values()
        .map(|s| s.tool_calls as u64)
        .sum();
    let total_tokens = total_cached + total_input + total_output;

    // Define summary rows with labels and values
    let summary_rows = vec![
        (
            "Tokens:",
            format_number(total_tokens, format_options),
            Color::LightBlue,
        ),
        (
            "Tool Calls:",
            format_number(total_tool_calls, format_options),
            Color::LightGreen,
        ),
        ("Cost:", format!("${:.2}", total_cost), Color::LightYellow),
        (
            "Days tracked:",
            stats.daily_stats.len().to_string(),
            Color::White,
        ),
    ];

    // Find the maximum label width for alignment
    let max_label_width = summary_rows
        .iter()
        .map(|(label, _, _)| label.len())
        .max()
        .unwrap_or(0);

    // Create lines with consistent spacing
    let summary_lines: Vec<Line> = summary_rows
        .into_iter()
        .map(|(label, value, color)| {
            Line::from(vec![
                Span::raw(format!("{:<width$}", label, width = max_label_width)),
                Span::raw("      "), // 6 spaces between label and value
                Span::styled(
                    value,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ])
        })
        .collect();

    let summary_widget =
        Paragraph::new(Text::from(summary_lines)).block(Block::default().title(""));
    frame.render_widget(summary_widget, area);
}
