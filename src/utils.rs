use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike};
use num_format::{Locale, ToFormattedString};
use serde::{Deserialize, Serialize};

use crate::analyzer::CachingInfo;
use crate::types::{ConversationMessage, DailyStats};

#[derive(Clone)]
pub struct NumberFormatOptions {
    pub use_comma: bool,
    pub use_human: bool,
    pub locale: String,
    pub decimal_places: usize,
}

pub fn format_number(n: u64, options: &NumberFormatOptions) -> String {
    let locale = match options.locale.as_str() {
        "de" => Locale::de,
        "fr" => Locale::fr,
        "es" => Locale::es,
        "it" => Locale::it,
        "ja" => Locale::ja,
        "ko" => Locale::ko,
        "zh" => Locale::zh,
        _ => Locale::en,
    };

    if options.use_human {
        if n >= 1_000_000_000_000 {
            format!(
                "{:.prec$}t",
                n as f64 / 1_000_000_000_000.0,
                prec = options.decimal_places
            )
        } else if n >= 1_000_000_000 {
            format!(
                "{:.prec$}b",
                n as f64 / 1_000_000_000.0,
                prec = options.decimal_places
            )
        } else if n >= 1_000_000 {
            format!(
                "{:.prec$}m",
                n as f64 / 1_000_000.0,
                prec = options.decimal_places
            )
        } else if n >= 1_000 {
            format!(
                "{:.prec$}k",
                n as f64 / 1_000.0,
                prec = options.decimal_places
            )
        } else {
            n.to_string()
        }
    } else if options.use_comma {
        n.to_formatted_string(&locale)
    } else {
        n.to_string()
    }
}

pub fn extract_date_from_timestamp(timestamp: &str) -> Option<String> {
    if timestamp.is_empty() {
        return None;
    }

    if let Ok(datetime_utc) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        let datetime_local = datetime_utc.with_timezone(&chrono::Local);
        Some(datetime_local.format("%Y-%m-%d").to_string())
    } else {
        None
    }
}

fn parse_timestamp_to_seconds(timestamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Calculate the maximum flow length (autonomous AI operation duration) for each day
fn calculate_max_flow_lengths(entries: &[ConversationMessage]) -> BTreeMap<String, u64> {
    let mut daily_max_flows: BTreeMap<String, u64> = BTreeMap::new();

    // Group messages by conversation file and sort by timestamp
    let mut conversations: BTreeMap<String, Vec<&ConversationMessage>> = BTreeMap::new();
    for entry in entries {
        let conversation_file = match entry {
            ConversationMessage::AI {
                conversation_file, ..
            } => conversation_file,
            ConversationMessage::User {
                conversation_file, ..
            } => conversation_file,
        };
        conversations
            .entry(conversation_file.clone())
            .or_default()
            .push(entry);
    }

    // Process each conversation to find flow lengths
    for messages in conversations.values() {
        let mut sorted_messages = messages.clone();
        sorted_messages.sort_by_key(|msg| {
            let timestamp = match msg {
                ConversationMessage::AI { timestamp, .. } => timestamp,
                ConversationMessage::User { timestamp, .. } => timestamp,
            };
            parse_timestamp_to_seconds(timestamp).unwrap_or(0)
        });

        let mut flow_start: Option<i64> = None;
        let mut last_ai_timestamp: Option<i64> = None;

        for message in &sorted_messages {
            match message {
                ConversationMessage::AI { timestamp, .. } => {
                    let ts = parse_timestamp_to_seconds(timestamp);
                    if let Some(ts) = ts {
                        if flow_start.is_none() {
                            flow_start = Some(ts); // Start of new flow
                        }
                        last_ai_timestamp = Some(ts); // Update last AI message time
                    }
                }
                ConversationMessage::User { timestamp, .. } => {
                    // User message ends the current flow
                    if let (Some(start), Some(end)) = (flow_start, last_ai_timestamp) {
                        let flow_duration = (end - start) as u64;
                        let date = match extract_date_from_timestamp(timestamp) {
                            Some(d) => d,
                            None => continue,
                        };

                        // Cap flows at 4 hours (14400 seconds) to filter out data artifacts
                        // Anything longer likely represents conversations left open rather than active work
                        let capped_duration = flow_duration.min(14400);

                        let current_max = daily_max_flows.get(&date).unwrap_or(&0);
                        if capped_duration > *current_max {
                            daily_max_flows.insert(date, capped_duration);
                        }
                    }
                    // Reset for next flow
                    flow_start = None;
                    last_ai_timestamp = None;
                }
            }
        }

        // Handle case where conversation ends with AI messages (no final user message)
        if let (Some(start), Some(end)) = (flow_start, last_ai_timestamp) {
            let flow_duration = (end - start) as u64;
            // Use the last AI message's date
            if let Some(last_ai_msg) = sorted_messages
                .iter()
                .rev()
                .find(|msg| matches!(msg, ConversationMessage::AI { .. }))
            {
                if let ConversationMessage::AI { timestamp, .. } = last_ai_msg {
                    let date = match extract_date_from_timestamp(timestamp) {
                        Some(d) => d,
                        None => continue,
                    };

                    // Cap flows at 4 hours (14400 seconds) to filter out data artifacts
                    // Anything longer likely represents conversations left open rather than active work
                    let capped_duration = flow_duration.min(14400);

                    let current_max = daily_max_flows.get(&date).unwrap_or(&0);
                    if capped_duration > *current_max {
                        daily_max_flows.insert(date, capped_duration);
                    }
                }
            }
        }
    }

    daily_max_flows
}

pub fn format_date_for_display(date: &str) -> String {
    if date == "unknown" {
        return "Unknown".to_string();
    }

    if let Ok(parsed) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        // Format with non-padded month and day
        let month = parsed.month();
        let day = parsed.day();
        let year = parsed.year();
        let formatted = format!("{}/{}/{}", month, day, year);

        // Check if this is today's date
        let today = chrono::Local::now().date_naive();
        if parsed == today {
            format!("{formatted}*")
        } else {
            formatted
        }
    } else {
        date.to_string()
    }
}

pub fn aggregate_by_date(entries: &[ConversationMessage]) -> BTreeMap<String, DailyStats> {
    let mut daily_stats: BTreeMap<String, DailyStats> = BTreeMap::new();

    // Calculate max flow lengths for each day
    let max_flows = calculate_max_flow_lengths(entries);

    // First, find the start date for each conversation
    let mut conversation_start_dates: BTreeMap<String, String> = BTreeMap::new();
    for entry in entries {
        let (timestamp, conversation_file) = match entry {
            ConversationMessage::AI {
                timestamp,
                conversation_file,
                ..
            } => (timestamp, conversation_file),
            ConversationMessage::User {
                timestamp,
                conversation_file,
                ..
            } => (timestamp, conversation_file),
        };
        let date = match extract_date_from_timestamp(timestamp) {
            Some(d) => d,
            None => continue, // Skip entries with invalid timestamps
        };

        // Only update if this is earlier than what we've seen, or if we haven't seen this conversation
        conversation_start_dates
            .entry(conversation_file.clone())
            .and_modify(|existing_date| {
                if date < *existing_date {
                    *existing_date = date.clone();
                }
            })
            .or_insert(date);
    }

    // Track conversations started on each date
    let mut daily_conversations_started: BTreeMap<String, u32> = BTreeMap::new();
    for start_date in conversation_start_dates.values() {
        *daily_conversations_started
            .entry(start_date.clone())
            .or_insert(0) += 1;
    }

    for entry in entries {
        let date = match entry {
            ConversationMessage::AI { timestamp, .. } => extract_date_from_timestamp(timestamp),
            ConversationMessage::User { timestamp, .. } => extract_date_from_timestamp(timestamp),
        };

        let date = match date {
            Some(d) => d,
            None => continue, // Skip entries with invalid timestamps
        };

        let stats = daily_stats
            .entry(date.clone())
            .or_insert_with(|| DailyStats {
                date: date.clone(),
                ..Default::default()
            });

        match entry {
            ConversationMessage::AI {
                model,
                general_stats,
                file_operations,
                todo_stats,
                ..
            } => {
                stats.cost += general_stats.cost;

                stats.cached_tokens += general_stats.cache_read_tokens;
                stats.cached_tokens += general_stats.cache_creation_tokens;
                stats.cached_tokens += general_stats.cached_tokens;

                stats.input_tokens += general_stats.input_tokens;
                stats.output_tokens += general_stats.output_tokens;
                stats.tool_calls += general_stats.tool_calls;
                stats.ai_messages += 1;
                *stats.models.entry(model.to_string()).or_insert(0) += 1;

                // Aggregate file operations for this day
                stats.file_operations.files_read += file_operations.files_read;
                stats.file_operations.files_edited += file_operations.files_edited;
                stats.file_operations.files_added += file_operations.files_added;
                stats.file_operations.terminal_commands += file_operations.terminal_commands;
                stats.file_operations.file_content_searches +=
                    file_operations.file_content_searches;
                stats.file_operations.file_searches += file_operations.file_searches;
                stats.file_operations.lines_read += file_operations.lines_read;
                stats.file_operations.lines_edited += file_operations.lines_edited;
                stats.file_operations.lines_added += file_operations.lines_added;
                stats.file_operations.bytes_read += file_operations.bytes_read;
                stats.file_operations.bytes_edited += file_operations.bytes_edited;
                stats.file_operations.bytes_added += file_operations.bytes_added;
                for (file_type, count) in &file_operations.file_types {
                    *stats
                        .file_operations
                        .file_types
                        .entry(file_type.clone())
                        .or_insert(0) += count;
                }

                // Aggregate todo stats for this day (if available)
                if let Some(todo_stats) = todo_stats {
                    if let Some(ref mut daily_todos) = stats.todo_stats {
                        daily_todos.todos_created += todo_stats.todos_created;
                        daily_todos.todos_completed += todo_stats.todos_completed;
                        daily_todos.todos_in_progress += todo_stats.todos_in_progress;
                        daily_todos.todo_writes += todo_stats.todo_writes;
                        daily_todos.todo_reads += todo_stats.todo_reads;
                    } else {
                        stats.todo_stats = Some(todo_stats.clone());
                    }
                }
            }
            ConversationMessage::User { todo_stats, .. } => {
                stats.user_messages += 1;

                // Aggregate todo stats from user messages too (if available)
                if let Some(todo_stats) = todo_stats {
                    if let Some(ref mut daily_todos) = stats.todo_stats {
                        daily_todos.todos_created += todo_stats.todos_created;
                        daily_todos.todos_completed += todo_stats.todos_completed;
                        daily_todos.todos_in_progress += todo_stats.todos_in_progress;
                        daily_todos.todo_writes += todo_stats.todo_writes;
                        daily_todos.todo_reads += todo_stats.todo_reads;
                    } else {
                        stats.todo_stats = Some(todo_stats.clone());
                    }
                }
            }
        };
    }

    // Put the number of conversations started on each day on the daily stats.
    for (date, count) in daily_conversations_started {
        if let Some(stats) = daily_stats.get_mut(&date) {
            stats.conversations = count;
        }
    }

    // Set max flow lengths for each day
    for (date, max_flow) in max_flows {
        if let Some(stats) = daily_stats.get_mut(&date) {
            stats.max_flow_length_seconds = max_flow;
        }
    }

    // If there are any gaps (days Claude Code wasn't run) fill them in with
    // empty stats.  (TODO: This should be a utility.)
    if !daily_stats.is_empty() {
        let mut filled_stats = BTreeMap::new();

        let earliest_date = daily_stats.keys().min().unwrap();
        let today_str = chrono::Local::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let latest_date = daily_stats.keys().max().unwrap().max(&today_str); // Either today or the highest date in data.

        let start_date = match chrono::NaiveDate::parse_from_str(earliest_date, "%Y-%m-%d") {
            Ok(date) => date,
            Err(_) => return daily_stats, // Ignore.
        };

        let end_date = match chrono::NaiveDate::parse_from_str(latest_date, "%Y-%m-%d") {
            Ok(date) => date,
            Err(_) => return daily_stats, // Ignore.
        };

        // Fill in the gaps.
        let mut current_date = start_date;
        while current_date <= end_date {
            let date_str = current_date.format("%Y-%m-%d").to_string();

            if let Some(existing_stats) = daily_stats.get(&date_str) {
                filled_stats.insert(date_str, existing_stats.clone());
            } else {
                filled_stats.insert(
                    date_str.clone(),
                    DailyStats {
                        date: date_str,
                        ..Default::default()
                    },
                );
            }

            current_date += chrono::Duration::days(1);
        }

        return filled_stats;
    }

    daily_stats
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAbbreviations {
    pub abbr_to_desc: BTreeMap<String, String>,
    pub model_to_abbr: BTreeMap<String, String>,
    pub abbr_to_model: BTreeMap<String, String>,
}

impl ModelAbbreviations {
    pub fn new() -> Self {
        Self {
            abbr_to_desc: BTreeMap::new(),
            model_to_abbr: BTreeMap::new(),
            abbr_to_model: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, model: String, abbr: String, desc: String) {
        self.abbr_to_desc.insert(abbr.clone(), desc.clone());
        self.model_to_abbr.insert(model.clone(), abbr.clone());
        self.abbr_to_model.insert(abbr.clone(), model.clone());
    }
}

/// Filters messages to only include those created after a specific date
pub async fn get_messages_later_than(
    date: i64,
    messages: Vec<ConversationMessage>,
) -> Result<Vec<ConversationMessage>> {
    let mut messages_later_than_date = Vec::new();
    for msg in messages {
        let timestamp = match &msg {
            ConversationMessage::AI { timestamp, .. } => timestamp,
            ConversationMessage::User { timestamp, .. } => timestamp,
        };
        if let Ok(timestamp) = DateTime::parse_from_rfc3339(timestamp)
            .with_context(|| format!("Failed to parse timestamp: {}", timestamp))
        {
            if timestamp.timestamp_millis() >= date {
                messages_later_than_date.push(msg);
            }
        }
    }

    Ok(messages_later_than_date)
}

/// Filters messages to only include those created after a specific date (alternative implementation)
pub async fn filter_messages_after_date(
    date: i64,
    messages: Vec<ConversationMessage>,
) -> Result<Vec<ConversationMessage>> {
    get_messages_later_than(date, messages).await
}
